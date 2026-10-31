//! `WorkerPort` and its supervisor over the fake worker binary: attach, crash
//! and heal, polling for an absent device, the hello timeout, backoff, the
//! scrubbed environment and shutdown on drop.
//!
//! Some tests need a worker that behaves differently from one spawn to the
//! next (a crash, then a worker that never says hello). They point `program`
//! at a small shell wrapper that picks the fake's script by how many `start`
//! lines the `--record` file already holds; the supervisor never runs two
//! workers at once, so the count is the spawn's index.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use zeta_core::port::{Port, PortError, Tuning};
use zeta_core::types::{Capabilities, Claim, Hz, SampleFormat, SourceId};
use zeta_source_worker::port::{Attach, Timings, WorkerConfig, WorkerPort};

const FAKE: &str = env!("CARGO_BIN_EXE_zeta-fake-worker");

fn timings() -> Timings {
    Timings {
        hello: Duration::from_secs(3),
        backoff_first: Duration::from_millis(100),
        backoff_max: Duration::from_millis(300),
        alive_reset: Duration::from_secs(2),
        shutdown_grace: Duration::from_millis(500),
    }
}

/// The fake worker running `script`, no record.
fn cfg(script: &str) -> WorkerConfig {
    WorkerConfig {
        id: SourceId("f".into()),
        program: PathBuf::from(FAKE),
        args: ["--id", "f", "--script", script].map(String::from).to_vec(),
        timings: timings(),
    }
}

/// The fake worker running `script` with `--record record`.
fn cfg_recorded(script: &str, record: &Path) -> WorkerConfig {
    let mut c = cfg(script);
    c.args
        .extend(["--record".into(), record.display().to_string()]);
    c
}

/// A wrapper script in `dir`, run by `/bin/sh`: `body` is shell run with
/// `$REC` the record path, `$FAKE` the fake binary and `$N` the spawn's index;
/// the wrapper is passed `--id f --record $REC`. The script is not executed
/// directly: a file just written can still be open for writing in a child
/// another test is forking, and exec would fail with `ETXTBSY`.
fn cfg_wrapped(dir: &Path, body: &str) -> (WorkerConfig, PathBuf) {
    let rec = dir.join("record");
    let path = dir.join("wrapper.sh");
    let script = format!(
        "REC='{}'\nFAKE='{FAKE}'\nN=$(grep -c '^start' \"$REC\" 2>/dev/null || true)\nN=${{N:-0}}\n{body}\n",
        rec.display()
    );
    std::fs::write(&path, script).unwrap();
    let cfg = WorkerConfig {
        id: SourceId("f".into()),
        program: PathBuf::from("/bin/sh"),
        args: vec![
            path.display().to_string(),
            "--id".into(),
            "f".into(),
            "--record".into(),
            rec.display().to_string(),
        ],
        timings: timings(),
    };
    (cfg, rec)
}

fn tempdir() -> TempDir {
    tempfile::Builder::new()
        .prefix("zeta-supervisor-")
        .tempdir()
        .unwrap()
}

/// The capability record of the fake's `hackrf_like` mock, as a node would
/// have cached it from an earlier open.
fn cached_caps() -> Capabilities {
    Capabilities {
        tune_min_hz: Claim::Asserted(Hz(1_000_000)),
        tune_max_hz: Claim::Asserted(Hz(6_000_000_000)),
        max_bandwidth_hz: Claim::Asserted(Hz(20_000_000)),
        formats: vec![SampleFormat::Cf32],
        channels: 1,
        rx: true,
        tx: false,
        remote: false,
        push: false,
    }
}

fn tuning(center_hz: u64, bandwidth_hz: u64) -> Tuning {
    Tuning {
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(bandwidth_hz),
    }
}

/// Poll `f` every 10 ms until it holds or `within` passes.
async fn wait_for(within: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + within;
    loop {
        if f() {
            return true;
        }
        if Instant::now() > end {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The `start <unix_ms>` times of the record, one per spawn.
fn starts(rec: &Path) -> Vec<u64> {
    std::fs::read_to_string(rec)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.strip_prefix("start "))
        .map(|ms| ms.parse().unwrap())
        .collect()
}

/// Whether a gap between two spawns fits a wait of `want` ms: never shorter
/// than the wait (10 % for clock rounding between processes), and at most
/// 250 ms longer, the process startup a loaded host adds on top.
fn within(gap: u64, want: u64) -> bool {
    gap * 10 >= want * 9 && gap <= want + 250
}

fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

async fn open(cfg: WorkerConfig, cached: Option<Capabilities>) -> (Arc<WorkerPort>, Attach) {
    WorkerPort::open(cfg, cached)
        .await
        .expect("the worker attaches")
}

#[tokio::test]
async fn hello_attaches_and_a_tune_streams_blocks() {
    let (port, attach) = open(cfg("stream"), None).await;
    assert!(matches!(attach, Attach::Hello), "{attach:?}");
    assert_eq!(port.capabilities().max_bandwidth_hz.value().0, 20_000_000);
    let mut rx = port.subscribe();
    let t = port.tune(tuning(100_000_000, 2_000_000)).await.unwrap();
    assert_eq!(t, tuning(100_000_000, 2_000_000));
    assert_eq!(port.current(), Some(t));
    let block = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a block within 5 s")
        .unwrap();
    assert_eq!(block.sample_rate_hz, Hz(2_000_000));
    assert_eq!(block.center_hz, Hz(100_000_000));
    assert!(!block.samples.is_empty());
    let w = port.worker().unwrap();
    assert!(w.pid.is_some());
    assert!(w.since_ns.is_some());
    assert_eq!(w.restarts, 0);
    assert_eq!(port.fault(), None);
    assert_eq!(*port.healed().unwrap().borrow(), 0);
}

#[tokio::test]
async fn a_crash_faults_names_the_signal_and_a_respawn_heals_once() {
    let dir = tempdir();
    let (cfg, _rec) = cfg_wrapped(
        dir.path(),
        "ZETA_FAKE_CRASH_ONCE=1 exec \"$FAKE\" --script 'crash-after-ms 500' \"$@\"",
    );
    let (port, attach) = open(cfg, None).await;
    assert!(matches!(attach, Attach::Hello), "{attach:?}");
    let healed = port.healed().unwrap();
    assert_eq!(*healed.borrow(), 0);
    let crashed = "soapy worker for f died: signal 6 (SIGABRT)";
    assert!(
        wait_for(Duration::from_secs(2), || port.fault().as_deref()
            == Some(crashed))
        .await,
        "fault: {:?}",
        port.fault()
    );
    assert_eq!(*healed.borrow(), 0);
    assert!(
        wait_for(Duration::from_secs(5), || *healed.borrow() > 0).await,
        "no heal"
    );
    // Give a second bump time to show if there were one.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(*healed.borrow(), 1);
    assert_eq!(port.fault(), None);
    let w = port.worker().unwrap();
    assert_eq!(w.restarts, 1);
    assert!(w.pid.is_some());
    // The healed worker answers.
    port.tune(tuning(100_000_000, 2_000_000)).await.unwrap();
}

#[tokio::test]
async fn not_found_with_a_cache_attaches_faulted_and_polls_at_the_cap() {
    let dir = tempdir();
    let rec = dir.path().join("record");
    let (port, attach) = open(cfg_recorded("not-found", &rec), Some(cached_caps())).await;
    match &attach {
        Attach::NotFound { args } => assert_eq!(args, "fake"),
        a => panic!("expected NotFound, got {a:?}"),
    }
    assert_eq!(port.capabilities(), &cached_caps());
    assert_eq!(port.fault().as_deref(), Some("device not found: fake"));
    let w = port.worker().unwrap();
    assert_eq!(w.pid, None);
    assert_eq!(w.since_ns, None);
    let before = starts(&rec).len();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let s = starts(&rec);
    // At a 300 ms cap, 1.5 s holds about five polls.
    // A loaded host stretches each poll by the fake's fork, exec and reap.
    let polls = s.len() - before;
    assert!((2..=7).contains(&polls), "{polls} polls in 1.5 s: {s:?}");
    for g in s.windows(2).map(|w| w[1] - w[0]) {
        assert!(within(g, 300), "gap {g} ms: {s:?}");
    }
    assert_eq!(port.fault().as_deref(), Some("device not found: fake"));
    assert!(port.worker().unwrap().restarts >= polls as u64);
}

#[tokio::test]
async fn not_found_without_a_cache_is_refused_and_leaves_no_child() {
    let dir = tempdir();
    let pidfile = dir.path().join("pid");
    let (cfg, rec) = cfg_wrapped(
        dir.path(),
        &format!(
            "echo $$ > '{}'\nexec \"$FAKE\" --script not-found \"$@\"",
            pidfile.display()
        ),
    );
    let err = WorkerPort::open(cfg, None).await.err().expect("refused");
    assert_eq!(err, "device not found: fake");
    let pid: u32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // Reaped, not a zombie: the /proc entry is gone.
    assert!(!alive(pid), "worker {pid} still exists");
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(starts(&rec).len(), 1, "no respawn after a refusal");
}

#[tokio::test]
async fn open_fails_is_refused_with_the_last_stderr_line() {
    let err = WorkerPort::open(cfg("open-fails"), Some(cached_caps()))
        .await
        .err()
        .expect("refused");
    assert_eq!(err, "soapy open failed: fake open: no such device");
}

#[tokio::test]
async fn a_hang_before_hello_is_killed_at_the_timeout() {
    let dir = tempdir();
    let pidfile = dir.path().join("pid");
    let (cfg, _rec) = cfg_wrapped(
        dir.path(),
        &format!(
            "echo $$ > '{}'\nexec \"$FAKE\" --script hang-hello \"$@\"",
            pidfile.display()
        ),
    );
    let t0 = Instant::now();
    let err = WorkerPort::open(cfg, None).await.err().expect("refused");
    let took = t0.elapsed();
    assert_eq!(err, "soapy worker for f: no hello in 3 s");
    assert!(
        took >= Duration::from_millis(2900) && took < Duration::from_secs(6),
        "{took:?}"
    );
    let pid: u32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(!alive(pid), "worker {pid} still exists");
}

#[tokio::test]
async fn tune_during_a_respawn_answers_the_fault_at_once() {
    let dir = tempdir();
    // The first spawn crashes after its hello; every respawn hangs before
    // hello, so the port stays down for the hello timeout.
    let (cfg, _rec) = cfg_wrapped(
        dir.path(),
        "if [ \"$N\" -eq 0 ]; then exec \"$FAKE\" --script 'crash-after-ms 500' \"$@\"; fi\n\
         exec \"$FAKE\" --script hang-hello \"$@\"",
    );
    let (port, _) = open(cfg, None).await;
    port.tune(tuning(100_000_000, 2_000_000)).await.unwrap();
    let crashed = "soapy worker for f died: signal 6 (SIGABRT)";
    assert!(
        wait_for(Duration::from_secs(2), || port.fault().as_deref()
            == Some(crashed))
        .await,
        "fault: {:?}",
        port.fault()
    );
    assert_eq!(port.current(), None);
    // Let the respawn start; it never says hello.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let t0 = Instant::now();
    let r = port.tune(tuning(100_000_000, 2_000_000)).await;
    let took = t0.elapsed();
    assert_eq!(r, Err(PortError::Io(crashed.into())));
    assert!(took < Duration::from_millis(100), "{took:?}");
}

#[tokio::test]
async fn a_tune_between_a_death_and_its_reap_answers_the_death() {
    let dir = tempdir();
    let pidfile = dir.path().join("pid");
    // A grandchild keeps the worker's stdout open for 2 s after the worker
    // dies, so the reap waits out its reader join (1 s) with the dead
    // worker's link still in the snapshot. Its stdin has no reader left, so a
    // tune in that window fails its write.
    let (cfg, _rec) = cfg_wrapped(
        dir.path(),
        &format!(
            "echo $$ > '{}'\nsleep 2 < /dev/null 2> /dev/null &\n\
             ZETA_FAKE_CRASH_ONCE=1 exec \"$FAKE\" --script 'crash-after-ms 300' \"$@\"",
            pidfile.display()
        ),
    );
    let (port, _) = open(cfg, None).await;
    let pid: u32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        wait_for(Duration::from_secs(3), || !alive(pid)).await,
        "the worker did not die"
    );
    // Reaped, but the death is not yet the port's fault: the window is open.
    assert_eq!(port.fault(), None);
    let r = port.tune(tuning(100_000_000, 2_000_000)).await;
    assert_eq!(
        r,
        Err(PortError::Io(
            "soapy worker for f died: signal 6 (SIGABRT)".into()
        ))
    );
}

#[tokio::test]
async fn backoff_doubles_to_the_cap_and_resets_after_alive() {
    let dir = tempdir();
    // Spawn 0 says hello and crashes at 300 ms; 1-3 fail to open at once
    // (backoff 100 → 200 → 300 → 300); 4 lives 2.3 s, past the 2 s alive
    // reset, then crashes (backoff back to 100); 5 fails (then 200); 6 streams.
    let (cfg, rec) = cfg_wrapped(
        dir.path(),
        "rm -f \"$REC.crashed\"\n\
         case \"$N\" in\n\
         0) exec \"$FAKE\" --script 'crash-after-ms 300' \"$@\";;\n\
         1|2|3|5) exec \"$FAKE\" --script open-fails \"$@\";;\n\
         4) exec \"$FAKE\" --script 'crash-after-ms 2300' \"$@\";;\n\
         *) exec \"$FAKE\" --script stream \"$@\";;\n\
         esac",
    );
    let (port, _) = open(cfg, None).await;
    assert!(
        wait_for(Duration::from_secs(10), || starts(&rec).len() >= 7).await,
        "spawns: {:?}",
        starts(&rec)
    );
    let s = starts(&rec);
    let gap = |i: usize| s[i] - s[i - 1];
    // The schedule is pinned by the floors: 200 is not 100, 300 is not 200,
    // and the cap is not a further doubling to 600.
    // Spawn 0 lived 300 ms from its start line, then 100 ms of backoff.
    let first = gap(1).saturating_sub(300);
    assert!(within(first, 100), "first backoff {first} ms: {s:?}");
    assert!(within(gap(2), 200), "second backoff {} ms: {s:?}", gap(2));
    assert!(within(gap(3), 300), "third backoff {} ms: {s:?}", gap(3));
    assert!(within(gap(4), 300), "capped backoff {} ms: {s:?}", gap(4));
    // Spawn 4 lived 2.3 s, past the alive reset: its backoff is the first
    // one again. Its gap holds the same startup as spawn 0's, so the two are
    // compared with each other; without the reset it would be the 300 ms cap,
    // 200 ms more.
    let reset = gap(5).saturating_sub(2300);
    assert!(within(reset, 100), "reset backoff {reset} ms: {s:?}");
    assert!(
        reset.abs_diff(first) < 150,
        "reset backoff {reset} ms against the first {first} ms: {s:?}"
    );
    assert!(within(gap(6), 200), "doubled again {} ms: {s:?}", gap(6));
    // Two episodes, each healed by a hello: spawn 4's and spawn 6's.
    let healed = port.healed().unwrap();
    assert!(
        wait_for(Duration::from_secs(3), || *healed.borrow() == 2).await,
        "healed {}",
        *healed.borrow()
    );
    assert_eq!(port.fault(), None);
    assert_eq!(port.worker().unwrap().restarts, 6);
}

#[tokio::test]
async fn the_child_environment_has_no_secrets() {
    // SAFETY: the variables are set once, before this test spawns anything;
    // other tests in this binary spawn concurrently, but std serialises its
    // own environment access and nothing here calls libc's getenv.
    unsafe {
        std::env::set_var("ZETA_SECRET", "s");
        std::env::set_var("ZETA_NODE_SECRET_X", "s");
        std::env::set_var("ZETA_KIWI_PASSWORD_Y", "s");
        std::env::set_var("ZETA_STT_NODE_SECRET", "s");
        if std::env::var_os("HOME").is_none() {
            std::env::set_var("HOME", "/nonexistent");
        }
    }
    let dir = tempdir();
    let rec = dir.path().join("record");
    let (_port, _) = open(cfg_recorded("stream", &rec), None).await;
    let text = std::fs::read_to_string(&rec).unwrap();
    let names: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("env "))
        .collect();
    for secret in [
        "ZETA_SECRET",
        "ZETA_NODE_SECRET_X",
        "ZETA_KIWI_PASSWORD_Y",
        "ZETA_STT_NODE_SECRET",
    ] {
        assert!(!names.contains(&secret), "{secret} reached the worker");
    }
    assert!(names.contains(&"HOME"), "{names:?}");
}

#[tokio::test]
async fn dropping_the_port_ends_the_child_within_the_grace() {
    let (port, _) = open(cfg("stream"), None).await;
    port.tune(tuning(100_000_000, 2_000_000)).await.unwrap();
    let pid = port.worker().unwrap().pid.unwrap();
    assert!(alive(pid));
    drop(port);
    assert!(
        wait_for(Duration::from_millis(700), || !alive(pid)).await,
        "worker {pid} outlived the port"
    );
}

#[tokio::test]
async fn a_worker_ignoring_shutdown_is_killed_after_the_grace() {
    let dir = tempdir();
    let pidfile = dir.path().join("pid");
    // A wrapper that never reads its stdin, so the `shutdown` goes nowhere and
    // only the kill ends it. The fake it runs reads a fifo the wrapper holds
    // open, so the fake ends with the wrapper.
    let (cfg, _rec) = cfg_wrapped(
        dir.path(),
        &format!(
            "echo $$ > '{}'\nmkfifo \"$REC.in\"\n\"$FAKE\" --script stream \"$@\" < \"$REC.in\" &\nexec 3> \"$REC.in\"\nwait",
            pidfile.display()
        ),
    );
    let (port, _) = open(cfg, None).await;
    let pid: u32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let t0 = Instant::now();
    drop(port);
    assert!(
        wait_for(Duration::from_secs(3), || !alive(pid)).await,
        "worker {pid} outlived the port"
    );
    assert!(
        t0.elapsed() >= Duration::from_millis(400),
        "{:?}",
        t0.elapsed()
    );
}

#[tokio::test]
async fn a_bad_band_is_refused_without_a_round_trip() {
    let dir = tempdir();
    let rec = dir.path().join("record");
    let (port, _) = open(cfg_recorded("not-found", &rec), Some(cached_caps())).await;
    let e = port
        .tune(tuning(10_000_000_000, 2_000_000))
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            PortError::OutOfRange {
                field: "center_hz",
                ..
            }
        ),
        "{e:?}"
    );
    assert_eq!(
        e.to_string(),
        "center_hz 10000000000 out of range 1000000..=6000000000"
    );
    let e = port.tune(tuning(100_000_000, 0)).await.unwrap_err();
    assert_eq!(e.to_string(), "bandwidth_hz 0 out of range 1..=20000000");
    let e = port
        .tune(tuning(100_000_000, 30_000_000))
        .await
        .unwrap_err();
    assert_eq!(
        e.to_string(),
        "bandwidth_hz 30000000 out of range 1..=20000000"
    );
    // A good band on a down worker is the fault, at once.
    let e = port.tune(tuning(100_000_000, 2_000_000)).await.unwrap_err();
    assert_eq!(e, PortError::Io("device not found: fake".into()));
}

#[tokio::test]
async fn a_hello_of_another_protocol_is_refused_and_the_child_reaped() {
    // A binary replaced under a running node says hello with its own
    // protocol; the node must name the mismatch, not decode its frames.
    let dir = tempdir();
    let pidfile = dir.path().join("pid");
    let (cfg, _rec) = cfg_wrapped(
        dir.path(),
        &format!(
            "echo $$ > '{}'\nZETA_FAKE_PROTOCOL=99 exec \"$FAKE\" --script stream \"$@\"",
            pidfile.display()
        ),
    );
    let t0 = Instant::now();
    let err = WorkerPort::open(cfg, None).await.err().expect("refused");
    assert_eq!(err, "soapy worker for f: protocol 99, expected 1");
    assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
    let pid: u32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(!alive(pid), "worker {pid} still exists");
}

#[test]
fn close_shuts_the_worker_down_before_the_runtime_ends() {
    // As the node's `main` ends: the port is closed, then the runtime is torn
    // down at once. The worker must have read `shutdown` and left `serve`
    // (its `served` line), not been killed with the runtime.
    let dir = tempdir();
    let rec = dir.path().join("record");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pid = rt.block_on(async {
        let (port, _) = open(cfg_recorded("stream", &rec), None).await;
        port.tune(tuning(100_000_000, 2_000_000)).await.unwrap();
        let pid = port.worker().unwrap().pid.unwrap();
        // Held elsewhere, as the node holds every port: closing must not wait
        // for the last reference.
        let held = port.clone();
        drop(port);
        let t0 = Instant::now();
        held.close().await;
        assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
        // A second close is a no-op.
        held.close().await;
        assert_eq!(
            held.fault().as_deref(),
            Some("soapy worker for f: shut down")
        );
        drop(held);
        pid
    });
    drop(rt);
    assert!(!alive(pid), "worker {pid} outlived the close");
    let text = std::fs::read_to_string(&rec).unwrap();
    assert!(text.lines().any(|l| l == "served"), "{text}");
}
