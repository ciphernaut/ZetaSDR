//! The host side of plugin protocol 1 ([`crate::wire`]): an [`ExternalStage`] is a `Stage`
//! whose work is done by a child process.
//!
//! **Lifecycle.** [`probe`] spawns the program once at registration, reads its `hello`
//! (within [`HELLO_TIMEOUT`]), sends `stop`, waits [`STOP_GRACE`] and kills it if it is still
//! alive; the `hello` becomes the stage's [`StageSpec`]. Each [`Stage::run`] spawns a fresh
//! process, requires the same `hello`, sends `run`, then pumps frames until the input closes
//! and the plugin exits, the plugin fails, or the job is cancelled (`stop`, then
//! [`STOP_GRACE`], then kill). Every process is spawned directly (never through a shell)
//! with `kill_on_drop`, and every path out of `probe` or `run` waits for (reaps) it.
//!
//! **Containment.** A plugin is untrusted and may be a wrapper (a Python script) that starts
//! processes of its own, so on Unix each plugin leads its own process group
//! (`process_group(0)`) and every kill, and the end of every `probe` or `run`, sends `SIGKILL`
//! to the whole group: nothing it started outlives it. Its environment is cleared and only
//! [`PASS_ENV`] is passed through, plus `PYTHONUNBUFFERED=1` and every host variable
//! `ZETA_PLUGIN_ENV_<NAME>` as `<NAME>` (so the node's credentials, `ZETA_SECRET` among them,
//! never reach a plugin). Its working directory is the program's own directory, and the
//! program must be given as an absolute path (no `PATH` lookup).
//!
//! **Pumping.** A reader task decodes stdout into a queue of [`OUT_QUEUE`] messages; a
//! writer task drains a queue of [`IN_QUEUE`] encoded messages into stdin. The run loop never
//! awaits stdin: an input frame that finds the writer queue full is dropped and counted, and
//! the count goes to the plugin as one `{"type":"lagged","missed":N}` as soon as there is
//! room (before any later frame), so a plugin that stops reading can slow nothing but itself.
//! A broadcast `Lagged(n)` on the input is reported the same way. Control messages the host
//! must deliver (`lagged`, `closed`) are never dropped; they wait for room.
//!
//! **Untrusted output.** Plugin output is checked before anything is published: a frame must
//! name one of the run's output streams and carry that stream's kind; its `source` and `job`
//! are replaced by the run's, its `channel` and span kept. Anything else fails the stage with
//! a message naming the plugin, never a panic.

use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageError, StageSpec};
use zeta_core::stream::{Frame, StreamBus};
use zeta_core::types::{JobId, Provenance, SourceId, StreamId, StreamKind};

use crate::wire::{
    Control, Hello, MAX_CONTROL, Message, OutPort, PROTOCOL, PortKind, Run, WireError,
    check_strict_schema, encode_control, encode_frame, kind_name, read_message,
};

/// How long a plugin has to send `hello` after it is spawned.
pub const HELLO_TIMEOUT: Duration = Duration::from_millis(5000);
/// How long a plugin has to exit after `stop` (or after its stdout ends) before it is killed.
pub const STOP_GRACE: Duration = Duration::from_millis(2000);
/// Decoded plugin messages buffered between the stdout reader and the run loop.
pub const OUT_QUEUE: usize = 64;
/// Encoded messages buffered between the run loop and the stdin writer.
pub const IN_QUEUE: usize = 16;
/// Host variables passed through to a plugin unchanged, when set.
pub const PASS_ENV: [&str; 7] = [
    "PATH",
    "HOME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "PYTHONPATH",
];
/// Host variables `ZETA_PLUGIN_ENV_<NAME>` reach the plugin as `<NAME>`.
pub const PLUGIN_ENV_PREFIX: &str = "ZETA_PLUGIN_ENV_";
/// Longest stderr line copied to the host's stderr in one piece.
const STDERR_LINE: usize = 4096;

/// A plugin program and its arguments, spawned directly (no shell).
#[derive(Clone, Debug, PartialEq)]
pub struct PluginCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// Why [`probe`] refused a plugin.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    /// The program is not an absolute path.
    #[error("plugin {kind}: program {program} is not an absolute path")]
    RelativeProgram { kind: String, program: String },
    #[error("plugin {kind}: cannot start {program}: {error}")]
    Spawn {
        kind: String,
        program: String,
        error: String,
    },
    /// No usable `hello`: none within [`HELLO_TIMEOUT`], a wire error, or an early exit.
    #[error("plugin {kind}: {reason}")]
    Hello { kind: String, reason: String },
    /// A `hello` that does not describe an acceptable stage.
    #[error("plugin {kind} refused: {reason}")]
    Refused { kind: String, reason: String },
}

/// A stage implemented by an external process speaking [`crate::wire`].
pub struct ExternalStage {
    spec: StageSpec,
    hello: Hello,
    cmd: PluginCommand,
}

impl ExternalStage {
    /// The program this stage runs.
    pub fn command(&self) -> &PluginCommand {
        &self.cmd
    }
}

/// Start `cmd` once, read and check its `hello`, stop it, and build the stage it describes,
/// registered as `kind`.
///
/// `cmd.program` must be an absolute path. The `hello` must arrive within [`HELLO_TIMEOUT`] and have `protocol` [`PROTOCOL`], `kind`
/// equal to `kind`, at least one input, only `iq`, `audio`, `text` or `event` ports, and a
/// `params_schema` passing [`check_strict_schema`].
///
/// `StageSpec::kind` is `&'static str`, so the kind string is leaked: one small allocation per
/// registered plugin, made once at startup.
pub async fn probe(kind: &str, cmd: PluginCommand) -> Result<ExternalStage, PluginError> {
    if !cmd.program.is_absolute() {
        return Err(PluginError::RelativeProgram {
            kind: kind.into(),
            program: cmd.program.display().to_string(),
        });
    }
    let mut p = Proc::spawn(kind, &cmd).map_err(|error| PluginError::Spawn {
        kind: kind.into(),
        program: cmd.program.display().to_string(),
        error,
    })?;
    let hello = match read_hello(&mut p.stdout).await {
        Ok(h) => h,
        Err(reason) => {
            let _ = kill_now(&mut p.child).await;
            return Err(PluginError::Hello {
                kind: kind.into(),
                reason,
            });
        }
    };
    // A plugin that does not read stdin must not hold the probe up.
    let stop = encode_control(&Control::Stop);
    let _ = tokio::time::timeout(STOP_GRACE, p.stdin.write_all(&stop)).await;
    drop(p.stdin);
    let _ = finish(&mut p.child).await;

    let params_schema = check_hello(kind, &hello).map_err(|reason| PluginError::Refused {
        kind: kind.into(),
        reason,
    })?;
    Ok(ExternalStage {
        spec: StageSpec {
            kind: Box::leak(kind.to_owned().into_boxed_str()),
            inputs: hello.inputs.clone(),
            outputs: hello.outputs.clone(),
            params_schema,
        },
        hello,
        cmd,
    })
}

/// The stage spec's schema if `h` is acceptable for `kind`, otherwise why not.
fn check_hello(kind: &str, h: &Hello) -> Result<schemars::Schema, String> {
    if h.protocol != PROTOCOL {
        return Err(format!(
            "it speaks protocol {}, the host speaks {PROTOCOL}",
            h.protocol
        ));
    }
    if h.kind != kind {
        return Err(format!(
            "its hello names kind {:?} but it is registered as {kind:?}",
            h.kind
        ));
    }
    if h.inputs.is_empty() {
        return Err("its hello declares no inputs".into());
    }
    // `run.inputs` names one kind (the stage has one input port), so the
    // kind declared must be the kind in use.
    if h.inputs.len() > 1 {
        return Err("multiple input kinds are not supported yet".into());
    }
    for (side, kinds) in [("input", &h.inputs), ("output", &h.outputs)] {
        if let Some(k) = kinds
            .iter()
            .find(|k| matches!(k, StreamKind::Video | StreamKind::Sink))
        {
            return Err(format!(
                "it declares a {} {side}; protocol 1 carries iq, audio, text and event",
                kind_name(*k)
            ));
        }
    }
    check_strict_schema(&h.params_schema)?;
    schemars::Schema::try_from(h.params_schema.clone())
        .map_err(|e| format!("params_schema is not a schema: {e}"))
}

/// A spawned plugin: stderr is already being copied to the host's stderr.
struct Proc {
    child: Kid,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Proc {
    fn spawn(kind: &str, cmd: &PluginCommand) -> Result<Proc, String> {
        let mut c = Command::new(&cmd.program);
        c.args(&cmd.args)
            .env_clear()
            .envs(plugin_env(std::env::vars_os()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(dir) = cmd.program.parent() {
            c.current_dir(dir);
        }
        #[cfg(unix)]
        c.process_group(0);
        let mut child = c.spawn().map_err(|e| e.to_string())?;
        // The group id is the leader's pid (process_group(0)).
        let group = Group(child.id());
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            // All three were requested as pipes; tokio always provides them.
            return Err("stdio pipes missing".into());
        };
        // Ends at stderr EOF, when the plugin exits.
        tokio::spawn(copy_stderr(kind.to_owned(), stderr));
        Ok(Proc {
            child: Kid { child, group },
            stdin,
            // Control lines are read a byte at a time: buffer the pipe.
            stdout: BufReader::new(stdout),
        })
    }
}

/// Copy the plugin's stderr to ours, a line at a time, prefixed `[plugin <kind>] `. Lines
/// longer than [`STDERR_LINE`] are split, so a plugin cannot make the host buffer without
/// limit.
async fn copy_stderr(kind: String, stderr: ChildStderr) {
    let mut r = BufReader::new(stderr);
    let mut line = Vec::new();
    let print = |line: &[u8]| {
        eprintln!(
            "[plugin {kind}] {}",
            String::from_utf8_lossy(line).trim_end()
        );
    };
    loop {
        let buf = match r.fill_buf().await {
            Ok(b) if !b.is_empty() => b,
            _ => break,
        };
        let (take, end) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (buf.len(), false),
        };
        line.extend_from_slice(&buf[..take]);
        r.consume(take);
        if end || line.len() >= STDERR_LINE {
            print(&line);
            line.clear();
        }
    }
    if !line.is_empty() {
        print(&line);
    }
}

/// The plugin's first message, which must be `hello`, within [`HELLO_TIMEOUT`].
async fn read_hello(stdout: &mut BufReader<ChildStdout>) -> Result<Hello, String> {
    let ms = HELLO_TIMEOUT.as_millis();
    match tokio::time::timeout(HELLO_TIMEOUT, read_message(stdout)).await {
        Err(_) => Err(format!("no hello within {ms} ms")),
        Ok(Ok(Some(Message::Control(Control::Hello(h))))) => Ok(h),
        Ok(Ok(Some(Message::Control(Control::Error { message })))) => {
            Err(format!("error instead of hello: {message}"))
        }
        Ok(Ok(Some(_))) => Err("first message is not a hello".into()),
        Ok(Ok(None)) => Err("exited before sending its hello".into()),
        Ok(Err(e)) => Err(format!("{e} while reading its hello")),
    }
}

/// An exit status as text: the code, or `signal <n>`.
fn status_text(s: ExitStatus) -> String {
    if let Some(c) = s.code() {
        return c.to_string();
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = s.signal() {
            return format!("signal {sig}");
        }
    }
    s.to_string()
}

/// The environment a plugin gets from the host's `vars`: [`PASS_ENV`] as is,
/// `ZETA_PLUGIN_ENV_<NAME>` as `<NAME>`, and `PYTHONUNBUFFERED=1` (so a Python plugin's
/// frames are not held in its stdout buffer). Nothing else.
fn plugin_env(
    vars: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let mut env: Vec<_> = vars
        .filter_map(|(k, v)| {
            let name = k.to_str()?;
            if PASS_ENV.contains(&name) {
                Some((k, v))
            } else {
                let rest = name.strip_prefix(PLUGIN_ENV_PREFIX)?;
                (!rest.is_empty()).then(|| (rest.into(), v))
            }
        })
        .collect();
    env.push(("PYTHONUNBUFFERED".into(), "1".into()));
    env
}

/// The plugin's process group; `SIGKILL`ed whole when dropped, so nothing the plugin
/// started outlives the `probe` or `run` that spawned it. `None` if the plugin had exited
/// before its id was read.
struct Group(Option<u32>);

#[cfg(unix)]
mod sys {
    // `killpg(2)`, declared here rather than taking a `libc` dependency for one call.
    unsafe extern "C" {
        fn killpg(pgrp: i32, sig: i32) -> i32;
    }
    /// `SIGKILL` is 9 on every Unix.
    const SIGKILL: i32 = 9;

    /// `SIGKILL` every process in group `pgrp`; a group already gone is not an error.
    pub(super) fn kill_group(pgrp: u32) {
        // A pid fits i32; 0 or less would mean "my own group" or "everything".
        if let Ok(g) = i32::try_from(pgrp)
            && g > 1
        {
            // SAFETY: killpg takes two integers and touches no memory of ours.
            unsafe {
                killpg(g, SIGKILL);
            }
        }
    }
}

impl Group {
    fn kill(&self) {
        #[cfg(unix)]
        if let Some(g) = self.0 {
            sys::kill_group(g);
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        self.kill();
    }
}

/// A plugin process and its process group.
struct Kid {
    child: Child,
    group: Group,
}

/// Wait up to [`STOP_GRACE`] for the plugin to exit, then kill it; its exit status. Either
/// way, whatever it started is killed with it.
async fn finish(kid: &mut Kid) -> std::io::Result<ExitStatus> {
    let s = match tokio::time::timeout(STOP_GRACE, kid.child.wait()).await {
        Ok(s) => s,
        Err(_) => {
            kid.group.kill();
            let _ = kid.child.kill().await;
            kid.child.wait().await
        }
    };
    kid.group.kill();
    s
}

/// Kill the plugin and its group now (a no-op if they have exited) and reap it; its exit
/// status.
async fn kill_now(kid: &mut Kid) -> std::io::Result<ExitStatus> {
    kid.group.kill();
    let _ = kid.child.kill().await;
    kid.child.wait().await
}

/// An exit status (or the failure to get one) as text.
fn describe(s: &std::io::Result<ExitStatus>) -> String {
    match s {
        Ok(s) => status_text(*s),
        Err(e) => format!("unknown ({e})"),
    }
}

/// Aborts the pump tasks when the run ends, however it ends.
struct Tasks(Vec<JoinHandle<()>>);

impl Drop for Tasks {
    fn drop(&mut self) {
        for t in &self.0 {
            t.abort();
        }
    }
}

/// Room in the writer queue, or never if the queue is gone.
async fn reserve(tx: &Option<mpsc::Sender<Vec<u8>>>) -> Option<mpsc::Permit<'_, Vec<u8>>> {
    match tx {
        Some(t) => t.reserve().await.ok(),
        None => std::future::pending().await,
    }
}

/// How the run loop ended.
enum End {
    Cancelled,
    /// Fail now: the plugin is killed.
    Fail(String),
    /// Stdout ended at a message boundary.
    Eof,
    Wire(WireError),
}

#[async_trait]
impl Stage for ExternalStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    async fn run(
        &self,
        params: serde_json::Value,
        mut inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
    ) -> Result<(), StageError> {
        let kind = self.spec.kind;
        let failed = |m: String| Err(StageError::Failed(m));
        if inputs.len() != 1 {
            return failed(format!(
                "plugin {kind} needs exactly 1 input, got {}",
                inputs.len()
            ));
        }
        let rx = &mut inputs[0];

        // Each output's kind and provenance, as registered on the bus.
        let streams = bus.list();
        let mut ports: Vec<(OutPort, Provenance)> = Vec::with_capacity(outputs.len());
        for id in &outputs {
            let Some(info) = streams.iter().find(|i| &i.id == id) else {
                return failed(format!(
                    "plugin {kind}: output stream {} is not registered",
                    id.0
                ));
            };
            if !self.spec.outputs.contains(&info.kind) {
                return failed(format!(
                    "plugin {kind} cannot produce {} for stream {}",
                    kind_name(info.kind),
                    id.0
                ));
            }
            ports.push((
                OutPort {
                    stream: id.clone(),
                    kind: info.kind,
                },
                info.provenance.clone(),
            ));
        }
        let run = encode_control(&Control::Run(Run {
            protocol: PROTOCOL,
            params,
            // `inputs` has exactly one kind (checked by probe): the kind in use.
            inputs: vec![PortKind {
                kind: self.spec.inputs[0],
            }],
            outputs: ports.iter().map(|(p, _)| p.clone()).collect(),
        }));
        if run.len() > MAX_CONTROL {
            return failed(format!(
                "plugin {kind}: run message of {} bytes exceeds the {MAX_CONTROL}-byte limit",
                run.len()
            ));
        }

        let mut p = match Proc::spawn(kind, &self.cmd) {
            Ok(p) => p,
            Err(e) => {
                return failed(format!(
                    "plugin {kind}: cannot start {}: {e}",
                    self.cmd.program.display()
                ));
            }
        };
        let hello = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                let _ = kill_now(&mut p.child).await;
                return Err(StageError::Cancelled);
            }
            h = read_hello(&mut p.stdout) => h,
        };
        match hello {
            Ok(h) if h == self.hello => {}
            Ok(_) => {
                let _ = kill_now(&mut p.child).await;
                return failed(format!("plugin {kind} changed its hello"));
            }
            Err(reason) => {
                let _ = kill_now(&mut p.child).await;
                return failed(format!("plugin {kind}: {reason}"));
            }
        }

        let Proc {
            mut child,
            mut stdin,
            mut stdout,
        } = p;
        let (to_plugin, mut writer_rx) = mpsc::channel::<Vec<u8>>(IN_QUEUE);
        let (reader_tx, mut from_plugin) = mpsc::channel(OUT_QUEUE);
        let _tasks = Tasks(vec![
            // Writes until the queue's sender is dropped (then closes stdin) or a write fails.
            tokio::spawn(async move {
                while let Some(b) = writer_rx.recv().await {
                    if stdin.write_all(&b).await.is_err() || stdin.flush().await.is_err() {
                        break;
                    }
                }
            }),
            // Forwards every message, then the end (EOF or error), then stops.
            tokio::spawn(async move {
                loop {
                    let m = read_message(&mut stdout).await;
                    let last = !matches!(m, Ok(Some(_)));
                    if reader_tx.send(m).await.is_err() || last {
                        break;
                    }
                }
            }),
        ]);
        // The queue is empty, so this cannot fail for lack of room.
        let _ = to_plugin.try_send(run);
        let mut to_plugin = Some(to_plugin);

        let mut missed: u64 = 0;
        let mut input_closed = false;
        // The run's source and job, from the first input frame.
        let mut run_prov: Option<(SourceId, Option<JobId>)> = None;

        let mut closed_sent = false;
        let end = loop {
            if closed_sent {
                // Dropping the sender closes stdin once the queue drains.
                to_plugin = None;
            }
            let pending = missed > 0 || (input_closed && to_plugin.is_some());
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break End::Cancelled,
                m = from_plugin.recv() => match m {
                    Some(Ok(Some(Message::Frame(f)))) => {
                        let sent = kind_name(f.kind);
                        let Some((port, reg)) = ports.iter().find(|(p, _)| p.stream == f.stream)
                        else {
                            break End::Fail(format!(
                                "plugin {kind} sent {sent} frame on undeclared stream {}",
                                f.stream.0
                            ));
                        };
                        if port.kind != f.kind {
                            break End::Fail(format!(
                                "plugin {kind} sent {sent} frame on stream {} declared {}",
                                f.stream.0,
                                kind_name(port.kind)
                            ));
                        }
                        let (source, job) = run_prov
                            .clone()
                            .unwrap_or_else(|| (reg.source.clone(), reg.job.clone()));
                        let provenance = Provenance {
                            source,
                            job,
                            channel: f.provenance.channel,
                            span: f.provenance.span,
                        };
                        bus.publish(Frame::new(f.stream, provenance, f.payload));
                    }
                    Some(Ok(Some(Message::Control(Control::Error { message })))) => {
                        break End::Fail(format!("plugin {kind}: {message}"));
                    }
                    Some(Ok(Some(Message::Control(_)))) => {
                        break End::Fail(format!(
                            "plugin {kind} sent a control message other than error after hello"
                        ));
                    }
                    Some(Ok(None)) | None => break End::Eof,
                    Some(Err(e)) => break End::Wire(e),
                },
                Some(permit) = reserve(&to_plugin), if pending => {
                    if missed > 0 {
                        permit.send(encode_control(&Control::Lagged { missed }));
                        missed = 0;
                    } else {
                        permit.send(encode_control(&Control::Closed {
                            reason: "input closed".into(),
                        }));
                        closed_sent = true;
                    }
                }
                r = rx.recv(), if !input_closed => match r {
                    Ok(frame) => {
                        if !self.spec.inputs.contains(&frame.kind) {
                            break End::Fail(format!(
                                "plugin {kind} got a {} input frame; it accepts {}",
                                kind_name(frame.kind),
                                self.spec
                                    .inputs
                                    .iter()
                                    .map(|k| kind_name(*k))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ));
                        }
                        if run_prov.is_none() {
                            run_prov = Some((
                                frame.provenance.source.clone(),
                                frame.provenance.job.clone(),
                            ));
                        }
                        let bytes = match encode_frame(&frame) {
                            Ok(b) => b,
                            Err(e) => break End::Fail(format!(
                                "plugin {kind}: cannot send an input frame: {e}"
                            )),
                        };
                        let Some(tx) = &to_plugin else { continue };
                        // A `lagged` owed goes first; frames never overtake it.
                        if missed > 0
                            && tx.try_send(encode_control(&Control::Lagged { missed })).is_ok()
                        {
                            missed = 0;
                        }
                        if missed > 0 {
                            missed += 1;
                        } else if let Err(mpsc::error::TrySendError::Full(_)) = tx.try_send(bytes) {
                            missed = 1;
                        }
                        // A closed queue means the writer saw stdin fail: the plugin is
                        // exiting, and the reader will report how.
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => missed += n,
                    Err(broadcast::error::RecvError::Closed) => input_closed = true,
                },
            }
        };

        // Close stdin (after what is queued) on every path.
        let stopping = to_plugin.take();
        match end {
            End::Cancelled => {
                if let Some(tx) = stopping {
                    let _ = tx.try_send(encode_control(&Control::Stop));
                }
                let _ = finish(&mut child).await;
                Err(StageError::Cancelled)
            }
            End::Fail(m) => {
                drop(stopping);
                let _ = kill_now(&mut child).await;
                failed(m)
            }
            End::Eof => {
                drop(stopping);
                let s = finish(&mut child).await;
                if !input_closed {
                    failed(format!(
                        "plugin {kind} exited unexpectedly (status {})",
                        describe(&s)
                    ))
                } else if s.as_ref().is_ok_and(ExitStatus::success) {
                    Ok(())
                } else {
                    failed(format!("plugin {kind} exited with status {}", describe(&s)))
                }
            }
            End::Wire(e) => {
                drop(stopping);
                // A truncation is stdout ending mid-message: the plugin is exiting, so let
                // it report its own status. Garbage from a live plugin gets no grace.
                let s = if matches!(e, WireError::Truncated { .. }) {
                    finish(&mut child).await
                } else {
                    kill_now(&mut child).await
                };
                failed(format!("plugin {kind}: {e} (exit status {})", describe(&s)))
            }
        }
    }
}
