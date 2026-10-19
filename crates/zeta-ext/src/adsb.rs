//! `adsb_decode`: Mode S / ADS-B decoding by dump1090 running as a child
//! process.
//!
//! Each run picks a free loopback port and spawns
//! `<program> --ifile - --iformat SC16 --net --net-bind-address 127.0.0.1
//! --net-sbs-port <port> --net-ri-port 0 --net-ro-port 0 --net-bi-port 0
//! --net-bo-port 0 --quiet`, connects to the SBS port, then feeds the input
//! IQ (resampled to [`ADSB_RATE_HZ`], as interleaved little-endian `i16`) to
//! its stdin while turning every SBS `MSG` line into one event
//! ([`crate::sbs::parse_line`]).
//!
//! **Process policy** (as for plugins): spawned directly, never through a
//! shell, with a cleared environment holding only [`PASS_ENV`] and whatever
//! [`AdsbDecodeStage::with_env`] adds; `kill_on_drop`; stderr copied to ours a
//! line at a time, at most 4 096 bytes per piece; every way out of `run`
//! kills the child if it is still running and reaps it. No await on the
//! child, its pipes or its socket can outlast the cancel token: each sits in
//! a `select!` with it or under a timeout.
//!
//! **Ending.** Cancel closes stdin, kills and reaps the child and returns
//! `Cancelled`. When the input closes, stdin is closed and the child gets
//! [`EXIT_GRACE`] to finish and exit; a clean exit then is `Ok(())` (lines
//! still in the socket are published first). Any other exit of the child is
//! `Failed("adsb_decode: <program> exited with <status>")`.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, ChildStderr, ChildStdin, Command};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Provenance, Span, StreamId, StreamKind, Timestamp};
use zeta_dsp::resample::Resampler;

/// The only sample rate dump1090 takes.
pub const ADSB_RATE_HZ: u64 = 2_400_000;
/// Program names searched for in `PATH`, in this order.
pub const PROGRAMS: [&str; 4] = ["dump1090", "dump1090-fa", "dump1090-mutability", "readsb"];
/// Host variables passed through to the child unchanged, when set.
pub const PASS_ENV: [&str; 6] = ["PATH", "HOME", "LANG", "LC_ALL", "LC_CTYPE", "TERM"];

/// Why a stage without a program fails.
const NO_PROGRAM: &str = "adsb_decode: no dump1090 program (install dump1090 or set ZETA_DUMP1090)";
/// Longest stderr or SBS line handled in one piece.
const LINE_MAX: usize = 4096;
/// How long the child has to open its SBS port, and how often it is tried.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_RETRY: Duration = Duration::from_millis(100);
/// How long the child has to exit once its input has closed.
pub const EXIT_GRACE: Duration = Duration::from_secs(5);
/// How long SBS lines still in the socket are read after the child exits.
const DRAIN: Duration = Duration::from_secs(1);
/// How long a killed child has to be reaped.
const REAP: Duration = Duration::from_secs(5);
/// Stderr lines kept for error messages.
const STDERR_TAIL: usize = 8;

/// The dump1090 to run: `ZETA_DUMP1090` when it is set to an absolute path
/// that exists, else the first of [`PROGRAMS`] found in `PATH`.
pub fn find_dump1090() -> Option<PathBuf> {
    find_program(std::env::var_os("ZETA_DUMP1090"), std::env::var_os("PATH"))
}

fn find_program(over: Option<OsString>, path: Option<OsString>) -> Option<PathBuf> {
    if let Some(p) = over.filter(|p| !p.is_empty()).map(PathBuf::from)
        && p.is_absolute()
        && p.is_file()
    {
        return Some(p);
    }
    let path = path?;
    PROGRAMS.iter().find_map(|name| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|c| is_executable(c))
    })
}

fn is_executable(p: &Path) -> bool {
    let Ok(m) = std::fs::metadata(p) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.is_file() && m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        m.is_file()
    }
}

/// `adsb_decode` takes no parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct AdsbParams {}

/// IQ in, one event per decoded Mode S message out, via dump1090.
pub struct AdsbDecodeStage {
    spec: StageSpec,
    program: Option<PathBuf>,
    env: Vec<(OsString, OsString)>,
}

impl AdsbDecodeStage {
    /// The stage running `program`; with `None` every run fails at once.
    pub fn new(program: Option<PathBuf>) -> Self {
        Self {
            spec: StageSpec {
                kind: "adsb_decode",
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Event],
                params_schema: schemars::schema_for!(AdsbParams),
            },
            program,
            env: Vec::new(),
        }
    }

    /// The program this stage runs.
    pub fn program(&self) -> Option<&Path> {
        self.program.as_deref()
    }

    /// Extra variables for the child, on top of [`PASS_ENV`].
    pub fn with_env<I, K, V>(mut self, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        self.env
            .extend(vars.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    fn spawn(&self, program: &Path, port: u16) -> io::Result<Child> {
        let port = port.to_string();
        Command::new(program)
            .args(["--ifile", "-", "--iformat", "SC16", "--net"])
            .args(["--net-bind-address", "127.0.0.1", "--net-sbs-port", &port])
            .args(["--net-ri-port", "0", "--net-ro-port", "0"])
            .args(["--net-bi-port", "0", "--net-bo-port", "0", "--quiet"])
            .env_clear()
            .envs(
                std::env::vars_os()
                    .filter(|(k, _)| k.to_str().is_some_and(|k| PASS_ENV.contains(&k))),
            )
            .envs(self.env.iter().cloned())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
    }
}

#[async_trait]
impl Stage for AdsbDecodeStage {
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
        ctx: StageContext,
    ) -> Result<(), StageError> {
        let Some(program) = self.program.clone() else {
            return Err(StageError::Failed(NO_PROGRAM.into()));
        };
        let _: AdsbParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("adsb_decode params: {e}")))?;
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        if inputs.len() != 1 {
            return Err(StageError::Failed("expected 1 input".into()));
        }
        let rx = inputs.remove(0);
        let name = program.display().to_string();
        let fail = |m: String| StageError::Failed(format!("adsb_decode: {m}"));

        let port = free_port().map_err(|e| fail(format!("no free loopback port: {e}")))?;
        let mut child = self
            .spawn(&program, port)
            .map_err(|e| fail(format!("cannot run {name}: {e}")))?;
        let (Some(stdin), Some(stderr)) = (child.stdin.take(), child.stderr.take()) else {
            reap(&mut child).await;
            return Err(fail("stdio pipes missing".into()));
        };
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let stderr_task = tokio::spawn(copy_stderr(stderr, tail.clone()));

        let sock = match connect(port, &mut child, &cancel).await {
            Ok(s) => s,
            Err(cancelled) => {
                reap(&mut child).await;
                if cancelled {
                    return Err(StageError::Cancelled);
                }
                // The child is gone: let the copier finish its last lines.
                let _ = tokio::time::timeout(Duration::from_millis(500), stderr_task).await;
                return Err(fail(format!(
                    "{name} did not open its SBS port: {}",
                    tail_text(&tail)
                )));
            }
        };

        let last_prov = Arc::new(Mutex::new(None));
        // Set by `feed` before it drops stdin, so a child exiting on that EOF
        // is seen as a clean end even before the writer task's result is.
        let input_done = Arc::new(AtomicBool::new(false));
        let mut writer = AbortOnDrop(tokio::spawn(feed(
            rx,
            stdin,
            last_prov.clone(),
            input_done.clone(),
        )));
        let publish = |line: &[u8]| {
            let Some(v) = crate::sbs::parse_line(&String::from_utf8_lossy(line)) else {
                return;
            };
            let prov = lock(&last_prov).clone().unwrap_or_else(|| Provenance {
                source: ctx.source.clone(),
                job: None,
                channel: None,
                span: Span {
                    start_ns: Timestamp(0),
                    end_ns: Timestamp(0),
                },
            });
            bus.publish(Frame::new(out.clone(), prov, Payload::Event(v)));
        };

        let mut sbs = BufReader::new(sock);
        let mut line = Vec::new();
        let mut sbs_open = true;
        let mut feeding = true;
        let mut input_closed = false;
        let mut exit_deadline: Option<Instant> = None;
        let result = loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break Err(StageError::Cancelled),
                r = read_line(&mut sbs, &mut line, LINE_MAX), if sbs_open => match r {
                    Ok(true) => {
                        publish(&line);
                        line.clear();
                    }
                    Ok(false) | Err(_) => {
                        publish(&line);
                        line.clear();
                        sbs_open = false;
                    }
                },
                end = &mut writer.0, if feeding => {
                    feeding = false;
                    match end {
                        Ok(FeedEnd::InputClosed) => {
                            input_closed = true;
                            exit_deadline = Some(Instant::now() + EXIT_GRACE);
                        }
                        // The child stopped reading: its exit follows.
                        Ok(FeedEnd::StdinClosed) => {}
                        Ok(FeedEnd::Failed(m)) => break Err(fail(m)),
                        Err(e) => break Err(fail(format!("input task: {e}"))),
                    }
                }
                status = child.wait() => {
                    if sbs_open {
                        drain(&mut sbs, &mut line, &publish).await;
                    }
                    let input_closed = input_closed || input_done.load(Ordering::SeqCst);
                    break match status {
                        Ok(s) if input_closed && s.success() => Ok(()),
                        Ok(s) => Err(fail(format!("{name} exited with {}", status_text(s)))),
                        Err(e) => Err(fail(format!("waiting for {name}: {e}"))),
                    };
                }
                _ = sleep_until(exit_deadline), if exit_deadline.is_some() => {
                    eprintln!("[adsb_decode] {name} still running {} s after its input closed; killing it", EXIT_GRACE.as_secs());
                    if sbs_open {
                        drain(&mut sbs, &mut line, &publish).await;
                    }
                    break Ok(());
                }
            }
        };
        writer.0.abort();
        reap(&mut child).await;
        result
    }
}

/// Aborts the task when dropped, so no path out of `run` leaves it running.
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A loopback port free at the time of asking.
fn free_port() -> io::Result<u16> {
    let l = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(l.local_addr()?.port())
}

/// Connect to the child's SBS port, retrying every [`CONNECT_RETRY`] for
/// [`CONNECT_TIMEOUT`]. `Err(true)` when cancelled, `Err(false)` when the time
/// ran out or the child exited.
async fn connect(
    port: u16,
    child: &mut Child,
    cancel: &CancellationToken,
) -> Result<TcpStream, bool> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(true),
            _ = child.wait() => return Err(false),
            r = tokio::time::timeout(CONNECT_RETRY, TcpStream::connect(("127.0.0.1", port))) => {
                if let Ok(Ok(s)) = r {
                    return Ok(s);
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(false);
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(true),
            _ = child.wait() => return Err(false),
            _ = tokio::time::sleep(CONNECT_RETRY) => {}
        }
    }
}

/// Kill the child if it is still running and wait for it, at most [`REAP`].
async fn reap(child: &mut Child) {
    let _ = child.start_kill();
    if tokio::time::timeout(REAP, child.wait()).await.is_err() {
        eprintln!("[adsb_decode] child not reaped within {} s", REAP.as_secs());
    }
}

/// Publish SBS lines still in the socket, for at most [`DRAIN`].
async fn drain<R, F>(r: &mut R, line: &mut Vec<u8>, publish: &F)
where
    R: AsyncBufRead + Unpin,
    F: Fn(&[u8]),
{
    let _ = tokio::time::timeout(DRAIN, async {
        loop {
            let more = matches!(read_line(r, line, LINE_MAX).await, Ok(true));
            publish(line);
            line.clear();
            if !more {
                break;
            }
        }
    })
    .await;
}

/// Append bytes from `r` to `line` up to and including the next `\n`, or
/// until `line` holds `max` bytes. `Ok(true)` when a line (or a `max`-byte
/// piece) is complete, `Ok(false)` at end of input with whatever was left in
/// `line`. Cancel-safe: progress lives in `line`, and the only await is
/// `fill_buf`.
async fn read_line<R: AsyncBufRead + Unpin>(
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

/// Copy the child's stderr to ours, prefixed `[adsb_decode] `, keeping the
/// last [`STDERR_TAIL`] lines in `tail`. Ends at EOF, when the child exits.
async fn copy_stderr(stderr: ChildStderr, tail: Arc<Mutex<VecDeque<String>>>) {
    let mut r = BufReader::new(stderr);
    let mut line = Vec::new();
    loop {
        let more = matches!(read_line(&mut r, &mut line, LINE_MAX).await, Ok(true));
        if !line.is_empty() {
            let text = String::from_utf8_lossy(&line).trim_end().to_owned();
            eprintln!("[adsb_decode] {text}");
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

fn tail_text(tail: &Mutex<VecDeque<String>>) -> String {
    let t = lock(tail);
    if t.is_empty() {
        "no output on stderr".into()
    } else {
        t.iter().cloned().collect::<Vec<_>>().join("; ")
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

/// How the input side ended.
enum FeedEnd {
    /// The input closed; stdin has been closed after it.
    InputClosed,
    /// Writing to the child failed: it has stopped reading.
    StdinClosed,
    Failed(String),
}

/// Per-rate conversion to [`ADSB_RATE_HZ`]: one resampler for I, one for Q.
struct Converter {
    rate_hz: u64,
    i: Resampler,
    q: Resampler,
}

impl Converter {
    fn new(rate_hz: u64) -> Result<Self, String> {
        Ok(Self {
            rate_hz,
            i: Resampler::new(rate_hz, ADSB_RATE_HZ)?,
            q: Resampler::new(rate_hz, ADSB_RATE_HZ)?,
        })
    }

    /// `samples` at [`ADSB_RATE_HZ`] as SC16: interleaved I/Q, each scaled by
    /// 32 767, clamped and written as a little-endian `i16`.
    fn sc16(&mut self, samples: &[Complex<f32>]) -> Vec<u8> {
        let to_i16 = |x: f32| (x * 32_767.0).round().clamp(-32_768.0, 32_767.0) as i16;
        let mut out = Vec::new();
        let mut push = |i: f32, q: f32| {
            out.extend_from_slice(&to_i16(i).to_le_bytes());
            out.extend_from_slice(&to_i16(q).to_le_bytes());
        };
        if self.rate_hz == ADSB_RATE_HZ {
            samples.iter().for_each(|s| push(s.re, s.im));
        } else {
            let re: Vec<f32> = samples.iter().map(|s| s.re).collect();
            let im: Vec<f32> = samples.iter().map(|s| s.im).collect();
            let (re, im) = (self.i.process(&re), self.q.process(&im));
            re.iter().zip(&im).for_each(|(&i, &q)| push(i, q));
        }
        out
    }
}

/// The input side: convert each frame and write it to the child's stdin,
/// recording the frame's provenance for the events. Lagged input is skipped;
/// a rate change starts fresh resamplers. When the input closes, `input_done`
/// is set before stdin is dropped.
async fn feed(
    mut rx: broadcast::Receiver<Frame>,
    mut stdin: ChildStdin,
    last_prov: Arc<Mutex<Option<Provenance>>>,
    input_done: Arc<AtomicBool>,
) -> FeedEnd {
    let mut conv: Option<Converter> = None;
    loop {
        let frame = match rx.recv().await {
            Ok(f) => f,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                eprintln!("[adsb_decode] input lagged, {n} frames skipped");
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => {
                // Before returning drops stdin (the child's EOF).
                input_done.store(true, Ordering::SeqCst);
                return FeedEnd::InputClosed;
            }
        };
        let Payload::Iq(block) = &frame.payload else {
            return FeedEnd::Failed("expects iq frames".into());
        };
        *lock(&last_prov) = Some(frame.provenance.clone());
        let rate = block.sample_rate_hz.0;
        let c = match &mut conv {
            Some(c) if c.rate_hz == rate => c,
            _ => match Converter::new(rate) {
                Ok(c) => conv.insert(c),
                Err(e) => return FeedEnd::Failed(e),
            },
        };
        let bytes = c.sc16(&block.samples);
        if stdin.write_all(&bytes).await.is_err() {
            return FeedEnd::StdinClosed;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_program_prefers_override_then_path_order() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let make = |p: &Path| {
            std::fs::write(p, "#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        };
        make(&b.join("readsb"));
        make(&b.join("dump1090-fa"));
        // A non-executable dump1090 earlier in PATH is passed over.
        std::fs::write(a.join("dump1090"), "").unwrap();
        let path = std::env::join_paths([&a, &b]).unwrap();
        assert_eq!(
            find_program(None, Some(path.clone())),
            Some(b.join("dump1090-fa"))
        );
        assert_eq!(
            find_program(Some("".into()), Some(path.clone())),
            Some(b.join("dump1090-fa"))
        );
        let over = b.join("readsb");
        assert_eq!(
            find_program(Some(over.clone().into()), Some(path.clone())),
            Some(over)
        );
        // A relative or missing override falls back to PATH.
        assert_eq!(
            find_program(Some("readsb".into()), Some(path.clone())),
            Some(b.join("dump1090-fa"))
        );
        assert_eq!(find_program(None, Some("/nonexistent".into())), None);
        assert_eq!(find_program(None, None), None);
    }

    #[test]
    fn sc16_scales_clamps_and_resamples() {
        let mut c = Converter::new(ADSB_RATE_HZ).unwrap();
        let b = c.sc16(&[Complex::new(0.5, -1.0), Complex::new(2.0, -2.0)]);
        let v: Vec<i16> = b
            .chunks(2)
            .map(|p| i16::from_le_bytes([p[0], p[1]]))
            .collect();
        assert_eq!(v, [16_384, -32_767, 32_767, -32_768]);
        let mut c = Converter::new(2_560_000).unwrap();
        let n: usize = (0..10)
            .map(|_| c.sc16(&[Complex::new(0.1, 0.1); 2_133]).len())
            .sum();
        // 21 330 samples at 2.56 MHz are 19 997 at 2.4 MHz, 4 bytes each.
        assert!((n / 4).abs_diff(19_997) <= 1, "{n}");
    }
}
