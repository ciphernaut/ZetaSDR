//! Network transports of the CLI: `--node` (client) and `serve --listen` (server).

use std::process::ExitCode;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use zeta_client::NodeClient;
use zeta_core::node::Node;
use zeta_ws::addr::parse_listen;
use zeta_ws::auth::Secret;
use zeta_ws::{Limits, ServerConfig, bind};

use std::path::{Path, PathBuf};

use zeta_speech::{NodeBackend, SocketBackend, SpeechBackend};

use crate::cli::{Cli, Command, ModesCmd, SPEECH_EXCLUSIVE, ServeArgs, SpeechArgs, StreamCmd};
use crate::{agents_md, ask, node_setup};

const MIN_SECRET_LEN: usize = 12;

/// Where `--stt-node`'s secret comes from; never a flag, so it stays out of `ps`.
const STT_NODE_SECRET_ENV: &str = "ZETA_STT_NODE_SECRET";

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
    if matches!(cli.command, Command::Ask(_)) {
        let (sources, plugins) = (cli.sources(), cli.plugins.clone());
        let source_positions = cli.source_positions();
        let burst_policy = cli.burst_policy();
        let Command::Ask(args) = cli.command else {
            unreachable!("matched above")
        };
        let state_dir = match cli.state_dir.map_or_else(node_setup::default_state_dir, Ok) {
            Ok(d) => d,
            Err(e) => return Ok(usage(&e)),
        };
        let speech = match speech_backend(&cli.speech, &state_dir).await {
            Ok(s) => s,
            Err(e) => return Ok(usage(&e)),
        };
        return Ok(ask::run(
            args,
            None,
            state_dir,
            &sources,
            source_positions,
            &plugins,
            cli.ring_budget_mb,
            burst_policy,
            cli.modes_dir,
            cli.themes_dir,
            cli.sets_dir,
            speech,
            cli.knowledge_acma,
        )
        .await);
    }
    Err(cli)
}

/// Apply the command on the node at `url`.
async fn remote(cli: Cli, url: &str) -> ExitCode {
    if matches!(cli.command, Command::Serve(_)) {
        return usage("serve does not take --node");
    }
    if matches!(cli.command, Command::Ops(_)) {
        return usage("ops tail reads the state directory: not supported with --node");
    }
    if matches!(cli.command, Command::Modes(ModesCmd::Check { .. })) {
        return usage(
            "modes check runs the rows' tests on its own node: not supported with --node",
        );
    }
    if matches!(cli.command, Command::Transcripts(_)) {
        return usage("transcripts tail reads the state directory: not supported with --node");
    }
    if matches!(cli.command, Command::Events(_)) {
        return usage("events tail reads the state directory: not supported with --node");
    }
    if cli.command.then_tail().is_some() {
        return usage("--then-tail is not supported with --node yet");
    }
    if cli.configures_local_node() {
        return usage(
            "sources, plugins, modes, themes and sets are configured on the serving node: drop --source-*, --plugin, --state-dir, --modes-dir, --themes-dir, --sets-dir, --knowledge-acma and --bursts-keep-* with --node (their environment variables are ignored)",
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
        return ask::run(
            args,
            Some(client),
            PathBuf::new(),
            &[],
            Default::default(),
            &[],
            None,
            Default::default(),
            None,
            None,
            None,
            None,
            None,
        )
        .await;
    }
    if let Command::Stream(StreamCmd::Save {
        stream,
        path,
        timeout_ms,
    }) = cli.command
    {
        return stream_save(&client, &stream, &path, timeout_ms, cli.json).await;
    }
    let plan = cli.command.plan_filter();
    let mut op = match cli.command.into_op() {
        Ok(Some(op)) => op,
        Ok(None) => return usage("this command cannot run on a remote node"),
        Err(e) => return print(serde_json::to_string_pretty(&e), 2),
    };
    // A `set start` without `--radio`: every role of the set, read from the node, as null.
    if crate::cli::wants_roles(&op)
        && let Ok(Ok(zeta_core::ops::OpResult::Sets(sets))) =
            client.apply(&zeta_core::ops::Op::SetList {}).await
    {
        crate::cli::null_roles(&mut op, &sets);
    }
    match client.apply(&op).await {
        Ok(Ok(mut r)) => {
            crate::cli::narrow_plans(&mut r, plan.as_deref());
            print(serde_json::to_string_pretty(&r), 0)
        }
        Ok(Err(e)) => print(serde_json::to_string_pretty(&e), 2),
        Err(e) => usage(&e.to_string()),
    }
}

/// `stream save`: write the stream's next video frame to `path` as the PNG the node sends
/// under the `sharp` profile (full width).
async fn stream_save(
    client: &NodeClient,
    stream: &str,
    path: &Path,
    timeout_ms: u64,
    json: bool,
) -> ExitCode {
    use std::time::Duration;
    use zeta_client::Received;
    use zeta_core::types::{StreamId, StreamKind};

    let mut sub = match client
        .subscribe_video(&StreamId(stream.to_string()), "sharp")
        .await
    {
        Ok(s) => s,
        Err(e @ zeta_client::ClientError::NotVideo { .. }) => {
            return usage(&format!(
                "{e}; IQ is saved with `zeta iq read`, audio has no still"
            ));
        }
        Err(e) => return usage(&e.to_string()),
    };
    let wait = async {
        loop {
            match sub.next().await {
                Some(Ok(Received::Frame { header, body })) if header.kind == StreamKind::Video => {
                    return Ok((header, body));
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.to_string()),
                None => return Err("the stream ended before a video frame".to_string()),
            }
        }
    };
    let (header, png) = match tokio::time::timeout(Duration::from_millis(timeout_ms), wait).await {
        Err(_) => return usage(&format!("no video frame in {timeout_ms} ms")),
        Ok(Err(e)) => return usage(&e),
        Ok(Ok(got)) => got,
    };
    let dim = |k: &str| header.payload[k].as_u64().unwrap_or(0) as u32;
    let (width, height) = (dim("width"), dim("height"));
    if let Err(e) = std::fs::write(path, &png) {
        return usage(&format!("{}: {e}", path.display()));
    }
    if json {
        print(
            serde_json::to_string_pretty(&serde_json::json!({
                "saved": path, "width": width, "height": height, "bytes": png.len()
            })),
            0,
        )
    } else {
        println!(
            "saved {} ({width}x{height}, {} bytes)",
            path.display(),
            png.len()
        );
        ExitCode::SUCCESS
    }
}

/// Resolve on Ctrl-C (SIGINT) or, on unix, SIGTERM (`systemctl stop`), so either one
/// shuts `serve` down gracefully and the caller still closes remote nodes' jobs.
async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate())?;
        tokio::select! {
            r = tokio::signal::ctrl_c() => r,
            _ = term.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}

/// Run `serve`: wait for Ctrl-C or SIGTERM, also serving the API when `--listen` is given
/// (`POST /stt` on `speech`).
pub async fn serve(
    node: Node,
    args: &ServeArgs,
    speech: Option<Arc<dyn SpeechBackend>>,
    secret: Option<&str>,
) -> ExitCode {
    let Some(listen) = &args.listen else {
        if std::env::args().any(|a| a.starts_with("--llm-")) {
            eprintln!("zeta: warning: --llm-url/--llm-model are ignored without --listen");
        }
        eprintln!("zeta: serving; Ctrl-C to stop");
        if let Err(e) = shutdown_signal().await {
            return usage(&format!("waiting for Ctrl-C or SIGTERM: {e}"));
        }
        return ExitCode::SUCCESS;
    };
    let listen = match parse_listen(listen) {
        Ok(a) => a,
        Err(e) => return usage(&e),
    };
    let llm = match crate::ask::llm_config(&args.llm) {
        Ok(l) => l,
        Err(e) => return usage(&e),
    };
    let ask_model = llm.as_ref().map(|l| l.model.clone());
    let secret_text = secret.unwrap_or_default();
    let secret = match Secret::new(secret_text) {
        Ok(s) => s,
        Err(e) => return usage(&e),
    };
    let speech_name = speech.as_ref().map(|s| s.name().to_string());
    let cfg = ServerConfig {
        listen,
        secret,
        agents_md: agents_md::rendering(),
        limits: Limits::default(),
        llm,
        speech,
    };
    let server = match bind(Arc::new(node), cfg).await {
        Ok(s) => s,
        Err(e) => return usage(&e.to_string()),
    };
    eprintln!("zeta: listening on {}", server.local_addr());
    if let Some(model) = &ask_model {
        eprintln!("zeta: POST /ask runs model {model}");
    }
    if let Some(name) = &speech_name {
        eprintln!("zeta: speech backend: {name}");
    }
    if secret_text.chars().count() < MIN_SECRET_LEN {
        eprintln!("zeta: warning: secret shorter than {MIN_SECRET_LEN} characters");
    }
    let stop = CancellationToken::new();
    let on_signal = stop.clone();
    tokio::spawn(async move {
        if let Err(e) = shutdown_signal().await {
            eprintln!("zeta: waiting for Ctrl-C or SIGTERM: {e}; stopping");
        }
        on_signal.cancel();
    });
    match server.run(stop).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => usage(&e.to_string()),
    }
}

/// The speech backend the flags name: the node at `--stt-node` (its secret from
/// `ZETA_STT_NODE_SECRET`), else the daemon at `--stt-socket` or the default socket, staging
/// its WAVs in `<state_dir>/stt`; `None` when neither is given and no default socket exists.
pub async fn speech_backend(
    args: &SpeechArgs,
    state_dir: &Path,
) -> Result<Option<Arc<dyn SpeechBackend>>, String> {
    match (&args.stt_socket, &args.stt_node) {
        (Some(_), Some(_)) => Err(SPEECH_EXCLUSIVE.to_string()),
        (None, Some(url)) => {
            let secret = std::env::var(STT_NODE_SECRET_ENV)
                .ok()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("--stt-node requires {STT_NODE_SECRET_ENV}"))?;
            let backend = NodeBackend::new(url, &secret)
                .await
                .map_err(|e| format!("--stt-node: {e}"))?;
            Ok(Some(Arc::new(backend)))
        }
        (socket, None) => {
            let Some(path) = socket.clone().or_else(default_stt_socket) else {
                return Ok(None);
            };
            let timeout = Limits::default().stt_timeout;
            let backend = SocketBackend::new(path, state_dir.join("stt"), timeout).await;
            Ok(Some(Arc::new(backend)))
        }
    }
}

/// `$XDG_RUNTIME_DIR/voiced.sock`, when it exists.
fn default_stt_socket() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join("voiced.sock");
    path.exists().then_some(path)
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
