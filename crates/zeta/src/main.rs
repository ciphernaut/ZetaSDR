//! `zeta`: the command-line transport over the ZetaSDR operation set.

mod agents_md;
mod ask;
mod cli;
mod modes_check;
mod node_setup;
mod serve_lock;
mod soapy_worker;
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
    // The node's own child: it serves one device to its parent, whatever else was given.
    if let cli::Command::SoapyWorker(args) = cli.command {
        return soapy_worker::run(args).await;
    }
    node_setup::set_soapy_in_process(cli.soapy_in_process);
    let cli = match transport::dispatch(cli).await {
        Ok(code) => return code,
        Err(cli) => cli,
    };
    let (sources, plugins) = (cli.sources(), cli.plugins.clone());
    let burst_policy = cli.burst_policy();
    let state_dir = match cli.state_dir.map_or_else(node_setup::default_state_dir, Ok) {
        Ok(d) => d,
        Err(e) => return fail(&e),
    };
    if let cli::Command::Ops(cli::OpsCmd::Tail { lines }) = cli.command {
        return tail_log(&state_dir, "ops.log", lines, |_| true);
    }
    if let cli::Command::Transcripts(cli::TranscriptsCmd::Tail { lines, job }) = cli.command {
        return tail_log(&state_dir, "transcripts.log", lines, |line| {
            job.as_deref().is_none_or(|id| {
                serde_json::from_str::<serde_json::Value>(line)
                    .is_ok_and(|v| v["job"] == id || v["input"] == id)
            })
        });
    }
    if (cli.command.mutates()
        || matches!(
            cli.command,
            cli::Command::Modes(cli::ModesCmd::Check { .. })
        ))
        && let Some(pid) = serve_lock::owner(&state_dir)
    {
        let e = OpError::Other {
            message: format!("zeta serve (pid {pid}) owns this state dir"),
        };
        return print(serde_json::to_string_pretty(&e), 2);
    }
    if let cli::Command::Modes(cli::ModesCmd::Check { id, strict }) = cli.command {
        return modes_check::run(state_dir, cli.modes_dir, &plugins, id, strict).await;
    }
    let (serve_args, speech_args, secret) = (
        match &cli.command {
            cli::Command::Serve(a) => a.clone(),
            _ => Default::default(),
        },
        cli.speech.clone(),
        cli.secret.clone(),
    );
    let then_tail = cli.command.then_tail();
    let keep = cli.command.keep();
    let plan = cli.command.plan_filter();
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
    // One backend serves `transcribe` jobs (a one-shot can start or attach one) and, with
    // `serve --listen`, `POST /stt`.
    let speech = match transport::speech_backend(&speech_args, &state_dir).await {
        Ok(s) => s,
        Err(e) => return fail(&e),
    };
    // Only `serve` (op `None`) resumes jobs; a one-shot never launches or fails them.
    let (node, remotes) = match node_setup::open(
        state_dir.clone(),
        &sources,
        &plugins,
        op.is_none(),
        cli.ring_budget_mb,
        burst_policy,
        cli.modes_dir.clone(),
        speech.clone(),
    )
    .await
    {
        Ok(n) => n,
        Err(e) => return fail(&e),
    };
    node_setup::attach_acma(&node, &state_dir, cli.knowledge_acma.as_ref()).await;
    let (jobs, others) = node.load_errors_by_origin();
    for name in jobs {
        eprintln!("zeta: warning: unreadable job record {name}");
    }
    for entry in others {
        eprintln!("zeta: warning: {entry}");
    }
    let code = match op {
        None => transport::serve(node, &serve_args, speech, secret.as_deref()).await,
        Some(op) => one_shot(&node, op, then_tail, keep, plan.as_deref()).await,
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
    plan: Option<&str>,
) -> ExitCode {
    let starts_job = matches!(
        op,
        Op::JobStart { .. } | Op::JobAttach { .. } | Op::MixStart { .. }
    );
    if let Op::IqRead { selection } | Op::Classify { selection } = &op {
        wait_for_ring(node, selection).await;
    }
    match node.apply(op).await {
        Ok(zeta_core::ops::OpResult::Job(id)) if then_tail.is_some() => {
            let t = then_tail.expect("checked");
            then_tail_job(node, id, t, keep).await
        }
        Ok(mut r) => {
            cli::narrow_plans(&mut r, plan);
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
/// `[job_result, frames_result, {"stopped": bool}]`. A job that ended during the tail
/// `stopped` with a verdict (`identify`) is a success too: the job its verdict names
/// (the proved mode's) is what is stopped unless `keep`.
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
    // A controller job (`identify`) that ended `stopped` with a verdict succeeded: what
    // is left to stop is the job its verdict names (a proof's), if any.
    let mut to_stop = Some(id.clone());
    if let Some(rec) = not_running() {
        match verdict_job(&rec) {
            Some(child) => to_stop = child,
            None => return print(serde_json::to_string_pretty(&(job, rec)), 2),
        }
    }
    let stopped = match (keep, to_stop) {
        (true, _) => Ok(false),
        (false, None) => Ok(true),
        (false, Some(j)) => node.apply(Op::JobStop { job: j }).await.map(|_| true),
    };
    // A proof that landed between the check and the stop left its job running.
    if !keep
        && let Some(Some(child)) = node
            .job_list()
            .into_iter()
            .find(|r| r.id == id)
            .and_then(|r| verdict_job(&r))
    {
        let _ = node.apply(Op::JobStop { job: child }).await;
    }
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

/// For a job that ended `stopped` with a verdict in `job_context.identify`, the job the
/// verdict names (`Some(None)` when it names none: an unknown); `None` for any other
/// record.
fn verdict_job(rec: &zeta_core::job::JobRecord) -> Option<Option<zeta_core::types::JobId>> {
    let verdict = &rec.context["identify"]["verdict"];
    if rec.state != zeta_core::job::JobState::Stopped || !verdict.is_object() {
        return None;
    }
    Some(
        verdict["job"]
            .as_str()
            .map(|j| zeta_core::types::JobId(j.into())),
    )
}

/// `--then-tail`: wait up to `STREAM_WAIT` for the job's audio output (a
/// demodulator's; its `squelch` events are not what a tail is for), else its
/// first event output stream (an edge between its stages never), then apply
/// `StreamTail` to it.
async fn tail_job(
    node: &zeta_core::node::Node,
    job: &zeta_core::types::JobId,
    t: cli::ThenTail,
) -> Result<zeta_core::ops::OpResult, OpError> {
    use zeta_core::types::StreamKind;
    let deadline = tokio::time::Instant::now() + STREAM_WAIT;
    let stream = loop {
        if let Ok(s) = node.job_audio_stream(job) {
            break s;
        }
        // An output, never an edge between two of its stages (a watch's spectrum rows).
        if let Some((_, s)) = node
            .job_outputs(job)
            .into_iter()
            .find(|(k, _)| *k == StreamKind::Event)
        {
            break s;
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

/// `ops tail`, `transcripts tail`: print the last `lines` lines of `<stem>.1` and
/// `<stem>` that `filter` keeps, oldest first.
fn tail_log(
    state_dir: &std::path::Path,
    stem: &str,
    lines: usize,
    filter: impl Fn(&str) -> bool,
) -> ExitCode {
    let mut all = Vec::new();
    for name in [format!("{stem}.1"), stem.to_owned()] {
        match std::fs::read_to_string(state_dir.join(&name)) {
            Ok(text) => all.extend(text.lines().filter(|l| filter(l)).map(str::to_owned)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return fail(&format!("{name}: {e}")),
        }
    }
    for line in &all[all.len().saturating_sub(lines)..] {
        println!("{line}");
    }
    ExitCode::SUCCESS
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
