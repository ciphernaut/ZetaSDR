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
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, TransportPath, Tuning};
use zeta_core::types::{Capabilities, Claim, Depth, Hz, SampleFormat, SourceId};
use zeta_source_worker::port::{
    Attach, HELLO_TIMEOUT, Timings, WorkerConfig, WorkerPort, worker_args,
};
use zeta_source_worker::ring::{DEFAULT_RING_BYTES, Ring};

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
        ring_bytes: DEFAULT_RING_BYTES,
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
        ring_bytes: DEFAULT_RING_BYTES,
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
        usable_fraction: Claim::Asserted(1.0),
        depths: vec![],
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

/// The command line of process `pid`.
fn cmdline(pid: u32) -> Vec<String> {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .unwrap_or_default()
        .split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect()
}

/// The `--ring-fd` value on process `pid`'s command line, if any.
fn ring_fd_arg(pid: u32) -> Option<String> {
    let args = cmdline(pid);
    let at = args.iter().position(|a| a == "--ring-fd")?;
    args.get(at + 1).cloned()
}

/// The next block on `rx`, within 5 s.
async fn next_block(rx: &mut IqReceiver) -> IqBlock {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("a block within 5 s")
        {
            Ok(b) => return b,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(e) => panic!("the block broadcast ended: {e}"),
        }
    }
}

/// Whether `b` holds the mock's samples: `v - jv` for a running count `v`.
fn mock_samples(b: &IqBlock) -> bool {
    !b.samples.is_empty()
        && b.samples.iter().all(|s| s.im == -s.re)
        && b.samples.windows(2).all(|w| w[1].re == w[0].re + 1.0)
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
    // Reported until the node has seen it, a tune or not: jobs poll the fault.
    assert_eq!(port.fault().as_deref(), Some(crashed));
    port.tune(tuning(100_000_000, 2_000_000)).await.unwrap();
    assert_eq!(port.fault().as_deref(), Some(crashed));
    port.heal_settled();
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

/// The field host's deploy (2026-11-08): the first B210 worker died with SIGSEGV
/// opening the device while UHD was still releasing it from the old node, and the
/// node exited. A first spawn that crashes before its hello, on a device an earlier
/// open cached, attaches faulted and respawns with backoff like any later crash;
/// never seen, it is still refused (the node could not list it).
#[tokio::test]
async fn a_crash_before_the_first_hello_with_a_cache_attaches_faulted_and_heals() {
    // Spawn 0 dies by SIGSEGV before the fake starts (it writes its own `start`
    // line, so the wrapper writes spawn 0's); later spawns stream.
    let crash_first = "if [ \"$N\" -eq 0 ]; then echo 'start 0' >> \"$REC\"; kill -SEGV $$; fi\n\
                       exec \"$FAKE\" --script stream \"$@\"";
    let crashed = "soapy worker for f died: signal 11 (SIGSEGV)";

    let dir = tempdir();
    let (cfg, rec) = cfg_wrapped(dir.path(), crash_first);
    let (port, attach) = open(cfg, Some(cached_caps())).await;
    match &attach {
        Attach::Crashed { fault } => assert_eq!(fault, crashed),
        a => panic!("expected Crashed, got {a:?}"),
    }
    assert_eq!(port.capabilities(), &cached_caps());
    assert_eq!(port.fault().as_deref(), Some(crashed));
    let healed = port.healed().unwrap();
    assert!(
        wait_for(Duration::from_secs(5), || *healed.borrow() > 0).await,
        "no heal: {:?}",
        port.fault()
    );
    assert_eq!(starts(&rec).len(), 2, "one respawn");
    port.heal_settled();
    assert_eq!(port.fault(), None);
    assert_eq!(port.worker().unwrap().restarts, 1);
    port.tune(tuning(100_000_000, 2_000_000)).await.unwrap();

    let dir = tempdir();
    let (cfg, rec) = cfg_wrapped(dir.path(), crash_first);
    let err = WorkerPort::open(cfg, None).await.err().expect("refused");
    assert_eq!(err, crashed);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(starts(&rec).len(), 1, "no respawn after a refusal");
}

/// A node upgraded over a source record cached before `usable_fraction` and
/// `depths` (it loads them as 1.0 and none) attaches faulted with it when the
/// device is absent; the respawn's hello carries the live values, and the
/// port takes them.
#[tokio::test]
async fn a_cached_record_from_before_this_plan_takes_the_hellos_new_fields() {
    let dir = tempdir();
    let (cfg, _rec) = cfg_wrapped(
        dir.path(),
        "if [ \"$N\" -eq 0 ]; then exec \"$FAKE\" --script not-found \"$@\"; fi\n\
         ZETA_FAKE_USABLE=0.9 exec \"$FAKE\" --script stream \"$@\"",
    );
    // The mock's whole record but for the two fields (`cached_caps` lists one
    // format only, which a hello would not replace).
    let mut cached = cached_caps();
    cached.formats = vec![SampleFormat::Ci8, SampleFormat::Ci16, SampleFormat::Cf32];
    assert_eq!(cached.usable_fraction, Claim::Asserted(1.0));
    assert!(cached.depths.is_empty());
    let (port, attach) = open(cfg, Some(cached.clone())).await;
    assert!(matches!(attach, Attach::NotFound { .. }), "{attach:?}");
    assert_eq!(port.capabilities(), &cached);
    let healed = port.healed().unwrap();
    assert!(
        wait_for(Duration::from_secs(5), || *healed.borrow() > 0).await,
        "no heal: {:?}",
        port.fault()
    );
    port.heal_settled();
    assert!(
        wait_for(Duration::from_secs(5), || port.fault().is_none()).await,
        "{:?}",
        port.fault()
    );
    assert_eq!(port.capabilities().usable_fraction, Claim::Asserted(0.9));
    // Nothing else of the record changed.
    let mut want = cached;
    want.usable_fraction = Claim::Asserted(0.9);
    assert_eq!(port.capabilities(), &want);
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
    port.heal_settled();
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
        std::env::set_var("ZETA_LLM_KEY", "s");
        std::env::set_var("ZETA_SOME_PASSWORD", "s");
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
        "ZETA_LLM_KEY",
        "ZETA_SOME_PASSWORD",
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
            "echo $$ > '{}'\nZETA_FAKE_PROTOCOL=3 exec \"$FAKE\" --script stream \"$@\"",
            pidfile.display()
        ),
    );
    let t0 = Instant::now();
    let err = WorkerPort::open(cfg, None).await.err().expect("refused");
    assert_eq!(err, "soapy worker for f: protocol 3, expected 1 or 2");
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

#[tokio::test]
async fn close_ends_a_worker_whose_runtime_waits_on_stdin_before_the_grace() {
    // A worker that, like the real one returning into `#[tokio::main]`, waits
    // for its blocked stdin read on the way out: it can only exit once the
    // parent closes its stdin after `shutdown`. With a 3 s grace, a close that
    // returns in under 1 s means the worker exited on its own, not by the kill.
    let dir = tempdir();
    let (mut cfg, rec) = cfg_wrapped(
        dir.path(),
        "ZETA_FAKE_WAIT_RUNTIME=1 exec \"$FAKE\" --script stream \"$@\"",
    );
    cfg.timings.shutdown_grace = Duration::from_secs(3);
    let (port, _) = open(cfg, None).await;
    port.tune(tuning(100_000_000, 2_000_000)).await.unwrap();
    let pid = port.worker().unwrap().pid.unwrap();
    let t0 = Instant::now();
    port.close().await;
    let took = t0.elapsed();
    assert!(took < Duration::from_secs(1), "{took:?}");
    assert!(!alive(pid), "worker {pid} outlived the close");
    let text = std::fs::read_to_string(&rec).unwrap();
    assert!(text.lines().any(|l| l == "served"), "{text}");
}

#[tokio::test]
async fn blocks_arrive_through_the_ring_and_the_worker_info_says_shm() {
    let (port, _) = open(cfg("stream"), None).await;
    let w = port.worker().unwrap();
    let transport = w
        .transport
        .expect("a protocol-2 worker names its transport");
    assert_eq!(transport.path, TransportPath::Shm);
    assert_eq!(transport.format, "cf32");
    // The header and as many whole slots as fit.
    let ring_bytes = Ring::create(DEFAULT_RING_BYTES).unwrap().bytes();
    assert_eq!(transport.ring_bytes, ring_bytes as u64);
    assert_eq!(w.transport_dropped, 0);
    assert_eq!(ring_fd_arg(w.pid.unwrap()).as_deref(), Some("3"));
    let mut rx = port.subscribe();
    port.tune(tuning(100_000_000, 2_000_000)).await.unwrap();
    let mut last: Option<IqBlock> = None;
    for _ in 0..3 {
        let b = next_block(&mut rx).await;
        assert_eq!(b.sample_rate_hz, Hz(2_000_000));
        assert_eq!(b.center_hz, Hz(100_000_000));
        assert!(
            mock_samples(&b),
            "{:?}",
            &b.samples[..4.min(b.samples.len())]
        );
        if let Some(prev) = &last {
            assert!(b.t0.0 > prev.t0.0, "{:?} after {:?}", b.t0, prev.t0);
        }
        last = Some(b);
    }
    assert_eq!(port.fault(), None);
}

#[tokio::test]
async fn each_spawn_gets_its_own_ring() {
    let dir = tempdir();
    let (cfg, _rec) = cfg_wrapped(
        dir.path(),
        "ZETA_FAKE_CRASH_ONCE=1 exec \"$FAKE\" --script 'crash-after-ms 500' \"$@\"",
    );
    let (port, _) = open(cfg, None).await;
    let first = port.worker().unwrap();
    let first_ino = port.ring_ino().expect("a live ring");
    assert_eq!(ring_fd_arg(first.pid.unwrap()).as_deref(), Some("3"));
    let healed = port.healed().unwrap();
    assert!(
        wait_for(Duration::from_secs(5), || *healed.borrow() > 0).await,
        "no heal"
    );
    let second = port.worker().unwrap();
    assert_eq!(second.restarts, 1);
    assert_ne!(second.pid, first.pid);
    assert!(
        second.since_ns > first.since_ns,
        "{second:?} after {first:?}"
    );
    assert_eq!(ring_fd_arg(second.pid.unwrap()).as_deref(), Some("3"));
    assert_eq!(second.transport.unwrap().path, TransportPath::Shm);
    let second_ino = port.ring_ino().expect("a live ring");
    assert_ne!(second_ino, first_ino, "the respawn reused the old memfd");
    let mut rx = port.subscribe();
    port.tune(tuning(100_000_000, 2_000_000)).await.unwrap();
    assert!(mock_samples(&next_block(&mut rx).await));
}

#[tokio::test]
async fn a_descriptor_outside_the_ring_is_a_bad_frame() {
    let dir = tempdir();
    // Only the first spawn names a slot the ring does not have.
    let (cfg, _rec) = cfg_wrapped(
        dir.path(),
        "[ \"$N\" = 0 ] && export ZETA_FAKE_BAD_SLOT=1\nexec \"$FAKE\" --script stream \"$@\"",
    );
    let (port, _) = open(cfg, None).await;
    let healed = port.healed().unwrap();
    // The first descriptor follows the tune; its reply may or may not get in
    // before the worker is ended for it.
    let _ = port.tune(tuning(100_000_000, 2_000_000)).await;
    assert!(
        wait_for(Duration::from_secs(3), || port
            .fault()
            .is_some_and(|f| f.contains("bad frame")))
        .await,
        "fault: {:?}",
        port.fault()
    );
    let fault = port.fault().unwrap();
    assert!(fault.contains("slot 9999"), "{fault}");
    assert!(
        wait_for(Duration::from_secs(5), || *healed.borrow() > 0).await,
        "no heal"
    );
    port.heal_settled();
    assert_eq!(port.worker().unwrap().restarts, 1);
    let mut rx = port.subscribe();
    port.tune(tuning(100_000_000, 2_000_000)).await.unwrap();
    assert!(mock_samples(&next_block(&mut rx).await));
    assert_eq!(port.fault(), None);
}

#[tokio::test]
async fn a_descriptor_out_of_order_is_a_bad_frame() {
    let dir = tempdir();
    // Only the first spawn repeats seq 1 as its third descriptor, a slot the
    // ring's own checks would let through.
    let (cfg, _rec) = cfg_wrapped(
        dir.path(),
        "[ \"$N\" = 0 ] && export ZETA_FAKE_STALE_SEQ=1\nexec \"$FAKE\" --script stream \"$@\"",
    );
    let (port, _) = open(cfg, None).await;
    let healed = port.healed().unwrap();
    let _ = port.tune(tuning(100_000_000, 2_000_000)).await;
    assert!(
        wait_for(Duration::from_secs(3), || port
            .fault()
            .is_some_and(|f| f.contains("bad frame")))
        .await,
        "fault: {:?}",
        port.fault()
    );
    let fault = port.fault().unwrap();
    assert!(
        fault.contains("bad frame: descriptor seq 1, expected 2"),
        "{fault}"
    );
    assert!(
        wait_for(Duration::from_secs(5), || *healed.borrow() > 0).await,
        "no heal"
    );
    port.heal_settled();
    assert_eq!(port.worker().unwrap().restarts, 1);
    let mut rx = port.subscribe();
    port.tune(tuning(100_000_000, 2_000_000)).await.unwrap();
    assert!(mock_samples(&next_block(&mut rx).await));
    assert_eq!(port.fault(), None);
}

#[tokio::test]
async fn without_a_ring_the_child_streams_on_the_pipe() {
    let mut c = cfg("stream");
    c.ring_bytes = 0;
    let (port, _) = open(c, None).await;
    let w = port.worker().unwrap();
    assert_eq!(ring_fd_arg(w.pid.unwrap()), None);
    let transport = w
        .transport
        .expect("a protocol-2 worker names its transport");
    assert_eq!(transport.path, TransportPath::Pipe);
    assert_eq!(transport.ring_bytes, 0);
    assert_eq!(port.ring_ino(), None);
    let mut rx = port.subscribe();
    port.tune(tuning(100_000_000, 2_000_000)).await.unwrap();
    let b = next_block(&mut rx).await;
    assert_eq!(b.sample_rate_hz, Hz(2_000_000));
    assert!(mock_samples(&b));
}

#[tokio::test]
async fn a_protocol_1_child_is_still_accepted() {
    // A worker of an older build: it ignores `--ring-fd`, says hello with
    // protocol 1 and no transport, and sends its blocks on the pipe.
    let dir = tempdir();
    let (cfg, _rec) = cfg_wrapped(
        dir.path(),
        "ZETA_FAKE_PROTOCOL=1 exec \"$FAKE\" --script stream \"$@\"",
    );
    let (port, attach) = open(cfg, None).await;
    assert!(matches!(attach, Attach::Hello), "{attach:?}");
    let w = port.worker().unwrap();
    assert_eq!(w.transport, None);
    assert_eq!(w.transport_dropped, 0);
    let mut rx = port.subscribe();
    port.tune(tuning(100_000_000, 2_000_000)).await.unwrap();
    let b = next_block(&mut rx).await;
    assert_eq!(b.center_hz, Hz(100_000_000));
    assert!(mock_samples(&b));
    assert_eq!(port.fault(), None);
}

/// The `arg` lines the wrapper recorded for each spawn, split at its `start`.
fn spawn_args(rec: &Path) -> Vec<Vec<String>> {
    let mut spawns = vec![vec![]];
    for l in std::fs::read_to_string(rec).unwrap_or_default().lines() {
        if let Some(a) = l.strip_prefix("arg ") {
            spawns.last_mut().unwrap().push(a.to_string());
        } else if l.starts_with("start ") {
            spawns.push(vec![]);
        }
    }
    spawns.pop();
    spawns
}

/// How often `--rate-cap-hz 20480000` is among a spawn's arguments.
fn caps_passed(args: &[String]) -> usize {
    args.windows(2)
        .filter(|w| w == &["--rate-cap-hz", "20480000"])
        .count()
}

/// A config whose wrapper records each spawn's arguments as `arg` lines, then
/// runs the fake: spawn 0 with `first` (shell before the exec, e.g. an env
/// assignment) and `crash-after-ms 300`, every later spawn streaming. `args`
/// are what the node would spawn `zeta` with.
fn cfg_recording_args(dir: &Path, first: &str, args: Vec<String>) -> (WorkerConfig, PathBuf) {
    let rec = dir.join("record");
    let wrapper = dir.join("wrapper.sh");
    std::fs::write(
        &wrapper,
        format!(
            "REC='{}'\nN=$(grep -c '^start' \"$REC\" 2>/dev/null || true)\n\
             for a in \"$@\"; do echo \"arg $a\" >> \"$REC\"; done\n\
             case \"${{N:-0}}\" in\n\
             0) {first} exec '{FAKE}' --id f --script 'crash-after-ms 300' --record \"$REC\";;\n\
             *) exec '{FAKE}' --id f --script stream --record \"$REC\";;\n\
             esac\n",
            rec.display()
        ),
    )
    .unwrap();
    let mut wrapped = vec![wrapper.display().to_string()];
    wrapped.extend(args);
    let cfg = WorkerConfig {
        id: SourceId("f".into()),
        program: PathBuf::from("/bin/sh"),
        args: wrapped,
        timings: timings(),
        ring_bytes: 0,
    };
    (cfg, rec)
}

/// The arguments of each spawn of `cfg`, once there are two.
async fn two_spawns(
    cfg: WorkerConfig,
    rec: &Path,
    cached: Option<Capabilities>,
) -> Vec<Vec<String>> {
    let (port, _) = open(cfg, cached).await;
    assert!(
        wait_for(Duration::from_secs(5), || starts(rec).len() >= 2).await,
        "spawns: {:?}",
        starts(rec)
    );
    drop(port);
    let spawns = spawn_args(rec);
    assert!(spawns.len() >= 2, "{spawns:?}");
    spawns
}

#[tokio::test]
async fn a_cached_measured_cap_is_passed_to_the_respawn() {
    let mut measured = cached_caps();
    measured.max_bandwidth_hz = Claim::Measured(Hz(20_480_000));
    let base = ["soapy-worker", "--id", "f", "--args", "driver=fake"];
    assert_eq!(worker_args("f", "driver=fake", None), base);
    assert_eq!(
        worker_args("f", "driver=fake", Some(&cached_caps())),
        base,
        "an asserted cap is probed again"
    );
    let args = worker_args("f", "driver=fake", Some(&measured));
    assert_eq!(args[..5], base);
    assert_eq!(args[5..], ["--rate-cap-hz", "20480000"]);

    // Cached at start: every spawn carries the cap once, the supervisor adding
    // no second copy.
    let dir = tempdir();
    let (cfg, rec) = cfg_recording_args(dir.path(), "", args);
    let spawns = two_spawns(cfg, &rec, Some(measured)).await;
    for args in &spawns[..2] {
        assert_eq!(caps_passed(args), 1, "{args:?}");
    }
}

#[tokio::test]
async fn a_cap_learned_from_the_hello_is_passed_to_the_respawn() {
    // Nothing cached: spawn 0 probes (its hello says Measured 20.48 MHz) and
    // crashes; the respawn is told the cap instead of probing again.
    let dir = tempdir();
    let (cfg, rec) = cfg_recording_args(
        dir.path(),
        "ZETA_FAKE_MEASURED_CAP=20480000",
        worker_args("f", "driver=fake", None),
    );
    let spawns = two_spawns(cfg, &rec, None).await;
    assert_eq!(caps_passed(&spawns[0]), 0, "{:?}", spawns[0]);
    assert_eq!(caps_passed(&spawns[1]), 1, "{:?}", spawns[1]);
}

#[tokio::test]
async fn a_cap_measured_on_a_respawn_replaces_the_asserted_one() {
    // Spawn 0 could not measure (its hello asserts the mock's 20 MHz) and
    // crashes; spawn 1 measures 20.48 MHz and crashes too; spawn 2 streams,
    // handed the `--rate-cap-hz` it was spawned with as the real worker is.
    let dir = tempdir();
    let rec = dir.path().join("record");
    let wrapper = dir.path().join("wrapper.sh");
    std::fs::write(
        &wrapper,
        format!(
            "REC='{}'\nN=$(grep -c '^start' \"$REC\" 2>/dev/null || true)\n\
             CAP=; prev=\n\
             for a in \"$@\"; do\n\
               echo \"arg $a\" >> \"$REC\"\n\
               [ \"$prev\" = --rate-cap-hz ] && CAP=\"--rate-cap-hz $a\"\n\
               prev=$a\n\
             done\n\
             case \"${{N:-0}}\" in\n\
             0) exec '{FAKE}' --id f --script 'crash-after-ms 300' --record \"$REC\";;\n\
             1) ZETA_FAKE_MEASURED_CAP=20480000 exec '{FAKE}' --id f --script 'crash-after-ms 300' --record \"$REC\";;\n\
             *) exec '{FAKE}' --id f --script stream --record \"$REC\" $CAP;;\n\
             esac\n",
            rec.display()
        ),
    )
    .unwrap();
    let mut args = vec![wrapper.display().to_string()];
    args.extend(worker_args("f", "driver=fake", None));
    let cfg = WorkerConfig {
        id: SourceId("f".into()),
        program: PathBuf::from("/bin/sh"),
        args,
        timings: timings(),
        ring_bytes: 0,
    };
    let (port, _) = open(cfg, None).await;
    let attached = port.capabilities().clone();
    assert_eq!(attached.max_bandwidth_hz, Claim::Asserted(Hz(20_000_000)));
    assert!(
        wait_for(Duration::from_secs(10), || starts(&rec).len() >= 3
            && port.worker().is_some_and(|w| w.pid.is_some()))
        .await,
        "spawns: {:?}",
        starts(&rec)
    );
    assert_eq!(
        port.capabilities().max_bandwidth_hz,
        Claim::Measured(Hz(20_480_000))
    );
    // The rest of the record is unchanged.
    let mut want = attached;
    want.max_bandwidth_hz = Claim::Measured(Hz(20_480_000));
    assert_eq!(port.capabilities(), &want);
    let spawns = spawn_args(&rec);
    assert_eq!(caps_passed(&spawns[0]), 0, "{:?}", spawns[0]);
    assert_eq!(caps_passed(&spawns[1]), 0, "{:?}", spawns[1]);
    assert_eq!(caps_passed(&spawns[2]), 1, "{:?}", spawns[2]);
}

// The sample depths on the device's link, across the worker's pipe.

/// The fake's link: 16 bits measured at 20.48 MHz, 8 bits at 30.72 MHz.
const DEPTHS: &str = "ZETA_FAKE_DEPTHS=16:20480000,8:30720000";

fn depth(bits: u8, lossless: bool, cap: Claim<Hz>) -> Depth {
    Depth {
        bits,
        lossless,
        max_bandwidth_hz: cap,
    }
}

fn measured(hz: u64) -> Claim<Hz> {
    Claim::Measured(Hz(hz))
}

/// The `--depth-cap` values among a spawn's arguments, in order.
fn depth_caps(args: &[String]) -> Vec<String> {
    args.windows(2)
        .filter(|w| w[0] == "--depth-cap")
        .map(|w| w[1].clone())
        .collect()
}

#[tokio::test]
async fn set_depth_crosses_the_pipe_and_the_port_reports_it() {
    let dir = tempdir();
    let (cfg, _rec) = cfg_wrapped(
        dir.path(),
        &format!("{DEPTHS} exec \"$FAKE\" --script stream \"$@\""),
    );
    let (port, _) = open(cfg, None).await;
    let caps = port.capabilities().clone();
    assert_eq!(
        caps.depths,
        [
            depth(16, true, measured(20_480_000)),
            depth(8, false, measured(30_720_000)),
        ]
    );
    // The record's cap is the source's own depth's, and that depth is in force.
    assert_eq!(caps.max_bandwidth_hz, measured(20_480_000));
    assert_eq!(port.depth(), Some(depth(16, true, measured(20_480_000))));
    let wide = tuning(100_000_000, 25_000_000);
    match port.tune(wide).await {
        Err(PortError::OutOfRange {
            max: 20_480_000, ..
        }) => {}
        r => panic!("expected the 16-bit cap, got {r:?}"),
    }
    assert_eq!(
        port.set_depth(8).await,
        Ok(depth(8, false, measured(30_720_000)))
    );
    assert_eq!(port.depth(), Some(depth(8, false, measured(30_720_000))));
    assert_eq!(port.current(), None);
    // Bounded by the depth in force now: the worker's own device stops short
    // of 25 MHz and says so against the 8-bit cap.
    match port.tune(wide).await {
        Err(PortError::OutOfRange {
            max: 30_720_000, ..
        }) => {}
        r => panic!("expected the 8-bit cap, got {r:?}"),
    }
    let mut rx = port.subscribe();
    port.tune(tuning(100_000_000, 2_000_000)).await.unwrap();
    assert!(mock_samples(&next_block(&mut rx).await));
    // The status that follows keeps it.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(port.depth().map(|d| d.bits), Some(8));
    // The record is the run's: the depth changed, its figures did not.
    assert_eq!(port.capabilities(), &caps);
    match port.set_depth(12).await {
        Err(PortError::Unsupported(m)) => assert!(m.contains("16, 8"), "{m}"),
        r => panic!("expected unsupported, got {r:?}"),
    }
    assert_eq!(port.depth().map(|d| d.bits), Some(8));
}

#[tokio::test]
async fn cached_depth_caps_are_passed_to_the_respawn() {
    // A record cached from an earlier open: 16 bits measured, 8 bits not (no
    // rung was clean), so only the measured one is passed back.
    let mut cached = cached_caps();
    cached.max_bandwidth_hz = measured(20_480_000);
    cached.depths = vec![
        depth(16, true, measured(20_480_000)),
        depth(8, false, Claim::Asserted(Hz(20_000_000))),
    ];
    let args = worker_args("f", "driver=fake", Some(&cached));
    assert_eq!(
        args[5..],
        ["--rate-cap-hz", "20480000", "--depth-cap", "16:20480000"]
    );

    // Nothing cached: spawn 0 measures both and crashes; the respawn is told
    // both, beside the rate cap.
    let dir = tempdir();
    let (cfg, rec) = cfg_recording_args(dir.path(), DEPTHS, worker_args("f", "driver=fake", None));
    let spawns = two_spawns(cfg, &rec, None).await;
    assert!(depth_caps(&spawns[0]).is_empty(), "{:?}", spawns[0]);
    assert_eq!(
        depth_caps(&spawns[1]),
        ["16:20480000", "8:30720000"],
        "{:?}",
        spawns[1]
    );
    assert_eq!(caps_passed(&spawns[1]), 1, "{:?}", spawns[1]);
}

/// A record cached with one depth measured and one not: the node spawns the
/// worker with the measured one; the first hello measures the other; the
/// respawn is passed both, the configured one once and the new one beside it,
/// so it probes nothing.
#[tokio::test]
async fn a_respawn_merges_the_configured_depth_caps_with_those_measured_since() {
    let mut cached = cached_caps();
    cached.max_bandwidth_hz = measured(20_480_000);
    cached.depths = vec![
        depth(16, true, measured(20_480_000)),
        depth(8, false, Claim::Asserted(Hz(20_000_000))),
    ];
    let args = worker_args("f", "driver=fake", Some(&cached));
    let dir = tempdir();
    let (cfg, rec) = cfg_recording_args(dir.path(), DEPTHS, args);
    let spawns = two_spawns(cfg, &rec, Some(cached)).await;
    assert_eq!(depth_caps(&spawns[0]), ["16:20480000"], "{:?}", spawns[0]);
    assert_eq!(
        depth_caps(&spawns[1]),
        ["16:20480000", "8:30720000"],
        "{:?}",
        spawns[1]
    );
    assert_eq!(caps_passed(&spawns[1]), 1, "{:?}", spawns[1]);
}

/// Once a respawn has replaced the port's record (set once per run), a depth
/// the next respawn measures cannot enter it; the respawn after that is still
/// passed its cap, from the latest hello, and does not probe it again.
#[tokio::test]
async fn a_depth_measured_after_the_record_was_set_is_passed_to_the_next_respawn() {
    let dir = tempdir();
    let rec = dir.path().join("record");
    let wrapper = dir.path().join("wrapper.sh");
    let crash = format!("exec '{FAKE}' --id f --script 'crash-after-ms 300' --record \"$REC\"");
    std::fs::write(
        &wrapper,
        format!(
            "REC='{}'\nN=$(grep -c '^start' \"$REC\" 2>/dev/null || true)\n\
             for a in \"$@\"; do echo \"arg $a\" >> \"$REC\"; done\n\
             case \"${{N:-0}}\" in\n\
             0) ZETA_FAKE_DEPTHS=16:20480000,8 {crash};;\n\
             1) ZETA_FAKE_DEPTHS=16:20480000,8 ZETA_FAKE_USABLE=0.95 {crash};;\n\
             2) ZETA_FAKE_DEPTHS=16:20480000,8:30720000 ZETA_FAKE_USABLE=0.95 {crash};;\n\
             *) exec '{FAKE}' --id f --script stream --record \"$REC\";;\n\
             esac\n",
            rec.display()
        ),
    )
    .unwrap();
    let mut args = vec![wrapper.display().to_string()];
    args.extend(worker_args("f", "driver=fake", None));
    let cfg = WorkerConfig {
        id: SourceId("f".into()),
        program: PathBuf::from("/bin/sh"),
        args,
        timings: timings(),
        ring_bytes: 0,
    };
    let (port, _) = open(cfg, None).await;
    assert!(
        wait_for(Duration::from_secs(10), || starts(&rec).len() >= 4).await,
        "spawns: {:?}",
        starts(&rec)
    );
    // Spawn 1 replaced the record; spawn 2's 8-bit figure stayed out of it.
    assert_eq!(port.capabilities().usable_fraction, Claim::Asserted(0.95));
    assert_eq!(
        port.capabilities().depths[1],
        depth(8, false, Claim::Asserted(Hz(20_000_000)))
    );
    drop(port);
    let spawns = spawn_args(&rec);
    assert_eq!(depth_caps(&spawns[2]), ["16:20480000"], "{:?}", spawns[2]);
    assert_eq!(
        depth_caps(&spawns[3]),
        ["16:20480000", "8:30720000"],
        "{:?}",
        spawns[3]
    );
}

#[tokio::test]
async fn a_respawn_is_back_at_the_sources_own_depth() {
    let dir = tempdir();
    let (cfg, _rec) = cfg_wrapped(
        dir.path(),
        &format!(
            "{DEPTHS} ZETA_FAKE_CRASH_ONCE=1 exec \"$FAKE\" --script 'crash-after-ms 3000' \"$@\""
        ),
    );
    let (port, _) = open(cfg, None).await;
    port.set_depth(8).await.unwrap();
    assert_eq!(port.depth().map(|d| d.bits), Some(8));
    let healed = port.healed().unwrap();
    assert!(
        wait_for(Duration::from_secs(10), || *healed.borrow() > 0).await,
        "no heal: {:?}",
        port.fault()
    );
    assert!(
        wait_for(Duration::from_secs(5), || port.depth().map(|d| d.bits)
            == Some(16))
        .await,
        "{:?}",
        port.depth()
    );
    assert_eq!(port.depth(), Some(depth(16, true, measured(20_480_000))));
}

/// The record the port attached with stays for the node's run once a respawn
/// has replaced it (the capability record is set once per run), so a later
/// respawn that measured a depth the record holds only as asserted cannot
/// put its figure there. The figure still governs: the depth reports it, and
/// it is the record the node caches for its next start.
#[tokio::test]
async fn a_respawns_measured_depth_cap_reaches_the_port_when_its_record_is_kept() {
    let dir = tempdir();
    let (cfg, _rec) = cfg_wrapped(
        dir.path(),
        "case $N in\n\
         0) ZETA_FAKE_DEPTHS=16:20480000,8 exec \"$FAKE\" --script 'crash-after-ms 300' \"$@\";;\n\
         1) ZETA_FAKE_DEPTHS=16:20480000,8 ZETA_FAKE_USABLE=0.95 exec \"$FAKE\" --script 'crash-after-ms 300' \"$@\";;\n\
         *) ZETA_FAKE_DEPTHS=16:20480000,8:30720000 ZETA_FAKE_USABLE=0.95 exec \"$FAKE\" --script stream \"$@\";;\n\
         esac",
    );
    let (port, _) = open(cfg, None).await;
    let asserted = Claim::Asserted(Hz(20_000_000));
    assert_eq!(
        port.capabilities().depths[1],
        depth(8, false, asserted.clone())
    );
    // Spawn 1 replaced the record (its usable fraction), spawn 2 streams.
    assert!(
        wait_for(Duration::from_secs(10), || port.worker().unwrap().restarts
            >= 2
            && port.worker().unwrap().pid.is_some()
            && port.capabilities().usable_fraction == Claim::Asserted(0.95))
        .await,
        "{:?} {:?}",
        port.worker(),
        port.capabilities()
    );
    assert_eq!(port.capabilities().depths[1], depth(8, false, asserted));
    let cache = port.record_to_cache();
    assert_eq!(cache.depths[1], depth(8, false, measured(30_720_000)));
    assert_eq!(cache.usable_fraction, Claim::Asserted(0.95));
    assert_eq!(
        port.set_depth(8).await,
        Ok(depth(8, false, measured(30_720_000)))
    );
    assert_eq!(port.depth(), Some(depth(8, false, measured(30_720_000))));
}

#[test]
fn the_hello_deadline_covers_a_three_depth_probe() {
    use zeta_source_soapy::probe::{PROBE_DWELL, PROBE_MAX_RUNGS, PROBE_RETRIES, PROBE_SETTLE};
    let rungs = PROBE_MAX_RUNGS as u32 * (1 + PROBE_RETRIES);
    let probe = (PROBE_SETTLE + PROBE_DWELL) * rungs;
    assert!(
        HELLO_TIMEOUT >= 3 * probe + Duration::from_secs(30),
        "{HELLO_TIMEOUT:?} for three probes of {probe:?}"
    );
    assert_eq!(Timings::default().hello, HELLO_TIMEOUT);
}
