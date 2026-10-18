//! KiwiSDR source against the scripted fake Kiwi on loopback.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use zeta_core::job::JobState;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpError, OpResult};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{Backend, StageRegistry};
use zeta_core::stream::Payload;
use zeta_core::types::{Hz, SourceId, StreamKind};
use zeta_source_kiwi::testing::{FakeKiwi, FakeReply, FakeScript};
use zeta_source_kiwi::{KiwiConfig, KiwiSource};

const PASSWORD: &str = "hunter2";

fn tuning(center: u64, bw: u64) -> Tuning {
    Tuning {
        center_hz: Hz(center),
        bandwidth_hz: Hz(bw),
    }
}

fn config(fake: &FakeKiwi, delays: &[u64]) -> KiwiConfig {
    let mut cfg = KiwiConfig::new("127.0.0.1", fake.addr().port(), None).unwrap();
    cfg.reconnect_delays_ms = delays.to_vec();
    cfg.read_timeout_ms = 2_000;
    cfg.handshake_timeout_ms = 2_000;
    cfg
}

fn source(fake: &FakeKiwi, delays: &[u64]) -> Arc<KiwiSource> {
    Arc::new(KiwiSource::new(
        SourceId("kiwi0".into()),
        config(fake, delays),
    ))
}

async fn fake(on_auth: Vec<FakeReply>, frames: Option<usize>) -> FakeKiwi {
    FakeKiwi::start(FakeScript {
        on_auth,
        frames_per_connection: frames,
        frame_interval_ms: 5,
        listen_after_drop: true,
    })
    .await
}

async fn wait_until(ms: u64, mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..ms / 10 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    f()
}

async fn next_block(rx: &mut IqReceiver) -> IqBlock {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match rx.recv().await {
                Ok(b) => return b,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => panic!("kiwi stream closed"),
            }
        }
    })
    .await
    .expect("a block within 3 s")
}

fn not_keepalive(cmds: Vec<String>) -> Vec<String> {
    cmds.into_iter().filter(|c| c != "SET keepalive").collect()
}

async fn node_with(src: Arc<KiwiSource>) -> (Node, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = StageRegistry::new();
    zeta_dsp::register_all(&mut reg);
    let cfg = NodeConfig {
        state_dir: dir.path().join("state"),
        ring_seconds: 2.0,
        backends: vec![Backend::Cpu],
        resume: true,
    };
    let port: Arc<dyn Port> = src;
    (Node::open(cfg, reg, vec![port]).await.unwrap(), dir)
}

fn measure(center: u64, bw: u64) -> Op {
    Op::JobStart {
        kind: JobKind::Measure,
        source: SourceId("kiwi0".into()),
        center_hz: Hz(center),
        bandwidth_hz: Hz(bw),
        deviation_hz: None,
    }
}

async fn wait_failed(node: &Node, ms: u64) -> String {
    let mut state = JobState::Running;
    wait_until(ms, || {
        state = node.job_list()[0].state.clone();
        state != JobState::Running
    })
    .await;
    match state {
        JobState::Failed { reason } => reason,
        s => panic!("expected Failed, got {s:?}"),
    }
}

#[tokio::test]
async fn tune_handshakes_and_sends_exact_commands() {
    let fake = fake(vec![FakeReply::Ok], None).await;
    let src = source(&fake, &[10]);
    let _rx = src.subscribe();
    assert_eq!(src.current(), None);
    let got = src.tune(tuning(7_100_000, 10_000)).await.unwrap();
    assert_eq!(got, tuning(7_100_000, 12_000));
    assert_eq!(src.current(), Some(got));
    assert_eq!(src.audio_rate_hz(), Some(12_000));
    assert_eq!(src.reported_sample_rate(), Some(11998.902088));
    let want = [
        "SET auth t=kiwi p=",
        "SET squelch=0 max=0",
        "SET genattn=0",
        "SET gen=0 mix=-1",
        "SET ident_user=zetasdr",
        "SET mod=iq low_cut=-5000 high_cut=5000 freq=7100.000",
        "SET agc=1 hang=0 thresh=-100 slope=6 decay=1000 manGain=50",
        "SET compression=0",
        "SET keepalive",
        "SET AR OK in=12000 out=44100",
    ];
    assert!(wait_until(1_000, || fake.commands().len() >= want.len()).await);
    assert_eq!(fake.commands()[..want.len()], want);
    let path = &fake.paths()[0];
    let ts = path
        .strip_prefix('/')
        .and_then(|p| p.strip_suffix("/SND"))
        .unwrap_or_else(|| panic!("path {path}"));
    assert!(
        !ts.is_empty() && ts.bytes().all(|b| b.is_ascii_digit()),
        "{path}"
    );

    let before = not_keepalive(fake.commands());
    assert_eq!(src.tune(tuning(7_100_000, 10_000)).await.unwrap(), got);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        not_keepalive(fake.commands()),
        before,
        "unchanged band sends nothing"
    );
    assert_eq!(fake.connections(), 1);

    // A new band on the live session: the three tuning commands only.
    src.tune(tuning(7_100_250, 2_000)).await.unwrap();
    let want_retune = [
        "SET mod=iq low_cut=-1000 high_cut=1000 freq=7100.250",
        "SET agc=1 hang=0 thresh=-100 slope=6 decay=1000 manGain=50",
        "SET compression=0",
    ];
    assert!(
        wait_until(1_000, || not_keepalive(fake.commands()).len()
            == before.len() + 3)
        .await
    );
    assert_eq!(not_keepalive(fake.commands())[before.len()..], want_retune);
    assert_eq!(fake.connections(), 1);
}

#[tokio::test]
async fn blocks_carry_audio_rate_and_contiguous_t0() {
    let fake = fake(vec![FakeReply::Ok], None).await;
    let src = source(&fake, &[10]);
    let mut rx = src.subscribe();
    src.tune(tuning(7_100_000, 10_000)).await.unwrap();
    let mut blocks = Vec::new();
    for _ in 0..5 {
        blocks.push(next_block(&mut rx).await);
    }
    for b in &blocks {
        assert_eq!(b.sample_rate_hz, Hz(12_000));
        assert_eq!(b.center_hz, Hz(7_100_000));
        assert_eq!(b.samples.len(), 512);
    }
    for w in blocks.windows(2) {
        let d = w[1].t0.0 - w[0].t0.0;
        assert!(d == 42_666_666 || d == 42_666_667, "t0 step {d}");
    }
    assert_eq!(src.rssi_dbm().map(|r| (r * 10.0).round()), Some(-706.0));
    assert_eq!(src.seq_gaps(), 0);
    assert_eq!(src.adc_overflows(), 0);
}

#[tokio::test]
async fn keepalive_once_per_second() {
    let fake = fake(vec![FakeReply::Ok], None).await;
    let src = source(&fake, &[10]);
    let _rx = src.subscribe();
    src.tune(tuning(7_100_000, 10_000)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    let n = fake
        .commands()
        .iter()
        .filter(|c| *c == "SET keepalive")
        .count();
    assert!(
        (3..=4).contains(&n),
        "{n} keepalives: the first plus 2 or 3"
    );
}

#[tokio::test]
async fn too_busy_at_tune_is_port_error_naming_slots() {
    let fake = fake(vec![FakeReply::TooBusy(8)], None).await;
    let src = source(&fake, &[10]);
    let Err(PortError::Io(m)) = src.tune(tuning(7_100_000, 10_000)).await else {
        panic!("expected Io");
    };
    let port = fake.addr().port();
    assert!(
        m.contains(&format!(
            "kiwi 127.0.0.1:{port}: too_busy: all 8 client slots taken"
        )),
        "{m}"
    );
    assert!(src.fault().unwrap().contains("too_busy"));
    assert_eq!(src.current(), None);

    let (node, _dir) = node_with(src.clone()).await;
    let Err(OpError::Other { message }) = node.apply(measure(7_100_000, 10_000)).await else {
        panic!("expected Other");
    };
    assert!(message.contains("too_busy"), "{message}");
    assert!(node.job_list().is_empty());
}

#[tokio::test]
async fn too_busy_on_reconnect_fails_job() {
    let fake = fake(vec![FakeReply::Ok, FakeReply::TooBusy(8)], Some(20)).await;
    let src = source(&fake, &[10]);
    let (node, _dir) = node_with(src.clone()).await;
    node.apply(measure(7_100_000, 10_000)).await.unwrap();
    let reason = wait_failed(&node, 3_000).await;
    assert!(reason.contains("source kiwi0: too_busy"), "{reason}");
}

/// Ruling R5: a terminal failure sets the fault before the session lets go.
/// The broadcast stays open (the node's ring feeder subscribed once, at
/// attach), so a retune that clears the fault resumes the same receivers.
#[tokio::test]
async fn terminal_fault_is_set_before_session_ends_and_retune_recovers() {
    let fake = fake(
        vec![FakeReply::Ok, FakeReply::TooBusy(8), FakeReply::Ok],
        Some(20),
    )
    .await;
    let src = source(&fake, &[10]);
    let mut rx = src.subscribe();
    src.tune(tuning(7_100_000, 10_000)).await.unwrap();
    let mut n = 0;
    while let Ok(Ok(_)) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
        n += 1;
    }
    assert_eq!(n, 20);
    assert!(
        src.fault().unwrap().contains("too_busy: all 8"),
        "{:?}",
        src.fault()
    );
    assert!(
        matches!(rx.try_recv(), Err(TryRecvError::Empty)),
        "not closed"
    );
    // Same band: a faulted port re-checks the device instead of short-circuiting.
    src.tune(tuning(7_100_000, 10_000)).await.unwrap();
    assert_eq!(src.fault(), None);
    next_block(&mut rx).await;
    assert_eq!(fake.connections(), 3);
}

#[tokio::test]
async fn drop_mid_stream_reconnects_and_t0_stays_monotonic() {
    // 30 frames (1.28 s) per connection: each one streams long enough to
    // reset the reconnect budget, and the second is still up at block 40.
    let fake = fake(vec![FakeReply::Ok, FakeReply::Ok], Some(30)).await;
    let src = source(&fake, &[10, 10]);
    let mut rx = src.subscribe();
    src.tune(tuning(7_100_000, 10_000)).await.unwrap();
    let mut blocks = Vec::new();
    for _ in 0..40 {
        blocks.push(next_block(&mut rx).await);
    }
    assert!(fake.connections() >= 2, "{}", fake.connections());
    let mod_cmd = "SET mod=iq low_cut=-5000 high_cut=5000 freq=7100.000";
    let count = |p: &dyn Fn(&String) -> bool| fake.commands().iter().filter(|c| p(c)).count();
    assert!(count(&|c| c == mod_cmd) >= 2);
    assert!(count(&|c| c.starts_with("SET auth")) >= 2);
    for w in blocks.windows(2) {
        assert!(w[1].t0.0 > w[0].t0.0, "t0 strictly increasing");
    }
    let end29 = blocks[29].t0.0 + 512 * 1_000_000_000 / 12_000;
    assert!(
        blocks[30].t0.0 >= end29,
        "first block after reconnect starts after the last"
    );
    assert_eq!(src.fault(), None);
}

/// A reconnected Kiwi that never sends `audio_rate` uses up an attempt per
/// `handshake_timeout_ms` instead of holding the session forever.
#[tokio::test]
async fn reconnect_handshake_is_bounded() {
    let fake = fake(vec![FakeReply::Ok, FakeReply::NoAudioRate], Some(20)).await;
    let mut cfg = config(&fake, &[10, 10]);
    cfg.handshake_timeout_ms = 300;
    let src = Arc::new(KiwiSource::new(SourceId("kiwi0".into()), cfg));
    let (node, _dir) = node_with(src.clone()).await;
    node.apply(measure(7_100_000, 10_000)).await.unwrap();
    let reason = wait_failed(&node, 3_000).await;
    assert!(
        reason.contains("connection lost (no sample_rate and audio_rate within 300 ms)"),
        "{reason}"
    );
    assert!(reason.contains("2 reconnect attempts failed"), "{reason}");
    assert_eq!(fake.connections(), 3);
}

/// A Kiwi that handshakes and drops before a second of samples never resets
/// the reconnect budget, so the session gives up.
#[tokio::test]
async fn short_lived_connections_do_not_reset_the_budget() {
    let fake = fake(vec![FakeReply::Ok], Some(5)).await;
    let src = source(&fake, &[10, 10]);
    let _rx = src.subscribe();
    src.tune(tuning(7_100_000, 10_000)).await.unwrap();
    assert!(wait_until(3_000, || src.fault().is_some()).await);
    let fault = src.fault().unwrap();
    assert!(fault.contains("2 reconnect attempts failed"), "{fault}");
    assert_eq!(fake.connections(), 3);
}

#[tokio::test]
async fn reconnect_exhausted_fails_job_with_reason() {
    let fake = FakeKiwi::start(FakeScript {
        on_auth: vec![FakeReply::Ok],
        frames_per_connection: Some(20),
        frame_interval_ms: 5,
        listen_after_drop: false,
    })
    .await;
    let src = source(&fake, &[10, 10, 10]);
    let (node, _dir) = node_with(src.clone()).await;
    node.apply(measure(7_100_000, 10_000)).await.unwrap();
    let reason = wait_failed(&node, 3_000).await;
    assert!(reason.contains("connection lost"), "{reason}");
    assert!(reason.contains("3 reconnect attempts failed"), "{reason}");
}

#[tokio::test]
async fn bad_password_and_down_are_named() {
    for (reply, want) in [
        (FakeReply::BadPassword(1), "badp=1: bad password"),
        (FakeReply::BadPassword(5), "badp=5: no multiple connections"),
        (FakeReply::Down, "down: server is down"),
    ] {
        let fake = fake(vec![reply], None).await;
        let cfg = KiwiConfig::new("127.0.0.1", fake.addr().port(), Some(PASSWORD.into())).unwrap();
        let debug = format!("{cfg:?}");
        assert!(debug.contains("password: <redacted>"), "{debug}");
        let src = KiwiSource::new(SourceId("kiwi0".into()), cfg);
        let Err(e) = src.tune(tuning(7_100_000, 10_000)).await else {
            panic!("expected an error");
        };
        let fault = src.fault().unwrap();
        for text in [e.to_string(), format!("{e:?}"), fault.clone(), debug] {
            assert!(!text.contains(PASSWORD), "password leaked: {text}");
        }
        assert!(e.to_string().contains(want), "{e}");
        assert!(fault.contains(want), "{fault}");
        assert_eq!(
            fake.commands()[0],
            format!("SET auth t=kiwi p={PASSWORD}"),
            "the password goes only to the Kiwi"
        );
    }
}

#[tokio::test]
async fn bad_password_characters_are_refused() {
    for pw in ["two words", "a=b", "tab\there", "nl\n", ""] {
        let r = KiwiConfig::new("kiwi.local", 8073, Some(pw.into()));
        if pw.is_empty() {
            assert!(r.is_ok());
            continue;
        }
        let e = r.unwrap_err();
        assert!(!e.contains(pw), "{e}");
    }
    let cfg = KiwiConfig::new("kiwi.local", 8073, None).unwrap();
    assert_eq!(cfg.reconnect_delays_ms, [1000, 2000, 4000, 8000, 16000]);
    assert_eq!(
        (cfg.read_timeout_ms, cfg.handshake_timeout_ms),
        (10_000, 10_000)
    );
}

#[tokio::test]
async fn job_start_wider_than_kiwi_is_out_of_range() {
    let fake = fake(vec![FakeReply::Ok], None).await;
    let src = source(&fake, &[10]);
    let caps = src.capabilities().clone();
    assert_eq!(*caps.max_bandwidth_hz.value(), Hz(12_000));
    assert_eq!(*caps.tune_max_hz.value(), Hz(30_000_000));
    assert!(caps.remote && caps.push && caps.rx && !caps.tx);
    let (node, _dir) = node_with(src.clone()).await;
    let Err(OpError::OutOfRange { field, max, .. }) = node.apply(measure(7_100_000, 20_000)).await
    else {
        panic!("expected OutOfRange");
    };
    assert_eq!((field.as_str(), max), ("bandwidth_hz", 12_000));
    let Err(OpError::OutOfRange { field, .. }) = node.apply(measure(31_000_000, 10_000)).await
    else {
        panic!("expected OutOfRange");
    };
    assert_eq!(field, "center_hz");
    // The port's own checks, for callers that bypass the node.
    for (t, f) in [
        (tuning(7_100_000, 0), "bandwidth_hz"),
        (tuning(7_100_000, 12_001), "bandwidth_hz"),
        (tuning(30_000_001, 10_000), "center_hz"),
    ] {
        let Err(PortError::OutOfRange { field, .. }) = src.tune(t).await else {
            panic!("expected OutOfRange for {t:?}");
        };
        assert_eq!(field, f);
    }
    assert_eq!(fake.connections(), 0);
}

#[tokio::test]
async fn measure_job_runs_on_fake_kiwi() {
    let fake = fake(vec![FakeReply::Ok], None).await;
    let src = source(&fake, &[10]);
    let (node, _dir) = node_with(src.clone()).await;
    let OpResult::Job(id) = node.apply(measure(7_100_000, 10_000)).await.unwrap() else {
        panic!("expected a job");
    };
    let bus = node.bus();
    let stream = wait_until(1_000, || {
        bus.list()
            .iter()
            .any(|s| s.provenance.job.as_ref() == Some(&id) && s.kind == StreamKind::Event)
    })
    .await
    .then(|| {
        bus.list()
            .into_iter()
            .find(|s| s.provenance.job.as_ref() == Some(&id) && s.kind == StreamKind::Event)
            .unwrap()
    })
    .expect("event stream");
    let mut rx = bus.subscribe(&stream.id).unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match rx.recv().await {
                Ok(f) => {
                    let Payload::Event(v) = f.payload.clone() else {
                        panic!("expected an event")
                    };
                    break v;
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(e) => panic!("event stream ended: {e}"),
            }
        }
    })
    .await
    .expect("a measure event within 3 s");
    assert!(ev["snr_db"].as_f64().unwrap().is_finite(), "{ev}");
}

#[tokio::test]
async fn dropping_the_source_or_its_receivers_closes_the_socket() {
    let fake = fake(vec![FakeReply::Ok], None).await;
    let src = source(&fake, &[10]);
    let rx = src.subscribe();
    src.tune(tuning(7_100_000, 10_000)).await.unwrap();
    drop(rx);
    assert!(
        wait_until(1_000, || fake.clean_closes() == 1).await,
        "no receivers"
    );

    let rx = src.subscribe();
    src.tune(tuning(7_100_000, 10_000)).await.unwrap();
    assert_eq!(fake.connections(), 2, "a dead session is reopened");
    drop(src);
    assert!(
        wait_until(1_000, || fake.clean_closes() == 2).await,
        "source dropped"
    );
    drop(rx);
}
