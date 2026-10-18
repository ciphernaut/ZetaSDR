//! `zeta`: the command-line transport over the ZetaSDR operation set.

mod agents_md;
mod cli;
mod node_setup;
mod serve_lock;

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
    if let cli::Command::AgentsMd { check } = cli.command {
        return agents_md::run(check);
    }
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
    let then_tail = cli.command.then_tail();
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
    let node = match node_setup::open(state_dir, &cli.source_files, op.is_none()).await {
        Ok(n) => n,
        Err(e) => return fail(&e),
    };
    for name in node.load_errors() {
        eprintln!("zeta: warning: unreadable job record {name}");
    }
    let Some(op) = op else {
        eprintln!("zeta: serving; Ctrl-C to stop");
        if let Err(e) = tokio::signal::ctrl_c().await {
            return fail(&format!("waiting for Ctrl-C: {e}"));
        }
        return ExitCode::SUCCESS;
    };
    let starts_job = matches!(op, Op::JobStart { .. });
    if let Op::IqRead { selection } | Op::Classify { selection } = &op {
        wait_for_ring(&node, selection).await;
    }
    match node.apply(op).await {
        Ok(zeta_core::ops::OpResult::Job(id)) if then_tail.is_some() => {
            let t = then_tail.expect("checked");
            let job = zeta_core::ops::OpResult::Job(id.clone());
            match tail_job(&node, &id, t).await {
                Ok(f) => print(serde_json::to_string_pretty(&(job, f)), 0),
                Err(e) => print(serde_json::to_string_pretty(&(job, e)), 2),
            }
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
