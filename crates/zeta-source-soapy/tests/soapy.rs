//! SoapySDR source against the scripted mock device.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast::error::RecvError;
use zeta_core::job::JobState;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Claim, Hz, SourceId};
use zeta_source_soapy::SoapySource;
use zeta_source_soapy::mock::{MockDevice, MockRead};
use zeta_source_soapy::source::{MAX_AHEAD_NS, MAX_OVERFLOW_RUN, MAX_RESTARTS};

fn tuning(center: u64, bw: u64) -> Tuning {
    Tuning {
        center_hz: Hz(center),
        bandwidth_hz: Hz(bw),
    }
}

fn source(dev: &Arc<MockDevice>) -> Arc<SoapySource> {
    Arc::new(SoapySource::with_device(SourceId("sdr0".into()), dev.clone()).unwrap())
}

fn samples(n: usize) -> Vec<MockRead> {
    vec![MockRead::Samples(16_384); n]
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
                Err(RecvError::Closed) => panic!("soapy stream closed"),
            }
        }
    })
    .await
    .expect("a block within 3 s")
}

/// The reader thread's last act is `deactivate`; wait up to 500 ms for it.
async fn assert_deactivated(dev: &MockDevice) {
    let done = wait_until(500, || {
        dev.calls()
            .iter()
            .rfind(|c| *c != "drop")
            .map(String::as_str)
            == Some("deactivate")
    })
    .await;
    assert!(done, "{:?}", dev.calls());
}

/// The reader thread's last act on a lost device is `abandon`; wait for it.
async fn assert_abandoned(dev: &MockDevice) {
    let done = wait_until(500, || {
        dev.calls()
            .iter()
            .rfind(|c| *c != "drop")
            .map(String::as_str)
            == Some("abandon")
    })
    .await;
    assert!(done, "{:?}", dev.calls());
}

fn end_ns(b: &IqBlock) -> u64 {
    b.t0.0 + (b.samples.len() as u128 * 1_000_000_000 / u128::from(b.sample_rate_hz.0)) as u64
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

/// The device calls so far, without the `drop` a dropped stream may record.
fn calls_without_drops(dev: &MockDevice) -> Vec<String> {
    dev.calls().into_iter().filter(|c| c != "drop").collect()
}

fn open_rx_count(dev: &MockDevice) -> usize {
    dev.calls()
        .iter()
        .filter(|c| c.starts_with("open_rx"))
        .count()
}

#[tokio::test]
async fn tune_sets_frequency_and_smallest_rate() {
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(samples(2)));
    let src = source(&dev);
    let _rx = src.subscribe();
    let got = src.tune(tuning(100_000_000, 12_500)).await.unwrap();
    assert_eq!(got, tuning(100_000_000, 1_000_000));
    assert_eq!(src.current(), Some(got));
    let opened = [
        "set_sample_rate(1000000)",
        "set_frequency(100000000)",
        "open_rx(Cs8)",
        "activate",
    ];
    assert_eq!(dev.calls(), opened);
    assert_eq!(src.tune(tuning(100_000_000, 12_500)).await.unwrap(), got);
    assert_eq!(dev.calls(), opened, "an unchanged band touches nothing");
}

#[tokio::test]
async fn tune_applies_ppm_to_the_hardware_and_reports_the_requested_centre() {
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(samples(2)));
    let src = Arc::new(
        SoapySource::with_device(SourceId("sdr0".into()), dev.clone())
            .unwrap()
            .with_ppm(3.4),
    );
    let mut rx = src.subscribe();
    let got = src.tune(tuning(146_975_000, 12_500)).await.unwrap();
    assert_eq!(got.center_hz, Hz(146_975_000));
    assert!(
        dev.calls()
            .contains(&"set_frequency(146975500)".to_string())
    );
    assert_eq!(src.current().unwrap().center_hz, Hz(146_975_000));
    let b = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(b.center_hz, Hz(146_975_000));
    // A live retune applies the same correction.
    src.tune(tuning(100_000_000, 12_500)).await.unwrap();
    assert!(
        dev.calls()
            .contains(&"set_frequency(100000340)".to_string())
    );
    assert_eq!(src.current().unwrap().center_hz, Hz(100_000_000));
}

#[tokio::test]
async fn tune_out_of_range_names_field() {
    let dev = Arc::new(MockDevice::hackrf_like());
    let src = source(&dev);
    let Err(PortError::OutOfRange { field, value, .. }) =
        src.tune(tuning(500_000, 1_000_000)).await
    else {
        panic!("expected OutOfRange")
    };
    assert_eq!((field, value), ("center_hz", 500_000));
    let Err(PortError::OutOfRange { field, max, .. }) =
        src.tune(tuning(100_000_000, 25_000_000)).await
    else {
        panic!("expected OutOfRange")
    };
    assert_eq!((field, max), ("bandwidth_hz", 20_000_000));
    assert!(dev.calls().is_empty(), "{:?}", dev.calls());
}

#[tokio::test]
async fn device_failure_names_the_call() {
    let dev = Arc::new(MockDevice::hackrf_like().fail_on("set_frequency", "PLL unlocked"));
    let src = source(&dev);
    let Err(PortError::Io(m)) = src.tune(tuning(100_000_000, 1_000_000)).await else {
        panic!("expected Io")
    };
    assert!(
        m.contains("set_frequency") && m.contains("PLL unlocked"),
        "{m}"
    );
    assert_eq!(src.current(), None);
}

#[tokio::test]
async fn blocks_have_contiguous_t0_from_sample_count() {
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(samples(3)));
    let src = source(&dev);
    let mut rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    let mut blocks = Vec::new();
    for _ in 0..3 {
        blocks.push(next_block(&mut rx).await);
    }
    for w in blocks.windows(2) {
        assert_eq!(w[1].t0.0 - w[0].t0.0, 16_384_000);
    }
    for b in &blocks {
        assert_eq!(b.sample_rate_hz, Hz(1_000_000));
        assert_eq!(b.center_hz, Hz(100_000_000));
        assert_eq!(b.samples.len(), 16_384);
    }
}

#[tokio::test]
async fn overflow_reanchors_without_going_backwards() {
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(vec![
        MockRead::Samples(16_384),
        MockRead::Overflow,
        MockRead::Samples(16_384),
    ]));
    let src = source(&dev);
    let mut rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    let a = next_block(&mut rx).await;
    let b = next_block(&mut rx).await;
    assert!(b.t0.0 >= end_ns(&a), "{} < {}", b.t0.0, end_ns(&a));
    assert_eq!(src.overflows(), 1);
}

#[tokio::test]
async fn retune_rate_change_reopens_the_stream() {
    // Timeouts keep the reader alive (but not faulted) while the test retunes.
    let mut reads = samples(1);
    reads.extend([MockRead::Timeout, MockRead::Timeout]);
    reads.extend(samples(2));
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(reads));
    let src = source(&dev);
    let mut rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    let first = next_block(&mut rx).await;
    assert_eq!(first.sample_rate_hz, Hz(1_000_000));
    let got = src.tune(tuning(101_000_000, 2_000_000)).await.unwrap();
    assert_eq!(got, tuning(101_000_000, 2_000_000));
    let calls = calls_without_drops(&dev);
    assert_eq!(
        calls[calls.len() - 4..],
        [
            "set_sample_rate(2000000)",
            "set_frequency(101000000)",
            "open_rx(Cs8)",
            "activate"
        ]
    );
    assert!(!calls.contains(&"deactivate".to_string()), "{calls:?}");
    let next = next_block(&mut rx).await;
    assert_eq!(next.sample_rate_hz, Hz(2_000_000));
    assert_eq!(next.center_hz, Hz(101_000_000));
    assert!(next.t0.0 >= end_ns(&first));

    // Centre only: one set_frequency, no stream restart.
    src.tune(tuning(102_000_000, 2_000_000)).await.unwrap();
    let calls = calls_without_drops(&dev);
    assert_eq!(calls.last().unwrap(), "set_frequency(102000000)");
    assert_eq!(calls.iter().filter(|c| *c == "activate").count(), 2);
    assert_eq!(open_rx_count(&dev), 2);
}

#[tokio::test]
async fn fast_blocks_restart_the_stream_and_stay_near_the_clock() {
    // 1 MHz, block_len 16 384: 150 instant blocks = 2.46 s of samples in ~0 s.
    // Then 120 Slow(16384, 20) blocks: about real time, while the clock catches up.
    let mut reads = samples(150);
    reads.extend(std::iter::repeat_n(MockRead::Slow(16_384, 20), 120));
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(reads));
    let src = source(&dev);
    let mut rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    let mut blocks = Vec::new();
    // When each block arrived: the catch-up shows as a pause of about 2 s.
    let mut arrived = Vec::new();
    // The burst may outrun the broadcast; lagged blocks were sent all the same.
    let mut lagged = 0u64;
    // The catch-up after the restart sends nothing for about 2 s.
    loop {
        match tokio::time::timeout(Duration::from_millis(3000), rx.recv()).await {
            Ok(Ok(b)) => {
                let now = now_ns();
                assert!(
                    end_ns(&b) <= now + MAX_AHEAD_NS + 100_000_000,
                    "block {} ms ahead",
                    (end_ns(&b) - now) / 1_000_000
                );
                assert_eq!(src.fault(), None);
                blocks.push(b);
                arrived.push(std::time::Instant::now());
            }
            Ok(Err(RecvError::Lagged(n))) => lagged += n,
            _ => break,
        }
    }
    // The burst stopped at the guard (about 122 blocks, 2 s of samples); the
    // slow blocks after the catch-up came through.
    let pause = arrived
        .windows(2)
        .position(|w| w[1].duration_since(w[0]) > Duration::from_secs(1))
        .expect("a catch-up pause");
    let burst = pause as u64 + 1 + lagged;
    assert!(
        (100..150).contains(&burst),
        "{burst} burst blocks ({lagged} lagged)"
    );
    assert!(blocks.len() > pause + 1, "no block after the restart");
    assert_eq!(open_rx_count(&dev), 2);
    assert_eq!(src.stats().unwrap().restarts, 1);
    // Monotonic across the restart.
    assert!(blocks.windows(2).all(|w| w[1].t0.0 >= end_ns(&w[0])));
}

#[tokio::test]
async fn slow_reads_never_restart_the_stream() {
    // 40 blocks of 16 384 at 1 MHz delivered every 30 ms: behind the clock, never ahead.
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(vec![MockRead::Slow(16_384, 30); 40]));
    let src = source(&dev);
    let mut rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    for _ in 0..40 {
        next_block(&mut rx).await;
    }
    assert_eq!(src.stats().unwrap().restarts, 0);
    assert_eq!(open_rx_count(&dev), 1);
}

#[tokio::test]
async fn too_many_restarts_fault_the_source() {
    // Reads of 16.4 ms of samples every ~1 ms run ahead of the clock again
    // after every catch-up (about 2 s of discarded reads), so the guard trips
    // once per ~2.1 s until the restarts in the window fault the source.
    let dev =
        Arc::new(MockDevice::hackrf_like().with_reads(vec![MockRead::Slow(16_384, 1); 30_000]));
    let src = source(&dev);
    let _rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    assert!(
        wait_until(20_000, || src.fault().is_some()).await,
        "no fault"
    );
    let fault = src.fault().unwrap();
    assert!(fault.contains("faster than"), "{fault}");
    assert!(src.stats().unwrap().restarts > MAX_RESTARTS as u64);
}

#[tokio::test]
async fn retune_failure_names_the_call_and_faults() {
    let mut reads = samples(1);
    reads.extend([MockRead::Timeout, MockRead::Timeout]);
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(reads));
    let src = source(&dev);
    let mut rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    next_block(&mut rx).await;
    dev.fail_next("set_sample_rate", "rate rejected");
    let Err(PortError::Io(m)) = src.tune(tuning(100_000_000, 2_000_000)).await else {
        panic!("expected Io")
    };
    assert!(
        m.contains("set_sample_rate(2000000)") && m.contains("rate rejected"),
        "{m}"
    );
    assert!(src.fault().unwrap().contains("rate rejected"));
    // A failed device call in a retune is a lost device: abandon, not deactivate.
    assert_abandoned(&dev).await;
    assert!(!dev.calls().contains(&"deactivate".to_string()));
}

#[tokio::test]
async fn device_loss_during_a_retune_abandons_the_stream() {
    let mut reads = samples(1);
    reads.extend([MockRead::Timeout, MockRead::Timeout]);
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(reads));
    let src = source(&dev);
    let mut rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    next_block(&mut rx).await;
    dev.fail_next("set_sample_rate", "LIBUSB_ERROR_NO_DEVICE");
    let e = src.tune(tuning(100_000_000, 2_000_000)).await.unwrap_err();
    assert!(e.to_string().contains("LIBUSB_ERROR_NO_DEVICE"), "{e}");
    let fault = src.fault().unwrap();
    assert!(fault.contains("set_sample_rate"), "{fault}");
    assert_abandoned(&dev).await;
    assert!(!dev.calls().contains(&"deactivate".to_string()));
    let calls = dev.calls().len();
    let e = src.tune(tuning(100_000_000, 1_000_000)).await.unwrap_err();
    assert!(e.to_string().contains("device lost"), "{e}");
    assert_eq!(dev.calls().len(), calls, "no device call after the loss");
}

#[tokio::test]
async fn vanished_device_faults_source_fails_jobs_and_is_never_touched_again() {
    let dev = Arc::new(MockDevice::hackrf_like());
    let src = source(&dev);
    let (node, _dir) = node_with(src.clone()).await;
    node.apply(passthrough()).await.unwrap();
    let mut reads = samples(3);
    reads.push(MockRead::Gone("LIBUSB_ERROR_NO_DEVICE".into()));
    dev.push_reads(reads);
    let reason = wait_failed(&node, 1_000).await;
    assert!(
        reason.starts_with("source sdr0: read failed") && reason.contains("NO_DEVICE"),
        "{reason}"
    );
    assert_abandoned(&dev).await;
    assert!(!dev.calls().contains(&"deactivate".to_string()));
    assert!(
        dev.calls_after_gone().is_empty(),
        "{:?}",
        dev.calls_after_gone()
    );
    let e = src.tune(tuning(100_000_000, 1_000_000)).await.unwrap_err();
    assert!(e.to_string().contains("device lost"), "{e}");
    let e = src.set_gain(Some(10.0)).await.unwrap_err();
    assert!(e.to_string().contains("device lost"), "{e}");
    assert!(src.fault().unwrap().contains("NO_DEVICE"));
    assert!(dev.calls_after_gone().is_empty());
}

#[tokio::test]
async fn an_overflow_storm_faults_the_source() {
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(vec![MockRead::Overflow; 1_000]));
    let src = source(&dev);
    let _rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    assert!(
        wait_until(2_000, || src
            .fault()
            .is_some_and(|f| f.contains("overflow storm")))
        .await
    );
    assert_eq!(src.stats().unwrap().overflows, u64::from(MAX_OVERFLOW_RUN));
    assert_abandoned(&dev).await;
    let e = src.tune(tuning(100_000_000, 1_000_000)).await.unwrap_err();
    assert!(e.to_string().contains("device lost"), "{e}");
}

#[tokio::test]
async fn interleaved_overflows_never_fault() {
    let mut reads = Vec::new();
    for _ in 0..300 {
        // Paced at the 1 MHz rate so the clock guard never trips.
        reads.extend([MockRead::Overflow, MockRead::Slow(16_384, 20)]);
    }
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(reads));
    let src = source(&dev);
    let mut rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    for _ in 0..300 {
        next_block(&mut rx).await;
    }
    assert_eq!(src.stats().unwrap().overflows, 300);
    assert_eq!(src.fault(), None);
}

#[tokio::test]
async fn retune_timeout_faults_and_next_tune_reopens() {
    let mut reads = samples(1);
    reads.extend(vec![MockRead::Timeout; 3]);
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(reads));
    let src = Arc::new(
        SoapySource::with_device(SourceId("sdr0".into()), dev.clone())
            .unwrap()
            .with_retune_timeout(Duration::from_millis(100)),
    );
    let mut rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    next_block(&mut rx).await;
    dev.delay_next("set_frequency", 300);
    let Err(PortError::Io(m)) = src.tune(tuning(101_000_000, 1_000_000)).await else {
        panic!("expected Io")
    };
    assert_eq!(m, "soapy retune timed out");
    assert_eq!(src.fault().as_deref(), Some("soapy retune timed out"));
    // The reader still applies the late retune; `current` follows the device.
    assert!(
        wait_until(1_000, || src.current()
            == Some(tuning(101_000_000, 1_000_000)))
        .await,
        "{:?}",
        src.current()
    );
    // Faulted: the old band is not short-circuited but reopened.
    let opens = |d: &MockDevice| d.calls().iter().filter(|c| *c == "activate").count();
    let got = src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    assert_eq!(got, tuning(100_000_000, 1_000_000));
    assert_eq!(src.current(), Some(got));
    assert_eq!(src.fault(), None);
    assert_eq!(opens(&dev), 2);
}

#[tokio::test]
async fn t0_monotonic_across_reopen() {
    // Ten instant blocks run 164 ms ahead of the wall clock, then a retune
    // times out (a fault that is not a device loss), and the next tune reopens.
    let mut reads = samples(10);
    reads.extend(vec![MockRead::Timeout; 3]);
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(reads));
    let src = Arc::new(
        SoapySource::with_device(SourceId("sdr0".into()), dev.clone())
            .unwrap()
            .with_retune_timeout(Duration::from_millis(100)),
    );
    let mut rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    let mut last = None;
    for _ in 0..10 {
        last = Some(next_block(&mut rx).await);
    }
    let last = last.unwrap();
    dev.delay_next("set_frequency", 300);
    assert!(src.tune(tuning(101_000_000, 1_000_000)).await.is_err());
    assert!(src.fault().is_some());
    dev.push_reads(samples(1));
    // Wait out the late retune so the reader has ended before the reopen.
    tokio::time::sleep(Duration::from_millis(400)).await;
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    let b = next_block(&mut rx).await;
    assert!(b.t0.0 >= end_ns(&last), "{} < {}", b.t0.0, end_ns(&last));
}

async fn node_with(src: Arc<SoapySource>) -> (Node, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = StageRegistry::new();
    reg.register(Arc::new(PassthroughStage::new()));
    let cfg = NodeConfig {
        state_dir: dir.path().join("state"),
        ring_seconds: 2.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory: None,
    };
    let port: Arc<dyn Port> = src;
    (Node::open(cfg, reg, vec![port]).await.unwrap(), dir)
}

fn passthrough() -> Op {
    Op::JobStart {
        kind: JobKind::Passthrough,
        source: SourceId("sdr0".into()),
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(1_000_000),
        deviation_hz: None,
        sideband: None,
        elastic: false,
        replace: vec![],
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
async fn unplug_mid_stream_fails_job() {
    // Empty script (100 ms timeouts) until the job runs, then the unplug.
    let dev = Arc::new(MockDevice::hackrf_like());
    let src = source(&dev);
    let (node, _dir) = node_with(src.clone()).await;
    node.apply(passthrough()).await.unwrap();
    assert_eq!(node.job_list()[0].state, JobState::Running);
    let mut reads = samples(5);
    reads.push(MockRead::Fatal("LIBUSB_ERROR_NO_DEVICE".into()));
    dev.push_reads(reads);
    let reason = wait_failed(&node, 1_000).await;
    assert!(
        reason.contains("source sdr0: read failed") && reason.contains("LIBUSB_ERROR_NO_DEVICE"),
        "{reason}"
    );
    assert_abandoned(&dev).await;
    assert!(!dev.calls().contains(&"deactivate".to_string()));

    // No samples after two blocks: the mock times out (100 ms each) forever.
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(samples(2)));
    let src = source(&dev);
    let (node, _dir) = node_with(src.clone()).await;
    node.apply(passthrough()).await.unwrap();
    let reason = wait_failed(&node, 2_000).await;
    assert!(reason.contains("no samples for 1000 ms"), "{reason}");
}

/// Ruling R5: the fault is set before the stream is abandoned, and a lost
/// device is not reopened by a tune: the fault stays and the tune says why.
#[tokio::test]
async fn fault_is_set_before_abandon_and_a_tune_on_a_lost_device_is_refused() {
    let dev = Arc::new(
        MockDevice::hackrf_like().with_reads(vec![MockRead::Fatal("LIBUSB_ERROR_IO".into())]),
    );
    let src = source(&dev);
    let _rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    let probe = src.clone();
    let dev2 = dev.clone();
    // Whenever `abandon` is visible, the fault must already be.
    assert!(
        wait_until(1_000, || {
            let closed = dev2.calls().iter().any(|c| c == "abandon");
            if closed {
                assert!(probe.fault().is_some(), "abandoned before the fault");
            }
            closed
        })
        .await
    );
    assert!(
        src.fault()
            .unwrap()
            .contains("read failed: LIBUSB_ERROR_IO")
    );
    let calls = dev.calls().len();
    let e = src.tune(tuning(100_000_000, 1_000_000)).await.unwrap_err();
    assert!(
        e.to_string().contains("device lost") && e.to_string().contains("LIBUSB_ERROR_IO"),
        "{e}"
    );
    assert!(src.fault().is_some(), "a refused tune keeps the fault");
    assert_eq!(dev.calls().len(), calls);
}

#[tokio::test]
async fn reader_exits_when_source_dropped() {
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(samples(1)));
    let src = source(&dev);
    let mut rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    next_block(&mut rx).await;
    drop(src);
    assert_deactivated(&dev).await;
    // The reader's sender clone is gone with it: the stream ends.
    let end = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            match rx.recv().await {
                Err(RecvError::Closed) => break,
                _ => continue,
            }
        }
    })
    .await;
    assert!(end.is_ok(), "stream not closed after drop");
}

#[tokio::test]
async fn reader_exits_without_receivers() {
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(samples(64)));
    let src = source(&dev);
    let rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    drop(rx);
    assert_deactivated(&dev).await;
    assert_eq!(src.fault(), None, "no receivers is not a fault");
    // A dead reader is restarted by the next tune even on the same band.
    let _rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    assert_eq!(dev.calls().iter().filter(|c| *c == "activate").count(), 2);
}

#[tokio::test]
async fn measured_rate_is_reported() {
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(samples(4)));
    let src = source(&dev);
    let mut rx = src.subscribe();
    assert_eq!(src.measured_rate_hz(), None);
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    for _ in 0..4 {
        next_block(&mut rx).await;
    }
    let r = src.measured_rate_hz().expect("a measured rate");
    assert!(r.is_finite() && r > 0.0, "{r}");
}

#[test]
fn uhd_source_asserts_the_range_at_the_maximum_clock() {
    // A B210's driver key is its device type, and it refuses rates past its clock.
    let dev = Arc::new(
        MockDevice::hackrf_like()
            .with_driver("b200")
            .with_rate_follow(16e6, 61.44e6)
            .refusing_above_ceiling(),
    );
    let src = source(&dev);
    assert_eq!(
        src.capabilities().max_bandwidth_hz,
        Claim::Asserted(Hz(61_440_000))
    );
    // Or the args say uhd whatever the key.
    let dev = Arc::new(
        MockDevice::hackrf_like()
            .with_driver("custom")
            .with_rate_follow(16e6, 61.44e6),
    );
    let src =
        SoapySource::with_device_args(SourceId("sdr0".into()), dev.clone(), "driver=uhd,type=b200")
            .unwrap();
    assert_eq!(
        src.capabilities().max_bandwidth_hz,
        Claim::Asserted(Hz(61_440_000))
    );
    let dev = Arc::new(MockDevice::hackrf_like().with_rate_follow(16e6, 61.44e6));
    let src = source(&dev);
    assert_eq!(
        src.capabilities().max_bandwidth_hz,
        Claim::Asserted(Hz(16_000_000))
    );
    assert_eq!(dev.range_reads(), 1, "a non-uhd device is probed once");
}

/// A B210 at its 61.44 MHz master clock: the device divides the clock by an
/// integer, so only the ladder's rungs come back as asked.
fn b210() -> MockDevice {
    MockDevice::hackrf_like()
        .with_driver("b200")
        .with_uhd_decimation(61.44e6)
}

fn rate_calls(dev: &MockDevice, from: usize) -> Vec<String> {
    dev.calls()[from..]
        .iter()
        .filter(|c| c.starts_with("set_sample_rate"))
        .cloned()
        .collect()
}

#[tokio::test]
async fn uhd_tune_asks_only_for_ladder_rates() {
    let dev = Arc::new(b210().with_reads(samples(1)));
    let src = source(&dev);
    let mut rx = src.subscribe();
    let probed = dev.calls().len();
    let got = src.tune(tuning(100_000_000, 200_000)).await.unwrap();
    assert_eq!(got, tuning(100_000_000, 204_800));
    assert_eq!(src.current(), Some(got));
    assert_eq!(rate_calls(&dev, probed), ["set_sample_rate(204800)"]);
    assert_eq!(next_block(&mut rx).await.sample_rate_hz, Hz(204_800));
}

#[tokio::test]
async fn actual_rate_below_the_band_retries_once_then_refuses() {
    // 25 % short: 204 800 reads 153 600; the retry asks the next rung above
    // the request, 240 000 (N 256: no N in 257..300 divides 2^15 * 3 * 5^4),
    // which reads 180 000.
    let want = "soapy: device gives 180000 Hz for 240000 Hz, below the 200000 Hz band";
    let dev = Arc::new(b210().with_rate_shortfall(0.25));
    let src = source(&dev);
    let probed = dev.calls().len();
    let Err(PortError::Unsupported(m)) = src.tune(tuning(100_000_000, 200_000)).await else {
        panic!("expected Unsupported")
    };
    assert_eq!(m, want);
    assert_eq!(
        rate_calls(&dev, probed),
        ["set_sample_rate(204800)", "set_sample_rate(240000)"]
    );
    assert_eq!(src.current(), None);
    assert_eq!(src.fault(), None, "a refused rate is not a device fault");

    let (node, _dir) = node_with(src.clone()).await;
    let mut op = passthrough();
    if let Op::JobStart { bandwidth_hz, .. } = &mut op {
        *bandwidth_hz = Hz(200_000);
    }
    let err = node.apply(op).await.expect_err("the job cannot start");
    assert!(err.to_string().contains(want), "{err}");
    assert_eq!(src.current(), None);
    // Refused before the stream opens: the device was never tuned or opened.
    let calls = dev.calls();
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("set_frequency") || c.starts_with("open_rx")),
        "{calls:?}"
    );
}

#[tokio::test]
async fn a_retry_a_rung_up_rescues_the_tune() {
    // 10 % short: 204 800 reads 184 320; the retry at 240 000 reads 216 000,
    // which covers the 200 000 Hz band.
    let dev = Arc::new(b210().with_rate_shortfall(0.1).with_reads(samples(1)));
    let src = source(&dev);
    let _rx = src.subscribe();
    let probed = dev.calls().len();
    let got = src.tune(tuning(100_000_000, 200_000)).await.unwrap();
    assert_eq!(got, tuning(100_000_000, 216_000));
    assert_eq!(src.current().map(|c| c.bandwidth_hz), Some(Hz(216_000)));
    assert_eq!(src.fault(), None);
    assert_eq!(
        rate_calls(&dev, probed),
        ["set_sample_rate(204800)", "set_sample_rate(240000)"]
    );
}

#[tokio::test]
async fn a_refused_retune_keeps_the_capture_it_had() {
    let mut reads = samples(1);
    reads.extend([MockRead::Timeout, MockRead::Timeout]);
    reads.extend(samples(2));
    let dev = Arc::new(b210().with_reads(reads));
    let src = source(&dev);
    let mut rx = src.subscribe();
    let was = src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    assert_eq!(was, tuning(100_000_000, 1_024_000));
    next_block(&mut rx).await;
    // The next two reads of the rate come back 10 % short: 2 457 600 reads
    // 2 211 840; the retry, the next rung above the request (N 24), asks
    // 2 560 000 and reads 2 304 000, still short of 2 400 000.
    dev.shortfall_next(0.1, 2);
    let from = calls_without_drops(&dev).len();
    let Err(PortError::Unsupported(m)) = src.tune(tuning(101_000_000, 2_400_000)).await else {
        panic!("expected Unsupported")
    };
    assert_eq!(
        m,
        "soapy: device gives 2304000 Hz for 2560000 Hz, below the 2400000 Hz band"
    );
    assert_eq!(src.current(), Some(was));
    assert_eq!(src.fault(), None);
    assert_eq!(
        calls_without_drops(&dev)[from..],
        [
            "set_sample_rate(2457600)",
            "set_sample_rate(2560000)",
            "set_sample_rate(1024000)",
            "open_rx(Cs8)",
            "activate"
        ]
    );
    let b = next_block(&mut rx).await;
    assert_eq!(
        (b.sample_rate_hz, b.center_hz),
        (Hz(1_024_000), Hz(100_000_000))
    );
}

#[tokio::test]
async fn non_uhd_rates_keep_the_quantum() {
    // A HackRF's range starts at 2 MHz; its rates stay on the 100 kHz quantum.
    let dev =
        Arc::new(
            MockDevice::hackrf_like().with_rates(vec![zeta_source_soapy::device::RangeHz {
                min_hz: 2e6,
                max_hz: 20e6,
                step_hz: 0.0,
            }]),
        );
    let src = source(&dev);
    let _rx = src.subscribe();
    let got = src.tune(tuning(100_000_000, 12_500)).await.unwrap();
    assert_eq!(got, tuning(100_000_000, 2_000_000));
    let got = src.tune(tuning(100_000_000, 2_345_678)).await.unwrap();
    assert_eq!(got, tuning(100_000_000, 2_400_000));
    assert_eq!(
        rate_calls(&dev, 0),
        ["set_sample_rate(2000000)", "set_sample_rate(2400000)"]
    );
}

/// A gain change and a retune issued back to back reach the device in order
/// through the reader, and the ack carries the new gain.
#[tokio::test]
async fn gain_and_retune_keep_their_order_on_the_reader() {
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(samples(400)));
    let src = source(&dev);
    let mut rx = src.subscribe();
    src.tune(tuning(100_000_000, 12_500)).await.unwrap();
    next_block(&mut rx).await;
    let from = dev.calls().len();
    let (gain, tuned) = tokio::join!(
        src.set_gain(Some(30.0)),
        src.tune(tuning(101_000_000, 12_500))
    );
    let gain = gain.unwrap();
    tuned.unwrap();
    assert_eq!(
        dev.calls()[from..],
        [
            "set_gain_mode(false)",
            "set_gain(30)",
            "set_frequency(101000000)"
        ]
    );
    assert_eq!(gain.db, Some(30.0));
    assert!(!gain.automatic);
    assert_eq!(src.gain().unwrap().db, Some(30.0));
    // Applied by the reader thread, not on the caller's.
    let gain_threads: Vec<String> = dev
        .call_threads()
        .into_iter()
        .filter(|(c, _)| c.starts_with("set_gain"))
        .map(|(_, t)| t)
        .collect();
    assert_eq!(gain_threads, ["soapy-sdr0", "soapy-sdr0"]);
    // Automatic gain through the live reader too.
    let auto = src.set_gain(None).await.unwrap();
    assert_eq!((auto.db, auto.automatic), (None, true));
    assert_eq!(dev.calls().last().unwrap(), "set_gain_mode(true)");
    assert_eq!(src.gain(), Some(auto));
}

fn slow_source(dev: &Arc<MockDevice>, timeout_ms: u64) -> Arc<SoapySource> {
    Arc::new(
        SoapySource::with_device(SourceId("sdr0".into()), dev.clone())
            .unwrap()
            .with_retune_timeout(Duration::from_millis(timeout_ms)),
    )
}

/// A faulted reader still inside a device call (a retune that timed out): the
/// gain waits for it to end, then goes straight to the device, never
/// overlapping the reader's call.
#[tokio::test]
async fn gain_on_a_faulted_reader_goes_straight_to_the_device() {
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(samples(400)));
    let src = slow_source(&dev, 200);
    let mut rx = src.subscribe();
    src.tune(tuning(100_000_000, 12_500)).await.unwrap();
    next_block(&mut rx).await;
    dev.delay_next("set_frequency", 300);
    let e = src.tune(tuning(101_000_000, 12_500)).await.unwrap_err();
    assert!(e.to_string().contains("retune timed out"), "{e}");
    assert!(src.fault().is_some());
    let g = src.set_gain(Some(20.0)).await.unwrap();
    assert_eq!(g.db, Some(20.0));
    assert_eq!(dev.overlaps(), 0, "{:?}", dev.call_threads());
    let calls: Vec<(String, String)> = dev.call_threads();
    let n = calls.len();
    assert_eq!(calls[n - 3].0, "set_frequency(101000000)");
    assert_eq!(calls[n - 3].1, "soapy-sdr0");
    assert_eq!(calls[n - 1].0, "set_gain(20)");
    assert_ne!(calls[n - 1].1, "soapy-sdr0", "applied directly");
    assert_eq!(src.gain().unwrap().db, Some(20.0));
    // The reader has ended (it deactivated before the gain): direct at once.
    let deactivated = dev.calls().iter().position(|c| c == "deactivate");
    assert!(deactivated.is_some_and(|i| i < dev.calls().len() - 2));
    src.set_gain(Some(21.0)).await.unwrap();
    assert_eq!(dev.calls().last().unwrap(), "set_gain(21)");
}

/// A faulted reader that stays inside its device call past `retune_timeout`:
/// the gain is refused and nothing touches the device.
#[tokio::test]
async fn gain_waiting_on_a_stuck_reader_is_refused() {
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(samples(400)));
    let src = slow_source(&dev, 100);
    let mut rx = src.subscribe();
    src.tune(tuning(100_000_000, 12_500)).await.unwrap();
    next_block(&mut rx).await;
    dev.delay_next("set_frequency", 600);
    src.tune(tuning(101_000_000, 12_500)).await.unwrap_err();
    let e = src.set_gain(Some(20.0)).await.unwrap_err();
    assert!(
        matches!(&e, PortError::Io(m) if m == "soapy set_gain: the reader thread has not stopped"),
        "{e}"
    );
    assert!(!dev.calls().iter().any(|c| c.starts_with("set_gain")));
    assert_deactivated(&dev).await;
    assert_eq!(dev.overlaps(), 0);
}

/// A reader that does not ack within `retune_timeout` (stuck in a slow read):
/// the caller gets `Io` and the reader drops the command instead of applying a
/// gain the node never persisted.
#[tokio::test]
async fn an_unacked_gain_is_not_applied_later() {
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(samples(1)));
    let src = slow_source(&dev, 100);
    let mut rx = src.subscribe();
    src.tune(tuning(100_000_000, 12_500)).await.unwrap();
    next_block(&mut rx).await;
    dev.push_reads(vec![MockRead::Slow(16_384, 400)]);
    tokio::time::sleep(Duration::from_millis(150)).await;
    let e = src.set_gain(Some(30.0)).await.unwrap_err();
    assert!(
        matches!(&e, PortError::Io(m) if m == "soapy set_gain timed out"),
        "{e}"
    );
    tokio::time::sleep(Duration::from_millis(450)).await;
    assert!(src.fault().is_none(), "{:?}", src.fault());
    assert!(
        !dev.calls().iter().any(|c| c.starts_with("set_gain")),
        "{:?}",
        dev.calls()
    );
    assert_eq!(src.gain().unwrap().db, Some(0.0));
}

/// `gain=` in the device args is applied at open by the same call as the op's,
/// and that call's refusals carry the op's texts.
#[tokio::test]
async fn the_gain_arg_at_open_uses_the_same_path() {
    let (args, gain) = zeta_source_soapy::split_zeta_args("driver=hackrf,gain=40").unwrap();
    let gain = gain.gain;
    let dev = Arc::new(MockDevice::hackrf_like());
    let src = SoapySource::with_device_args(SourceId("sdr0".into()), dev.clone(), &args).unwrap();
    let opened = src.gain().unwrap();
    assert_eq!(
        (opened.min_db, opened.max_db, opened.automatic_available),
        (0.0, 76.0, true)
    );
    let g = src.set_gain_now(gain).unwrap();
    assert_eq!(dev.calls(), ["set_gain_mode(false)", "set_gain(40)"]);
    assert_eq!(g.db, Some(40.0));
    // The port's call takes the same path while nothing streams.
    src.set_gain(Some(41.0)).await.unwrap();
    assert_eq!(dev.calls()[2..], ["set_gain_mode(false)", "set_gain(41)"]);
    let e = src.set_gain(Some(90.0)).await.unwrap_err();
    assert!(
        matches!(&e, PortError::Unsupported(m) if m == "gain_db 90 out of range 0..=76 dB for source sdr0"),
        "{e}"
    );
    assert_eq!(dev.calls().len(), 4, "a refused gain touches nothing");

    let dev = Arc::new(MockDevice::hackrf_like().with_gain_mode(false));
    let src = source(&dev);
    assert!(!src.gain().unwrap().automatic_available);
    let e = src.set_gain(None).await.unwrap_err();
    assert!(
        matches!(&e, PortError::Unsupported(m) if m == "source sdr0 has no automatic gain"),
        "{e}"
    );
    src.set_gain(Some(10.0)).await.unwrap();
    assert_eq!(dev.calls(), ["set_gain(10)"]);
}
