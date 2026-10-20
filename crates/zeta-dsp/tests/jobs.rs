//! End-to-end jobs: file source → node → DSP graph → bus.
#![cfg(feature = "test-fixtures")]

use std::sync::Arc;
use std::time::Duration;

use zeta_core::job::JobState;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{AUDIO_RATE_HZ, JobKind, Op, OpResult};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, StageRegistry};
use zeta_core::stream::Payload;
use zeta_core::types::{Hz, JobId, SourceId, StreamId, StreamKind};
use zeta_dsp::fixtures::{nbfm_modulate, tone, write_fixture};
use zeta_source_file::{FileOpts, FileSource};

/// Frequency (Hz) of the largest DFT bin of `x` in `[1, len/2)` and the RMS of `x`.
fn peak_and_rms(x: &[f32], rate_hz: f64) -> (f64, f64) {
    let n = x.len();
    let mag = |k: usize| {
        let (re, im) = x
            .iter()
            .enumerate()
            .fold((0.0f64, 0.0f64), |(re, im), (i, &v)| {
                let ph = -2.0 * std::f64::consts::PI * ((k * i) % n) as f64 / n as f64;
                (re + v as f64 * ph.cos(), im + v as f64 * ph.sin())
            });
        re * re + im * im
    };
    let k = (1..n / 2)
        .max_by(|&a, &b| mag(a).partial_cmp(&mag(b)).unwrap())
        .unwrap();
    let rms = (x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / n as f64).sqrt();
    (k as f64 * rate_hz / n as f64, rms)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fm_demod_job_produces_audio_with_tone() {
    const RATE: u32 = 48_000;
    const CENTER: u64 = 100_000_000;
    let dir = tempfile::tempdir().unwrap();
    // One second of a 1 kHz tone at full deviation, carried 5 kHz above the file's centre.
    let audio = tone(RATE, RATE as usize, 1_000.0, 1.0);
    let iq = nbfm_modulate(&audio, RATE, RATE, 2_500.0, 5_000.0);
    let meta = write_fixture(dir.path(), "nbfm", &iq, RATE, CENTER);
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
        state_dir: dir.path().join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
    };
    let node = Node::open(cfg, reg, vec![port]).await.unwrap();
    src.start();

    let OpResult::Job(id) = node
        .apply(Op::JobStart {
            kind: JobKind::FmDemod,
            source: SourceId("file0".into()),
            center_hz: Hz(100_005_000),
            bandwidth_hz: Hz(12_500),
            deviation_hz: Some(2_500), // the fixture's deviation
            sideband: None,
            elastic: false,
            replace: vec![],
        })
        .await
        .unwrap()
    else {
        panic!("expected a job id")
    };

    let bus = node.bus();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let stream = loop {
        let found = bus
            .list()
            .into_iter()
            .find(|s| s.kind == StreamKind::Audio && s.provenance.job.as_ref() == Some(&id));
        if let Some(s) = found {
            break s.id;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no audio stream for the job"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let mut rx = bus.subscribe(&stream).unwrap();

    let mut samples: Vec<f32> = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while samples.len() < RATE as usize {
            match rx.recv().await {
                Ok(f) => {
                    let Payload::Audio {
                        sample_rate_hz,
                        channels: _,
                        samples: s,
                    } = &f.payload
                    else {
                        panic!("expected an audio payload")
                    };
                    assert_eq!(*sample_rate_hz, Hz(RATE as u64));
                    samples.extend_from_slice(s);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(e) => panic!("audio stream ended: {e}"),
            }
        }
    })
    .await
    .expect("1 s of audio within 5 s");

    let (peak, rms) = peak_and_rms(&samples[samples.len() - 4_800..], RATE as f64);
    eprintln!("fm_demod end to end: peak {peak} Hz, rms {rms:.4}");
    assert_eq!(peak, 1_000.0);
    assert!((rms - 0.707).abs() <= 0.0707, "rms {rms}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aprs_decode_job_decodes_fixture_packet() {
    const RATE: u32 = 48_000;
    const CENTER: u64 = 144_800_000;
    let dir = tempfile::tempdir().unwrap();
    let iq = zeta_dsp::fixtures::aprs_packet_iq("VK3ABC", "APRS", ">hi", RATE, 0.0);
    let meta = write_fixture(dir.path(), "aprs", &iq, RATE, CENTER);
    let opts = FileOpts {
        // Paced like a radio: an unpaced source outruns the debug-build pipeline,
        // lagged frames are skipped and the packet is damaged.
        realtime: true,
        block_samples: 4800,
        repeat: true,
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(SourceId("file0".into()), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    let mut reg = StageRegistry::new();
    zeta_dsp::register_all(&mut reg);
    let cfg = NodeConfig {
        state_dir: dir.path().join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
    };
    let node = Node::open(cfg, reg, vec![port]).await.unwrap();
    src.start();

    let started = tokio::time::Instant::now();
    let OpResult::Job(id) = node
        .apply(Op::JobStart {
            kind: JobKind::AprsDecode,
            source: SourceId("file0".into()),
            center_hz: Hz(CENTER),
            bandwidth_hz: Hz(12_500),
            deviation_hz: None,
            sideband: None,
            elastic: false,
            replace: vec![],
        })
        .await
        .unwrap()
    else {
        panic!("expected a job id")
    };

    let bus = node.bus();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let stream = loop {
        let found = bus
            .list()
            .into_iter()
            .find(|s| s.kind == StreamKind::Event && s.provenance.job.as_ref() == Some(&id));
        if let Some(s) = found {
            break s.id;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no event stream for the job"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let mut rx = bus.subscribe(&stream).unwrap();

    let ev = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match rx.recv().await {
                Ok(f) => {
                    let Payload::Event(v) = f.payload.clone() else {
                        panic!("expected an event payload")
                    };
                    break v;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(e) => panic!("event stream ended: {e}"),
            }
        }
    })
    .await
    .expect("an event within 5 s");
    eprintln!(
        "aprs_decode end to end: first event after {:?}: {ev}",
        started.elapsed()
    );
    assert_eq!(ev["src"], "VK3ABC");
    assert_eq!(ev["dst"], "APRS");
    assert_eq!(ev["info"], ">hi");
    assert_eq!(ev["fcs_ok"], true);
}

/// A node over one SigMF fixture `iq` at `rate_hz` centred on `center_hz`,
/// playback started, with the DSP stages registered.
async fn node_over(
    dir: &std::path::Path,
    iq: &[num_complex::Complex<f32>],
    rate_hz: u32,
    center_hz: u64,
    opts: FileOpts,
) -> Node {
    let meta = write_fixture(dir, "fixture", iq, rate_hz, center_hz);
    let src = Arc::new(FileSource::open(SourceId("file0".into()), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    let mut reg = StageRegistry::new();
    zeta_dsp::register_all(&mut reg);
    let cfg = NodeConfig {
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
    };
    let node = Node::open(cfg, reg, vec![port]).await.unwrap();
    src.start();
    node
}

async fn start(node: &Node, kind: JobKind, center_hz: u64, bandwidth_hz: u64) -> JobId {
    let OpResult::Job(id) = node
        .apply(Op::JobStart {
            kind,
            source: SourceId("file0".into()),
            center_hz: Hz(center_hz),
            bandwidth_hz: Hz(bandwidth_hz),
            deviation_hz: None,
            sideband: None,
            elastic: false,
            replace: vec![],
        })
        .await
        .unwrap()
    else {
        panic!("expected a job id")
    };
    id
}

/// The job's stream of `kind`, waiting up to 2 s for it to be registered.
async fn job_stream(node: &Node, id: &JobId, kind: StreamKind) -> StreamId {
    let bus = node.bus();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let found = bus
            .list()
            .into_iter()
            .find(|s| s.kind == kind && s.provenance.job.as_ref() == Some(id));
        if let Some(s) = found {
            return s.id;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no {kind:?} stream for the job"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Collect `want` audio samples from the job, asserting every frame is at `rate_hz`.
async fn collect_audio(node: &Node, id: &JobId, rate_hz: u64, want: usize) -> Vec<f32> {
    let stream = job_stream(node, id, StreamKind::Audio).await;
    let mut rx = node.bus().subscribe(&stream).unwrap();
    let mut samples: Vec<f32> = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while samples.len() < want {
            match rx.recv().await {
                Ok(f) => {
                    let Payload::Audio {
                        sample_rate_hz,
                        channels: _,
                        samples: s,
                    } = &f.payload
                    else {
                        panic!("expected an audio payload")
                    };
                    assert_eq!(*sample_rate_hz, Hz(rate_hz));
                    samples.extend_from_slice(s);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(e) => panic!("audio stream ended: {e}"),
            }
        }
    })
    .await
    .expect("audio within 10 s");
    samples
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fm_demod_job_on_12_khz_source() {
    const RATE: u32 = 12_000;
    const CENTER: u64 = 7_100_000;
    let dir = tempfile::tempdir().unwrap();
    let audio = tone(48_000, 48_000, 1_000.0, 1.0);
    let iq = nbfm_modulate(&audio, 48_000, RATE, 2_500.0, 0.0);
    let opts = FileOpts {
        realtime: false,
        block_samples: 1_200,
        repeat: true,
        tune_range: None,
    };
    let node = node_over(dir.path(), &iq, RATE, CENTER, opts).await;
    let id = start(&node, JobKind::FmDemod, CENTER, 10_000).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let rec = node.job_list().into_iter().find(|r| r.id == id).unwrap();
    assert_eq!(rec.state, JobState::Running);

    // Upsampled from the 12 kHz channel to 48 kHz audio.
    let samples = collect_audio(&node, &id, AUDIO_RATE_HZ, AUDIO_RATE_HZ as usize).await;
    // 4 800 samples: 10 Hz bins at 48 kHz.
    let (peak, rms) = peak_and_rms(&samples[samples.len() - 4_800..], AUDIO_RATE_HZ as f64);
    eprintln!("fm_demod at 12 kHz: peak {peak} Hz, rms {rms:.4}");
    assert_eq!(peak, 1_000.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fm_demod_job_at_2_048_msps() {
    const RATE: u32 = 2_048_000;
    const CENTER: u64 = 144_000_000;
    let dir = tempfile::tempdir().unwrap();
    // 0.5 s of a 1 kHz tone (whole cycles, so the repeat is seamless) 100 kHz above centre.
    let audio = tone(48_000, 24_000, 1_000.0, 1.0);
    let iq = nbfm_modulate(&audio, 48_000, RATE, 2_500.0, 100_000.0);
    let opts = FileOpts {
        realtime: false,
        block_samples: 20_480,
        repeat: true,
        tune_range: None,
    };
    let node = node_over(dir.path(), &iq, RATE, CENTER, opts).await;
    let id = start(&node, JobKind::FmDemod, CENTER + 100_000, 12_500).await;

    // Channelised at 51.2 kHz, resampled to 48 kHz audio.
    let samples = collect_audio(&node, &id, AUDIO_RATE_HZ, AUDIO_RATE_HZ as usize / 2).await;
    // 4 800 samples: 10 Hz bins at 48 kHz.
    let (peak, rms) = peak_and_rms(&samples[samples.len() - 4_800..], AUDIO_RATE_HZ as f64);
    eprintln!("fm_demod at 2.048 MS/s: peak {peak} Hz, rms {rms:.4}");
    assert_eq!(peak, 1_000.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aprs_decode_job_decodes_at_12_khz() {
    const RATE: u32 = 12_000;
    const CENTER: u64 = 144_800_000;
    let dir = tempfile::tempdir().unwrap();
    let iq = zeta_dsp::fixtures::aprs_packet_iq("VK3ABC", "APRS", ">hi", RATE, 0.0);
    let opts = FileOpts {
        realtime: true,
        block_samples: 1_200,
        repeat: true,
        tune_range: None,
    };
    let node = node_over(dir.path(), &iq, RATE, CENTER, opts).await;
    let started = tokio::time::Instant::now();
    let id = start(&node, JobKind::AprsDecode, CENTER, 10_000).await;
    let stream = job_stream(&node, &id, StreamKind::Event).await;
    let mut rx = node.bus().subscribe(&stream).unwrap();
    let left = Duration::from_secs(3).saturating_sub(started.elapsed());
    let ev = tokio::time::timeout(left, async {
        loop {
            match rx.recv().await {
                Ok(f) => {
                    let Payload::Event(v) = f.payload.clone() else {
                        panic!("expected an event payload")
                    };
                    break v;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(e) => panic!("event stream ended: {e}"),
            }
        }
    })
    .await
    .expect("an event within 3 s");
    eprintln!(
        "aprs_decode at 12 kHz: first event after {:?}: {ev}",
        started.elapsed()
    );
    assert_eq!(ev["src"], "VK3ABC");
    assert_eq!(ev["fcs_ok"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn measure_job_measures_only_its_band() {
    const CENTER: u64 = 144_800_000;
    let dir = tempfile::tempdir().unwrap();
    // APRS 12 kHz above centre, an FM tone 10 dB stronger 12 kHz below it.
    let aprs = zeta_dsp::fixtures::aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 12_000.0);
    let audio = tone(48_000, aprs.len(), 1_000.0, 1.0);
    let fm = nbfm_modulate(&audio, 48_000, 48_000, 2_500.0, -12_000.0);
    let iq: Vec<_> = aprs.iter().zip(&fm).map(|(a, t)| a + t * 3.16).collect();
    let opts = FileOpts {
        realtime: true,
        block_samples: 4_800,
        repeat: true,
        tune_range: None,
    };
    let node = node_over(dir.path(), &iq, 48_000, CENTER, opts).await;
    let id = start(&node, JobKind::Measure, CENTER + 12_000, 12_500).await;
    let stream = job_stream(&node, &id, StreamKind::Event).await;
    let mut rx = node.bus().subscribe(&stream).unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match rx.recv().await {
                Ok(f) => {
                    let Payload::Event(v) = f.payload.clone() else {
                        panic!("expected an event payload")
                    };
                    break v;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(e) => panic!("event stream ended: {e}"),
            }
        }
    })
    .await
    .expect("a measure event within 10 s");
    eprintln!("measure job: {ev}");
    let center = ev["center_hz"].as_u64().unwrap();
    assert!(center.abs_diff(CENTER + 12_000) <= 200, "{ev}");
    assert!(ev["bandwidth_hz"].as_u64().unwrap() <= 12_500, "{ev}");
    assert_eq!(ev["label"]["name"], "fsk", "{ev}");
}

/// A spectrum job persisted before plan 8 has an `fft` node without `rows_per_s`;
/// it still validates on resume (default 20) and emits v2 rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v1_spectrum_graph_without_rows_per_s_still_launches() {
    const CENTER: u64 = 100_000_000;
    let dir = tempfile::tempdir().unwrap();
    let iq = tone(48_000, 48_000, 5_000.0, 0.5)
        .into_iter()
        .map(|v| num_complex::Complex::new(v, 0.0))
        .collect::<Vec<_>>();
    let opts = || FileOpts {
        realtime: true,
        block_samples: 4_800,
        repeat: true,
        tune_range: None,
    };
    let node = node_over(dir.path(), &iq, 48_000, CENTER, opts()).await;
    let id = start(&node, JobKind::Spectrum, CENTER, 48_000).await;
    drop(node);
    tokio::time::sleep(Duration::from_millis(50)).await;
    // Rewrite the persisted record as a pre-plan-8 one: no rows_per_s on the fft node.
    let path = dir
        .path()
        .join("state")
        .join("jobs")
        .join(format!("{}.json", id.0));
    let mut rec: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut removed = 0;
    for n in rec["graph"]["nodes"].as_array_mut().unwrap() {
        if n["kind"] == "fft" {
            removed += n["params"]
                .as_object_mut()
                .unwrap()
                .remove("rows_per_s")
                .is_some() as usize;
        }
    }
    assert_eq!(removed, 1, "the spectrum graph has one fft node: {rec}");
    std::fs::write(&path, serde_json::to_vec_pretty(&rec).unwrap()).unwrap();

    let node = node_over(dir.path(), &iq, 48_000, CENTER, opts()).await;
    assert!(node.load_errors().is_empty(), "{:?}", node.load_errors());
    let job = node.job_list().into_iter().find(|j| j.id == id).unwrap();
    assert_eq!(job.state, JobState::Running, "{job:?}");
    let stream = job_stream(&node, &id, StreamKind::Event).await;
    let mut rx = node.bus().subscribe(&stream).unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match rx.recv().await {
                Ok(f) => {
                    let Payload::Event(v) = f.payload.clone() else {
                        panic!("expected an event payload")
                    };
                    break v;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(e) => panic!("event stream ended: {e}"),
            }
        }
    })
    .await
    .expect("a spectrum row within 10 s");
    assert_eq!(ev["rows_per_s"], 20, "{ev}");
    assert!(ev["q"].is_array(), "v2 row: {ev}");
    assert!(ev.get("db").is_none(), "v2 rows carry no db key: {ev}");
}
