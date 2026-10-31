//! `serve` over a real pipe: the fake worker binary, spawned as the node will
//! spawn the real one.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use zeta_core::port::{IqBlock, Tuning};
use zeta_core::types::{Claim, Hz};
use zeta_source_worker::wire::{
    Cmd, Control, Frame, Hello, PROTOCOL, Reply, Request, Status, read_frame, write_request,
};

const FAKE: &str = env!("CARGO_BIN_EXE_zeta-fake-worker");

/// The fake worker running `script` with `envs` set, stdio piped.
fn spawn_with(script: &str, envs: &[(&str, &str)]) -> Child {
    let mut cmd = Command::new(FAKE);
    cmd.args(["--id", "f", "--script", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.spawn().expect("spawn the fake worker")
}

fn spawn(script: &str) -> (Child, ChildStdin, ChildStdout) {
    let mut child = spawn_with(script, &[]);
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
    assert_eq!(hello.protocol, 1);
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
        let mut child = spawn_with(script, &[]);
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
    let mut child = spawn_with("stream", &[("ZETA_FAKE_PRINT", "1")]);
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
