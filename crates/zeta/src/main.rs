//! `zeta`: the command-line transport over the ZetaSDR operation set.

mod cli;
mod node_setup;

use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use zeta_core::ops::Op;

/// Time a one-shot `job start` keeps the node up so the first blocks flow.
const JOB_START_GRACE: Duration = Duration::from_millis(150);

#[tokio::main]
async fn main() -> ExitCode {
    // Usage errors exit 1, not clap's 2: exit 2 is reserved for `OpError`.
    let cli = match cli::Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { ExitCode::FAILURE } else { ExitCode::SUCCESS };
        }
    };
    let op = match cli.command.into_op() {
        Ok(op) => op,
        Err(e) => return print(serde_json::to_string_pretty(&e), 2),
    };
    let state_dir = match cli.state_dir.map_or_else(node_setup::default_state_dir, Ok) {
        Ok(d) => d,
        Err(e) => return fail(&e),
    };
    let node = match node_setup::open(state_dir, &cli.source_files).await {
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
    match node.apply(op).await {
        Ok(r) => {
            if starts_job {
                tokio::time::sleep(JOB_START_GRACE).await;
            }
            print(serde_json::to_string_pretty(&r), 0)
        }
        Err(e) => print(serde_json::to_string_pretty(&e), 2),
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
