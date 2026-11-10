//! `zeta-fake-worker --id ID --script SCRIPT [--record PATH] [--rate-cap-hz N] [--ring-fd N]`: a
//! source worker for tests. Its stdout speaks the real protocol through
//! [`serve`]; its port is a `SoapySource` over a scripted `MockDevice`, or a
//! `FileSource` over a SigMF recording, chosen by `--script`:
//!
//! - `stream`: samples forever, paced at the tuned rate.
//! - `gone-after-ms N`: samples, then after N ms the device vanishes, so the
//!   port faults and the worker exits 5.
//! - `not-found`: exits 3 after `device not found: fake` on stderr.
//! - `open-fails`: exits 4 after `fake open: no such device` on stderr.
//! - `crash-after-ms N`: `stream`, and N ms after start `abort()` (SIGABRT,
//!   without a core dump).
//! - `hang-hello`: never writes a hello.
//! - `hang-open`: installs the stop-signal handling the real worker installs,
//!   prints `hanging in open` on stderr and blocks for good, as a driver hung
//!   in `open()` does.
//! - `file PATH[,crash-after-ms N]`: a looping real-time `FileSource` over the
//!   recording `PATH`, tunable 2 MHz either side of its centre.
//!
//! `--record PATH` appends `start <unix_ms>` and one `env <NAME>` per
//! environment variable at startup, then `tune <c> <bw>` and `gain <db|auto>`
//! per call, so a test sees when the worker was spawned, what it inherited and
//! what the node re-applied, and `served` when `serve` returns (a `shutdown`
//! or a closed stdin, never a kill). `ZETA_FAKE_CRASH_ONCE=1` crashes only on
//! the first spawn (the marker `<record>.crashed` says it has);
//! `ZETA_FAKE_PRINT=1` prints `noise` to stdout after the redirect, which must
//! land on stderr; `ZETA_FAKE_PROTOCOL=1` serves as a protocol-1 worker would
//! (it ignores `--ring-fd`, sends every block on the pipe, and its hello and
//! status have no `transport` or `transport_dropped`), while any other
//! `ZETA_FAKE_PROTOCOL=N` says hello with protocol `N` instead of serving,
//! then waits to be killed; `ZETA_FAKE_WAIT_RUNTIME=1` ends with a plain
//! runtime drop, which waits on the blocked stdin read.
//!
//! A mock source's hello reports `max_bandwidth_hz: Measured(N)` with
//! `--rate-cap-hz N` (as the real worker reports a cap the node passed back),
//! or else with `ZETA_FAKE_MEASURED_CAP=N` (as a worker that probed); without
//! either the mock's own asserted cap. Its `usable_fraction` is the Soapy
//! source's asserted default, or `ZETA_FAKE_USABLE=F` (as `usable=F`).
//!
//! `--ring-fd N` serves through the ring inherited on descriptor `N`, as the
//! real worker does (a ring that will not open is one stderr line, then the
//! pipe); `ZETA_FAKE_NO_RING=1` ignores the flag, as a worker of an older
//! build would. `ZETA_FAKE_BAD_SLOT=1` rewrites the first descriptor to name
//! slot 9999, which no ring has. `ZETA_FAKE_STALE_SEQ=1` rewrites the third
//! descriptor to repeat the second (seq 1 in slot 1), a stale descriptor the
//! ring's own checks would pass. `ZETA_FAKE_READ_SAMPLES=N` scripts the mock's reads `N`
//! samples long instead of [`READ_SAMPLES`], and raises the mock's rate
//! ceiling to `100 N` when that is above its own, since the source reads a
//! hundredth of a second at a time: a tune to that rate then gets blocks of
//! `N` samples.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::broadcast::error::TryRecvError;

use async_trait::async_trait;
use zeta_core::port::{GainInfo, IqReceiver, Port, PortError, PortStats, Tuning};
use zeta_core::types::{Capabilities, Claim, Hz, SourceId};
use zeta_source_file::{FileOpts, FileSource, read_meta};
use zeta_source_soapy::SoapySource;
use zeta_source_soapy::device::RangeHz;
use zeta_source_soapy::mock::{MockDevice, MockRead};
use zeta_source_worker::ring::Ring;
use zeta_source_worker::serve::{
    EXIT_NOT_FOUND, EXIT_OPEN_FAILED, redirect_stdout_to_stderr, requests_until_signal, serve,
};
use zeta_source_worker::wire::{Control, Frame, Hello, read_frame, write_frame};

/// Samples per scripted read, unless `ZETA_FAKE_READ_SAMPLES` says otherwise.
const READ_SAMPLES: usize = 16_384;
/// The mock device's own rate ceiling (`MockDevice::hackrf_like`).
const MOCK_MAX_RATE_HZ: f64 = 20e6;
/// How much the feeder keeps scripted ahead of the reader (at least
/// [`FEED_QUEUE_MIN`] reads), so the reader never finds the script empty (an
/// empty script reads as a 100 ms timeout).
const FEED_QUEUE: Duration = Duration::from_millis(50);
const FEED_QUEUE_MIN: u64 = 4;
/// The feeder's wake-up interval.
const FEED_TICK: Duration = Duration::from_millis(5);

struct Args {
    id: String,
    script: String,
    record: Option<PathBuf>,
    rate_cap_hz: Option<u64>,
    ring_fd: Option<RawFd>,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let (mut id, mut script, mut record, mut ring_fd) = (None, None, None, None);
    let mut rate_cap_hz = None;
    while let Some(flag) = it.next() {
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--id" => id = Some(value),
            "--script" => script = Some(value),
            "--record" => record = Some(PathBuf::from(value)),
            "--rate-cap-hz" => {
                rate_cap_hz = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--rate-cap-hz {value:?}: not a rate"))?,
                );
            }
            "--ring-fd" => {
                ring_fd = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--ring-fd {value:?}: not a descriptor"))?,
                );
            }
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok(Args {
        id: id.ok_or("--id is required")?,
        script: script.ok_or("--script is required")?,
        record,
        rate_cap_hz,
        ring_fd,
    })
}

/// The scripted read size: `ZETA_FAKE_READ_SAMPLES`, or [`READ_SAMPLES`].
fn read_samples() -> usize {
    std::env::var("ZETA_FAKE_READ_SAMPLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(READ_SAMPLES)
}

/// The ring on `--ring-fd`, unless there is none or `ZETA_FAKE_NO_RING=1`; a
/// ring that will not open is a stderr line and the pipe.
fn open_ring(fd: Option<RawFd>) -> Option<Ring> {
    let fd = fd?;
    if std::env::var("ZETA_FAKE_NO_RING").is_ok_and(|v| v == "1") {
        return None;
    }
    // SAFETY: the parent put the ring on `fd` for this process, and nothing
    // else here owns that descriptor.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    match Ring::open(owned) {
        Ok(r) => Some(r),
        Err(e) => {
            eprintln!("zeta-fake-worker: ring fd {fd}: {e}; serving on the pipe");
            None
        }
    }
}

/// The `--record` file, appended one line at a time.
#[derive(Clone)]
struct Record(Option<Arc<Mutex<File>>>);

impl Record {
    fn open(path: Option<&Path>) -> Result<Self, String> {
        let Some(path) = path else {
            return Ok(Record(None));
        };
        let f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Record(Some(Arc::new(Mutex::new(f)))))
    }

    fn line(&self, line: &str) {
        if let Some(f) = &self.0 {
            let mut f = f.lock().unwrap();
            let _ = writeln!(f, "{line}");
        }
    }
}

/// A port that records every `tune` and `set_gain` before handing it on.
struct Recorded {
    inner: Arc<dyn Port>,
    record: Record,
}

#[async_trait]
impl Port for Recorded {
    fn id(&self) -> &SourceId {
        self.inner.id()
    }
    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        self.record
            .line(&format!("tune {} {}", t.center_hz.0, t.bandwidth_hz.0));
        self.inner.tune(t).await
    }
    fn current(&self) -> Option<Tuning> {
        self.inner.current()
    }
    fn subscribe(&self) -> IqReceiver {
        self.inner.subscribe()
    }
    fn fault(&self) -> Option<String> {
        self.inner.fault()
    }
    async fn idle(&self) {
        self.inner.idle().await
    }
    fn gain(&self) -> Option<GainInfo> {
        self.inner.gain()
    }
    fn stats(&self) -> Option<PortStats> {
        self.inner.stats()
    }
    async fn set_gain(&self, db: Option<f64>) -> Result<GainInfo, PortError> {
        let v = db.map_or("auto".to_string(), |d| d.to_string());
        self.record.line(&format!("gain {v}"));
        self.inner.set_gain(db).await
    }
}

/// A `SoapySource` over a `MockDevice` whose reads a feeder thread scripts at
/// the tuned rate: a mock read returns at once, and a stream that ran ahead of
/// the wall clock would be restarted and in the end faulted by the source's
/// clock guard. With `gone_after`, the feeder stops after that long from start
/// and scripts `Gone("unplugged")`.
fn mock_source(
    id: &str,
    gone_after: Option<Duration>,
    rate_cap_hz: Option<u64>,
) -> Result<Arc<SoapySource>, String> {
    let read = read_samples();
    let mut dev = MockDevice::hackrf_like();
    // The source reads a hundredth of a second at a time: a wider scripted
    // read needs a rate at which the source's buffer holds it whole.
    let ceiling = read as f64 * 100.0;
    if ceiling > MOCK_MAX_RATE_HZ {
        dev = dev.with_rates(vec![RangeHz {
            min_hz: 1e6,
            max_hz: ceiling,
            step_hz: 0.0,
        }]);
    }
    let dev = Arc::new(dev);
    let mut src =
        SoapySource::with_device(SourceId(id.into()), dev.clone()).map_err(|e| e.to_string())?;
    let measured = rate_cap_hz.or_else(|| {
        std::env::var("ZETA_FAKE_MEASURED_CAP")
            .ok()
            .and_then(|v| v.parse().ok())
    });
    if let Some(hz) = measured {
        src.set_max_bandwidth(Claim::Measured(Hz(hz)));
    }
    if let Some(usable) = std::env::var("ZETA_FAKE_USABLE")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        src = src.with_usable(usable);
    }
    let src = Arc::new(src);
    let weak = Arc::downgrade(&src);
    std::thread::Builder::new()
        .name("fake-feeder".into())
        .spawn(move || feed(&dev, &weak, gone_after, read))
        .map_err(|e| e.to_string())?;
    Ok(src)
}

/// Keep [`FEED_QUEUE`] of paced reads of `read` samples in the mock's script
/// at the source's current rate. Each read is a `Slow` step: its sleep is the
/// read's share of the rate, shortened while the samples actually delivered
/// (the source's own [`SoapySource::samples_delivered`], which counts the
/// samples a tune discards as well as those it sends) lag the wall clock and
/// lengthened while they lead it, so thread wake-up overshoot does not add up
/// to a slow stream. Whole milliseconds are carried from read to read. The
/// feeder holds a receiver of the source's broadcast, so the reader runs.
fn feed(dev: &MockDevice, src: &Weak<SoapySource>, gone_after: Option<Duration>, read: usize) {
    let start = Instant::now();
    let Some(mut rx) = src.upgrade().map(|s| s.subscribe()) else {
        return;
    };
    let mut pace: Option<Pace> = None;
    let mut total = 0u64;
    loop {
        std::thread::sleep(FEED_TICK);
        if gone_after.is_some_and(|g| start.elapsed() >= g) {
            dev.push_reads(vec![MockRead::Gone("unplugged".into())]);
            return;
        }
        loop {
            match rx.try_recv() {
                Ok(_) | Err(TryRecvError::Lagged(_)) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => {
                    let Some(s) = src.upgrade() else { return };
                    rx = s.subscribe();
                    break;
                }
            }
        }
        let Some(src) = src.upgrade() else { return };
        let now_total = src.samples_delivered();
        let delivered = now_total - total;
        total = now_total;
        let Some(rate) = src.current().map(|t| t.bandwidth_hz.0).filter(|r| *r > 0) else {
            continue;
        };
        let p = match pace.as_mut() {
            Some(p) if p.rate == rate => p,
            _ => pace.insert(Pace {
                rate,
                since: Instant::now(),
                delivered: 0,
                pushed: 0,
                ideal_ms: 0.0,
                whole_ms: 0,
            }),
        };
        p.delivered += delivered;
        let per_read_ms = read as f64 * 1e3 / rate as f64;
        let due = p.since.elapsed().as_secs_f64() * rate as f64;
        let lag_ms = (due - p.delivered as f64) * 1e3 / rate as f64;
        let sleep_ms = (per_read_ms - lag_ms / 8.0).max(0.0);
        let queued = p.pushed.saturating_sub(p.delivered / read as u64);
        let target =
            ((FEED_QUEUE.as_secs_f64() * 1e3 / per_read_ms).ceil() as u64).max(FEED_QUEUE_MIN);
        let reads: Vec<MockRead> = (queued..target)
            .map(|_| {
                p.ideal_ms += sleep_ms;
                let ms = (p.ideal_ms.round() as u64).saturating_sub(p.whole_ms);
                p.whole_ms += ms;
                p.pushed += 1;
                MockRead::Slow(read, ms)
            })
            .collect();
        if !reads.is_empty() {
            dev.push_reads(reads);
        }
    }
}

/// The feeder's account of the current rate since it took effect.
struct Pace {
    rate: u64,
    since: Instant,
    /// Samples delivered and reads scripted at this rate.
    delivered: u64,
    pushed: u64,
    /// Sleep scheduled, exact and in the whole milliseconds the mock takes.
    ideal_ms: f64,
    whole_ms: u64,
}

/// A looping, real-time `FileSource` over `meta`, tunable 2 MHz either side
/// of the recording's centre.
fn file_source(id: &str, meta: &Path) -> Result<Arc<FileSource>, String> {
    let centre = read_meta(meta).map_err(|e| e.to_string())?.center_hz.0;
    let opts = FileOpts {
        realtime: true,
        block_samples: READ_SAMPLES,
        repeat: true,
        tune_range: Some((Hz(centre.saturating_sub(2_000_000)), Hz(centre + 2_000_000))),
    };
    FileSource::open(SourceId(id.into()), meta, opts)
        .map(Arc::new)
        .map_err(|e| e.to_string())
}

fn parse_ms(s: &str) -> Result<Duration, String> {
    s.trim()
        .parse()
        .map(Duration::from_millis)
        .map_err(|_| format!("{s:?}: not a number of milliseconds"))
}

/// Abort the process `after` from now, unless `ZETA_FAKE_CRASH_ONCE=1` and the
/// marker beside the record file says an earlier spawn has crashed already.
///
/// The process is made non-dumpable first: the death is still SIGABRT, but
/// without a core dump. A dump goes through the host's crash handler (apport,
/// systemd-coredump), which holds the dying process for 100 ms unloaded and
/// several times that when tests crash fakes at once on a loaded runner, so
/// the time from the abort to the exit, and every timing measured across it,
/// would depend on the host rather than on the supervisor.
fn crash_after(after: Duration, record: Option<&Path>) {
    let marker = record.map(|p| {
        let mut m = p.as_os_str().to_owned();
        m.push(".crashed");
        PathBuf::from(m)
    });
    let once = std::env::var("ZETA_FAKE_CRASH_ONCE").is_ok_and(|v| v == "1");
    if once && marker.as_ref().is_some_and(|m| m.exists()) {
        return;
    }
    std::thread::spawn(move || {
        std::thread::sleep(after);
        if let Some(m) = marker {
            let _ = File::create(m);
        }
        // SAFETY: `prctl` with plain integer arguments; no pointers.
        unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
        std::process::abort();
    });
}

/// How the fake bends the frames `serve` writes before they reach the parent.
#[derive(Clone, Copy)]
struct Bend {
    /// Speak protocol 1: no `transport` in the hello, no `transport_dropped`
    /// in the status.
    protocol_1: bool,
    /// Name slot 9999 in the first descriptor.
    bad_slot: bool,
    /// Repeat seq 1 in slot 1 as the third descriptor.
    stale_seq: bool,
}

impl Bend {
    fn any(self) -> bool {
        self.protocol_1 || self.bad_slot || self.stale_seq
    }
}

/// How long the end of the worker waits for the bending thread to pass the
/// last frames on.
const BEND_FLUSH: Duration = Duration::from_secs(1);

/// Put a thread between `serve` and `frames` that rewrites each frame per
/// `bend`: the returned writer is what `serve` writes to, and the receiver
/// answers once the thread has passed the last frame on.
fn bend_frames(mut frames: File, bend: Bend) -> io::Result<(File, std::sync::mpsc::Receiver<()>)> {
    let mut fds = [0; 2];
    // SAFETY: `pipe2` writes two descriptors into the array it is given.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both descriptors were just opened and nothing else owns them.
    let (mut rd, wr) = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
    let (done_tx, done) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("fake-bend".into())
        .spawn(move || {
            let mut bad_slot = bend.bad_slot;
            let mut descriptors = 0u64;
            while let Ok(Some(mut f)) = read_frame(&mut rd) {
                if let Frame::Descriptor(d) = &mut f {
                    if bad_slot {
                        d.slot = 9999;
                        bad_slot = false;
                    }
                    if bend.stale_seq && descriptors == 2 {
                        d.seq = 1;
                        d.slot = 1;
                    }
                    descriptors += 1;
                }
                let written = match (&f, bend.protocol_1) {
                    (Frame::Control(c), true) => write_protocol_1(&mut frames, c),
                    _ => write_frame(&mut frames, &f),
                };
                if written.and_then(|()| frames.flush()).is_err() {
                    break;
                }
            }
            let _ = done_tx.send(());
        })?;
    Ok((wr, done))
}

/// Write `c` as a protocol-1 worker would: protocol 1 and no `transport` in a
/// hello, no `transport_dropped` in a status. The frame is written by hand
/// (kind 0, the length, the JSON), as the keys are left out entirely.
fn write_protocol_1(w: &mut impl Write, c: &Control) -> io::Result<()> {
    let mut v = serde_json::to_value(c).map_err(io::Error::other)?;
    if let Some(h) = v.get_mut("hello").and_then(|h| h.as_object_mut()) {
        h.remove("transport");
        h.insert("protocol".into(), 1.into());
    }
    if let Some(s) = v.get_mut("status").and_then(|s| s.as_object_mut()) {
        s.remove("transport_dropped");
    }
    let payload = serde_json::to_vec(&v).map_err(io::Error::other)?;
    let len = u32::try_from(payload.len()).map_err(io::Error::other)?;
    w.write_all(&[0])?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&payload)
}

/// Say hello with `protocol` on `frames`, as a worker of another build would,
/// then wait for the parent to kill the process.
fn wrong_hello(port: &dyn Port, protocol: u32, mut frames: File) -> ExitCode {
    let hello = Frame::Control(Control::Hello(Hello {
        protocol,
        capabilities: port.capabilities().clone(),
        gain: port.gain(),
        driver: "fake".into(),
        transport: None,
    }));
    if let Err(e) = write_frame(&mut frames, &hello).and_then(|()| frames.flush()) {
        eprintln!("zeta-fake-worker: hello: {e}");
        return ExitCode::from(EXIT_OPEN_FAILED);
    }
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("zeta-fake-worker: {e}");
            return ExitCode::from(2);
        }
    };
    let record = match Record::open(args.record.as_deref()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("zeta-fake-worker: --record {e}");
            return ExitCode::from(2);
        }
    };
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    record.line(&format!("start {now_ms}"));
    for (name, _) in std::env::vars_os() {
        record.line(&format!("env {}", name.to_string_lossy()));
    }

    let (name, arg) = args
        .script
        .split_once(' ')
        .unwrap_or((args.script.as_str(), ""));
    match name {
        "not-found" => {
            eprintln!("device not found: fake");
            return ExitCode::from(EXIT_NOT_FOUND);
        }
        "open-fails" => {
            eprintln!("fake open: no such device");
            return ExitCode::from(EXIT_OPEN_FAILED);
        }
        "hang-hello" => loop {
            std::thread::sleep(Duration::from_secs(3600));
        },
        _ => {}
    }

    let frames = match redirect_stdout_to_stderr() {
        // SAFETY: the descriptor was just duplicated for us and nothing else
        // owns it.
        Ok(fd) => unsafe { File::from_raw_fd(fd) },
        Err(e) => {
            eprintln!("zeta-fake-worker: redirect stdout: {e}");
            return ExitCode::from(EXIT_OPEN_FAILED);
        }
    };
    if std::env::var("ZETA_FAKE_PRINT").is_ok_and(|v| v == "1") {
        println!("noise");
    }

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("zeta-fake-worker: runtime: {e}");
            return ExitCode::from(EXIT_OPEN_FAILED);
        }
    };
    let code = rt.block_on(async {
        // Installed before the port opens, as the real worker does, so a
        // signal during a hung open is handled.
        let requests = match requests_until_signal() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("zeta-fake-worker: signal handlers: {e}");
                return ExitCode::from(EXIT_OPEN_FAILED);
            }
        };
        if name == "hang-open" {
            eprintln!("hanging in open");
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        // A receiver held for the life of the worker, see the `file` script.
        let mut keep: Option<IqReceiver> = None;
        let port: Result<Arc<dyn Port>, String> = match (name, arg) {
            ("stream", _) => {
                mock_source(&args.id, None, args.rate_cap_hz).map(|s| s as Arc<dyn Port>)
            }
            ("gone-after-ms", ms) => parse_ms(ms)
                .and_then(|g| mock_source(&args.id, Some(g), args.rate_cap_hz))
                .map(|s| s as Arc<dyn Port>),
            ("crash-after-ms", ms) => parse_ms(ms).and_then(|after| {
                crash_after(after, args.record.as_deref());
                mock_source(&args.id, None, args.rate_cap_hz).map(|s| s as Arc<dyn Port>)
            }),
            ("file", spec) => {
                let (path, crash) = match spec.split_once(',') {
                    Some((p, c)) => (p, Some(c)),
                    None => (spec, None),
                };
                let crash = match crash.map(|c| c.trim().strip_prefix("crash-after-ms ")) {
                    None => Ok(None),
                    Some(Some(ms)) => parse_ms(ms).map(Some),
                    Some(None) => Err(format!("file {spec}: expected ,crash-after-ms N")),
                };
                crash.and_then(|crash| {
                    let src = file_source(&args.id, Path::new(path))?;
                    if let Some(after) = crash {
                        crash_after(after, args.record.as_deref());
                    }
                    // Started once something holds a receiver: a FileSource
                    // stops when its last subscriber goes, and `serve`
                    // subscribes only when it runs.
                    keep = Some(src.subscribe());
                    src.start();
                    Ok(src as Arc<dyn Port>)
                })
            }
            _ => Err(format!("unknown script {:?}", args.script)),
        };
        let port = match port {
            Ok(p) => p,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(EXIT_OPEN_FAILED);
            }
        };
        let protocol: Option<u32> = std::env::var("ZETA_FAKE_PROTOCOL")
            .ok()
            .and_then(|v| v.parse().ok());
        let bend = Bend {
            protocol_1: protocol == Some(1),
            bad_slot: std::env::var("ZETA_FAKE_BAD_SLOT").is_ok_and(|v| v == "1"),
            stale_seq: std::env::var("ZETA_FAKE_STALE_SEQ").is_ok_and(|v| v == "1"),
        };
        if let Some(p) = protocol.filter(|p| *p != 1) {
            return wrong_hello(port.as_ref(), p, frames);
        }
        let port: Arc<dyn Port> = Arc::new(Recorded {
            inner: port,
            record: record.clone(),
        });
        // A protocol-1 worker knows nothing of rings.
        let ring = if bend.protocol_1 {
            None
        } else {
            open_ring(args.ring_fd)
        };
        let code = if bend.any() {
            let (out, bent) = match bend_frames(frames, bend) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("zeta-fake-worker: frame pipe: {e}");
                    return ExitCode::from(EXIT_OPEN_FAILED);
                }
            };
            let code = serve(port, "fake", requests, out, ring).await;
            let _ = tokio::task::spawn_blocking(move || bent.recv_timeout(BEND_FLUSH)).await;
            code
        } else {
            serve(port, "fake", requests, frames, ring).await
        };
        record.line("served");
        drop(keep);
        code
    });
    // Tokio's stdin read blocks a pool thread until a line arrives; after a
    // `shutdown` there may be none, so the runtime is not waited for, unless
    // `ZETA_FAKE_WAIT_RUNTIME=1` asks for the plain drop a `#[tokio::main]`
    // does: that waits for the read, which only the parent closing the
    // worker's stdin ends.
    if std::env::var("ZETA_FAKE_WAIT_RUNTIME").is_ok_and(|v| v == "1") {
        drop(rt);
    } else {
        rt.shutdown_timeout(Duration::from_millis(100));
    }
    code
}
