//! Attached jobs on the real chain: a `wfm_demod` job on a broadcast-FM speech fixture,
//! `job_attach` of `transcribe` (`stream_in → denoise → vad → transcribe`) with an
//! in-process [`FakeBackend`], in a node opened in this process.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use zeta_core::job::{JobRecord, JobState};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpError, OpResult};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::stream::{Payload, StreamInfo};
use zeta_core::stream_in::StreamInStage;
use zeta_core::types::{Hz, JobId, SourceId, StreamKind};
use zeta_source_file::{FileOpts, FileSource};
use zeta_speech::TranscribeStage;
use zeta_speech::testing::FakeBackend;

const TEXT: &str = "this is the fake transcript";
const CENTER_HZ: u64 = common::SPEECH_HZ;

fn registry(fake: Arc<FakeBackend>) -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    r.register(Arc::new(StreamInStage::new()));
    zeta_dsp::register_all(&mut r);
    r.register(Arc::new(TranscribeStage::new(
        Some(fake),
        Duration::from_secs(5),
    )));
    r
}

/// The broadcast-FM speech fixture ([`common::speech_fixture`]), played in real time, looping.
fn speech_port(dir: &Path) -> (Arc<FileSource>, Arc<dyn Port>) {
    let meta = common::speech_fixture(dir);
    let opts = FileOpts {
        realtime: true,
        block_samples: 4800,
        repeat: true,
    };
    let src = Arc::new(FileSource::open(SourceId("speech".into()), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    (src, port)
}

fn cfg(dir: &Path) -> NodeConfig {
    NodeConfig {
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
    }
}

async fn start(node: &Node, kind: JobKind, bandwidth_hz: u64) -> JobId {
    let op = Op::JobStart {
        kind,
        source: SourceId("speech".into()),
        center_hz: Hz(CENTER_HZ),
        bandwidth_hz: Hz(bandwidth_hz),
        deviation_hz: None,
        sideband: None,
    };
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("job_start {kind:?}: {r:?}"),
    }
}

async fn attach(node: &Node, job: &JobId) -> Result<OpResult, OpError> {
    node.apply(Op::JobAttach {
        kind: JobKind::Transcribe,
        job: job.clone(),
        language: None,
    })
    .await
}

fn record(node: &Node, id: &JobId) -> JobRecord {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .expect("job listed")
}

fn streams_of(node: &Node, id: &JobId) -> Vec<StreamInfo> {
    node.bus()
        .list()
        .into_iter()
        .filter(|s| s.provenance.job.as_ref() == Some(id))
        .collect()
}

/// The first `transcript` event on `job`'s event streams within `ms`.
async fn transcript(node: &Node, job: &JobId, ms: u64) -> Option<serde_json::Value> {
    let mut rxs: Vec<_> = streams_of(node, job)
        .iter()
        .filter(|s| s.kind == StreamKind::Event)
        .filter_map(|s| node.bus().subscribe(&s.id))
        .collect();
    assert_eq!(rxs.len(), 2, "vad's and transcribe's event streams");
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    while tokio::time::Instant::now() < deadline {
        for rx in &mut rxs {
            while let Ok(f) = rx.try_recv() {
                if let Payload::Event(v) = f.payload
                    && v["protocol"] == "transcript"
                {
                    assert_eq!(
                        f.provenance.job.as_ref(),
                        Some(job),
                        "pinned to the attached job"
                    );
                    return Some(v);
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attach_transcribe_emits_transcript_events() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = speech_port(dir.path());
    let fake = Arc::new(FakeBackend::new(TEXT));
    let node = Node::open(cfg(dir.path()), registry(fake.clone()), vec![port])
        .await
        .unwrap();
    src.start();
    let wfm = start(&node, JobKind::WfmDemod, 200_000).await;
    let id = match attach(&node, &wfm).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("attach: {r:?}"),
    };
    let (rec, input) = (record(&node, &id), record(&node, &wfm));
    assert_eq!(rec.kind, "transcribe");
    assert_eq!(rec.input, Some(wfm.clone()));
    assert_eq!(rec.source, input.source);
    assert_eq!(rec.tuning, input.tuning);
    match node.apply(Op::SourceList {}).await.unwrap() {
        OpResult::Sources(s) => assert_eq!(s[0].holders, vec![wfm.clone()]),
        r => panic!("{r:?}"),
    }
    let t = transcript(&node, &id, 20_000).await.expect("a transcript");
    assert_eq!(t["text"], TEXT, "{t}");
    assert_eq!(t["language"], "en", "{t}");
    assert!(t["end_ns"].as_u64() > t["start_ns"].as_u64(), "{t}");
    assert_eq!(fake.calls()[0].1, "en");
    assert_eq!(record(&node, &id).state, JobState::Running);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attached_job_stops_when_input_stops() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = speech_port(dir.path());
    let node = Node::open(
        cfg(dir.path()),
        registry(Arc::new(FakeBackend::new(TEXT))),
        vec![port],
    )
    .await
    .unwrap();
    src.start();
    let wfm = start(&node, JobKind::WfmDemod, 200_000).await;
    let Ok(OpResult::Job(id)) = attach(&node, &wfm).await else {
        panic!("attach")
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    node.apply(Op::JobStop { job: wfm }).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while (record(&node, &id).state == JobState::Running || !streams_of(&node, &id).is_empty())
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(record(&node, &id).state, JobState::Stopped);
    assert!(streams_of(&node, &id).is_empty(), "streams unregistered");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attach_refuses_non_running_or_audio_less_input() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = speech_port(dir.path());
    let node = Node::open(
        cfg(dir.path()),
        registry(Arc::new(FakeBackend::new(TEXT))),
        vec![port],
    )
    .await
    .unwrap();
    src.start();
    let spectrum = start(&node, JobKind::Spectrum, 200_000).await;
    assert_eq!(
        attach(&node, &spectrum).await,
        Err(OpError::Other {
            message: format!("job {} has no audio stream", spectrum.0)
        })
    );
    node.apply(Op::JobStop {
        job: spectrum.clone(),
    })
    .await
    .unwrap();
    let wfm = start(&node, JobKind::WfmDemod, 200_000).await;
    node.apply(Op::JobStop { job: wfm.clone() }).await.unwrap();
    assert_eq!(
        attach(&node, &wfm).await,
        Err(OpError::Other {
            message: format!("job {} is not running", wfm.0)
        })
    );
    assert_eq!(
        node.job_list().len(),
        2,
        "nothing recorded for a refused attach"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn job_start_with_transcribe_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, port) = speech_port(dir.path());
    let node = Node::open(
        cfg(dir.path()),
        registry(Arc::new(FakeBackend::new(TEXT))),
        vec![port],
    )
    .await
    .unwrap();
    let r = node
        .apply(Op::JobStart {
            kind: JobKind::Transcribe,
            source: SourceId("speech".into()),
            center_hz: Hz(CENTER_HZ),
            bandwidth_hz: Hz(200_000),
            deviation_hz: None,
            sideband: None,
        })
        .await;
    assert_eq!(
        r,
        Err(OpError::Other {
            message: "transcribe attaches to a job: use job_attach".into()
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attached_job_resumes_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = speech_port(dir.path());
    let node = Node::open(
        cfg(dir.path()),
        registry(Arc::new(FakeBackend::new(TEXT))),
        vec![port.clone()],
    )
    .await
    .unwrap();
    // The file source plays only while subscribed: keep it playing between nodes.
    let _hold = port.subscribe();
    src.start();
    let wfm = start(&node, JobKind::WfmDemod, 200_000).await;
    let Ok(OpResult::Job(id)) = attach(&node, &wfm).await else {
        panic!("attach")
    };
    drop(node);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let fake = Arc::new(FakeBackend::new(TEXT));
    let node = Node::open(cfg(dir.path()), registry(fake.clone()), vec![port.clone()])
        .await
        .unwrap();
    assert_eq!(record(&node, &wfm).state, JobState::Running);
    assert_eq!(record(&node, &id).state, JobState::Running);
    let t = transcript(&node, &id, 20_000)
        .await
        .expect("a transcript after resume");
    assert_eq!(t["text"], TEXT);
    drop(node);
    tokio::time::sleep(Duration::from_millis(50)).await;

    // The input job edited to stopped: the attached job fails, naming it.
    let path = dir
        .path()
        .join("state/jobs")
        .join(format!("{}.json", wfm.0));
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    v["state"] = serde_json::json!({"status": "stopped"});
    std::fs::write(&path, v.to_string()).unwrap();
    let node = Node::open(
        cfg(dir.path()),
        registry(Arc::new(FakeBackend::new(TEXT))),
        vec![port],
    )
    .await
    .unwrap();
    assert_eq!(
        record(&node, &id).state,
        JobState::Failed {
            reason: format!("input job {} not running", wfm.0)
        }
    );
}
