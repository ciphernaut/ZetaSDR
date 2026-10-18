//! Network transports of the CLI: `--node` (client) and `serve --listen` (server).

use std::process::ExitCode;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use zeta_client::NodeClient;
use zeta_core::node::Node;
use zeta_ws::addr::parse_listen;
use zeta_ws::auth::Secret;
use zeta_ws::{Limits, ServerConfig, bind};

use std::path::PathBuf;

use crate::cli::{Cli, Command, ServeArgs};
use crate::{agents_md, ask, node_setup};

const MIN_SECRET_LEN: usize = 12;

/// Handle the commands that involve a network transport, returning the exit code;
/// give `cli` back untouched for everything else.
pub async fn dispatch(cli: Cli) -> Result<ExitCode, Cli> {
    if let Some(url) = cli.node.clone() {
        return Ok(remote(cli, &url).await);
    }
    if let Command::Serve(args) = &cli.command
        && let Some(listen) = &args.listen
    {
        if let Err(e) = parse_listen(listen) {
            return Ok(usage(&e));
        }
        if cli.secret.as_deref().is_none_or(str::is_empty) {
            return Ok(usage("--listen requires --secret or ZETA_SECRET"));
        }
    }
    if let Command::Ask(args) = cli.command {
        let state_dir = match cli.state_dir.map_or_else(node_setup::default_state_dir, Ok) {
            Ok(d) => d,
            Err(e) => return Ok(usage(&e)),
        };
        return Ok(ask::run(args, None, state_dir, &cli.source_files).await);
    }
    Err(cli)
}

/// Apply the command on the node at `url`.
async fn remote(cli: Cli, url: &str) -> ExitCode {
    if matches!(cli.command, Command::Serve(_)) {
        return usage("serve does not take --node");
    }
    if cli.command.then_tail().is_some() {
        return usage("--then-tail is not supported with --node yet");
    }
    if !cli.source_files.is_empty() || cli.state_dir.is_some() {
        return usage(
            "--source-file and --state-dir belong to the serving node; drop them with --node",
        );
    }
    let Some(secret) = cli.secret.as_deref().filter(|s| !s.is_empty()) else {
        return usage("--node requires --secret or ZETA_SECRET");
    };
    let client = match NodeClient::new(url, secret) {
        Ok(c) => c,
        Err(e) => return usage(&e.to_string()),
    };
    if let Command::Ask(args) = cli.command {
        return ask::run(args, Some(client), PathBuf::new(), &[]).await;
    }
    let op = match cli.command.into_op() {
        Ok(Some(op)) => op,
        Ok(None) => return usage("this command cannot run on a remote node"),
        Err(e) => return print(serde_json::to_string_pretty(&e), 2),
    };
    match client.apply(&op).await {
        Ok(Ok(r)) => print(serde_json::to_string_pretty(&r), 0),
        Ok(Err(e)) => print(serde_json::to_string_pretty(&e), 2),
        Err(e) => usage(&e.to_string()),
    }
}

/// Run `serve`: wait for Ctrl-C, also serving the API when `--listen` is given.
pub async fn serve(node: Node, args: &ServeArgs, secret: Option<&str>) -> ExitCode {
    let Some(listen) = &args.listen else {
        eprintln!("zeta: serving; Ctrl-C to stop");
        if let Err(e) = tokio::signal::ctrl_c().await {
            return usage(&format!("waiting for Ctrl-C: {e}"));
        }
        return ExitCode::SUCCESS;
    };
    let listen = match parse_listen(listen) {
        Ok(a) => a,
        Err(e) => return usage(&e),
    };
    let secret_text = secret.unwrap_or_default();
    let secret = match Secret::new(secret_text) {
        Ok(s) => s,
        Err(e) => return usage(&e),
    };
    let cfg = ServerConfig {
        listen,
        secret,
        agents_md: agents_md::rendering(),
        limits: Limits::default(),
    };
    let server = match bind(Arc::new(node), cfg).await {
        Ok(s) => s,
        Err(e) => return usage(&e.to_string()),
    };
    eprintln!("zeta: listening on {}", server.local_addr());
    if secret_text.chars().count() < MIN_SECRET_LEN {
        eprintln!("zeta: warning: secret shorter than {MIN_SECRET_LEN} characters");
    }
    let stop = CancellationToken::new();
    let on_ctrl_c = stop.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        on_ctrl_c.cancel();
    });
    match server.run(stop).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => usage(&e.to_string()),
    }
}

/// Print serialised JSON on stdout and return `code`.
fn print(json: serde_json::Result<String>, code: u8) -> ExitCode {
    match json {
        Ok(s) => {
            println!("{s}");
            ExitCode::from(code)
        }
        Err(e) => usage(&format!("serialising output: {e}")),
    }
}

/// Report `msg` on stderr and exit 1.
fn usage(msg: &str) -> ExitCode {
    eprintln!("zeta: {msg}");
    ExitCode::FAILURE
}
