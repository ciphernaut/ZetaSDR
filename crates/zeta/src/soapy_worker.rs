//! `zeta soapy-worker --id ID --args ARGS`: one SoapySDR device served over stdin and
//! stdout to the node that spawned it.
//!
//! A node attaches every `--source-soapy` device through this child (see
//! `node_setup::open_soapy`), so a driver that crashes, hangs or leaks takes down a
//! process the node can respawn instead of the node itself. The child speaks the
//! protocol of `zeta_source_worker::serve`: its exit code tells the node what happened
//! ([`EXIT_NOT_FOUND`] when no device matches, [`EXIT_OPEN_FAILED`] when the device
//! would not open, the last stderr line saying why).
//!
//! [`EXIT_NOT_FOUND`]: zeta_source_worker::serve::EXIT_NOT_FOUND
//! [`EXIT_OPEN_FAILED`]: zeta_source_worker::serve::EXIT_OPEN_FAILED

use std::process::ExitCode;

use crate::cli::SoapyWorkerArgs;

/// Run the worker and end the process with its exit code; never returns.
///
/// The process is ended with [`std::process::exit`] rather than by returning to
/// `main`: tokio's stdin read blocks a pool thread until a line arrives, and after a
/// `shutdown` there may be none, so dropping the runtime would wait on it forever.
/// `serve` has already flushed its frames (or given up on a parent that stopped
/// reading) by the time it returns.
pub async fn run(args: SoapyWorkerArgs) -> ExitCode {
    let code = imp::run(args).await;
    std::process::exit(exit_value(code));
}

/// The number behind `code`: `ExitCode` has no getter, but it compares, and every code
/// the worker gives is a `u8`.
fn exit_value(code: ExitCode) -> i32 {
    (0..=u8::MAX)
        .find(|n| ExitCode::from(*n) == code)
        .map_or(1, i32::from)
}

#[cfg(not(soapy))]
mod imp {
    use std::process::ExitCode;

    use crate::cli::SoapyWorkerArgs;
    use crate::node_setup::NO_SOAPY;

    /// A build without the driver has nothing to serve. A node of such a build never
    /// spawns the worker (it refuses `--source-soapy` first); this answers someone who
    /// runs it by hand.
    pub async fn run(_args: SoapyWorkerArgs) -> ExitCode {
        eprintln!("zeta: soapy-worker: {NO_SOAPY}");
        ExitCode::FAILURE
    }
}

#[cfg(soapy)]
mod imp {
    use std::fs::File;
    use std::os::fd::FromRawFd;
    use std::process::ExitCode;
    use std::sync::Arc;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
    use tokio::signal::unix::{SignalKind, signal};
    use zeta_core::port::Port;
    use zeta_core::types::SourceId;
    use zeta_source_worker::serve::{
        EXIT_NOT_FOUND, EXIT_OPEN_FAILED, redirect_stdout_to_stderr, serve,
    };
    use zeta_source_worker::wire::{Cmd, Request};

    use crate::cli::SoapyWorkerArgs;

    /// Room in the pipe between the stdin forwarder and `serve`: requests are short
    /// lines, so this only has to absorb a burst while `serve` is busy.
    const REQUEST_PIPE: usize = 64 * 1024;

    pub async fn run(args: SoapyWorkerArgs) -> ExitCode {
        die_with_parent();
        // Before anything that can write to stdout: the driver logs there.
        let frames = match redirect_stdout_to_stderr() {
            // SAFETY: the descriptor was just duplicated for us and nothing else owns it.
            Ok(fd) => unsafe { File::from_raw_fd(fd) },
            Err(e) => {
                eprintln!("zeta: soapy-worker: redirect stdout: {e}");
                return ExitCode::from(EXIT_OPEN_FAILED);
            }
        };
        let requests = match requests_until_sigterm() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("zeta: soapy-worker: SIGTERM handler: {e}");
                return ExitCode::from(EXIT_OPEN_FAILED);
            }
        };
        // Enumerate first, on the driver's keys alone: an absent device is exit 3, which
        // the node polls on, not an open failure.
        let found = match zeta_source_soapy::split_zeta_args(&args.args)
            .and_then(|(rest, _)| zeta_source_soapy_driver::find(&rest))
        {
            Ok(found) => found,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(EXIT_OPEN_FAILED);
            }
        };
        let Some(first) = found.first() else {
            eprintln!("device not found: {}", args.args);
            return ExitCode::from(EXIT_NOT_FOUND);
        };
        let driver = first
            .get("driver")
            .cloned()
            .unwrap_or_else(|| "soapy".into());
        let port = match zeta_source_soapy_driver::open(SourceId(args.id.clone()), &args.args) {
            Ok(src) => Arc::new(src) as Arc<dyn Port>,
            Err(e) => {
                // The last stderr line is the reason the node reports.
                eprintln!("{e}");
                return ExitCode::from(EXIT_OPEN_FAILED);
            }
        };
        serve(port, &driver, requests, frames).await
    }

    /// Ask the kernel to send SIGTERM when the parent goes, so a node killed with
    /// SIGKILL does not leave the device held by an orphan. The signal fires when the
    /// thread that spawned the worker exits; the node spawns it from a runtime worker
    /// thread, which lives as long as the node. A parent that died before the call
    /// left us reparented already, so that is checked after it.
    #[cfg(target_os = "linux")]
    fn die_with_parent() {
        // SAFETY: getppid and prctl take no pointers; PR_SET_PDEATHSIG takes a signal
        // number.
        unsafe {
            let parent = libc::getppid();
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            if libc::getppid() != parent {
                std::process::exit(0);
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn die_with_parent() {}

    /// Stdin as `serve` reads it, with SIGTERM turned into a `shutdown` request: a
    /// terminated worker then leaves exactly as one the node shut down, closing the
    /// device on the way out. Lines are forwarded whole, so the injected request never
    /// lands inside one of the node's.
    fn requests_until_sigterm() -> std::io::Result<DuplexStream> {
        let mut term = signal(SignalKind::terminate())?;
        let (mut tx, rx) = tokio::io::duplex(REQUEST_PIPE);
        tokio::spawn(async move {
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            loop {
                tokio::select! {
                    line = lines.next_line() => match line {
                        Ok(Some(mut l)) => {
                            l.push('\n');
                            if tx.write_all(l.as_bytes()).await.is_err() {
                                return;
                            }
                        }
                        // Closing the pipe ends `serve` as a closed stdin does.
                        Ok(None) | Err(_) => return,
                    },
                    _ = term.recv() => {
                        let req = Request { id: 0, cmd: Cmd::Shutdown };
                        if let Ok(mut l) = serde_json::to_string(&req) {
                            l.push('\n');
                            let _ = tx.write_all(l.as_bytes()).await;
                        }
                        return;
                    }
                }
            }
        });
        Ok(rx)
    }
}
