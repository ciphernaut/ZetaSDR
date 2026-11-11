//! `serve` over a real pipe: the fake worker binary, spawned as the node will
//! spawn the real one.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use zeta_core::port::{IqBlock, TransportPath, Tuning};
use zeta_core::types::{Claim, Hz};
use zeta_source_worker::ring::{DEFAULT_RING_BYTES, MIN_SLOTS, Ring, SLOT_SAMPLES, SlotFormat};
use zeta_source_worker::spawn::{RING_FD, inherit_ring_std};
use zeta_source_worker::wire::{
    Cmd, Control, Descriptor, Frame, Hello, PROTOCOL, Reply, Request, Status, read_frame,
    write_request,
};

const FAKE: &str = env!("CARGO_BIN_EXE_zeta-fake-worker");

/// The fake worker running `script` with `envs` set, stdio piped, and with
/// `ring` inherited as fd 3 (`--ring-fd 3`) when there is one.
fn spawn_with(script: &str, envs: &[(&str, &str)], ring: Option<&Ring>) -> Child {
    let mut cmd = Command::new(FAKE);
    cmd.args(["--id", "f", "--script", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    if let Some(ring) = ring {
        cmd.args(["--ring-fd", &RING_FD.to_string()]);
        inherit_ring_std(&mut cmd, ring);
    }
    cmd.spawn().expect("spawn the fake worker")
}

fn spawn(script: &str) -> (Child, ChildStdin, ChildStdout) {
    let mut child = spawn_with(script, &[], None);
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    (child, stdin, stdout)
}

/// The child's stdout read on a thread, so every wait in a test is bounded.
struct Frames(mpsc::Receiver<std::io::Result<Frame>>);

impl Frames {
    fn new(mut stdout: ChildStdout) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            loop {
                let f = match read_frame(&mut stdout) {
                    Ok(Some(f)) => Ok(f),
                    Ok(None) => return,
                    Err(e) => Err(e),
                };
                let stop = f.is_err();
                if tx.send(f).is_err() || stop {
                    return;
                }
            }
        });
        Frames(rx)
    }

    fn next(&self) -> Frame {
        self.0
            .recv_timeout(Duration::from_secs(5))
            .expect("a frame within 5 s")
            .expect("a well-formed frame")
    }
}

/// The next control message, skipping blocks.
fn read_control(frames: &Frames) -> Control {
    loop {
        if let Frame::Control(c) = frames.next() {
            return c;
        }
    }
}

/// The next reply, skipping blocks and status messages.
fn read_reply(frames: &Frames) -> (u64, Result<Reply, zeta_core::port::PortError>) {
    loop {
        if let Control::Reply { reply, result } = read_control(frames) {
            return (reply, result);
        }
    }
}

fn read_hello(frames: &Frames) -> Hello {
    match frames.next() {
        Frame::Control(Control::Hello(h)) => h,
        f => panic!("expected hello first, got {f:?}"),
    }
}

fn send(stdin: &mut ChildStdin, id: u64, cmd: Cmd) {
    write_request(stdin, &Request { id, cmd }).unwrap();
    stdin.flush().unwrap();
}

fn tune(center_hz: u64, bandwidth_hz: u64) -> Cmd {
    Cmd::Tune {
        center_hz,
        bandwidth_hz,
    }
}

fn wait_exit(child: &mut Child, within: Duration) -> ExitStatus {
    let end = Instant::now() + within;
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s;
        }
        if Instant::now() > end {
            let _ = child.kill();
            panic!("the worker did not exit within {within:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn stderr_of(child: &mut Child) -> String {
    let mut s = String::new();
    child.stderr.take().unwrap().read_to_string(&mut s).unwrap();
    s
}

#[test]
fn hello_is_the_first_frame_and_carries_the_capabilities() {
    let (mut child, _stdin, stdout) = spawn("stream");
    let hello = read_hello(&Frames::new(stdout));
    assert_eq!(hello.driver, "fake");
    assert_eq!(hello.protocol, PROTOCOL);
    assert_eq!(hello.protocol, 3);
    let Claim::Asserted(Hz(max)) = hello.capabilities.max_bandwidth_hz else {
        panic!("{:?}", hello.capabilities.max_bandwidth_hz);
    };
    assert!(max > 0);
    child.kill().unwrap();
}

#[test]
fn a_tune_is_answered_and_blocks_follow_with_rising_t0() {
    let (mut child, mut stdin, stdout) = spawn("stream");
    let frames = Frames::new(stdout);
    read_hello(&frames);
    send(&mut stdin, 7, tune(100_000_000, 2_000_000));
    let (id, result) = read_reply(&frames);
    assert_eq!(id, 7);
    let Ok(Reply::Tuning(tuning)) = result else {
        panic!("{result:?}");
    };
    assert_eq!(tuning.center_hz, Hz(100_000_000));
    let mut blocks: Vec<IqBlock> = Vec::new();
    while blocks.len() < 3 {
        if let Frame::Block(b) = frames.next() {
            blocks.push(b);
        }
    }
    for w in blocks.windows(2) {
        assert!(w[1].t0.0 > w[0].t0.0, "{:?} then {:?}", w[0].t0, w[1].t0);
    }
    for b in &blocks {
        assert_eq!(b.sample_rate_hz, tuning.bandwidth_hz);
        assert!(!b.samples.is_empty());
    }
    child.kill().unwrap();
}

#[test]
fn a_bad_tune_is_answered_with_the_same_text_as_in_process() {
    let (mut child, mut stdin, stdout) = spawn("stream");
    let frames = Frames::new(stdout);
    read_hello(&frames);
    send(&mut stdin, 1, tune(10_000_000_000, 2_000_000));
    let (id, result) = read_reply(&frames);
    assert_eq!(id, 1);
    let err = result.expect_err("a 10 GHz tune on a hackrf is refused");
    assert!(
        err.to_string()
            .starts_with("center_hz 10000000000 out of range "),
        "{err}"
    );
    // Still alive: a good tune is answered after the bad one.
    send(&mut stdin, 2, tune(100_000_000, 2_000_000));
    let (id, result) = read_reply(&frames);
    assert_eq!(id, 2);
    assert!(result.is_ok(), "{result:?}");
    assert!(child.try_wait().unwrap().is_none());
    child.kill().unwrap();
}

#[test]
fn a_fault_is_published_then_the_worker_exits_5() {
    let (mut child, mut stdin, stdout) = spawn("gone-after-ms 300");
    let frames = Frames::new(stdout);
    read_hello(&frames);
    send(&mut stdin, 1, tune(100_000_000, 2_000_000));
    let start = Instant::now();
    let fault = loop {
        assert!(start.elapsed() < Duration::from_secs(3), "no fault status");
        if let Control::Status(Status { fault: Some(f), .. }) = read_control(&frames) {
            break f;
        }
    };
    assert!(fault.contains("unplugged"), "{fault}");
    let status = wait_exit(&mut child, Duration::from_secs(3));
    assert_eq!(status.code(), Some(5));
}

#[test]
fn stdin_eof_ends_the_worker_cleanly() {
    let (mut child, stdin, stdout) = spawn("stream");
    let frames = Frames::new(stdout);
    read_hello(&frames);
    drop(stdin);
    let status = wait_exit(&mut child, Duration::from_secs(2));
    assert_eq!(status.code(), Some(0));
}

#[test]
fn shutdown_ends_the_worker_cleanly() {
    let (mut child, mut stdin, stdout) = spawn("stream");
    let frames = Frames::new(stdout);
    read_hello(&frames);
    stdin
        .write_all(b"{\"id\":1,\"cmd\":\"shutdown\"}\n")
        .unwrap();
    stdin.flush().unwrap();
    let status = wait_exit(&mut child, Duration::from_secs(2));
    assert_eq!(status.code(), Some(0));
}

#[test]
fn not_found_and_open_failed_exit_3_and_4_with_a_last_stderr_line() {
    for (script, code, line) in [
        ("not-found", 3, "device not found: fake"),
        ("open-fails", 4, "fake open: no such device"),
    ] {
        let mut child = spawn_with(script, &[], None);
        let status = wait_exit(&mut child, Duration::from_secs(2));
        assert_eq!(status.code(), Some(code), "{script}");
        let stderr = stderr_of(&mut child);
        assert_eq!(stderr.lines().last(), Some(line), "{script}: {stderr:?}");
        let mut out = Vec::new();
        child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
        assert!(out.is_empty(), "{script} wrote to stdout");
    }
}

#[test]
fn a_print_to_stdout_in_the_worker_lands_on_stderr() {
    let mut child = spawn_with("stream", &[("ZETA_FAKE_PRINT", "1")], None);
    let mut stdin = child.stdin.take().unwrap();
    let frames = Frames::new(child.stdout.take().unwrap());
    read_hello(&frames);
    send(&mut stdin, 1, tune(100_000_000, 2_000_000));
    let (_, result) = read_reply(&frames);
    let Ok(Reply::Tuning(Tuning { .. })) = result else {
        panic!("{result:?}");
    };
    while !matches!(frames.next(), Frame::Block(_)) {}
    drop(stdin);
    assert_eq!(
        wait_exit(&mut child, Duration::from_secs(2)).code(),
        Some(0)
    );
    let stderr = stderr_of(&mut child);
    assert!(stderr.lines().any(|l| l == "noise"), "{stderr:?}");
}

/// Send `sig` to the child.
fn signal(child: &Child, sig: i32) {
    let pid = libc::pid_t::try_from(child.id()).unwrap();
    // SAFETY: kill takes no pointers; the child is ours and not yet reaped.
    assert_eq!(unsafe { libc::kill(pid, sig) }, 0);
}

#[test]
fn sigint_takes_the_shutdown_path() {
    // A terminal Ctrl-C reaches the whole foreground group, the worker
    // included: it must leave as one the node shut down, not die of SIGINT.
    let (mut child, _stdin, stdout) = spawn("stream");
    let frames = Frames::new(stdout);
    read_hello(&frames);
    signal(&child, libc::SIGINT);
    let status = wait_exit(&mut child, Duration::from_secs(3));
    assert_eq!(status.code(), Some(0), "{status:?}");
}

#[test]
fn sigterm_takes_the_shutdown_path() {
    let (mut child, _stdin, stdout) = spawn("stream");
    let frames = Frames::new(stdout);
    read_hello(&frames);
    signal(&child, libc::SIGTERM);
    let status = wait_exit(&mut child, Duration::from_secs(3));
    assert_eq!(status.code(), Some(0), "{status:?}");
}

#[test]
fn sigterm_during_a_hung_open_exits_at_the_watchdog() {
    // `hang-open` installs the signal forwarding the real worker installs,
    // then blocks as a driver hung in `open()` does: `serve` never runs, so
    // the `shutdown` the signal becomes is never read. Only the watchdog ends
    // the process.
    let mut child = spawn_with("hang-open", &[], None);
    let _stdin = child.stdin.take().unwrap();
    let mut stderr = std::io::BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    std::io::BufRead::read_line(&mut stderr, &mut line).unwrap();
    assert_eq!(line.trim(), "hanging in open", "{line:?}");
    let t0 = Instant::now();
    signal(&child, libc::SIGTERM);
    let status = wait_exit(&mut child, Duration::from_secs(10));
    let took = t0.elapsed();
    assert_eq!(status.code(), Some(0), "{status:?}");
    assert!(
        took >= Duration::from_millis(4500) && took < Duration::from_secs(8),
        "{took:?}"
    );
}

/// The fake worker over `ring`, with its stdin and frames.
fn spawn_ring(script: &str, envs: &[(&str, &str)], ring: &Ring) -> (Child, ChildStdin, Frames) {
    let mut child = spawn_with(script, envs, Some(ring));
    let stdin = child.stdin.take().unwrap();
    let frames = Frames::new(child.stdout.take().unwrap());
    (child, stdin, frames)
}

/// The next descriptor within 5 s, skipping control messages; a block on the
/// pipe fails.
fn read_descriptor(frames: &Frames) -> Descriptor {
    let start = Instant::now();
    loop {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "no descriptor within 5 s"
        );
        match frames.next() {
            Frame::Descriptor(d) => return d,
            Frame::Block(_) => panic!("a block on the pipe while a ring is in use"),
            Frame::Control(_) => {}
        }
    }
}

/// A ring of `slots` slots: the 64-byte header and `slots` slots of `cf32`.
fn ring_of(slots: usize) -> Ring {
    Ring::create(64 + slots * SLOT_SAMPLES * 8).expect("a ring")
}

#[test]
fn with_a_ring_the_hello_says_shm_and_blocks_arrive_as_descriptors() {
    let ring = Ring::create(DEFAULT_RING_BYTES).unwrap();
    let (mut child, mut stdin, frames) = spawn_ring("stream", &[], &ring);
    let hello = read_hello(&frames);
    assert_eq!(hello.protocol, 3);
    let transport = hello
        .transport
        .expect("a protocol-3 hello names its transport");
    assert_eq!(transport.path, TransportPath::Shm);
    assert_eq!(transport.format, "cf32");
    assert_eq!(transport.ring_bytes, ring.bytes() as u64);
    send(&mut stdin, 1, tune(100_000_000, 2_000_000));
    let mut last_t0 = None;
    for want in 0..3u64 {
        let d = read_descriptor(&frames);
        assert_eq!(d.seq, want, "{d:?}");
        assert_eq!(d.sample_rate_hz, 2_000_000);
        assert_eq!(d.center_hz, 100_000_000);
        assert!(
            last_t0.is_none_or(|t| d.t0_ns > t),
            "{d:?} after {last_t0:?}"
        );
        last_t0 = Some(d.t0_ns);
        assert!(d.n > 0);
        let samples = ring.read(d.seq, d.slot, d.n).expect("the slot reads back");
        assert_eq!(samples.len(), d.n as usize);
        // The mock's samples are `v - jv` for a running count `v`.
        assert!(samples.iter().all(|s| s.im == -s.re), "{:?}", &samples[..4]);
        ring.release(d.seq);
    }
    child.kill().unwrap();
}

#[test]
fn a_full_ring_drops_and_counts() {
    let slots = MIN_SLOTS;
    let ring = ring_of(slots);
    let (mut child, mut stdin, frames) = spawn_ring("stream", &[], &ring);
    read_hello(&frames);
    send(&mut stdin, 1, tune(100_000_000, 2_000_000));
    // Nothing is released: the ring holds `slots` blocks, then the child
    // drops and counts every later one.
    for want in 0..slots as u64 {
        assert_eq!(read_descriptor(&frames).seq, want);
    }
    let start = Instant::now();
    let dropped = loop {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "no status counting 8 drops"
        );
        match frames.next() {
            Frame::Descriptor(d) => panic!("a descriptor past a full ring: {d:?}"),
            Frame::Block(_) => panic!("a block on the pipe while a ring is in use"),
            Frame::Control(Control::Status(s)) if s.transport_dropped >= 8 => {
                break s.transport_dropped;
            }
            Frame::Control(_) => {}
        }
    };
    assert!(dropped >= 8, "{dropped}");
    // Still alive and answering.
    send(&mut stdin, 2, tune(101_000_000, 2_000_000));
    let (id, result) = read_reply(&frames);
    assert_eq!(id, 2);
    assert!(result.is_ok(), "{result:?}");
    assert!(child.try_wait().unwrap().is_none());
    child.kill().unwrap();
}

#[test]
fn without_a_ring_blocks_come_on_the_pipe() {
    let (mut child, mut stdin, stdout) = spawn("stream");
    let frames = Frames::new(stdout);
    let hello = read_hello(&frames);
    let transport = hello
        .transport
        .expect("a protocol-3 hello names its transport");
    assert_eq!(transport.path, TransportPath::Pipe);
    assert_eq!(transport.format, "cf32");
    assert_eq!(transport.ring_bytes, 0);
    send(&mut stdin, 1, tune(100_000_000, 2_000_000));
    let mut blocks = 0;
    while blocks < 3 {
        match frames.next() {
            Frame::Block(b) => {
                assert!(!b.samples.is_empty());
                blocks += 1;
            }
            Frame::Descriptor(d) => panic!("a descriptor without a ring: {d:?}"),
            Frame::Control(_) => {}
        }
    }
    child.kill().unwrap();
}

#[test]
fn no_ring_env_makes_the_fake_ignore_the_fd() {
    let ring = Ring::create(DEFAULT_RING_BYTES).unwrap();
    let (mut child, mut stdin, frames) = spawn_ring("stream", &[("ZETA_FAKE_NO_RING", "1")], &ring);
    let hello = read_hello(&frames);
    let transport = hello
        .transport
        .expect("a protocol-3 hello names its transport");
    assert_eq!(transport.path, TransportPath::Pipe);
    assert_eq!(transport.ring_bytes, 0);
    send(&mut stdin, 1, tune(100_000_000, 2_000_000));
    loop {
        match frames.next() {
            Frame::Block(_) => break,
            Frame::Descriptor(d) => {
                panic!("a descriptor from a fake told to ignore the ring: {d:?}")
            }
            Frame::Control(_) => {}
        }
    }
    child.kill().unwrap();
}

/// The next whole block on the pipe within 5 s, skipping control messages; a
/// descriptor fails.
fn read_block(frames: &Frames) -> IqBlock {
    let start = Instant::now();
    loop {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "no block within 5 s"
        );
        match frames.next() {
            Frame::Block(b) => return b,
            Frame::Descriptor(d) => panic!("a descriptor without a ring: {d:?}"),
            Frame::Control(_) => {}
        }
    }
}

/// Whether `samples` are the fake's 16-bit mock's, converted: `v - jv` for a
/// running count `v` below 32768, over 32768.
fn converted_mock_samples(samples: &[num_complex::Complex<f32>]) -> bool {
    !samples.is_empty()
        && samples
            .iter()
            .all(|s| s.im == -s.re && (0.0..1.0).contains(&s.re))
}

#[test]
fn with_a_raw_stream_the_hello_says_sc16() {
    // A 16-bit device: the worker writes its samples to the ring as they
    // are, an sc16 slot holding twice a cf32 slot's, and the node converts.
    let ring = Ring::create(DEFAULT_RING_BYTES).unwrap();
    let (mut child, mut stdin, frames) =
        spawn_ring("stream", &[("ZETA_FAKE_FORMAT", "sc16")], &ring);
    let hello = read_hello(&frames);
    let transport = hello.transport.expect("a transport");
    assert_eq!(transport.path, TransportPath::Shm);
    assert_eq!(transport.format, "sc16");
    assert_eq!(transport.ring_bytes, ring.bytes() as u64);
    // Set before the hello, so the node reads it once the hello is in.
    assert_eq!(ring.format(), Some(SlotFormat::Sc16));
    send(&mut stdin, 1, tune(100_000_000, 2_000_000));
    for want in 0..3u64 {
        let d = read_descriptor(&frames);
        assert_eq!(d.seq, want, "{d:?}");
        assert!(d.n as usize <= SlotFormat::Sc16.samples_per_slot(), "{d:?}");
        let samples = ring.read(d.seq, d.slot, d.n).expect("the slot reads back");
        assert!(
            converted_mock_samples(&samples),
            "{:?}",
            &samples[..4.min(samples.len())]
        );
        ring.release(d.seq);
    }
    child.kill().unwrap();
}

#[test]
fn without_one_it_says_cf32() {
    // The same 16-bit device, its samples converted in the worker; and the
    // fake's default device, whose native format is 8-bit.
    for envs in [&[("ZETA_FAKE_FORMAT", "cf32")][..], &[]] {
        let ring = Ring::create(DEFAULT_RING_BYTES).unwrap();
        let (mut child, mut stdin, frames) = spawn_ring("stream", envs, &ring);
        let transport = read_hello(&frames).transport.expect("a transport");
        assert_eq!(transport.path, TransportPath::Shm, "{envs:?}");
        assert_eq!(transport.format, "cf32", "{envs:?}");
        assert_eq!(ring.format(), Some(SlotFormat::Cf32), "{envs:?}");
        send(&mut stdin, 1, tune(100_000_000, 2_000_000));
        let d = read_descriptor(&frames);
        assert!(d.n as usize <= SLOT_SAMPLES, "{d:?}");
        assert!(!ring.read(d.seq, d.slot, d.n).unwrap().is_empty());
        child.kill().unwrap();
    }
}

#[test]
fn on_the_pipe_blocks_stay_cf32() {
    // A 16-bit device with no ring: the pipe carries cf32, converted in the
    // worker.
    let mut child = spawn_with("stream", &[("ZETA_FAKE_FORMAT", "sc16")], None);
    let mut stdin = child.stdin.take().unwrap();
    let frames = Frames::new(child.stdout.take().unwrap());
    let transport = read_hello(&frames).transport.expect("a transport");
    assert_eq!(transport.path, TransportPath::Pipe);
    assert_eq!(transport.format, "cf32");
    send(&mut stdin, 1, tune(100_000_000, 2_000_000));
    for _ in 0..3 {
        let b = read_block(&frames);
        assert!(
            converted_mock_samples(&b.samples),
            "{:?}",
            &b.samples[..4.min(b.samples.len())]
        );
    }
    child.kill().unwrap();
    let _ = child.wait();
}

#[test]
fn a_block_wider_than_a_slot_is_split() {
    const READ: usize = 600_000;
    // The source reads a hundredth of a second at a time, so a whole read of
    // `READ` arrives as one block only at 100 × `READ` samples a second or more.
    const RATE: u64 = 60_000_000;
    let ring = Ring::create(DEFAULT_RING_BYTES).unwrap();
    let read = READ.to_string();
    let (mut child, mut stdin, frames) =
        spawn_ring("stream", &[("ZETA_FAKE_READ_SAMPLES", &read)], &ring);
    read_hello(&frames);
    send(&mut stdin, 1, tune(100_000_000, RATE));
    let (_, result) = read_reply(&frames);
    let Ok(Reply::Tuning(t)) = result else {
        panic!("{result:?}");
    };
    assert_eq!(t.bandwidth_hz, Hz(RATE));
    let mut pieces: Vec<Descriptor> = Vec::new();
    while pieces.len() < 7 {
        let d = read_descriptor(&frames);
        ring.release(d.seq);
        pieces.push(d);
    }
    for d in &pieces {
        assert!(d.n as usize <= SLOT_SAMPLES, "{d:?}");
        assert_eq!(d.sample_rate_hz, RATE);
    }
    // A read of 600 000 is two full slots and the rest.
    assert!(
        pieces.iter().any(|d| d.n as usize == SLOT_SAMPLES),
        "{pieces:?}"
    );
    assert!(
        pieces
            .iter()
            .any(|d| d.n as usize == READ - 2 * SLOT_SAMPLES),
        "{pieces:?}"
    );
    let mut within_a_block = 0;
    for w in pieces.windows(2) {
        assert_eq!(w[1].seq, w[0].seq + 1);
        // Only pieces of one block are spaced exactly: a full piece is never
        // a block's last (a read is two full slots and the rest), and the
        // next block may start re-anchored, as an overflow or a slow host
        // under load makes the source do.
        if w[0].n as usize != SLOT_SAMPLES {
            continue;
        }
        within_a_block += 1;
        let want = u64::from(w[0].n) * 1_000_000_000 / RATE;
        let got = w[1].t0_ns - w[0].t0_ns;
        // Each t0 is rounded down on its own, so a step may be 1 ns longer.
        assert!(got == want || got == want + 1, "{:?} then {:?}", w[0], w[1]);
    }
    assert!(within_a_block >= 2, "{pieces:?}");
    child.kill().unwrap();
}

#[test]
fn a_hello_carries_the_attachment_set_on_the_source() {
    let mut child = spawn_with(
        "stream",
        &[("ZETA_FAKE_ATTACHMENT", "usb:2-1.4@5000")],
        None,
    );
    let frames = Frames::new(child.stdout.take().unwrap());
    let h = read_hello(&frames);
    assert_eq!(h.attachment.as_deref(), Some("usb:2-1.4@5000"));
    let _ = child.kill();
    let _ = child.wait();

    // A source that cannot tell names none.
    let mut child = spawn_with("stream", &[], None);
    let frames = Frames::new(child.stdout.take().unwrap());
    assert_eq!(read_hello(&frames).attachment, None);
    let _ = child.kill();
    let _ = child.wait();
}
