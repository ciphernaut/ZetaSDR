//! The `capture` job: `channel → burst_capture` over a 48 kHz file source of 50 ms GFSK
//! bursts every 100 ms writes its bursts to the node's store and ends `stopped`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use num_complex::Complex;
use zeta_core::capture::{CaptureMeta, write_cf32};
use zeta_core::job::JobState;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpResult};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Hz, JobId, SourceId, Timestamp};
use zeta_source_file::{FileOpts, FileSource};

const RATE_HZ: u32 = 48_000;
const CENTER_HZ: u64 = 433_920_000;
/// The kind's default `bursts`.
const BURSTS: usize = 32;

/// One second of a 50 ms 4800-baud GFSK burst every 100 ms (50 ms of noise, then the
/// burst), 20 dB over the noise.
fn fixture(dir: &Path) -> std::path::PathBuf {
    let mut state = 0x5eed_u64;
    let symbols: Vec<u8> = (0..240)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 63) as u8
        })
        .collect();
    let mut burst = zeta_dsp::synth::gfsk_modulate(&symbols, 2, 4_800, 2_400.0, RATE_HZ);
    // The gap first: a job joining at a block (one period) starts on noise, so its first
    // window has its pre guard.
    burst.resize(RATE_HZ as usize / 10, Complex::new(0.0, 0.0));
    burst.rotate_right(RATE_HZ as usize / 20);
    let mut iq: Vec<Complex<f32>> = burst
        .iter()
        .copied()
        .cycle()
        .take(RATE_HZ as usize)
        .collect();
    zeta_dsp::synth::awgn(&mut iq, 20.0, 7);
    let meta = CaptureMeta {
        sample_rate_hz: Hz(RATE_HZ as u64),
        center_hz: Hz(CENTER_HZ),
        start: Timestamp(0),
        annotations: Vec::new(),
    };
    write_cf32(dir, "bursts", &meta, &iq).expect("write fixture")
}

async fn until(what: &str, within: Duration, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn state(node: &Node, id: &JobId) -> Option<JobState> {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .map(|r| r.state)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_capture_job_ends_stopped_with_its_records_in_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let meta = fixture(dir.path());
    let source = Arc::new(
        FileSource::open(
            SourceId("rx".into()),
            &meta,
            FileOpts {
                realtime: true,
                block_samples: 4_800,
                repeat: true,
                tune_range: None,
            },
        )
        .unwrap(),
    );
    let mut registry = StageRegistry::new();
    registry.register(Arc::new(PassthroughStage::new()));
    zeta_dsp::register_all(&mut registry);
    let cfg = NodeConfig {
        state_dir: dir.path().join("state"),
        ring_seconds: 4.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory: None,
        modes_dir: None,
        burst_policy: Default::default(),
    };
    let port: Arc<dyn Port> = source.clone();
    let node = Node::open(cfg, registry, vec![port]).await.unwrap();
    source.start();
    let op = Op::JobStart {
        kind: JobKind::Capture,
        source: SourceId("rx".into()),
        center_hz: Hz(CENTER_HZ),
        bandwidth_hz: Hz(12_500),
        deviation_hz: None,
        sideband: None,
        elastic: false,
        replace: vec![],
    };
    let job = match node.apply(op).await {
        Ok(OpResult::Job(j)) => j,
        r => panic!("job_start: {r:?}"),
    };
    until("the capture job ends", Duration::from_secs(30), || {
        state(&node, &job) != Some(JobState::Running)
    })
    .await;
    assert_eq!(state(&node, &job), Some(JobState::Stopped));
    let records = node.bursts().list();
    assert_eq!(records.len(), BURSTS);
    for r in &records {
        assert_eq!(r.set, job);
        assert_eq!(r.job, job);
        assert_eq!(r.source, SourceId("rx".into()));
        assert_eq!(r.center_hz, Hz(CENTER_HZ));
        assert_eq!(r.bandwidth_hz, Hz(12_500));
        assert!(r.data_path.is_file(), "{r:?}");
        // Each window holds one 50 ms burst and its 20 ms guards.
        let ms = (r.end_ns - r.start_ns) as f64 / 1e6;
        assert!((ms - 90.0).abs() <= 2.0, "{ms} ms");
    }
    assert!(
        records[0]
            .data_path
            .starts_with(dir.path().join("state").join("bursts"))
    );
    let ctx = node.job_context(&job).expect("the context is kept");
    let capture = &ctx["capture"];
    let mut written: Vec<String> = capture["written"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let mut ids: Vec<String> = records.iter().map(|r| r.id.clone()).collect();
    written.sort();
    ids.sort();
    assert_eq!(written, ids);
    assert_eq!(capture["timed_out"], false);
    assert_eq!(
        capture["bytes"],
        records.iter().map(|r| r.bytes).sum::<u64>()
    );
    assert!(capture["bursts_seen"].as_u64().unwrap() >= BURSTS as u64);
}
