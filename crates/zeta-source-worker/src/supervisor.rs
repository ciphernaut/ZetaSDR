//! The supervisor: one task per [`WorkerPort`](crate::port::WorkerPort) that
//! owns the worker child.
//!
//! It runs a small state machine. `Starting` spawns a worker and waits for its
//! hello; `Live` waits for the worker to end; `Waiting` sleeps out the backoff
//! before the next spawn. A worker that ended is always reaped before the next
//! one is spawned: two opens of one B210 cannot overlap, and the old worker
//! may hold the device until it is gone.
//!
//! The worker's stdout is read on a thread of its own with the blocking
//! [`read_frame`]: status frames update the shared snapshot directly, replies
//! are handed to the request that waits for them, blocks go to the port's
//! broadcast and the hello wakes the supervisor. Its stderr is read on another
//! thread and passed through to the node's stderr, each line prefixed with
//! `zeta-soapy-worker[<id>]: ` so a driver's own log lines are attributable;
//! the last line is kept, as an open failure explains itself there.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::unix::process::ExitStatusExt;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, oneshot, watch};
use zeta_core::port::{GainInfo, IqBlock, PortError, PortStats, Tuning};
use zeta_core::types::{Capabilities, SourceId};

use crate::port::{Attach, WorkerConfig};
use crate::serve::{EXIT_FAULTED, EXIT_NOT_FOUND, EXIT_OPEN_FAILED};
use crate::wire::{
    Cmd, Control, Frame, Hello, PROTOCOL, Reply, Request, read_frame, write_request,
};

/// Capacity of the port's block broadcast, as `zeta-source-node` uses.
const BROADCAST_CAPACITY: usize = 64;
/// How long a reaped worker's reader threads are waited for, so its last
/// status and stderr line are in before the fault is worked out. They end as
/// soon as the pipes close, which a dead process does at once; the bound only
/// matters when something the worker spawned still holds a pipe.
pub(crate) const READER_JOIN: Duration = Duration::from_secs(1);
/// A worker that keeps dying soon after each respawn is logged at most this
/// often.
const DYING_LOG_EVERY: Duration = Duration::from_secs(60);

/// What the port and the supervisor share.
pub(crate) struct Shared {
    pub(crate) id: SourceId,
    snap: Mutex<Snapshot>,
    pub(crate) tx: broadcast::Sender<IqBlock>,
}

impl Shared {
    pub(crate) fn new(id: SourceId) -> Self {
        Shared {
            id,
            snap: Mutex::new(Snapshot::default()),
            tx: broadcast::channel(BROADCAST_CAPACITY).0,
        }
    }

    pub(crate) fn snapshot(&self) -> MutexGuard<'_, Snapshot> {
        // A panic while the lock was held leaves plain data behind; keep
        // serving it rather than poison every later call.
        self.snap.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The port's view of its worker: the last status, plus the supervision state.
#[derive(Default)]
pub(crate) struct Snapshot {
    /// The live worker's request channel: `Some` from its hello until it ends.
    pub(crate) link: Option<Arc<Link>>,
    /// The live worker's last reported fault.
    pub(crate) worker_fault: Option<String>,
    /// Why no worker is live: how the last one ended.
    pub(crate) supervision_fault: Option<String>,
    /// The supervision fault a fresh worker's hello ended, still reported until the
    /// node has seen it ([`Port::heal_settled`](zeta_core::port::Port::heal_settled)):
    /// jobs poll the fault, and a quick respawn must not fall between two polls. A tune
    /// does not clear it.
    pub(crate) healing_fault: Option<String>,
    pub(crate) current: Option<Tuning>,
    pub(crate) gain: Option<GainInfo>,
    pub(crate) stats: Option<PortStats>,
    pub(crate) pid: Option<u32>,
    pub(crate) since_ns: Option<u64>,
    pub(crate) restarts: u64,
}

impl Snapshot {
    /// The worker's own fault while one is live, or the fault its hello is healing;
    /// else the supervision fault.
    pub(crate) fn fault(&self) -> Option<String> {
        if self.link.is_some() {
            self.worker_fault
                .clone()
                .or_else(|| self.healing_fault.clone())
        } else {
            self.supervision_fault.clone()
        }
    }
}

type ReplyTx = oneshot::Sender<Result<Reply, PortError>>;

/// One worker's stdin and the requests waiting for its replies.
pub(crate) struct Link {
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    pending: Mutex<HashMap<u64, ReplyTx>>,
    /// Set once by [`Link::fail_all`], under the `pending` lock: after it, no
    /// request is registered, so none can wait on a worker that has ended
    /// (something the worker spawned may still hold its stdin, and a write
    /// would then succeed into a pipe nobody answers).
    closed: watch::Sender<bool>,
}

impl Link {
    pub(crate) fn new(stdin: ChildStdin) -> Self {
        Link {
            stdin: tokio::sync::Mutex::new(Some(stdin)),
            pending: Mutex::new(HashMap::new()),
            closed: watch::channel(false).0,
        }
    }

    fn pending(&self) -> MutexGuard<'_, HashMap<u64, ReplyTx>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Write request `id` and return the receiver its reply arrives on. The
    /// receiver errors when the worker ends without replying.
    pub(crate) async fn send(
        &self,
        id: u64,
        cmd: Cmd,
    ) -> std::io::Result<oneshot::Receiver<Result<Reply, PortError>>> {
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending();
            if *self.closed.borrow() {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            pending.insert(id, tx);
        }
        let written = self.write(&Request { id, cmd }).await;
        if written.is_err() {
            self.forget(id);
        }
        written.map(|()| rx)
    }

    async fn write(&self, req: &Request) -> std::io::Result<()> {
        let mut line = Vec::new();
        write_request(&mut line, req)?;
        let mut stdin = self.stdin.lock().await;
        let Some(stdin) = stdin.as_mut() else {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        };
        stdin.write_all(&line).await?;
        stdin.flush().await
    }

    /// Close the worker's stdin. A worker reads it on a blocking thread its
    /// runtime cannot cancel, so it only gets past that read, and out of the
    /// process, once it sees EOF; later writes fail as a broken pipe.
    async fn close_stdin(&self) {
        drop(self.stdin.lock().await.take());
    }

    pub(crate) fn forget(&self, id: u64) {
        self.pending().remove(&id);
    }

    /// Close the link: drop every waiting request, so each sees the worker's
    /// end, and refuse any later one.
    fn fail_all(&self) {
        let mut pending = self.pending();
        self.closed.send_replace(true);
        pending.clear();
    }

    /// Wait up to `within` for the supervisor to close the link. A request
    /// that found the worker gone (a failed write, a reply that never came)
    /// waits here so it answers with the fault the worker's end leaves, which
    /// is set before the link closes, rather than with what the snapshot said
    /// while the death was not yet reaped.
    pub(crate) async fn wait_closed(&self, within: Duration) {
        let mut rx = self.closed.subscribe();
        let _ = tokio::time::timeout(within, rx.wait_for(|c| *c)).await;
    }
}

/// One spawned worker and the threads reading it.
struct Worker {
    child: Child,
    pid: Option<u32>,
    link: Arc<Link>,
    hello: oneshot::Receiver<Hello>,
    /// Closed when the stdout thread ends (`None` once seen).
    stdout_done: Option<oneshot::Receiver<()>>,
    stderr_done: oneshot::Receiver<()>,
    last_stderr: Arc<Mutex<Option<String>>>,
}

/// The supervisor's states; see the module docs.
enum State {
    Starting,
    /// `since` is when the worker said hello. Boxed: a worker is large and
    /// the other states are small.
    Live {
        worker: Box<Worker>,
        since: Instant,
    },
    /// The fault that ended the last worker is in the snapshot meanwhile.
    Waiting {
        next_at: Instant,
    },
}

/// A run of worker ends without a hello in between: the device is gone, or a
/// worker keeps dying.
struct Episode {
    since: Instant,
    /// Spawns since the episode began.
    attempts: u32,
    /// Whether the episode's `waiting for the device` line was printed; its
    /// `back after` line is printed only then, so the two always pair.
    logged: bool,
}

/// How a worker ended, worked out once it was reaped.
struct Ended {
    fault: String,
    /// The device was not found (exit 3): poll at the cap.
    not_found: Option<String>,
    /// How long the worker lived after its hello; `None` without a hello.
    lived: Option<Duration>,
}

pub(crate) struct Supervisor {
    cfg: WorkerConfig,
    shared: Arc<Shared>,
    healed: watch::Sender<u64>,
    /// The cached capability record from an earlier open.
    cached: Option<Capabilities>,
    /// The port's capability record: the first hello's, or the cached one
    /// when the first spawn found no device.
    caps: Option<Capabilities>,
    first: Option<oneshot::Sender<(Attach, Option<Capabilities>)>>,
    spawns: u64,
    backoff: Duration,
    episode: Option<Episode>,
    last_dying_log: Option<Instant>,
}

impl Supervisor {
    pub(crate) fn new(
        cfg: WorkerConfig,
        shared: Arc<Shared>,
        healed: watch::Sender<u64>,
        cached: Option<Capabilities>,
    ) -> Self {
        let backoff = cfg.timings.backoff_first;
        Supervisor {
            cfg,
            shared,
            healed,
            cached,
            caps: None,
            first: None,
            spawns: 0,
            backoff,
            episode: None,
            last_dying_log: None,
        }
    }

    fn id(&self) -> &str {
        &self.cfg.id.0
    }

    /// `fault` as a log line names it: prefixed `soapy worker for <id>: `
    /// unless the fault text already starts with the worker's name.
    fn named(&self, fault: &str) -> String {
        let name = format!("soapy worker for {}", self.id());
        if fault.starts_with(&name) {
            fault.to_string()
        } else {
            format!("{name}: {fault}")
        }
    }

    /// Run until `cancel` fires or its sender is dropped (the port dropped),
    /// or the first spawn is refused. `first` is answered once, with what the
    /// first spawn said.
    pub(crate) async fn run(
        mut self,
        mut cancel: oneshot::Receiver<()>,
        first: oneshot::Sender<(Attach, Option<Capabilities>)>,
    ) {
        self.first = Some(first);
        let mut state = State::Starting;
        loop {
            state = match state {
                State::Starting => match self.start(&mut cancel).await {
                    Some(s) => s,
                    None => return,
                },
                State::Live { worker, since } => {
                    match self.live(worker, since, &mut cancel).await {
                        Some(s) => s,
                        None => return,
                    }
                }
                State::Waiting { next_at } => {
                    tokio::select! {
                        _ = tokio::time::sleep_until(next_at.into()) => State::Starting,
                        _ = &mut cancel => return,
                    }
                }
            };
        }
    }

    /// Spawn a worker and wait for its hello. `None` when cancelled or when
    /// the first spawn was refused.
    async fn start(&mut self, cancel: &mut oneshot::Receiver<()>) -> Option<State> {
        self.spawns += 1;
        if self.spawns > 1 {
            self.shared.snapshot().restarts += 1;
        }
        if let Some(e) = &mut self.episode {
            e.attempts += 1;
        }
        let mut w = match self.spawn().await {
            Ok(w) => w,
            Err(e) => {
                let fault = format!(
                    "soapy worker for {}: cannot start {}: {e}",
                    self.id(),
                    self.cfg.program.display()
                );
                return self.ended(Ended {
                    fault,
                    not_found: None,
                    lived: None,
                });
            }
        };
        let deadline = tokio::time::Instant::now() + self.cfg.timings.hello;
        let mut hello_open = true;
        loop {
            tokio::select! {
                h = &mut w.hello, if hello_open => match h {
                    Ok(hello) if hello.protocol != PROTOCOL => {
                        // Another build's worker (the binary replaced under a
                        // running node): its frames cannot be trusted to mean
                        // what ours do, so it is ended before anything is sent.
                        let _ = w.child.kill().await;
                        let mut ended = self.reap(w, None, None).await;
                        ended.fault = format!(
                            "soapy worker for {}: protocol {}, expected {PROTOCOL}",
                            self.id(),
                            hello.protocol
                        );
                        ended.not_found = None;
                        return self.ended(ended);
                    }
                    Ok(hello) => return Some(self.on_hello(w, hello)),
                    // The stdout thread ended without a hello: wait for the
                    // exit, still bounded by the hello deadline.
                    Err(_) => hello_open = false,
                },
                status = w.child.wait() => {
                    let ended = self.reap(w, status.ok(), None).await;
                    return self.ended(ended);
                }
                _ = tokio::time::sleep_until(deadline) => {
                    let _ = w.child.kill().await;
                    let mut ended = self.reap(w, None, None).await;
                    ended.fault = format!(
                        "soapy worker for {}: no hello in {} s",
                        self.id(),
                        self.cfg.timings.hello.as_secs_f64()
                    );
                    ended.not_found = None;
                    return self.ended(ended);
                }
                _ = &mut *cancel => {
                    self.shutdown(w).await;
                    return None;
                }
            }
        }
    }

    /// A worker of our protocol said hello: it is live. Heals an episode, answers `first`.
    fn on_hello(&mut self, w: Worker, hello: Hello) -> State {
        match &self.caps {
            None => self.caps = Some(hello.capabilities.clone()),
            Some(c) if *c != hello.capabilities => eprintln!(
                "zeta: soapy worker for {}: capabilities changed; keeping the first",
                self.id()
            ),
            Some(_) => {}
        }
        {
            let mut s = self.shared.snapshot();
            s.pid = w.pid;
            s.since_ns = Some(now_ns());
        }
        if let Some(e) = self.episode.take() {
            self.healed.send_modify(|n| *n += 1);
            if e.logged {
                eprintln!(
                    "zeta: soapy worker for {}: back after {} attempts, {:.1} s",
                    self.id(),
                    e.attempts,
                    e.since.elapsed().as_secs_f64()
                );
            }
        }
        if let Some(first) = self.first.take() {
            let _ = first.send((Attach::Hello, self.caps.clone()));
        }
        State::Live {
            worker: Box::new(w),
            since: Instant::now(),
        }
    }

    /// Wait for a live worker to end. `None` when cancelled.
    async fn live(
        &mut self,
        mut w: Box<Worker>,
        since: Instant,
        cancel: &mut oneshot::Receiver<()>,
    ) -> Option<State> {
        let mut stdout_done = w.stdout_done.take();
        let status = tokio::select! {
            status = w.child.wait() => status.ok(),
            // Its stdout closed or went bad while it runs: nothing more can be
            // heard from it, so give it the grace to exit and then end it.
            _ = async { stdout_done.as_mut().unwrap().await }, if stdout_done.is_some() => {
                stdout_done = None;
                let grace = self.cfg.timings.shutdown_grace;
                match tokio::time::timeout(grace, w.child.wait()).await {
                    Ok(status) => status.ok(),
                    Err(_) => {
                        let _ = w.child.kill().await;
                        w.child.wait().await.ok()
                    }
                }
            }
            _ = &mut *cancel => {
                self.shutdown(*w).await;
                return None;
            }
        };
        w.stdout_done = stdout_done;
        let ended = self.reap(*w, status, Some(since.elapsed())).await;
        self.ended(ended)
    }

    /// Spawn the worker with the scrubbed environment and start its reader
    /// threads.
    async fn spawn(&self) -> std::io::Result<Worker> {
        let mut child = Command::new(&self.cfg.program)
            .args(&self.cfg.args)
            .env_clear()
            .envs(crate::env::scrubbed())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let pid = child.id();
        let pipes = (|| {
            let stdin = child.stdin.take().ok_or(std::io::ErrorKind::BrokenPipe)?;
            let stdout = child.stdout.take().ok_or(std::io::ErrorKind::BrokenPipe)?;
            let stderr = child.stderr.take().ok_or(std::io::ErrorKind::BrokenPipe)?;
            Ok::<_, std::io::Error>((
                stdin,
                File::from(stdout.into_owned_fd()?),
                File::from(stderr.into_owned_fd()?),
            ))
        })();
        let (stdin, stdout, stderr) = match pipes {
            Ok(p) => p,
            Err(e) => {
                let _ = child.kill().await;
                return Err(e);
            }
        };
        let link = Arc::new(Link::new(stdin));
        let (hello_tx, hello) = oneshot::channel();
        let (stdout_done_tx, stdout_done) = oneshot::channel();
        let (stderr_done_tx, stderr_done) = oneshot::channel();
        let last_stderr = Arc::new(Mutex::new(None));
        let id = self.id().to_string();

        let (shared, l) = (self.shared.clone(), link.clone());
        let reader = std::thread::Builder::new()
            .name(format!("worker-stdout-{id}"))
            .spawn(move || {
                read_stdout(stdout, &shared, &l, hello_tx);
                drop(stdout_done_tx);
            });
        let last = last_stderr.clone();
        let errs = std::thread::Builder::new()
            .name(format!("worker-stderr-{id}"))
            .spawn(move || {
                read_stderr(stderr, &id, &last);
                drop(stderr_done_tx);
            });
        if let Err(e) = reader.and(errs) {
            let _ = child.kill().await;
            return Err(e);
        }
        Ok(Worker {
            child,
            pid,
            link,
            hello,
            stdout_done: Some(stdout_done),
            stderr_done,
            last_stderr,
        })
    }

    /// A worker has exited (or been killed: `status` is then waited for here).
    /// Collect what its reader threads still had, take it off the snapshot and
    /// work out the fault its end leaves.
    async fn reap(
        &self,
        mut w: Worker,
        status: Option<ExitStatus>,
        lived: Option<Duration>,
    ) -> Ended {
        let status = match status {
            Some(s) => Some(s),
            None => w.child.wait().await.ok(),
        };
        if let Some(rx) = w.stdout_done.take() {
            let _ = tokio::time::timeout(READER_JOIN, rx).await;
        }
        let _ = tokio::time::timeout(READER_JOIN, &mut w.stderr_done).await;
        let last_stderr = w
            .last_stderr
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let worker_fault = {
            let mut s = self.shared.snapshot();
            let mine = s.link.as_ref().is_some_and(|l| Arc::ptr_eq(l, &w.link));
            if mine { s.worker_fault.take() } else { None }
        };
        let (fault, not_found) =
            exit_fault(self.id(), &self.cfg.args, status, worker_fault, last_stderr);
        {
            let mut s = self.shared.snapshot();
            if s.link.as_ref().is_some_and(|l| Arc::ptr_eq(l, &w.link)) {
                s.link = None;
            }
            s.worker_fault = None;
            s.healing_fault = None;
            s.supervision_fault = Some(fault.clone());
            s.current = None;
            s.pid = None;
            s.since_ns = None;
        }
        // After the fault is set, so a waiting request reads it.
        w.link.fail_all();
        Ended {
            fault,
            not_found,
            lived,
        }
    }

    /// A worker ended: answer `first` if it was the first spawn, log, and
    /// schedule the next spawn. `None` when the first spawn is refused.
    fn ended(&mut self, ended: Ended) -> Option<State> {
        // Set again here for the ends that did not go through `reap` (a
        // spawn that failed) or whose fault was decided after it (no hello).
        self.shared.snapshot().supervision_fault = Some(ended.fault.clone());
        if let Some(first) = self.first.take() {
            match (&ended.not_found, &self.cached) {
                (Some(args), Some(cached)) => {
                    self.caps = Some(cached.clone());
                    let attach = Attach::NotFound { args: args.clone() };
                    let _ = first.send((attach, self.caps.clone()));
                }
                _ => {
                    let _ = first.send((Attach::Refused(ended.fault), None));
                    return None;
                }
            }
        }

        let t = self.cfg.timings;
        let lived_long = ended.lived.is_some_and(|l| l >= t.alive_reset);
        let may_log_dying = self
            .last_dying_log
            .is_none_or(|at| at.elapsed() >= DYING_LOG_EVERY);
        match &mut self.episode {
            None => {
                // A worker that had lived its fill starts a new story; one
                // that died young after a respawn is a crash loop, logged at
                // most once a minute.
                let logged = lived_long || may_log_dying;
                if logged {
                    eprintln!("zeta: {}; waiting for the device", self.named(&ended.fault));
                    self.last_dying_log = Some(Instant::now());
                }
                self.episode = Some(Episode {
                    since: Instant::now(),
                    attempts: 0,
                    logged,
                });
            }
            // Polling an absent device is the quiet wait the episode's line
            // announced; a respawn that died is worth a line now and then.
            Some(_) if ended.not_found.is_none() && may_log_dying => {
                eprintln!("zeta: {}; respawning", self.named(&ended.fault));
                self.last_dying_log = Some(Instant::now());
            }
            Some(_) => {}
        }

        if lived_long {
            self.backoff = t.backoff_first;
        }
        let wait = if ended.not_found.is_some() {
            t.backoff_max
        } else {
            let w = self.backoff;
            self.backoff = (self.backoff * 2).min(t.backoff_max);
            w
        };
        Some(State::Waiting {
            next_at: Instant::now() + wait,
        })
    }

    /// Stop a worker: `shutdown` on its stdin, its stdin closed, the grace to
    /// exit, then kill.
    /// Always reaped before returning.
    async fn shutdown(&self, mut w: Worker) {
        let _ = w
            .link
            .write(&Request {
                id: 0,
                cmd: Cmd::Shutdown,
            })
            .await;
        // Then EOF: a worker whose runtime waits for its stdin read on the way
        // out would otherwise sit out the grace and be killed.
        w.link.close_stdin().await;
        let grace = self.cfg.timings.shutdown_grace;
        if tokio::time::timeout(grace, w.child.wait()).await.is_err() {
            let _ = w.child.kill().await;
        }
        let mut s = self.shared.snapshot();
        if s.link.as_ref().is_some_and(|l| Arc::ptr_eq(l, &w.link)) {
            s.link = None;
        }
        s.supervision_fault = Some(format!("soapy worker for {}: shut down", self.id()));
        s.pid = None;
        s.since_ns = None;
        drop(s);
        w.link.fail_all();
    }
}

/// The fault a worker's end leaves, and for a device-not-found exit what was
/// looked for. The worker names it on its last stderr line (`device not
/// found: <args>`); a worker that did not is described by its arguments.
fn exit_fault(
    id: &str,
    args: &[String],
    status: Option<ExitStatus>,
    worker_fault: Option<String>,
    last_stderr: Option<String>,
) -> (String, Option<String>) {
    let Some(status) = status else {
        return (
            format!("soapy worker for {id} died: exit status unknown"),
            None,
        );
    };
    if let Some(sig) = status.signal() {
        return (
            format!("soapy worker for {id} died: {}", signal_text(sig)),
            None,
        );
    }
    let code = status.code().unwrap_or(-1);
    match u8::try_from(code) {
        Ok(EXIT_FAULTED) => (
            worker_fault.unwrap_or_else(|| format!("soapy worker for {id} died: exit code {code}")),
            None,
        ),
        Ok(EXIT_NOT_FOUND) => {
            let looked_for = last_stderr
                .as_deref()
                .and_then(|l| l.strip_prefix("device not found: "))
                .map(str::to_string)
                .unwrap_or_else(|| args.join(" "));
            (format!("device not found: {looked_for}"), Some(looked_for))
        }
        Ok(EXIT_OPEN_FAILED) => (
            format!(
                "soapy open failed: {}",
                last_stderr.as_deref().unwrap_or("no reason given")
            ),
            None,
        ),
        _ => (
            format!("soapy worker for {id} died: exit code {code}"),
            None,
        ),
    }
}

/// `signal N (NAME)` for the signals a crashing or killed driver dies of,
/// `signal N` for the rest.
fn signal_text(sig: i32) -> String {
    let name = match sig {
        6 => Some("SIGABRT"),
        7 => Some("SIGBUS"),
        9 => Some("SIGKILL"),
        11 => Some("SIGSEGV"),
        15 => Some("SIGTERM"),
        _ => None,
    };
    match name {
        Some(n) => format!("signal {sig} ({n})"),
        None => format!("signal {sig}"),
    }
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// The stdout thread: read frames until the stream ends.
fn read_stdout(mut stdout: File, shared: &Shared, link: &Arc<Link>, hello: oneshot::Sender<Hello>) {
    let mut hello = Some(hello);
    loop {
        let frame = match read_frame(&mut stdout) {
            Ok(Some(f)) => f,
            Ok(None) => return,
            Err(e) => {
                eprintln!("zeta: soapy worker for {}: bad frame: {e}", shared.id.0);
                return;
            }
        };
        match frame {
            Frame::Block(b) => {
                // No receivers is normal: nothing is subscribed yet.
                let _ = shared.tx.send(b);
            }
            Frame::Control(Control::Hello(h)) => {
                let Some(tx) = hello.take() else {
                    eprintln!("zeta: soapy worker for {}: a second hello", shared.id.0);
                    continue;
                };
                // The link goes live here, before the supervisor hears of the
                // hello, so the status that follows at once is not lost.
                {
                    let mut s = shared.snapshot();
                    s.link = Some(link.clone());
                    s.worker_fault = None;
                    s.healing_fault = s.supervision_fault.take();
                    s.current = None;
                    s.gain = h.gain;
                }
                let _ = tx.send(h);
            }
            Frame::Control(Control::Status(st)) => {
                let mut s = shared.snapshot();
                if s.link.as_ref().is_some_and(|l| Arc::ptr_eq(l, link)) {
                    s.worker_fault = st.fault;
                    s.current = st.current;
                    s.gain = st.gain;
                    s.stats = st.stats;
                }
            }
            Frame::Control(Control::Reply { reply, result }) => {
                if let Some(tx) = link.pending().remove(&reply) {
                    let _ = tx.send(result);
                }
            }
        }
    }
}

/// The stderr thread: pass each line through with the worker's prefix and
/// keep the last non-empty one.
fn read_stderr(stderr: File, id: &str, last: &Mutex<Option<String>>) {
    for line in BufReader::new(stderr).split(b'\n') {
        let Ok(line) = line else { return };
        let line = String::from_utf8_lossy(&line);
        let line = line.trim_end_matches('\r');
        eprintln!("zeta-soapy-worker[{id}]: {line}");
        if !line.trim().is_empty() {
            *last.lock().unwrap_or_else(|e| e.into_inner()) = Some(line.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signals_are_named_where_it_matters() {
        assert_eq!(signal_text(6), "signal 6 (SIGABRT)");
        assert_eq!(signal_text(11), "signal 11 (SIGSEGV)");
        assert_eq!(signal_text(7), "signal 7 (SIGBUS)");
        assert_eq!(signal_text(9), "signal 9 (SIGKILL)");
        assert_eq!(signal_text(15), "signal 15 (SIGTERM)");
        assert_eq!(signal_text(1), "signal 1");
    }

    #[test]
    fn exit_codes_map_to_their_faults() {
        let exit = |code: i32| Some(ExitStatus::from_raw(code << 8));
        let args = ["--id".to_string(), "b".to_string()];
        assert_eq!(
            exit_fault("b", &args, exit(5), Some("read failed: x".into()), None),
            ("read failed: x".into(), None)
        );
        assert_eq!(
            exit_fault("b", &args, exit(5), None, None).0,
            "soapy worker for b died: exit code 5"
        );
        assert_eq!(
            exit_fault(
                "b",
                &args,
                exit(3),
                None,
                Some("device not found: driver=uhd".into())
            ),
            (
                "device not found: driver=uhd".into(),
                Some("driver=uhd".into())
            )
        );
        assert_eq!(
            exit_fault("b", &args, exit(3), None, None).0,
            "device not found: --id b"
        );
        assert_eq!(
            exit_fault("b", &args, exit(4), None, Some("open: busy".into())).0,
            "soapy open failed: open: busy"
        );
        assert_eq!(
            exit_fault("b", &args, exit(1), None, None).0,
            "soapy worker for b died: exit code 1"
        );
        assert_eq!(
            exit_fault("b", &args, Some(ExitStatus::from_raw(11)), None, None).0,
            "soapy worker for b died: signal 11 (SIGSEGV)"
        );
    }
}
