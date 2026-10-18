//! End-to-end jobs: file source → node → DSP graph → bus.
#![cfg(feature = "test-fixtures")]

use std::sync::Arc;
use std::time::Duration;

use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpResult};
use zeta_core::port::{Port, Tuning};
use zeta_core::stage::{Backend, StageRegistry};
use zeta_core::stream::Payload;
use zeta_core::types::{Hz, SourceId, StreamKind};
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
    };
    let src = Arc::new(FileSource::open(SourceId("file0".into()), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    let mut reg = StageRegistry::new();
    zeta_dsp::register_all(&mut reg);
    let cfg = NodeConfig {
        state_dir: dir.path().join("state"),
        ring_seconds: 1.0,
        backends: vec![Backend::Cpu],
        resume: true,
    };
    let node = Node::open(cfg, reg, vec![port]).await.unwrap();
    src.start();

    // A file port tunes only to its own centre, so a spectrum job holds it
    // there and the fm_demod job shares it as a sub-band of the recording.
    let source = SourceId("file0".into());
    let held = node
        .apply(Op::JobStart {
            kind: JobKind::Spectrum,
            source: source.clone(),
            center_hz: Hz(CENTER),
            bandwidth_hz: Hz(RATE as u64),
        })
        .await
        .unwrap();
    assert!(matches!(held, OpResult::Job(_)));
    let tuning = Tuning {
        center_hz: Hz(100_005_000),
        bandwidth_hz: Hz(12_500),
    };
    let kind = JobKind::FmDemod;
    let id = node
        .job_start(
            kind.name(),
            source.clone(),
            tuning,
            kind.expand(&source, tuning),
        )
        .await
        .unwrap();

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
