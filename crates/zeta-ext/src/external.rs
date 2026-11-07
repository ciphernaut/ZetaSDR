//! `external_decode`: a mode row's external program run as a stage (spec 5.2).
//!
//! The params are `{"mode": "<row id>"}` only: the program block (programs, args,
//! input, output, parser) is resolved from the loaded [`ModeRegistry`] at launch, never
//! taken from the graph, so a `graph_put` cannot run an arbitrary program. The stage
//! reaches the registry through a [`ModesHandle`] set once the registry is loaded (the
//! stage registry the rows validate against is built first) and replaced on a reload,
//! read at each run; a run with the handle still empty fails `external_decode: no mode
//! registry`.
//!
//! **A run.** Pick a free loopback port when the args or output use `$port`; substitute
//! `$port`, `$rate_hz` (the row's `input.rate_hz`), `$center_hz` (the first IQ frame's
//! centre, waited for only when used) and `$fifo` (the named pipe the input goes to with
//! `via: fifo`, in a directory of its own under the stage's run directory,
//! `<state_dir>/run` on a node, removed when the run ends; a stage without one fails a
//! fifo row); spawn ([`crate::proc`]); feed the IQ
//! frames resampled from the rate each frame carries to `input.rate_hz` and written as
//! `input.format` (`cu8`, `cs8`, `cs16`, `cf32`, interleaved I/Q, little-endian) to stdin
//! or the fifo, or for an `input.stream: audio` row the audio frames of its demod mixed
//! to mono, resampled and written `s16` or `f32`; with `container: wav` one WAV header
//! ([`wav_header`]) goes first, once per run; read output lines from stdout or the TCP
//! port (connecting with retries for [`CONNECT_TIMEOUT`]); parse each with the row's parser ([`crate::parsers`]) and
//! publish it in the row's envelope (`protocol`, `summary`, `t_ns` the latest input
//! frame's start). A line the parser cannot read is counted in the job's context as
//! `external.unparsed`, never fatal; a program that prints nothing is not an error.
//!
//! **Ending.** Cancel kills and reaps the child and returns `Cancelled`. When the input
//! closes, the child's input is closed and it gets [`EXIT_GRACE`] to finish; a clean exit
//! then is `Ok(())` (lines still unread are published first). Any other exit is
//! `Failed("<mode id>: <program> exited with <status> (<stderr tail>)")`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, ChildStdin};
use tokio::sync::broadcast;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use zeta_core::modes::{
    Container, Decoder, FieldPath, InputStream, InputVia, ModeRegistry, ModeRow, ModeState,
    OutputVia, Parser, ProgramBlock, SampleFormat,
};
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Provenance, Span, StreamId, StreamKind, Timestamp};
use zeta_dsp::resample::Resampler;

use crate::proc::{self, AbortOnDrop, EXIT_GRACE, LINE_MAX, Pipes, Proc, lock};

/// The stage kind.
pub const EXTERNAL_DECODE_KIND: &str = "external_decode";
/// How long the child has to open its output port, and how often it is tried.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_RETRY: Duration = Duration::from_millis(100);
/// How long output lines still unread are published after the child exits.
const DRAIN: Duration = Duration::from_secs(1);
/// How often the job's `external.unparsed` count is noted at most.
const NOTE_EVERY: Duration = Duration::from_secs(1);
/// How often opening the fifo is retried while the child has not opened it to read.
const FIFO_RETRY: Duration = Duration::from_millis(20);

/// The length of the header [`wav_header`] writes.
pub const WAV_HEADER_LEN: usize = 44;
/// The RIFF and data sizes of a WAV header on a stream of unknown length.
pub const WAV_STREAM_SIZE: u32 = 0xFFFF_FFFF;

/// The loaded mode registry, set once the rows are loaded and replaced on a reload; a
/// run reads the rows the handle holds when it starts.
pub use zeta_core::modes::ModesHandle;

/// `external_decode`'s params: the row to run.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ExternalParams {
    /// A program row's id (`modes_list`).
    pub mode: String,
}

/// IQ or audio in (as the row's `input.stream` says), one event per parsed output line
/// of the row's program out.
pub struct ExternalDecodeStage {
    spec: StageSpec,
    modes: ModesHandle,
    env: Vec<(OsString, OsString)>,
    run_dir: Option<PathBuf>,
}

impl ExternalDecodeStage {
    /// The stage running the rows of `modes`.
    pub fn new(modes: Arc<ModeRegistry>) -> Self {
        Self::deferred(ModesHandle::from(modes))
    }

    /// The stage running the rows of the registry `handle` holds at each run (set it
    /// once the registry is loaded; until then every run fails).
    pub fn deferred(handle: ModesHandle) -> Self {
        Self {
            spec: StageSpec {
                kind: EXTERNAL_DECODE_KIND,
                inputs: vec![StreamKind::Iq, StreamKind::Audio],
                outputs: vec![StreamKind::Event],
                params_schema: schemars::schema_for!(ExternalParams),
            },
            modes: handle,
            env: Vec::new(),
            run_dir: None,
        }
    }

    /// The directory fifo rows make their pipes in (`<state_dir>/run` on a node), made
    /// when first needed. Without one a fifo row fails.
    pub fn with_run_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.run_dir = Some(dir.into());
        self
    }

    /// Extra variables for the child, on top of the cleared environment (tests).
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
}

/// The ready program row `id` of the registry `handle` holds and the program it runs:
/// the registry's probed program, else (a registry nobody probed, as in tests) the row's
/// `programs` looked up now. Shared by `external_decode` and the `adsb_decode` alias so
/// both apply the same readiness rules. Failures: `<prefix>: no mode registry`,
/// `<prefix>: unknown mode <id>`, `<id>: <reason>` for a row that is not ready,
/// `<prefix>: mode <id> is not a program row`, `<id>: no program found (tried ...)`.
pub(crate) fn resolve(
    handle: &ModesHandle,
    id: &str,
    prefix: &str,
) -> Result<(ModeRow, PathBuf), StageError> {
    let failed = |m: String| StageError::Failed(m);
    let reg = handle
        .get()
        .ok_or_else(|| failed(format!("{prefix}: no mode registry")))?;
    let m = reg
        .get(id)
        .ok_or_else(|| failed(format!("{prefix}: unknown mode {id}")))?;
    if m.state != ModeState::Ready {
        return Err(failed(format!(
            "{id}: {}",
            m.reason.as_deref().unwrap_or("not ready")
        )));
    }
    let Decoder::Program(block) = &m.row.decoder else {
        return Err(failed(format!("{prefix}: mode {id} is not a program row")));
    };
    let program = m
        .program
        .clone()
        .or_else(|| proc::find_handler(&block.programs))
        .ok_or_else(|| {
            failed(format!(
                "{id}: no program found (tried {})",
                block.programs.join(", ")
            ))
        })?;
    Ok((m.row.clone(), program))
}

#[async_trait]
impl Stage for ExternalDecodeStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    async fn run(
        &self,
        params: serde_json::Value,
        inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        ctx: StageContext,
    ) -> Result<(), StageError> {
        let p: ExternalParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("external_decode params: {e}")))?;
        let (row, program) = resolve(&self.modes, &p.mode, EXTERNAL_DECODE_KIND)?;
        let run_dir = self.run_dir.as_deref();
        run_row(
            &row, &program, &self.env, run_dir, inputs, outputs, bus, cancel, ctx,
        )
        .await
    }
}

/// Run `program` as program row `row`'s decoder (see the module docs), a fifo input in
/// a directory of its own under `run_dir`. `row` must be a program row.
#[allow(clippy::too_many_arguments)]
pub async fn run_row(
    row: &ModeRow,
    program: &Path,
    env: &[(OsString, OsString)],
    run_dir: Option<&Path>,
    mut inputs: Vec<broadcast::Receiver<Frame>>,
    outputs: Vec<StreamId>,
    bus: Arc<StreamBus>,
    cancel: CancellationToken,
    ctx: StageContext,
) -> Result<(), StageError> {
    let id = row.id.0.as_str();
    let fail = |m: String| StageError::Failed(format!("{id}: {m}"));
    let Decoder::Program(block) = &row.decoder else {
        return Err(fail("not a program row".into()));
    };
    let Some(out) = outputs.first().cloned() else {
        return Err(fail("expected 1 output".into()));
    };
    if inputs.len() != 1 {
        return Err(fail("expected 1 input".into()));
    }
    let mut rx = inputs.remove(0);
    let name = program.display().to_string();

    // `$center_hz` is the first frame's centre: wait for it only when it is used.
    let mut first = None;
    if block
        .args
        .iter()
        .any(|a| a.contains("$center_hz") || a.contains("$center_khz"))
    {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(StageError::Cancelled),
            f = recv(&mut rx, id) => match f {
                Some(f) => first = Some(f),
                None => return Ok(()),
            },
        }
    }
    let header = match block.input.container {
        Container::Raw => None,
        Container::Wav => {
            let rate = u32::try_from(block.input.rate_hz)
                .map_err(|_| fail(format!("rate {} too high for wav", block.input.rate_hz)))?;
            Some(wav_header(rate, block.input.format))
        }
    };
    let center_hz = match first.as_ref().map(|f| &f.payload) {
        Some(Payload::Iq(b)) => b.center_hz.0,
        _ => 0,
    };
    let uses_port =
        block.output.via == OutputVia::Tcp || block.args.iter().any(|a| a.contains("$port"));
    let port = match uses_port {
        true => free_port().map_err(|e| fail(format!("no free loopback port: {e}")))?,
        false => 0,
    };
    // The fifo's directory lives as long as the run.
    let fifo_dir = match block.input.via {
        InputVia::Fifo => {
            let Some(run_dir) = run_dir else {
                return Err(fail("fifo input needs a run directory".into()));
            };
            Some(fifo_dir(run_dir).map_err(|e| {
                fail(format!(
                    "no fifo directory under {}: {e}",
                    run_dir.display()
                ))
            })?)
        }
        InputVia::Stdin => None,
    };
    let fifo = fifo_dir.as_ref().map(|d| d.path().join("input"));
    if let Some(f) = &fifo {
        make_fifo(f).map_err(|e| fail(format!("cannot make fifo {}: {e}", f.display())))?;
    }
    let args = substitute(block, port, center_hz, fifo.as_deref());
    let mut child = proc::spawn(
        program,
        &args,
        env,
        Pipes {
            stdin: block.input.via == InputVia::Stdin,
            stdout: block.output.via == OutputVia::Stdout,
        },
    )
    .map_err(|e| fail(format!("cannot run {name}: {e}")))?;
    let Some(stderr) = child.child.stderr.take() else {
        proc::reap(&mut child, id).await;
        return Err(fail("stdio pipes missing".into()));
    };
    let tail = proc::Tail::default();
    let stderr_task = tokio::spawn(proc::copy_stderr(id.to_string(), stderr, tail.clone()));
    let sink = match &fifo {
        Some(f) => Sink::Fifo(f.clone()),
        None => match child.child.stdin.take() {
            Some(s) => Sink::Stdin(s),
            None => {
                proc::reap(&mut child, id).await;
                return Err(fail("stdio pipes missing".into()));
            }
        },
    };

    let last_prov = Arc::new(Mutex::new(None));
    // Set by the feed before it drops its writer, so a child exiting on that EOF is
    // seen as a clean end even before the writer task's result is.
    let input_done = Arc::new(AtomicBool::new(false));
    let feed = Feed {
        id: id.to_string(),
        rate_hz: block.input.rate_hz,
        format: block.input.format,
        stream: block.input.stream,
        header,
        rx,
        first,
        sink,
        last_prov: last_prov.clone(),
        input_done: input_done.clone(),
    };
    // A fifo is opened by both ends at once: feed it from the start, so a program that
    // opens its input before its output port never waits on us. Stdin is fed once the
    // output is connected (as dump1090 has always been), the frames waiting meanwhile.
    let (early, feed) = match block.input.via {
        InputVia::Fifo => (Some(AbortOnDrop(tokio::spawn(feed.run()))), None),
        InputVia::Stdin => (None, Some(feed)),
    };

    let reader: Box<dyn AsyncBufRead + Unpin + Send> = match block.output.via {
        OutputVia::Stdout => match child.child.stdout.take() {
            Some(s) => Box::new(BufReader::new(s)),
            None => {
                proc::reap(&mut child, id).await;
                return Err(fail("stdio pipes missing".into()));
            }
        },
        OutputVia::Tcp => match connect(port, &mut child.child, &cancel).await {
            Ok(s) => Box::new(BufReader::new(s)),
            Err(cancelled) => {
                proc::reap(&mut child, id).await;
                if cancelled {
                    return Err(StageError::Cancelled);
                }
                // The child is gone: let the copier finish its last lines.
                let _ = tokio::time::timeout(Duration::from_millis(500), stderr_task).await;
                return Err(fail(format!(
                    "{name} did not open its {} port: {}",
                    parser_name(block.output.parser),
                    proc::tail_text(&tail)
                )));
            }
        },
    };
    let mut writer = match (early, feed) {
        (Some(w), _) => w,
        (None, Some(f)) => AbortOnDrop(tokio::spawn(f.run())),
        (None, None) => return Err(fail("input task missing".into())),
    };
    let publisher = Publisher {
        row,
        parser: block.output.parser,
        fields: block.output.fields.as_ref(),
        out,
        bus: &bus,
        ctx: &ctx,
        last_prov,
        unparsed: Mutex::new(Unparsed::default()),
    };
    let ends = Ends {
        cancel: &cancel,
        input_done: &input_done,
        name: &name,
        tail: &tail,
        fail: &fail,
    };
    let result = pump(reader, &mut writer, &mut child, &publisher, ends).await;
    publisher.note_unparsed(true);
    writer.0.abort();
    proc::reap(&mut child, id).await;
    drop(fifo_dir);
    result
}

/// What [`pump`] needs to know to end a run.
struct Ends<'a> {
    cancel: &'a CancellationToken,
    input_done: &'a AtomicBool,
    name: &'a str,
    tail: &'a proc::Tail,
    fail: &'a (dyn Fn(String) -> StageError + Sync),
}

/// The main loop: publish output lines until the child exits, the input closes and
/// the child is given up on, or the run is cancelled.
async fn pump(
    mut reader: Box<dyn AsyncBufRead + Unpin + Send>,
    writer: &mut AbortOnDrop<FeedEnd>,
    child: &mut Proc,
    publisher: &Publisher<'_>,
    e: Ends<'_>,
) -> Result<(), StageError> {
    let mut line = Vec::new();
    let mut output_open = true;
    let mut feeding = true;
    let mut input_closed = false;
    let mut exit_deadline: Option<Instant> = None;
    let name = e.name;
    loop {
        tokio::select! {
            biased;
            _ = e.cancel.cancelled() => break Err(StageError::Cancelled),
            r = proc::read_line(&mut reader, &mut line, LINE_MAX), if output_open => {
                if !matches!(r, Ok(true)) {
                    output_open = false;
                }
                publisher.publish(&line);
                line.clear();
            }
            end = &mut writer.0, if feeding => {
                feeding = false;
                match end {
                    Ok(FeedEnd::InputClosed) => {
                        input_closed = true;
                        exit_deadline = Some(Instant::now() + EXIT_GRACE);
                    }
                    // The child stopped reading: its exit follows.
                    Ok(FeedEnd::WriterClosed) => {}
                    Ok(FeedEnd::Failed(m)) => break Err((e.fail)(m)),
                    Err(err) => break Err((e.fail)(format!("input task: {err}"))),
                }
            }
            status = child.child.wait() => {
                if output_open {
                    drain(&mut reader, &mut line, publisher).await;
                }
                let input_closed = input_closed || e.input_done.load(Ordering::SeqCst);
                break match status {
                    Ok(s) if input_closed && s.success() => Ok(()),
                    Ok(s) => {
                        // Let the copier take the child's last stderr lines.
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        Err((e.fail)(format!(
                            "{name} exited with {} ({})",
                            proc::status_text(s),
                            proc::tail_text(e.tail)
                        )))
                    }
                    Err(err) => Err((e.fail)(format!("waiting for {name}: {err}"))),
                };
            }
            _ = sleep_until(exit_deadline), if exit_deadline.is_some() => {
                eprintln!(
                    "[{}] {name} still running {} s after its input closed; killing it",
                    publisher.row.id.0,
                    EXIT_GRACE.as_secs()
                );
                if output_open {
                    drain(&mut reader, &mut line, publisher).await;
                }
                break Ok(());
            }
        }
    }
}

/// The row's args with `$port`, `$rate_hz`, `$center_hz`, `$center_khz` and `$fifo`
/// substituted.
fn substitute(block: &ProgramBlock, port: u16, center_hz: u64, fifo: Option<&Path>) -> Vec<String> {
    let fifo = fifo.map(|f| f.display().to_string()).unwrap_or_default();
    block
        .args
        .iter()
        .map(|a| {
            a.replace("$port", &port.to_string())
                .replace("$rate_hz", &block.input.rate_hz.to_string())
                .replace("$center_hz", &center_hz.to_string())
                .replace("$center_khz", &zeta_core::modes::center_khz_text(center_hz))
                .replace("$fifo", &fifo)
        })
        .collect()
}

/// The event's fields read out of a parsed `line`: for each key of `map` the first pointer
/// that resolves to a non-null value, cloned (a string stripped of the path's `trim`
/// characters at both ends); a key with none is absent.
pub fn apply_fields(map: &BTreeMap<String, FieldPath>, line: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for (key, path) in map {
        let found = path
            .pointers()
            .iter()
            .filter_map(|p| line.pointer(p))
            .find(|v| !v.is_null());
        let Some(v) = found else { continue };
        let v = match (v.as_str(), path.trim()) {
            (Some(s), Some(t)) => Value::from(s.trim_matches(|c| t.contains(c))),
            _ => v.clone(),
        };
        out.insert(key.clone(), v);
    }
    Value::Object(out)
}

/// The parser's name as a row spells it, upper-cased (`SBS`, `JSON_LINES`).
fn parser_name(p: Parser) -> String {
    serde_json::to_value(p)
        .ok()
        .and_then(|v| v.as_str().map(str::to_uppercase))
        .unwrap_or_default()
}

/// Turns output lines into published events, counting the unreadable ones.
struct Publisher<'a> {
    row: &'a ModeRow,
    parser: Parser,
    fields: Option<&'a BTreeMap<String, FieldPath>>,
    out: StreamId,
    bus: &'a StreamBus,
    ctx: &'a StageContext,
    last_prov: Arc<Mutex<Option<Provenance>>>,
    unparsed: Mutex<Unparsed>,
}

/// Unreadable lines so far, and when the count was last noted.
#[derive(Default)]
struct Unparsed {
    count: u64,
    noted: u64,
    at: Option<Instant>,
}

impl Publisher<'_> {
    /// Publish one output line's event; count it when the parser cannot read it. Blank
    /// lines are skipped.
    fn publish(&self, line: &[u8]) {
        let text = String::from_utf8_lossy(line);
        if text.trim().is_empty() {
            return;
        }
        let Some(mut fields) = crate::parsers::parse(self.parser, &text) else {
            lock(&self.unparsed).count += 1;
            self.note_unparsed(false);
            return;
        };
        if let Some(map) = self.fields {
            fields = apply_fields(map, &fields);
        }
        let prov = lock(&self.last_prov).clone().unwrap_or_else(|| {
            let t = now();
            Provenance {
                source: self.ctx.source.clone(),
                job: None,
                channel: None,
                span: Span {
                    start_ns: t,
                    end_ns: t,
                },
            }
        });
        let ev = zeta_core::modes::summary::envelope(self.row, prov.span.start_ns.0, fields);
        self.bus
            .publish(Frame::new(self.out.clone(), prov, Payload::Event(ev)));
    }

    /// Note `{"external": {"unparsed": n}}` in the job's context when the count changed,
    /// at most once per [`NOTE_EVERY`] unless `force`.
    fn note_unparsed(&self, force: bool) {
        let mut u = lock(&self.unparsed);
        let due = force || u.at.is_none_or(|t| t.elapsed() >= NOTE_EVERY);
        if u.count == u.noted || !due {
            return;
        }
        u.noted = u.count;
        u.at = Some(Instant::now());
        let n = u.count;
        drop(u);
        self.ctx
            .note(serde_json::json!({ "external": { "unparsed": n } }));
    }
}

fn now() -> Timestamp {
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    Timestamp(ns)
}

/// Publish output lines still unread, for at most [`DRAIN`].
async fn drain<R>(r: &mut R, line: &mut Vec<u8>, publisher: &Publisher<'_>)
where
    R: AsyncBufRead + Unpin + ?Sized,
{
    let _ = tokio::time::timeout(DRAIN, async {
        loop {
            let more = matches!(proc::read_line(r, line, LINE_MAX).await, Ok(true));
            publisher.publish(line);
            line.clear();
            if !more {
                break;
            }
        }
    })
    .await;
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

/// A loopback port free at the time of asking.
fn free_port() -> io::Result<u16> {
    let l = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(l.local_addr()?.port())
}

/// Connect to the child's output port, retrying every [`CONNECT_RETRY`] for
/// [`CONNECT_TIMEOUT`]. `Err(true)` when cancelled, `Err(false)` when the time ran out
/// or the child exited.
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

/// A directory of this run's own under `run_dir` (made when missing) for its fifo,
/// removed with the fifo when dropped.
fn fifo_dir(run_dir: &Path) -> io::Result<tempfile::TempDir> {
    std::fs::create_dir_all(run_dir)?;
    tempfile::Builder::new()
        .prefix("external-")
        .tempdir_in(run_dir)
}

#[cfg(unix)]
fn make_fifo(path: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    #[cfg(target_vendor = "apple")]
    type ModeT = u16;
    #[cfg(not(target_vendor = "apple"))]
    type ModeT = u32;
    // `mkfifo(3)`, declared here rather than taking a `libc` dependency for one call.
    unsafe extern "C" {
        fn mkfifo(path: *const std::ffi::c_char, mode: ModeT) -> i32;
    }
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    // SAFETY: `c` is a NUL-terminated string that outlives the call.
    if unsafe { mkfifo(c.as_ptr(), 0o600) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn make_fifo(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "fifo input needs a Unix host",
    ))
}

/// Where the input goes.
enum Sink {
    Stdin(ChildStdin),
    /// A named pipe, opened for writing once the child has opened it to read.
    Fifo(PathBuf),
}

/// How the input side ended.
enum FeedEnd {
    /// The input closed; the child's input has been closed after it.
    InputClosed,
    /// Writing to the child failed: it has stopped reading.
    WriterClosed,
    Failed(String),
}

/// The input side of a run: convert each frame and write it to the child.
struct Feed {
    id: String,
    rate_hz: u64,
    format: SampleFormat,
    stream: InputStream,
    /// The WAV header still to write before the first sample bytes (`container: wav`).
    header: Option<[u8; WAV_HEADER_LEN]>,
    rx: broadcast::Receiver<Frame>,
    /// A frame already taken off `rx` (for `$center_hz`), fed first.
    first: Option<Frame>,
    sink: Sink,
    last_prov: Arc<Mutex<Option<Provenance>>>,
    input_done: Arc<AtomicBool>,
}

impl Feed {
    /// Feed until the input closes or the child stops reading, recording each frame's
    /// provenance for the events. Lagged input is skipped; a rate change starts fresh
    /// resamplers. When the input closes, `input_done` is set before the writer is
    /// dropped (the child's EOF).
    async fn run(mut self) -> FeedEnd {
        let mut w: Box<dyn AsyncWrite + Unpin + Send> = match self.sink {
            Sink::Stdin(s) => Box::new(s),
            Sink::Fifo(ref p) => match open_fifo(p).await {
                Ok(s) => Box::new(s),
                Err(e) => return FeedEnd::Failed(format!("cannot open fifo: {e}")),
            },
        };
        let mut conv: Option<Converter> = None;
        loop {
            let frame = match self.first.take() {
                Some(f) => f,
                None => match recv(&mut self.rx, &self.id).await {
                    Some(f) => f,
                    None => {
                        let _ = w.flush().await;
                        // Before returning drops the writer (the child's EOF).
                        self.input_done.store(true, Ordering::SeqCst);
                        return FeedEnd::InputClosed;
                    }
                },
            };
            let (out_rate, format) = (self.rate_hz, self.format);
            let bytes = match (&frame.payload, self.stream) {
                (Payload::Iq(b), InputStream::Iq) => {
                    converter(&mut conv, b.sample_rate_hz.0, out_rate, format)
                        .map(|c| c.convert(&b.samples))
                }
                (
                    Payload::Audio {
                        sample_rate_hz,
                        channels,
                        samples,
                    },
                    InputStream::Audio,
                ) => converter(&mut conv, sample_rate_hz.0, out_rate, format)
                    .map(|c| c.convert_audio(samples, *channels)),
                (_, InputStream::Iq) => Err("expects iq frames".into()),
                (_, InputStream::Audio) => Err("expects audio frames".into()),
            };
            let bytes = match bytes {
                Ok(b) => b,
                Err(e) => return FeedEnd::Failed(e),
            };
            *lock(&self.last_prov) = Some(frame.provenance.clone());
            if let Some(h) = self.header.take()
                && w.write_all(&h).await.is_err()
            {
                return FeedEnd::WriterClosed;
            }
            if w.write_all(&bytes).await.is_err() {
                return FeedEnd::WriterClosed;
            }
        }
    }
}

/// The converter for frames at `in_rate_hz`: `conv` when it already converts that rate,
/// else a fresh one (a rate change starts fresh resamplers).
fn converter(
    conv: &mut Option<Converter>,
    in_rate_hz: u64,
    out_rate_hz: u64,
    format: SampleFormat,
) -> Result<&mut Converter, String> {
    match conv.take() {
        Some(c) if c.in_rate_hz == in_rate_hz => Ok(conv.insert(c)),
        _ => Ok(conv.insert(Converter::new(in_rate_hz, out_rate_hz, format)?)),
    }
}

/// The next frame, skipping (and reporting) lagged ones; `None` once the input closed.
async fn recv(rx: &mut broadcast::Receiver<Frame>, id: &str) -> Option<Frame> {
    loop {
        match rx.recv().await {
            Ok(f) => return Some(f),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                eprintln!("[{id}] input lagged, {n} frames skipped");
            }
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
}

/// Open the fifo for writing, retrying while no reader has it open yet (`ENXIO`).
#[cfg(unix)]
async fn open_fifo(path: &Path) -> io::Result<tokio::net::unix::pipe::Sender> {
    /// `ENXIO` on Linux and the BSDs alike.
    const ENXIO: i32 = 6;
    loop {
        match tokio::net::unix::pipe::OpenOptions::new().open_sender(path) {
            Ok(s) => return Ok(s),
            Err(e) if e.raw_os_error() == Some(ENXIO) => tokio::time::sleep(FIFO_RETRY).await,
            Err(e) => return Err(e),
        }
    }
}

#[cfg(not(unix))]
async fn open_fifo(_path: &Path) -> io::Result<tokio::io::Sink> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "fifo input needs a Unix host",
    ))
}

/// A RIFF/WAVE header for a stream of `format` samples at `rate_hz` whose length is not
/// known: RIFF and data sizes [`WAV_STREAM_SIZE`], format tag 3 (IEEE float) for `f32`
/// and `cf32`, else 1 (PCM), 2 channels for an IQ format, else 1. A row pairs `wav` with
/// `s16`, `f32`, `cs16` or `cf32` only.
pub fn wav_header(rate_hz: u32, format: SampleFormat) -> [u8; WAV_HEADER_LEN] {
    use SampleFormat::{Cf32, Cs8, Cs16, Cu8, F32, S16};
    let (tag, bits): (u16, u16) = match format {
        F32 | Cf32 => (3, 32),
        S16 | Cs16 => (1, 16),
        Cu8 | Cs8 => (1, 8),
    };
    let channels: u16 = if format.is_iq() { 2 } else { 1 };
    let align = channels * bits / 8;
    let mut h = [0u8; WAV_HEADER_LEN];
    let fields: [&[u8]; 13] = [
        b"RIFF",
        &WAV_STREAM_SIZE.to_le_bytes(),
        b"WAVE",
        b"fmt ",
        &16u32.to_le_bytes(),
        &tag.to_le_bytes(),
        &channels.to_le_bytes(),
        &rate_hz.to_le_bytes(),
        &rate_hz.saturating_mul(u32::from(align)).to_le_bytes(),
        &align.to_le_bytes(),
        &bits.to_le_bytes(),
        b"data",
        &WAV_STREAM_SIZE.to_le_bytes(),
    ];
    let mut at = 0;
    for f in fields {
        h[at..at + f.len()].copy_from_slice(f);
        at += f.len();
    }
    h
}

/// Per-rate conversion to the program's rate and sample format: one resampler per
/// channel written (I and Q for an IQ format, one for audio), none when the rates match.
pub struct Converter {
    in_rate_hz: u64,
    format: SampleFormat,
    resample: Vec<Resampler>,
}

impl Converter {
    /// Frames at `in_rate_hz` to `out_rate_hz` as `format`, IQ (`cu8`, `cs8`, `cs16`,
    /// `cf32`, through [`Self::convert`]) or audio (`s16`, `f32`, through
    /// [`Self::convert_audio`]).
    pub fn new(in_rate_hz: u64, out_rate_hz: u64, format: SampleFormat) -> Result<Self, String> {
        let n = match (in_rate_hz == out_rate_hz, format.is_iq()) {
            (true, _) => 0,
            (false, true) => 2,
            (false, false) => 1,
        };
        let resample = (0..n)
            .map(|_| Resampler::new(in_rate_hz, out_rate_hz))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            in_rate_hz,
            format,
            resample,
        })
    }

    /// `samples` resampled and written interleaved I/Q, little-endian: `cu8` as
    /// `(x + 1) * 127.5`, `cs8` as `x * 127`, `cs16` as `x * 32 767` (each rounded and
    /// clamped), `cf32` as is.
    pub fn convert(&mut self, samples: &[Complex<f32>]) -> Vec<u8> {
        let (mut re, mut im): (Vec<f32>, Vec<f32>) = samples.iter().map(|s| (s.re, s.im)).unzip();
        if let [ri, rq] = self.resample.as_mut_slice() {
            (re, im) = (ri.process(&re), rq.process(&im));
        }
        self.write(re.iter().zip(&im).flat_map(|(&i, &q)| [i, q]))
    }

    /// Interleaved audio of `channels` channels mixed to mono (their mean), resampled
    /// and written as `s16` (`x * 32 767`, rounded and clamped) or `f32`, little-endian.
    pub fn convert_audio(&mut self, samples: &[f32], channels: u16) -> Vec<u8> {
        let n = usize::from(channels.max(1));
        let mut mono: Vec<f32> = match n {
            1 => samples.to_vec(),
            _ => samples
                .chunks_exact(n)
                .map(|f| f.iter().sum::<f32>() / n as f32)
                .collect(),
        };
        if let [r] = self.resample.as_mut_slice() {
            mono = r.process(&mono);
        }
        self.write(mono)
    }

    fn write(&self, values: impl IntoIterator<Item = f32>) -> Vec<u8> {
        let mut out = Vec::new();
        for x in values {
            match self.format {
                SampleFormat::Cu8 => out.push(((x + 1.0) * 127.5).round().clamp(0.0, 255.0) as u8),
                SampleFormat::Cs8 => out.push((x * 127.0).round().clamp(-128.0, 127.0) as i8 as u8),
                SampleFormat::Cs16 | SampleFormat::S16 => out.extend_from_slice(
                    &((x * 32_767.0).round().clamp(-32_768.0, 32_767.0) as i16).to_le_bytes(),
                ),
                SampleFormat::Cf32 | SampleFormat::F32 => out.extend_from_slice(&x.to_le_bytes()),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(format: SampleFormat, x: &[Complex<f32>]) -> Vec<u8> {
        Converter::new(48_000, 48_000, format).unwrap().convert(x)
    }

    #[test]
    fn formats_scale_and_clamp() {
        let x = [Complex::new(0.5, -1.0), Complex::new(2.0, -2.0)];
        let v: Vec<i16> = conv(SampleFormat::Cs16, &x)
            .chunks(2)
            .map(|p| i16::from_le_bytes([p[0], p[1]]))
            .collect();
        assert_eq!(v, [16_384, -32_767, 32_767, -32_768]);
        assert_eq!(conv(SampleFormat::Cu8, &x), [191, 0, 255, 0]);
        let v: Vec<i8> = conv(SampleFormat::Cs8, &x)
            .iter()
            .map(|&b| b as i8)
            .collect();
        assert_eq!(v, [64, -127, 127, -128]);
        let v: Vec<f32> = conv(SampleFormat::Cf32, &x)
            .chunks(4)
            .map(|p| f32::from_le_bytes([p[0], p[1], p[2], p[3]]))
            .collect();
        assert_eq!(v, [0.5, -1.0, 2.0, -2.0]);
    }

    #[test]
    fn odd_rates_are_resampled() {
        let mut c = Converter::new(2_560_000, 2_400_000, SampleFormat::Cs16).unwrap();
        let n: usize = (0..10)
            .map(|_| c.convert(&[Complex::new(0.1, 0.1); 2_133]).len())
            .sum();
        // 21 330 samples at 2.56 MHz are 19 997 at 2.4 MHz, 4 bytes each.
        assert!((n / 4).abs_diff(19_997) <= 1, "{n}");
    }

    #[test]
    fn audio_is_mixed_to_mono_and_resampled() {
        let mut c = Converter::new(48_000, 48_000, SampleFormat::S16).unwrap();
        let v: Vec<i16> = c
            .convert_audio(&[0.5, -0.5, 1.0, 0.0], 2)
            .chunks(2)
            .map(|p| i16::from_le_bytes([p[0], p[1]]))
            .collect();
        assert_eq!(v, [0, 16_384]);
        let mut c = Converter::new(24_000, 48_000, SampleFormat::F32).unwrap();
        let n: usize = (0..10).map(|_| c.convert_audio(&[0.1; 480], 1).len()).sum();
        // 4 800 samples at 24 kHz are 9 600 at 48 kHz, 4 bytes each.
        assert!((n / 4).abs_diff(9_600) <= 2, "{n}");
    }

    #[test]
    fn wav_headers_name_rate_bits_and_channels() {
        let u16_at = |h: &[u8], i: usize| u16::from_le_bytes([h[i], h[i + 1]]);
        let u32_at = |h: &[u8], i: usize| u32::from_le_bytes([h[i], h[i + 1], h[i + 2], h[i + 3]]);
        let h = wav_header(48_000, SampleFormat::S16);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(u32_at(&h, 4), WAV_STREAM_SIZE);
        assert_eq!(&h[8..16], b"WAVEfmt ");
        assert_eq!(u32_at(&h, 16), 16);
        assert_eq!(
            [
                u16_at(&h, 20),
                u16_at(&h, 22),
                u16_at(&h, 32),
                u16_at(&h, 34)
            ],
            [1, 1, 2, 16]
        );
        assert_eq!([u32_at(&h, 24), u32_at(&h, 28)], [48_000, 96_000]);
        assert_eq!(&h[36..40], b"data");
        assert_eq!(u32_at(&h, 40), WAV_STREAM_SIZE);
        let h = wav_header(48_000, SampleFormat::F32);
        assert_eq!([u16_at(&h, 20), u16_at(&h, 22), u16_at(&h, 34)], [3, 1, 32]);
        let h = wav_header(2_400_000, SampleFormat::Cs16);
        assert_eq!([u16_at(&h, 20), u16_at(&h, 22), u16_at(&h, 32)], [1, 2, 4]);
        assert_eq!(u32_at(&h, 28), 9_600_000);
    }

    #[test]
    fn args_are_substituted() {
        let row = zeta_core::modes::parse(include_str!("../../../modes/adsb_decode.json")).unwrap();
        let Decoder::Program(mut block) = row.decoder else {
            panic!("a program row")
        };
        block.args = ["--port=$port", "-r", "$rate_hz", "-f$center_hz", "$fifo"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            substitute(
                &block,
                30_005,
                1_090_000_000,
                Some(Path::new("/run/x/input"))
            ),
            [
                "--port=30005",
                "-r",
                "2400000",
                "-f1090000000",
                "/run/x/input"
            ]
        );
        assert_eq!(parser_name(Parser::Sbs), "SBS");
    }

    #[test]
    fn substitute_fills_center_khz() {
        let row = zeta_core::modes::parse(include_str!("../../../modes/adsb_decode.json")).unwrap();
        let Decoder::Program(mut block) = row.decoder else {
            panic!("a program row")
        };
        block.args = ["$center_khz", "$center_hz"].map(String::from).to_vec();
        assert_eq!(
            substitute(&block, 0, 8_942_500, None),
            ["8942.5", "8942500"]
        );
    }
}
