//! Attached jobs on the real chain: a `wfm_demod` job on a broadcast-FM speech fixture,
//! `job_attach` of `transcribe` (`stream_in → denoise → vad → transcribe`) with an
//! in-process [`FakeBackend`], and `job_attach` of `packets` (`stream_in →
//! packet_capture`) on an `aprs_decode` job over a synthetic APRS fixture and on a frame
//! row's job, in a node opened in this process.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use zeta_core::job::{JobRecord, JobState};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpError, OpResult};
use zeta_core::packet_capture::PacketCaptureStage;
use zeta_core::packets::PacketCaptureRecord;
use zeta_core::pcapng;
use zeta_core::port::Port;
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::stream::{LinkType, Payload, StreamInfo};
use zeta_core::stream_in::StreamInStage;
use zeta_core::types::{Hz, JobId, SourceId, StreamKind};
use zeta_source_file::{FileOpts, FileSource};
use zeta_speech::TranscribeStage;
use zeta_speech::testing::FakeBackend;

const TEXT: &str = "this is the fake transcript";
const CENTER_HZ: u64 = common::SPEECH_HZ;

fn registry(fake: Arc<FakeBackend>) -> StageRegistry {
    registry_with(Some(fake))
}

fn registry_with(fake: Option<Arc<FakeBackend>>) -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    r.register(Arc::new(StreamInStage::new()));
    zeta_dsp::register_all(&mut r);
    r.register(Arc::new(TranscribeStage::new(
        fake.map(|f| f as Arc<dyn zeta_speech::SpeechBackend>),
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
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(SourceId("speech".into()), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    (src, port)
}

fn cfg(dir: &Path) -> NodeConfig {
    NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory: None,
        modes_dir: None,
    }
}

async fn start(node: &Node, kind: JobKind, bandwidth_hz: u64) -> JobId {
    let op = Op::JobStart {
        kind: kind.clone(),
        source: SourceId("speech".into()),
        center_hz: Hz(CENTER_HZ),
        bandwidth_hz: Hz(bandwidth_hz),
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        bits: None,
        method: None,
        bin_hz: None,
        audio_policy: None,
        dwell_scale: None,
        elastic: false,
        replace: vec![],
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
        stream: None,
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
    let feed = node
        .bus()
        .feed_of(&record(&node, &wfm).source)
        .expect("the source's feed");
    let mut feed_rx = node.bus().subscribe(&feed).unwrap();
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
    let mut on_feed = false;
    while let Ok(f) = feed_rx.try_recv() {
        if let Payload::Event(v) = &f.payload
            && v["protocol"] == "transcript"
        {
            assert_eq!(f.provenance.job.as_ref(), Some(&id));
            on_feed = true;
        }
    }
    assert!(on_feed, "the transcript event reached the source's feed");
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
            dwell_ms: None,
            average: None,
            bits: None,
            method: None,
            bin_hz: None,
            audio_policy: None,
            dwell_scale: None,
            elastic: false,
            replace: vec![],
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn job_attach_is_refused_at_op_time_without_a_backend() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = speech_port(dir.path());
    let node = Node::open(cfg(dir.path()), registry_with(None), vec![port])
        .await
        .unwrap();
    src.start();
    let wfm = start(&node, JobKind::WfmDemod, 200_000).await;
    let before = node.job_list().len();
    match attach(&node, &wfm).await {
        Err(OpError::Other { message }) => {
            assert!(
                message.ends_with(zeta_speech::NO_BACKEND_REASON),
                "{message}"
            )
        }
        r => panic!("attach: {r:?}"),
    }
    assert_eq!(node.job_list().len(), before, "no job record");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn schema_lists_transcribe_readiness() {
    let readiness = |node: &Node| {
        node.schema()["stages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["kind"] == "transcribe")
            .unwrap()["readiness"]
            .clone()
    };
    let dir = tempfile::tempdir().unwrap();
    let (_src, port) = speech_port(dir.path());
    let node = Node::open(cfg(dir.path()), registry_with(None), vec![port])
        .await
        .unwrap();
    let r = readiness(&node);
    assert_eq!(r["ready"], false, "{r}");
    assert_eq!(r["reason"], zeta_speech::NO_BACKEND_REASON);
    let dir = tempfile::tempdir().unwrap();
    let (_src, port) = speech_port(dir.path());
    let fake = Arc::new(FakeBackend::new(TEXT));
    let node = Node::open(cfg(dir.path()), registry_with(Some(fake)), vec![port])
        .await
        .unwrap();
    let r = readiness(&node);
    assert_eq!(r["ready"], true, "{r}");
    assert_eq!(r["capacity"], 4, "{r}");
}

/// The tone recording as a radio tunable over 470-480 MHz, source `cb`, and a node
/// over it with the real stages (`transcribe` over `fake`, or without a backend).
async fn cb_node(dir: &Path, fake: Option<Arc<FakeBackend>>) -> Node {
    let tone = zeta_source_file::fixtures::write_tone(dir);
    let opts = FileOpts {
        realtime: true,
        block_samples: 4800,
        repeat: true,
        tune_range: Some((Hz(470_000_000), Hz(480_000_000))),
    };
    let src = Arc::new(FileSource::open(SourceId("cb".into()), &tone, opts).unwrap());
    src.start();
    let port: Arc<dyn Port> = src;
    Node::open(cfg(dir), registry_with(fake), vec![port])
        .await
        .unwrap()
}

fn cb_plan() -> Op {
    Op::PlanStart {
        plan: "uhf_cb_au".into(),
        channels: Some(vec![1, 2]),
        group: None,
        kind: JobKind::FmDemod,
        source: SourceId("cb".into()),
        deviation_hz: None,
        sideband: None,
        attach: Some(JobKind::Transcribe),
        language: None,
        replace: vec![],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plan_start_with_transcribe_is_refused_without_a_backend() {
    let dir = tempfile::tempdir().unwrap();
    let node = cb_node(dir.path(), None).await;
    match node.apply(cb_plan()).await {
        Err(OpError::Other { message }) => {
            assert!(
                message.contains(zeta_speech::NO_BACKEND_REASON),
                "{message}"
            );
            assert!(message.ends_with("start without attach"), "{message}");
        }
        r => panic!("plan_start: {r:?}"),
    }
    assert!(node.job_list().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plan_start_with_transcribe_over_a_fake_backend_attaches_real_transcribers() {
    let dir = tempfile::tempdir().unwrap();
    let node = cb_node(dir.path(), Some(Arc::new(FakeBackend::new(TEXT)))).await;
    match node.apply(cb_plan()).await {
        Ok(OpResult::PlanStart {
            started,
            refused,
            warnings,
            ..
        }) => {
            assert!(refused.is_empty(), "{refused:?}");
            assert_eq!(started.len(), 2);
            assert!(started.iter().all(|s| s.attached.is_some()));
            assert!(warnings.is_empty(), "{warnings:?}");
        }
        r => panic!("plan_start: {r:?}"),
    }
    let running = |kind: &str| {
        node.job_list()
            .iter()
            .filter(|r| r.state == JobState::Running && r.kind == kind)
            .count()
    };
    assert_eq!(running("fm_demod"), 2);
    assert_eq!(running("transcribe"), 2);
}

/// [`registry_with`] plus `packet_capture`, the sink of a `packets` attachment.
fn packets_registry() -> StageRegistry {
    let mut r = registry_with(None);
    r.register(Arc::new(PacketCaptureStage::new()));
    r
}

fn aprs() -> SourceId {
    SourceId("aprs".into())
}

/// A node over the `aprs` fixture ([`common::aprs_fixture`]) played in real time,
/// looping, as source `aprs`, with `modes_dir` for its operator rows.
async fn aprs_node(dir: &Path) -> Node {
    let meta = common::aprs_fixture(dir);
    let opts = FileOpts {
        realtime: true,
        block_samples: 4800,
        repeat: true,
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(aprs(), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    let node = Node::open(cfg(dir), packets_registry(), vec![port])
        .await
        .unwrap();
    src.start();
    node
}

fn job_start_op(kind: &str, source: SourceId, center_hz: u64, bandwidth_hz: u64) -> Op {
    Op::JobStart {
        kind: JobKind::from_name(kind).unwrap(),
        source,
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(bandwidth_hz),
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        bits: None,
        method: None,
        bin_hz: None,
        audio_policy: None,
        dwell_scale: None,
        elastic: false,
        replace: vec![],
    }
}

/// An `aprs_decode` job on the `aprs` fixture's centre.
async fn start_aprs(node: &Node) -> JobId {
    match node
        .apply(job_start_op("aprs_decode", aprs(), common::APRS_HZ, 12_500))
        .await
    {
        Ok(OpResult::Job(id)) => id,
        r => panic!("aprs_decode start: {r:?}"),
    }
}

async fn attach_packets(node: &Node, job: &JobId) -> JobId {
    let op = Op::JobAttach {
        kind: JobKind::Packets,
        job: job.clone(),
        language: None,
        stream: None,
    };
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("attach packets: {r:?}"),
    }
}

async fn packets_list(node: &Node) -> Vec<PacketCaptureRecord> {
    match node.apply(Op::PacketsList {}).await {
        Ok(OpResult::PacketCaptures(v)) => v,
        r => panic!("packets_list: {r:?}"),
    }
}

/// The captures of the decoder job `job`, newest first.
fn captures_of(node: &Node, job: &JobId) -> Vec<PacketCaptureRecord> {
    node.packets()
        .list()
        .into_iter()
        .filter(|r| &r.job == job)
        .collect()
}

/// Poll `f` every 20 ms until it holds, panicking with `what` after `within`.
async fn until(what: &str, within: Duration, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The events (other than `bits`) `job` publishes from now on, each with its frame's
/// start, collected in the background.
type Events = Arc<std::sync::Mutex<Vec<(u64, serde_json::Value)>>>;

fn collect_events(node: &Node, job: &JobId) -> Events {
    let events = Events::default();
    for (_, stream) in node
        .job_outputs(job)
        .into_iter()
        .filter(|(k, _)| *k == StreamKind::Event)
    {
        let mut rx = node.bus().subscribe(&stream).unwrap();
        let events = events.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(f) => {
                        if let Payload::Event(v) = f.payload
                            && v["protocol"] != "bits"
                        {
                            let t = f.provenance.span.start_ns.0;
                            events.lock().unwrap().push((t, v));
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => return,
                }
            }
        });
    }
    events
}

/// The event collected whose frame started at `t_ns`.
fn event_at(events: &Events, t_ns: u64) -> Option<serde_json::Value> {
    let events = events.lock().unwrap();
    events
        .iter()
        .find(|(t, _)| *t == t_ns)
        .map(|(_, v)| v.clone())
}

/// The whole blocks of the capture `r`'s file, read.
fn read_capture(r: &PacketCaptureRecord) -> (u16, Vec<pcapng::Packet>) {
    let bytes = std::fs::read(&r.path).unwrap();
    pcapng::read(&bytes[..pcapng::whole_blocks(&bytes)]).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_packets_attachment_writes_a_capture_wireshark_reads() {
    let dir = tempfile::tempdir().unwrap();
    let node = aprs_node(dir.path()).await;
    let job = start_aprs(&node).await;
    let events = collect_events(&node, &job);
    let att = attach_packets(&node, &job).await;
    until("two packets captured", Duration::from_secs(30), || {
        captures_of(&node, &job)
            .first()
            .is_some_and(|r| r.packets >= 2)
    })
    .await;
    let list = packets_list(&node).await;
    let r = &list[0];
    assert_eq!(r.job, job);
    assert_eq!(r.source, aprs());
    assert_eq!(r.center_hz, Hz(common::APRS_HZ));
    assert_eq!(r.link_type, LinkType::Ax25Kiss);
    assert_eq!(r.mode.as_deref(), Some("aprs_decode"));
    assert_eq!(r.access, None);
    assert!(r.writing, "{r:?}");

    let (link, packets) = read_capture(r);
    assert_eq!(link, 202);
    assert!(packets.len() >= 2, "{packets:?}");
    let comment = format!("job={} source=aprs freq_hz={}", job.0, common::APRS_HZ);
    until("each packet's event", Duration::from_secs(5), || {
        packets.iter().all(|p| event_at(&events, p.t_ns).is_some())
    })
    .await;
    for p in &packets {
        assert_eq!(p.data[0], 0, "a KISS data frame on port 0");
        let frame = zeta_dsp::ax25::parse_ax25_ui(&p.data[1..]).expect("an AX.25 UI frame");
        let ev = event_at(&events, p.t_ns).unwrap();
        assert_eq!(ev["protocol"], "aprs", "{ev}");
        assert_eq!(frame.src, ev["src"], "{ev}");
        assert_eq!(frame.dst, ev["dst"], "{ev}");
        assert_eq!(ev["info"], String::from_utf8_lossy(&frame.info).as_ref());
        assert_eq!(frame.src, common::APRS_CALLS[0]);
        assert_eq!(p.comment.as_deref(), Some(comment.as_str()));
    }

    let rec = record(&node, &att);
    assert_eq!(rec.kind, "packets");
    assert_eq!(rec.input, Some(job.clone()));
    until(
        "the job's context names the capture",
        Duration::from_secs(5),
        || record(&node, &att).context["capture"] == r.id.as_str(),
    )
    .await;

    match std::process::Command::new("tshark")
        .arg("-r")
        .arg(&r.path)
        .args(["-T", "fields", "-e", "ax25.src"])
        .output()
    {
        Err(_) => eprintln!("tshark check skipped: tshark not on PATH"),
        Ok(out) => {
            assert!(out.status.success(), "{out:?}");
            let text = String::from_utf8_lossy(&out.stdout);
            let lines: Vec<&str> = text.lines().collect();
            assert!(lines.len() >= packets.len(), "{text}");
            assert!(
                lines.iter().all(|l| l.trim() == common::APRS_CALLS[0]),
                "{text}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_the_packets_job_closes_the_capture_and_leaves_the_decoder_running() {
    let dir = tempfile::tempdir().unwrap();
    let node = aprs_node(dir.path()).await;
    let job = start_aprs(&node).await;
    let att = attach_packets(&node, &job).await;
    until("a packet captured", Duration::from_secs(30), || {
        captures_of(&node, &job)
            .first()
            .is_some_and(|r| r.packets >= 1)
    })
    .await;
    node.apply(Op::JobStop { job: att.clone() }).await.unwrap();
    until("the capture closed", Duration::from_secs(10), || {
        record(&node, &att).state == JobState::Stopped
            && captures_of(&node, &job).iter().all(|r| !r.writing)
    })
    .await;
    let r = captures_of(&node, &job).remove(0);
    let bytes = std::fs::read(&r.path).unwrap();
    assert_eq!(pcapng::whole_blocks(&bytes), bytes.len(), "closed whole");
    let (_, packets) = pcapng::read(&bytes).unwrap();
    assert_eq!(packets.len() as u64, r.packets);
    assert_eq!(bytes.len() as u64, r.bytes);
    assert_eq!(record(&node, &job).state, JobState::Running);
    let events = collect_events(&node, &job);
    until("the decoder decodes on", Duration::from_secs(30), || {
        !events.lock().unwrap().is_empty()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_the_decoder_stops_its_packets_job() {
    let dir = tempfile::tempdir().unwrap();
    let node = aprs_node(dir.path()).await;
    let job = start_aprs(&node).await;
    let att = attach_packets(&node, &job).await;
    until("a packet captured", Duration::from_secs(30), || {
        captures_of(&node, &job)
            .first()
            .is_some_and(|r| r.packets >= 1)
    })
    .await;
    node.apply(Op::JobStop { job: job.clone() }).await.unwrap();
    until("the packets job stopped", Duration::from_secs(10), || {
        record(&node, &att).state == JobState::Stopped
            && captures_of(&node, &job).iter().all(|r| !r.writing)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retuned_input_is_captured_into_a_fresh_file_with_its_new_frequency() {
    let dir = tempfile::tempdir().unwrap();
    let node = aprs_node(dir.path()).await;
    let job = start_aprs(&node).await;
    let att = attach_packets(&node, &job).await;
    until("a packet captured", Duration::from_secs(30), || {
        captures_of(&node, &job)
            .first()
            .is_some_and(|r| r.packets >= 1)
    })
    .await;
    let up = common::APRS_HZ + common::APRS_OFFSET_HZ;
    let retune = Op::JobRetune {
        job: job.clone(),
        center_hz: Hz(up),
        bandwidth_hz: Hz(12_500),
        bin_hz: None,
        replace: vec![],
    };
    assert_eq!(node.apply(retune).await, Ok(OpResult::Job(job.clone())));
    until(
        "a second capture, the first closed",
        Duration::from_secs(30),
        || {
            let list = captures_of(&node, &job);
            list.len() == 2 && list[0].packets >= 1 && !list[1].writing
        },
    )
    .await;
    let list = captures_of(&node, &job);
    let (new, old) = (&list[0], &list[1]);
    assert_eq!(old.center_hz, Hz(common::APRS_HZ));
    let bytes = std::fs::read(&old.path).unwrap();
    assert_eq!(pcapng::whole_blocks(&bytes), bytes.len(), "closed whole");
    let (_, packets) = pcapng::read(&bytes).unwrap();
    assert!(!packets.is_empty());
    assert_eq!(new.center_hz, Hz(up));
    assert!(new.writing);
    let (_, packets) = read_capture(new);
    assert_eq!(
        packets[0].comment.as_deref(),
        Some(format!("job={} source=aprs freq_hz={up}", job.0).as_str())
    );
    let frame = zeta_dsp::ax25::parse_ax25_ui(&packets[0].data[1..]).unwrap();
    assert_eq!(frame.src, common::APRS_CALLS[1], "the station above");
    assert_eq!(record(&node, &att).state, JobState::Running);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deleting_a_capture_being_written_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let node = aprs_node(dir.path()).await;
    let job = start_aprs(&node).await;
    let att = attach_packets(&node, &job).await;
    until("a packet captured", Duration::from_secs(30), || {
        captures_of(&node, &job)
            .first()
            .is_some_and(|r| r.packets >= 1)
    })
    .await;
    let r = captures_of(&node, &job).remove(0);
    let delete = |capture: &str| Op::PacketsDelete {
        capture: capture.into(),
    };
    assert_eq!(
        node.apply(delete(&r.id)).await,
        Err(OpError::Other {
            message: format!(
                "capture {} is being written by job {}: stop the packets job first",
                r.id, att.0
            )
        })
    );
    assert!(r.path.exists(), "the file is intact");
    assert!(packets_list(&node).await.iter().any(|c| c.id == r.id));
    assert_eq!(
        node.apply(delete("01NOPE")).await,
        Err(OpError::Unknown {
            field: "capture".into(),
            value: "01NOPE".into(),
            known: vec![r.id.clone()],
        })
    );
    // Once the packets job stops, the capture may go.
    node.apply(Op::JobStop { job: att }).await.unwrap();
    until("the capture closed", Duration::from_secs(10), || {
        captures_of(&node, &job).iter().all(|c| !c.writing)
    })
    .await;
    assert_eq!(node.apply(delete(&r.id)).await, Ok(OpResult::Ok));
    assert!(!r.path.exists());
    assert!(packets_list(&node).await.is_empty());
}

/// Ruling R8: a capture that cannot be written ends its `packets` job `failed`, naming
/// the file error, and its decoder decodes on. The store's directory is replaced by a
/// plain file before the first packet, so the capture's first file cannot be made.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_packets_job_whose_writes_fail_fails_and_the_decoder_runs_on() {
    let dir = tempfile::tempdir().unwrap();
    let node = aprs_node(dir.path()).await;
    let job = start_aprs(&node).await;
    let store = node.packets().dir().to_path_buf();
    std::fs::remove_dir_all(&store).unwrap();
    std::fs::write(&store, b"not a directory").unwrap();
    let att = attach_packets(&node, &job).await;
    until("the packets job failed", Duration::from_secs(30), || {
        matches!(record(&node, &att).state, JobState::Failed { .. })
    })
    .await;
    let JobState::Failed { reason } = record(&node, &att).state else {
        unreachable!()
    };
    assert!(
        reason.contains(&store.display().to_string()),
        "names the file error: {reason}"
    );
    assert_eq!(record(&node, &job).state, JobState::Running);
    let events = collect_events(&node, &job);
    until("the decoder decodes on", Duration::from_secs(30), || {
        !events.lock().unwrap().is_empty()
    })
    .await;
    assert_eq!(record(&node, &job).state, JobState::Running);
}

/// The frame row of `zeta-core`'s `tests/frame_rows.rs`: 4800 Bd 2-FSK frames with a
/// 16-bit sync, 272 bits long, CRC-16/CCITT, at 433.92 MHz.
const FRAME_ROW: &str = "fsk_test_frames";
const FRAME_HZ: u64 = 433_920_000;

fn frame_row() -> serde_json::Value {
    let framing = serde_json::json!({"sync": {"hex": "2dd4", "bits": 16},
        "length": {"fixed_bits": 272}, "crc": "crc16-ccitt"});
    serde_json::json!({
        "id": FRAME_ROW,
        "name": "FSK test frames",
        "family": "general",
        "band": {"bandwidth_hz": 12_500, "centers_hz": [FRAME_HZ]},
        "modulation": ["fsk", "fm"],
        "label": "FSK frames",
        "protocol": "frame",
        "decoder": {
            "kind": "frame",
            "demod": {"modulation": "fsk", "symbol_rate_baud": 4_800, "order": 2, "tone_spacing_hz": null},
            "framing": framing,
            "parser": "opaque"
        },
        "event": {
            "schema": {"type": "object", "properties": {
                "protocol": {"type": "string"},
                "length_bytes": {"type": "integer"},
                "bytes": {"type": "string"},
                "crc_ok": {"type": "boolean"}
            }},
            "summary": "frame {length_bytes} B"
        },
        "identify": {"proof": "field", "field": "crc_ok", "min_events": 1, "dwell_ms": 10000,
            "min_span_ms": 1000, "quick": "frame"},
        "catalogue": []
    })
}

/// [`frame_row`]'s capture: the `framing` synthesiser family's 8 frames of 32
/// pseudo-random bytes at 20 dB, sampled at 48 kHz.
fn frame_fixture(dir: &Path) -> std::path::PathBuf {
    let params = serde_json::json!({"family": "framing", "modulation": "fsk",
        "symbol_rate_baud": 4_800, "deviation_hz": 2400.0, "frames": 8, "body_bytes": 32,
        "seed": 7, "framing": frame_row()["decoder"]["framing"], "repeat": 1, "snr_db": 20.0});
    let (spec, imp) = zeta_dsp::synth::parse_params(params.as_object().unwrap().clone()).unwrap();
    let iq = zeta_dsp::synth::synthesise(&spec, &imp, 48_000, FRAME_HZ);
    zeta_dsp::fixtures::write_fixture(dir, "frames", &iq, 48_000, FRAME_HZ)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_frame_row_is_captured_as_user0() {
    let dir = tempfile::tempdir().unwrap();
    let modes = dir.path().join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    std::fs::write(
        modes.join(format!("{FRAME_ROW}.json")),
        frame_row().to_string(),
    )
    .unwrap();
    let opts = FileOpts {
        realtime: true,
        block_samples: 2_400,
        repeat: true,
        tune_range: None,
    };
    let rx = SourceId("rx".into());
    let src = Arc::new(FileSource::open(rx.clone(), &frame_fixture(dir.path()), opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    let cfg = NodeConfig {
        modes_dir: Some(modes),
        ..cfg(dir.path())
    };
    let node = Node::open(cfg, packets_registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let job = match node
        .apply(job_start_op(FRAME_ROW, rx, FRAME_HZ, 12_500))
        .await
    {
        Ok(OpResult::Job(id)) => id,
        r => panic!("{FRAME_ROW} start: {r:?}"),
    };
    let events = collect_events(&node, &job);
    attach_packets(&node, &job).await;
    until("a frame captured", Duration::from_secs(30), || {
        captures_of(&node, &job)
            .first()
            .is_some_and(|r| r.packets >= 1)
    })
    .await;
    let r = captures_of(&node, &job).remove(0);
    assert_eq!(r.link_type, LinkType::User0);
    assert_eq!(r.mode.as_deref(), Some(FRAME_ROW));
    let (link, packets) = read_capture(&r);
    assert_eq!(link, 147);
    until("the frame's event", Duration::from_secs(5), || {
        event_at(&events, packets[0].t_ns).is_some()
    })
    .await;
    let ev = event_at(&events, packets[0].t_ns).unwrap();
    let hex = ev["bytes"].as_str().expect("the event's bytes");
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    assert_eq!(packets[0].data, bytes, "{ev}");
}
