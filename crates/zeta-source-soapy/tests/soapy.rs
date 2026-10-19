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
        dev.calls().last().map(String::as_str) == Some("deactivate")
    })
    .await;
    assert!(done, "{:?}", dev.calls());
}

fn end_ns(b: &IqBlock) -> u64 {
    b.t0.0 + (b.samples.len() as u128 * 1_000_000_000 / u128::from(b.sample_rate_hz.0)) as u64
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
async fn retune_rate_change_reactivates_stream() {
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
    let calls = dev.calls();
    assert_eq!(
        calls[calls.len() - 4..],
        [
            "deactivate",
            "set_sample_rate(2000000)",
            "set_frequency(101000000)",
            "activate"
        ]
    );
    let next = next_block(&mut rx).await;
    assert_eq!(next.sample_rate_hz, Hz(2_000_000));
    assert_eq!(next.center_hz, Hz(101_000_000));
    assert!(next.t0.0 >= end_ns(&first));

    // Centre only: one set_frequency, no stream restart.
    src.tune(tuning(102_000_000, 2_000_000)).await.unwrap();
    let calls = dev.calls();
    assert_eq!(calls.last().unwrap(), "set_frequency(102000000)");
    assert_eq!(calls.iter().filter(|c| *c == "activate").count(), 2);
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
    assert_deactivated(&dev).await;
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
    // Ten instant blocks run 164 ms ahead of the wall clock, then the unplug.
    let mut reads = samples(10);
    reads.push(MockRead::Fatal("LIBUSB_ERROR_IO".into()));
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(reads));
    let src = source(&dev);
    let mut rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    let mut last = None;
    for _ in 0..10 {
        last = Some(next_block(&mut rx).await);
    }
    let last = last.unwrap();
    assert!(wait_until(500, || src.fault().is_some()).await);
    dev.push_reads(samples(1));
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

    // No samples after two blocks: the mock times out (100 ms each) forever.
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(samples(2)));
    let src = source(&dev);
    let (node, _dir) = node_with(src.clone()).await;
    node.apply(passthrough()).await.unwrap();
    let reason = wait_failed(&node, 2_000).await;
    assert!(reason.contains("no samples for 1000 ms"), "{reason}");
}

/// Ruling R5: the fault is set before the stream is deactivated, and a tune of
/// the same band on a faulted port re-opens the device and clears it.
#[tokio::test]
async fn fault_is_set_before_close_and_same_band_tune_recovers() {
    let dev = Arc::new(
        MockDevice::hackrf_like().with_reads(vec![MockRead::Fatal("LIBUSB_ERROR_IO".into())]),
    );
    let src = source(&dev);
    let mut rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    let probe = src.clone();
    let dev2 = dev.clone();
    // Whenever `deactivate` is visible, the fault must already be.
    assert!(
        wait_until(1_000, || {
            let closed = dev2.calls().iter().any(|c| c == "deactivate");
            if closed {
                assert!(probe.fault().is_some(), "deactivated before the fault");
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
    dev.push_reads(samples(1));
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    assert_eq!(src.fault(), None);
    let b = next_block(&mut rx).await;
    assert_eq!(b.sample_rate_hz, Hz(1_000_000));
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
    // A B210's driver key is its device type.
    let dev = Arc::new(
        MockDevice::hackrf_like()
            .with_driver("b200")
            .with_rate_follow(16e6, 61.44e6),
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
