//! `stream_tail` over a real DSP job: a spectrum job's FFT events.

use std::sync::Arc;
use std::time::Duration;

use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpResult};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, StageRegistry};
use zeta_core::types::{Hz, SourceId, StreamKind};
use zeta_source_file::{FileOpts, FileSource, fixtures};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_tail_returns_events_from_running_job() {
    let dir = tempfile::tempdir().unwrap();
    let meta = fixtures::write_tone(dir.path());
    let opts = FileOpts {
        realtime: false,
        block_samples: 4800,
        repeat: true,
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(SourceId("file0".into()), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    let mut reg = StageRegistry::new();
    zeta_dsp::register_all(&mut reg);
    let cfg = NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
        burst_policy: Default::default(),
        state_dir: dir.path().join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory: None,
        modes_dir: None,
    };
    let node = Node::open(cfg, reg, vec![port]).await.unwrap();
    src.start();
    let job = match node
        .apply(Op::JobStart {
            kind: JobKind::Spectrum,
            source: SourceId("file0".into()),
            center_hz: Hz(100_000_000),
            bandwidth_hz: Hz(48_000),
            deviation_hz: None,
            sideband: None,
            dwell_ms: None,
            average: None,
            bits: None,
            elastic: false,
            replace: vec![],
        })
        .await
    {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected Job, got {other:?}"),
    };
    let mut stream = None;
    for _ in 0..200 {
        stream = node
            .bus()
            .list()
            .into_iter()
            .find(|s| s.kind == StreamKind::Event && s.provenance.job.as_ref() == Some(&job))
            .map(|s| s.id);
        if stream.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let stream = stream.expect("the job's event stream");
    let op = Op::StreamTail {
        stream: stream.clone(),
        max_frames: 3,
        timeout_ms: 2000,
    };
    let frames = match node.apply(op).await {
        Ok(OpResult::Frames { frames, .. }) => frames,
        other => panic!("expected Frames, got {other:?}"),
    };
    assert_eq!(frames.len(), 3);
    for f in &frames {
        assert_eq!(f.stream, stream);
        assert_eq!(f.source, SourceId("file0".into()));
        assert_eq!(f.job.as_ref(), Some(&job));
        assert_eq!(f.channel, None);
        assert_eq!(f.kind, StreamKind::Event);
        assert!(f.end_ns.0 > f.start_ns.0, "{f:?}");
        assert_eq!(f.payload["size"], 1024);
    }
}
