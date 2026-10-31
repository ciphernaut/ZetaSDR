//! The child's side of the protocol: any [`Port`] served over stdin and stdout.
//!
//! [`serve`] writes the hello, then answers requests read line by line from
//! stdin while two tasks run beside it: a block pump that forwards every block
//! of the port's broadcast as an IQ frame, and a status task that reports the
//! port's health every [`STATUS_TICK`] and after every reply. A fault on the
//! port is published and the worker exits [`EXIT_FAULTED`]: reopening the
//! device is the parent's job, and a fresh process leaves nothing of the old
//! driver behind.
//!
//! Every frame goes through one writer thread that owns the buffered stdout
//! and flushes per frame, so control messages and blocks keep one order and a
//! parent that reads slowly never blocks the status task's fault check (blocks
//! beyond [`MAX_QUEUED_BLOCKS`] are dropped and counted as lost instead).
//!
//! [`requests_until_signal`] is the stdin a worker hands `serve`: SIGTERM and
//! SIGINT become a `shutdown` request, and a watchdog ends the process if the
//! request is never read.

use std::io::{self, BufWriter, Write};
use std::os::fd::RawFd;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader, DuplexStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{Notify, broadcast, oneshot};
use zeta_core::port::{Port, Tuning};
use zeta_core::types::Hz;

use crate::wire::{
    Cmd, Control, Frame, Hello, PROTOCOL, Reply, Request, Status, parse_request, write_frame,
    write_request,
};

/// Exit code of a worker that found no device to open: the parent polls.
pub const EXIT_NOT_FOUND: u8 = 3;
/// Exit code of a worker whose device would not open; its last stderr line
/// says why.
pub const EXIT_OPEN_FAILED: u8 = 4;
/// Exit code of a worker whose port faulted, after it published the fault.
pub const EXIT_FAULTED: u8 = 5;

/// How often the status is written when nothing else prompts it.
pub const STATUS_TICK: Duration = Duration::from_millis(1000);
/// How long a faulted worker waits after publishing the fault before it
/// exits, so the status reaches the parent.
pub const FAULT_EXIT_GRACE: Duration = Duration::from_millis(200);
/// Blocks queued for the writer thread beyond which the pump drops blocks
/// rather than let a slow parent grow the queue without bound.
const MAX_QUEUED_BLOCKS: usize = 64;
/// Interval of the `blocks lost` log line.
const LOST_LOG_EVERY: Duration = Duration::from_secs(10);
/// How long the end of `serve` waits for queued frames to reach stdout; a
/// parent that stopped reading must not keep the worker alive.
const FINAL_FLUSH: Duration = Duration::from_secs(1);
/// How long a worker told to stop by SIGTERM or SIGINT has to leave on its own
/// before the process is ended regardless. A worker whose driver hangs in
/// `open()` never reaches `serve`, so the `shutdown` the signal became is never
/// read; without the watchdog it would hold the device until SIGKILL, and the
/// parent-death signal (SIGTERM as well) would be swallowed the same way.
pub const SIGNAL_EXIT_WATCHDOG: Duration = Duration::from_secs(5);
/// Room in the pipe between the stdin forwarder and `serve`: requests are
/// short lines, so this only has to absorb a burst while `serve` is busy.
const REQUEST_PIPE: usize = 64 * 1024;

/// What the writer thread is handed.
enum Out {
    Frame(Frame),
    /// Flush, then stop.
    End,
}

/// The sending side of the writer thread, shared by the request loop and the
/// tasks.
#[derive(Clone)]
struct Writer {
    tx: std_mpsc::Sender<Out>,
    queued_blocks: Arc<AtomicUsize>,
}

impl Writer {
    /// Start the writer thread over `stdout`; the receiver answers once it has
    /// flushed its last frame and stopped.
    fn start(stdout: impl Write + Send + 'static) -> io::Result<(Self, std_mpsc::Receiver<()>)> {
        let (tx, rx) = std_mpsc::channel::<Out>();
        let (done_tx, done_rx) = std_mpsc::channel();
        let queued_blocks = Arc::new(AtomicUsize::new(0));
        let queued = queued_blocks.clone();
        std::thread::Builder::new()
            .name("worker-stdout".into())
            .spawn(move || {
                let mut out = BufWriter::new(stdout);
                while let Ok(Out::Frame(f)) = rx.recv() {
                    let block = matches!(f, Frame::Block(_));
                    let written = write_frame(&mut out, &f).and_then(|()| out.flush());
                    if block {
                        queued.fetch_sub(1, Ordering::Relaxed);
                    }
                    if let Err(e) = written {
                        // The parent is gone; the request loop sees stdin
                        // close and ends the worker.
                        eprintln!("zeta-source-worker: stdout: {e}");
                        break;
                    }
                }
                let _ = out.flush();
                let _ = done_tx.send(());
            })?;
        Ok((Writer { tx, queued_blocks }, done_rx))
    }

    fn control(&self, c: Control) {
        let _ = self.tx.send(Out::Frame(Frame::Control(c)));
    }

    /// Queue a block unless the writer is too far behind; false when dropped.
    fn block(&self, b: zeta_core::port::IqBlock) -> bool {
        if self.queued_blocks.load(Ordering::Relaxed) >= MAX_QUEUED_BLOCKS {
            return false;
        }
        self.queued_blocks.fetch_add(1, Ordering::Relaxed);
        if self.tx.send(Out::Frame(Frame::Block(b))).is_err() {
            self.queued_blocks.fetch_sub(1, Ordering::Relaxed);
            return false;
        }
        true
    }
}

/// The port's health as the status message carries it.
fn status_of(port: &dyn Port) -> Status {
    Status {
        fault: port.fault(),
        current: port.current(),
        gain: port.gain(),
        stats: port.stats(),
    }
}

/// Serve `port` over `stdin` and `stdout` until stdin closes, a `shutdown`
/// arrives (both exit 0) or the port faults ([`EXIT_FAULTED`]). `driver` is the
/// driver key the hello names.
pub async fn serve(
    port: Arc<dyn Port>,
    driver: &str,
    stdin: impl AsyncRead + Unpin + Send,
    stdout: impl Write + Send + 'static,
) -> ExitCode {
    let (writer, done) = match Writer::start(stdout) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("zeta-source-worker: stdout writer thread: {e}");
            return ExitCode::FAILURE;
        }
    };
    writer.control(Control::Hello(Hello {
        protocol: PROTOCOL,
        capabilities: port.capabilities().clone(),
        gain: port.gain(),
        driver: driver.to_string(),
    }));

    let pump = tokio::spawn(pump(port.subscribe(), writer.clone()));
    let replied = Arc::new(Notify::new());
    let (faulted_tx, mut faulted) = oneshot::channel();
    let status = tokio::spawn(status_loop(
        port.clone(),
        writer.clone(),
        replied.clone(),
        faulted_tx,
    ));

    let mut lines = BufReader::new(stdin).lines();
    let code = loop {
        tokio::select! {
            _ = &mut faulted => break ExitCode::from(EXIT_FAULTED),
            line = lines.next_line() => {
                let line = match line {
                    Ok(Some(l)) => l,
                    Ok(None) => break ExitCode::SUCCESS,
                    Err(e) => {
                        eprintln!("zeta-source-worker: stdin: {e}");
                        break ExitCode::SUCCESS;
                    }
                };
                if line.trim().is_empty() {
                    continue;
                }
                let req = match parse_request(&line) {
                    Ok(r) => r,
                    Err(e) => {
                        // No id to answer: the parent only sends what it
                        // built, so this is a bug worth a line, not a crash.
                        eprintln!("zeta-source-worker: bad request {line:?}: {e}");
                        continue;
                    }
                };
                let result = match req.cmd {
                    Cmd::Shutdown => break ExitCode::SUCCESS,
                    Cmd::Tune { center_hz, bandwidth_hz } => port
                        .tune(Tuning {
                            center_hz: Hz(center_hz),
                            bandwidth_hz: Hz(bandwidth_hz),
                        })
                        .await
                        .map(Reply::Tuning),
                    Cmd::SetGain { gain_db } => port.set_gain(gain_db).await.map(Reply::Gain),
                };
                writer.control(Control::Reply { reply: req.id, result });
                replied.notify_one();
            }
        }
    };

    pump.abort();
    status.abort();
    let _ = pump.await;
    let _ = status.await;
    let _ = writer.tx.send(Out::End);
    drop(writer);
    // Waited for off the runtime: a parent that stopped reading must not
    // stall a worker thread, and must not keep the worker alive either.
    let _ = tokio::task::spawn_blocking(move || done.recv_timeout(FINAL_FLUSH)).await;
    code
}

/// Forward every block of `rx` to the writer. Blocks lost to a lagging
/// broadcast or a full writer queue are counted and logged at most every
/// [`LOST_LOG_EVERY`].
async fn pump(mut rx: broadcast::Receiver<zeta_core::port::IqBlock>, writer: Writer) {
    let mut lost: u64 = 0;
    let mut logged: Option<Instant> = None;
    loop {
        match rx.recv().await {
            Ok(b) => {
                if !writer.block(b) {
                    lost += 1;
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => lost += n,
            Err(broadcast::error::RecvError::Closed) => return,
        }
        if lost > 0 && logged.is_none_or(|t| t.elapsed() >= LOST_LOG_EVERY) {
            eprintln!("zeta-source-worker: {lost} blocks lost");
            lost = 0;
            logged = Some(Instant::now());
        }
    }
}

/// Write the status every [`STATUS_TICK`] and after every reply. A fault is
/// written, then after [`FAULT_EXIT_GRACE`] `faulted` fires and the loop ends.
async fn status_loop(
    port: Arc<dyn Port>,
    writer: Writer,
    replied: Arc<Notify>,
    faulted: oneshot::Sender<()>,
) {
    let mut tick = tokio::time::interval(STATUS_TICK);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = replied.notified() => {}
        }
        let status = status_of(port.as_ref());
        let fault = status.fault.is_some();
        writer.control(Control::Status(status));
        if fault {
            tokio::time::sleep(FAULT_EXIT_GRACE).await;
            let _ = faulted.send(());
            return;
        }
    }
}

/// Stdin as [`serve`] reads it, with SIGTERM and SIGINT turned into a
/// `shutdown` request, so a signalled worker leaves exactly as one the node
/// shut down, closing the device on the way out. SIGINT is taken too because a
/// terminal Ctrl-C reaches the whole foreground group, the worker included,
/// while the node is shutting it down properly. Lines are forwarded whole, so
/// the injected request never lands inside one of the node's.
///
/// The handlers replace the default dispositions for the life of the process,
/// so the first signal also arms [`SIGNAL_EXIT_WATCHDOG`]: a later signal is
/// swallowed, and a worker that never reads the request still ends. Call it
/// from inside a tokio runtime, before opening the device.
pub fn requests_until_signal() -> io::Result<DuplexStream> {
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
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
                _ = term.recv() => break,
                _ = int.recv() => break,
            }
        }
        arm_exit_watchdog(SIGNAL_EXIT_WATCHDOG);
        let mut line = Vec::new();
        let req = Request {
            id: 0,
            cmd: Cmd::Shutdown,
        };
        if write_request(&mut line, &req).is_ok() {
            let _ = tx.write_all(&line).await;
        }
    });
    Ok(rx)
}

/// End the process `after` from now, from a thread of its own so a blocked
/// runtime cannot hold it up. `_exit` rather than `std::process::exit`: `exit`
/// runs the C `atexit` handlers and static destructors, and a driver hung in
/// `open()` may hold a lock one of them takes, which would hang the exit too.
/// Code 0: the worker was asked to stop and stopped, as after a `shutdown`.
fn arm_exit_watchdog(after: Duration) {
    let spawned = std::thread::Builder::new()
        .name("worker-exit-watchdog".into())
        .spawn(move || {
            std::thread::sleep(after);
            eprintln!(
                "zeta-source-worker: still running {} s after a stop signal; exiting",
                after.as_secs()
            );
            // SAFETY: `_exit` takes no pointers and never returns.
            unsafe { libc::_exit(0) }
        });
    if let Err(e) = spawned {
        eprintln!("zeta-source-worker: exit watchdog thread: {e}");
    }
}

/// Point fd 1 at stderr and return a duplicate of the original stdout for the
/// frame writer. A driver (or a stray `println!`) that writes to stdout would
/// otherwise corrupt the frame stream; after this it lands on stderr, which
/// the parent forwards as log lines. The duplicate is close-on-exec, so a
/// program the driver spawns does not inherit the frame stream.
pub fn redirect_stdout_to_stderr() -> io::Result<RawFd> {
    io::stdout().flush()?;
    // SAFETY: plain fd syscalls on descriptors this process owns; no memory is
    // shared with them.
    let frames = unsafe { libc::fcntl(1, libc::F_DUPFD_CLOEXEC, 3) };
    if frames < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above; fd 1 is replaced atomically by a copy of fd 2.
    if unsafe { libc::dup2(2, 1) } < 0 {
        let e = io::Error::last_os_error();
        // SAFETY: `frames` was opened above and is not used after this.
        unsafe { libc::close(frames) };
        return Err(e);
    }
    Ok(frames)
}
