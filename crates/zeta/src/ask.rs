//! `zeta ask`: let a local model call the node's operations to satisfy a request.

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use zeta_agent::{
    Allow, Caller, Executor, FocusFrom, LlmConfig, LocalExecutor, RadioContext, RemoteExecutor,
    build_agent,
};
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
    source_positions: std::collections::BTreeMap<
        zeta_core::types::SourceId,
        zeta_core::types::SourcePosition,
    >,
    plugins: &[PluginSpec],
    ring_budget_mb: Option<u64>,
    burst_policy: zeta_core::bursts::BurstPolicy,
    modes_dir: Option<PathBuf>,
    themes_dir: Option<PathBuf>,
    sets_dir: Option<PathBuf>,
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
    let allow = allow_of(&args);
    let radio = radio_context(&args);
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
                radio,
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
                source_positions,
                plugins,
                false,
                ring_budget_mb,
                burst_policy,
                modes_dir,
                themes_dir,
                sets_dir,
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
                radio,
                &request,
            )
            .await;
            remotes.close().await;
            code
        }
    }
}

/// What the run may do beyond reading and starting, from the `--allow-*` flags.
pub fn allow_of(args: &AskArgs) -> Allow {
    Allow {
        stop_jobs: args.allow_stop,
        replace: args.allow_replace,
        delete_captures: args.allow_delete,
        accept_signatures: args.allow_signatures,
    }
}

/// The agent's radio context from `--radio`, `--radio-policy` and `--radio-prefer`: none
/// of the three is no context (`free-first`); an omitted policy is `focus` with a radio.
pub fn radio_context(args: &AskArgs) -> RadioContext {
    if args.radio.is_none() && args.radio_policy.is_none() && args.radio_prefer.is_empty() {
        return RadioContext::default();
    }
    let focus = args.radio.clone().map(zeta_core::types::SourceId);
    let from = if focus.is_some() {
        FocusFrom::Flag
    } else {
        FocusFrom::None
    };
    let prefer = args
        .radio_prefer
        .iter()
        .cloned()
        .map(zeta_core::types::SourceId)
        .collect();
    RadioContext::from_parts(focus, from, args.radio_policy, prefer)
}

#[allow(clippy::too_many_arguments)]
async fn drive<E: Executor>(
    cfg: LlmConfig,
    exec: E,
    schema: &serde_json::Value,
    md: &str,
    max_turns: u32,
    allow: Allow,
    radio: RadioContext,
    request: &str,
) -> ExitCode {
    let agent = match build_agent(
        cfg,
        exec,
        schema,
        md,
        max_turns,
        allow,
        None,
        radio,
        Caller::cli(),
    ) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    use zeta_agent::{FocusFrom, RadioPolicy};

    fn parse(flags: &[&str]) -> AskArgs {
        let mut args = vec!["zeta", "ask", "--llm-url", "http://llm", "--llm-model", "m"];
        args.extend_from_slice(flags);
        args.push("what sources?");
        match crate::cli::Cli::try_parse_from(args).unwrap().command {
            crate::cli::Command::Ask(a) => a,
            c => panic!("ask: {c:?}"),
        }
    }

    fn id(s: &str) -> zeta_core::types::SourceId {
        zeta_core::types::SourceId(s.into())
    }

    #[test]
    fn ask_radio_flags_parse_and_read_the_environment() {
        // No flag, no variable: no context, today's free-first run.
        assert_eq!(radio_context(&parse(&[])), RadioContext::default());
        // Only --radio: the policy is focus, the focus comes from the flag.
        let c = radio_context(&parse(&["--radio", "b210"]));
        assert_eq!(c.policy, RadioPolicy::Focus);
        assert_eq!(c.focus, Some(id("b210")));
        assert_eq!(c.focus_from, FocusFrom::Flag);
        // All three flags.
        let c = radio_context(&parse(&[
            "--radio",
            "b210",
            "--radio-policy",
            "focus-fallback",
            "--radio-prefer",
            "b210,hackrf",
        ]));
        assert_eq!(c.policy, RadioPolicy::FocusFallback);
        assert_eq!(c.prefer, vec![id("b210"), id("hackrf")]);
        // The variables are read at parse time. Only this test touches them, and it
        // runs them in sequence after the flag cases above.
        // SAFETY: no other test in this binary reads or writes ZETA_RADIO*.
        unsafe {
            std::env::set_var("ZETA_RADIO_POLICY", "ask");
            std::env::set_var("ZETA_RADIO_PREFER", "hackrf,b210");
        }
        let c = radio_context(&parse(&[]));
        unsafe {
            std::env::remove_var("ZETA_RADIO_POLICY");
            std::env::remove_var("ZETA_RADIO_PREFER");
        }
        assert_eq!(c.policy, RadioPolicy::Ask);
        assert_eq!(c.focus, None);
        assert_eq!(c.prefer, vec![id("hackrf"), id("b210")]);
    }

    fn allow(flags: &[&str]) -> Allow {
        let mut args = vec!["zeta", "ask", "--llm-url", "http://llm", "--llm-model", "m"];
        args.extend_from_slice(flags);
        args.push("what sources?");
        match crate::cli::Cli::try_parse_from(args).unwrap().command {
            crate::cli::Command::Ask(a) => allow_of(&a),
            c => panic!("ask: {c:?}"),
        }
    }

    #[test]
    fn ask_allow_signatures_allows_accepting_signatures_only() {
        assert_eq!(allow(&[]), Allow::default());
        assert_eq!(
            allow(&["--allow-signatures"]),
            Allow {
                accept_signatures: true,
                ..Allow::default()
            }
        );
        assert_eq!(
            allow(&["--allow-delete", "--allow-stop"]),
            Allow {
                stop_jobs: true,
                delete_captures: true,
                ..Allow::default()
            }
        );
    }
}
