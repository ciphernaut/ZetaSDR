//! `zeta`: the command-line transport over the ZetaSDR operation set.

mod agents_md;
mod ask;
mod cli;
mod node_setup;
mod serve_lock;
mod sources;
mod transport;

use std::process::ExitCode;
use std::time::Duration;

use zeta_core::ops::{Op, OpError};

/// Time a one-shot `job start` keeps the node up so the first blocks flow.
const JOB_START_GRACE: Duration = Duration::from_millis(150);

/// Longest a one-shot ring read waits for the ring to cover its span, and the poll interval.
const RING_WAIT: Duration = Duration::from_secs(5);
const RING_POLL: Duration = Duration::from_millis(20);

/// Longest `--then-tail` waits for the job's streams to appear.
const STREAM_WAIT: Duration = Duration::from_secs(2);

#[tokio::main]
async fn main() -> ExitCode {
    // Usage errors exit 1, not clap's 2: exit 2 is reserved for `OpError`.
    let cli = match cli::Cli::try_parse_checked() {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            };
        }
    };
    if let cli::Command::AgentsMd { check, skill } = cli.command {
        return agents_md::run(check, skill);
    }
    // Device discovery is local to this machine, whatever --node says.
    if let cli::Command::Soapy(cli::SoapyCmd::Find { filter }) = &cli.command {
        return node_setup::soapy_find(filter);
    }
    let cli = match transport::dispatch(cli).await {
        Ok(code) => return code,
        Err(cli) => cli,
    };
    let (sources, plugins) = (cli.sources(), cli.plugins.clone());
    let state_dir = match cli.state_dir.map_or_else(node_setup::default_state_dir, Ok) {
        Ok(d) => d,
        Err(e) => return fail(&e),
    };
    if cli.command.mutates()
        && let Some(pid) = serve_lock::owner(&state_dir)
    {
        let e = OpError::Other {
            message: format!("zeta serve (pid {pid}) owns this state dir"),
        };
        return print(serde_json::to_string_pretty(&e), 2);
    }
    let (serve_args, secret) = (
        match &cli.command {
            cli::Command::Serve(a) => a.clone(),
            _ => Default::default(),
        },
        cli.secret.clone(),
    );
    let then_tail = cli.command.then_tail();
    let keep = cli.command.keep();
    let op = match cli.command.into_op() {
        Ok(op) => op,
        Err(e) => return print(serde_json::to_string_pretty(&e), 2),
    };
    // `serve` holds the state dir until it exits; the guard removes the lock.
    let _lock = match op {
        None => match serve_lock::acquire(&state_dir) {
            Ok(l) => Some(l),
            Err(e) => return fail(&e),
        },
        Some(_) => None,
    };
    // Only `serve` (op `None`) resumes jobs; a one-shot never launches or fails them.
    let (node, remotes) =
        match node_setup::open(state_dir.clone(), &sources, &plugins, op.is_none()).await {
            Ok(n) => n,
            Err(e) => return fail(&e),
        };
    for name in node.load_errors() {
        eprintln!("zeta: warning: unreadable job record {name}");
    }
    let code = match op {
        None => transport::serve(node, &serve_args, secret.as_deref(), state_dir).await,
        Some(op) => one_shot(&node, op, then_tail, keep).await,
    };
    // Remote nodes' `passthrough` jobs end with this process, not with the runtime.
    remotes.close().await;
    code
}

/// Apply `op` on the local node and print its result (or tail the job it starts).
async fn one_shot(
    node: &zeta_core::node::Node,
    op: Op,
    then_tail: Option<cli::ThenTail>,
    keep: bool,
) -> ExitCode {
    let starts_job = matches!(op, Op::JobStart { .. });
    if let Op::IqRead { selection } | Op::Classify { selection } = &op {
        wait_for_ring(node, selection).await;
    }
    match node.apply(op).await {
        Ok(zeta_core::ops::OpResult::Job(id)) if then_tail.is_some() => {
            let t = then_tail.expect("checked");
            then_tail_job(node, id, t, keep).await
        }
        Ok(r) => {
            if starts_job {
                tokio::time::sleep(JOB_START_GRACE).await;
            }
            print(serde_json::to_string_pretty(&r), 0)
        }
        Err(e) => print(serde_json::to_string_pretty(&e), 2),
    }
}

/// `job start --then-tail`: report a job that is not running (before or
/// after the tail) as `[job_result, job_record]` with exit 2; otherwise tail
/// it, stop it unless `keep`, and print
/// `[job_result, frames_result, {"stopped": bool}]`.
async fn then_tail_job(
    node: &zeta_core::node::Node,
    id: zeta_core::types::JobId,
    t: cli::ThenTail,
    keep: bool,
) -> ExitCode {
    use zeta_core::job::JobState;
    let job = zeta_core::ops::OpResult::Job(id.clone());
    let not_running = || {
        node.job_list()
            .into_iter()
            .find(|r| r.id == id)
            .filter(|r| r.state != JobState::Running)
    };
    if let Some(rec) = not_running() {
        return print(serde_json::to_string_pretty(&(job, rec)), 2);
    }
    let tail = tail_job(node, &id, t).await;
    if let Some(rec) = not_running() {
        return print(serde_json::to_string_pretty(&(job, rec)), 2);
    }
    let stopped = if keep {
        Ok(false)
    } else {
        node.apply(Op::JobStop { job: id.clone() })
            .await
            .map(|_| true)
    };
    match (tail, stopped) {
        (Ok(f), Ok(s)) => print(
            serde_json::to_string_pretty(&(job, f, serde_json::json!({"stopped": s}))),
            0,
        ),
        (Ok(f), Err(e)) => print(serde_json::to_string_pretty(&(job, f, e)), 2),
        (Err(e), Ok(s)) => print(
            serde_json::to_string_pretty(&(job, e, serde_json::json!({"stopped": s}))),
            2,
        ),
        (Err(e), Err(e2)) => print(serde_json::to_string_pretty(&(job, e, e2)), 2),
    }
}

/// `--then-tail`: wait up to `STREAM_WAIT` for the job's first event (else
/// audio) stream, then apply `StreamTail` to it.
async fn tail_job(
    node: &zeta_core::node::Node,
    job: &zeta_core::types::JobId,
    t: cli::ThenTail,
) -> Result<zeta_core::ops::OpResult, OpError> {
    use zeta_core::types::StreamKind;
    let deadline = tokio::time::Instant::now() + STREAM_WAIT;
    let stream = loop {
        let mut mine: Vec<_> = node
            .bus()
            .list()
            .into_iter()
            .filter(|s| s.provenance.job.as_ref() == Some(job))
            .collect();
        mine.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        let pick = [StreamKind::Event, StreamKind::Audio]
            .into_iter()
            .find_map(|k| mine.iter().find(|s| s.kind == k));
        if let Some(s) = pick {
            break s.id.clone();
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(OpError::Other {
                message: format!("job {} has no event or audio stream to tail", job.0),
            });
        }
        tokio::time::sleep(RING_POLL).await;
    };
    let op = Op::StreamTail {
        stream,
        max_frames: t.max_frames,
        timeout_ms: t.timeout_ms,
    };
    node.apply(op).await
}

/// Poll until the source's ring covers the selection's span end, or `RING_WAIT` passes
/// (then the op runs anyway and reports the normal ring error).
async fn wait_for_ring(node: &zeta_core::node::Node, sel: &zeta_core::types::Selection) {
    let deadline = tokio::time::Instant::now() + RING_WAIT;
    loop {
        let covered = node.ring(&sel.source).is_some_and(|r| {
            r.read()
                .unwrap()
                .latest()
                .is_some_and(|t| t.0 >= sel.span.end_ns.0)
        });
        if covered || tokio::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(RING_POLL).await;
    }
}

/// Print serialised JSON on stdout and return `code`.
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
