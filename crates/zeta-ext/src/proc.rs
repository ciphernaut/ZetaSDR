//! Everything process-shaped about running an external decoder program: finding it
//! (`PATH`, an absolute path, the `ZETA_HANDLER_<PROGRAM>` override: the lookup lives
//! in [`zeta_core::modes::handlers`], re-exported here), its version, the cleared
//! environment, its process group, reaping it, and copying its stderr.
//!
//! **Process policy** (the plugin host's): spawned directly, never through a shell,
//! with a cleared environment ([`cleared_env`]); `kill_on_drop`; on Unix the leader of
//! its own process group ([`Group`]), `SIGKILL`ed whole when the run ends, so nothing a
//! wrapper script started outlives it; stderr copied to ours a line at a time, prefixed
//! `[<mode id>]`, at most [`LINE_MAX`] bytes per piece, the last [`STDERR_TAIL`] lines
//! kept for the failure reason.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{Child, ChildStderr, Command};
use tokio::task::JoinHandle;

pub use zeta_core::modes::handlers::{find_handler, find_program, handler_vars};
pub use zeta_plugin::host::{Group, status_text};

/// Longest stderr or output line handled in one piece.
pub const LINE_MAX: usize = 4096;
/// How long the child has to exit once its input has closed.
pub const EXIT_GRACE: Duration = Duration::from_secs(5);
/// How long a killed child has to be reaped.
pub const REAP: Duration = Duration::from_secs(5);
/// Stderr lines kept for error messages.
pub const STDERR_TAIL: usize = 8;
/// How long a program has to answer `--version` (and again `-V`).
pub const HANDLER_VERSION_TIMEOUT: Duration = Duration::from_secs(2);
/// Longest version text kept.
const VERSION_MAX: usize = 80;

/// The environment a program gets: the plugin host's rule
/// ([`zeta_plugin::host::plugin_env`]: `PASS_ENV` as is, `ZETA_PLUGIN_ENV_<NAME>` as
/// `<NAME>`, `PYTHONUNBUFFERED=1`), then `extra`. Nothing else of ours, no
/// `ZETA_SECRET`, reaches it.
pub fn cleared_env(extra: &[(OsString, OsString)]) -> Vec<(OsString, OsString)> {
    let mut env = zeta_plugin::host::plugin_env(std::env::vars_os());
    env.extend(extra.iter().cloned());
    env
}

/// A child started with [`spawn`]: the process and its group.
pub struct Proc {
    pub child: Child,
    pub group: Group,
}

/// What [`spawn`] connects to the child.
pub struct Pipes {
    pub stdin: bool,
    pub stdout: bool,
}

/// Spawn `program` with `args` and the [`cleared_env`] plus `extra`: stdin and stdout
/// piped when `pipes` says so (else null), stderr always piped, `kill_on_drop`, its own
/// process group on Unix.
pub fn spawn(
    program: &Path,
    args: &[String],
    extra: &[(OsString, OsString)],
    pipes: Pipes,
) -> io::Result<Proc> {
    let pipe = |p: bool| if p { Stdio::piped() } else { Stdio::null() };
    let mut c = Command::new(program);
    c.args(args)
        .env_clear()
        .envs(cleared_env(extra))
        .stdin(pipe(pipes.stdin))
        .stdout(pipe(pipes.stdout))
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    c.process_group(0);
    let child = c.spawn()?;
    // The group id is the leader's pid (process_group(0)).
    let group = Group(child.id());
    Ok(Proc { child, group })
}

/// Aborts the task when dropped, so no path out of a run leaves it running.
pub struct AbortOnDrop<T>(pub JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Kill the child's group and the child if it is still running, and wait for it, at
/// most [`REAP`]. `name` prefixes the complaint when it is not reaped.
pub async fn reap(p: &mut Proc, name: &str) {
    p.group.kill();
    let _ = p.child.start_kill();
    if tokio::time::timeout(REAP, p.child.wait()).await.is_err() {
        eprintln!("[{name}] child not reaped within {} s", REAP.as_secs());
    }
}

/// Append bytes from `r` to `line` up to and including the next `\n`, or until `line`
/// holds `max` bytes. `Ok(true)` when a line (or a `max`-byte piece) is complete,
/// `Ok(false)` at end of input with whatever was left in `line`. Cancel-safe: progress
/// lives in `line`, and the only await is `fill_buf`.
pub async fn read_line<R: AsyncBufRead + Unpin + ?Sized>(
    r: &mut R,
    line: &mut Vec<u8>,
    max: usize,
) -> io::Result<bool> {
    loop {
        let buf = r.fill_buf().await?;
        if buf.is_empty() {
            return Ok(false);
        }
        let (take, end) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (buf.len(), false),
        };
        let room = max.saturating_sub(line.len());
        let (take, end) = if take >= room {
            (room, true)
        } else {
            (take, end)
        };
        line.extend_from_slice(&buf[..take]);
        r.consume(take);
        if end {
            return Ok(true);
        }
    }
}

/// The last stderr lines of a child, shared with its [`copy_stderr`].
pub type Tail = Arc<Mutex<VecDeque<String>>>;

/// Copy the child's stderr to ours, each line prefixed `[<prefix>] `, keeping the last
/// [`STDERR_TAIL`] lines in `tail`. Ends at EOF, when the child exits.
pub async fn copy_stderr(prefix: String, stderr: ChildStderr, tail: Tail) {
    let mut r = BufReader::new(stderr);
    let mut line = Vec::new();
    loop {
        let more = matches!(read_line(&mut r, &mut line, LINE_MAX).await, Ok(true));
        if !line.is_empty() {
            let text = String::from_utf8_lossy(&line).trim_end().to_owned();
            eprintln!("[{prefix}] {text}");
            let mut t = lock(&tail);
            if t.len() == STDERR_TAIL {
                t.pop_front();
            }
            t.push_back(text);
            line.clear();
        }
        if !more {
            break;
        }
    }
}

/// The kept stderr lines joined by `; `, or `no output on stderr`.
pub fn tail_text(tail: &Mutex<VecDeque<String>>) -> String {
    let t = lock(tail);
    if t.is_empty() {
        "no output on stderr".into()
    } else {
        t.iter().cloned().collect::<Vec<_>>().join("; ")
    }
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The program's version text: `<program> --version`, then `-V` when that fails or
/// prints nothing, each bounded by [`HANDLER_VERSION_TIMEOUT`]; the first line with a
/// letter or digit (stdout, else stderr), box-drawing `|` and `-` trimmed off and
/// spaces collapsed, at most 80 characters. Empty when neither answers.
pub async fn handler_version(program: &Path) -> String {
    for flag in ["--version", "-V"] {
        if let Some(v) = version_with(program, flag).await {
            return v;
        }
    }
    String::new()
}

async fn version_with(program: &Path, flag: &str) -> Option<String> {
    let mut p = spawn(
        program,
        &[flag.to_string()],
        &[],
        Pipes {
            stdin: false,
            stdout: true,
        },
    )
    .ok()?;
    let (out, err) = (p.child.stdout.take()?, p.child.stderr.take()?);
    let read = async {
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let (mut out, mut err) = (out.take(64 * 1024), err.take(64 * 1024));
        let _ = tokio::join!(out.read_to_end(&mut o), err.read_to_end(&mut e));
        let status = p.child.wait().await.ok()?;
        Some((status, o, e))
    };
    let got = tokio::time::timeout(HANDLER_VERSION_TIMEOUT, read).await;
    reap(&mut p, "handlers").await;
    let (status, o, e) = got.ok()??;
    if !status.success() {
        return None;
    }
    [o, e]
        .iter()
        .find_map(|b| version_line(&String::from_utf8_lossy(b)))
}

/// The first line of `text` holding a letter or digit, tidied (see [`handler_version`]).
fn version_line(text: &str) -> Option<String> {
    let line = text
        .lines()
        .find(|l| l.chars().any(|c| c.is_alphanumeric()))?;
    let tidy = line
        .trim_matches(|c: char| c == '|' || c == '-' || c.is_whitespace())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    Some(tidy.chars().take(VERSION_MAX).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_line_skips_borders() {
        let text = "-----\n| dump1090 ModeS Receiver      dump1090-fa 8.2-1 |\n| build |\n";
        assert_eq!(
            version_line(text).unwrap(),
            "dump1090 ModeS Receiver dump1090-fa 8.2-1"
        );
        assert_eq!(
            version_line("multimon-ng 1.3.0\n").unwrap(),
            "multimon-ng 1.3.0"
        );
        assert!(version_line("\n---\n").is_none());
    }

    #[test]
    fn cleared_env_passes_only_the_listed_names() {
        let env = cleared_env(&[("EXTRA".into(), "1".into())]);
        let keys: Vec<String> = env
            .iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        for k in &keys {
            assert!(
                zeta_plugin::host::PASS_ENV.contains(&k.as_str())
                    || k == "PYTHONUNBUFFERED"
                    || k == "EXTRA"
                    || std::env::var_os(format!("ZETA_PLUGIN_ENV_{k}")).is_some(),
                "{k} passed through"
            );
        }
        assert!(keys.iter().any(|k| k == "PYTHONUNBUFFERED"));
        assert_eq!(keys.last().map(String::as_str), Some("EXTRA"));
    }
}
