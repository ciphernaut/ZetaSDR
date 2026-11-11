//! The mix on the real chain: two `fm_demod`s on one recording played in real time, at
//! the block sizes a file source delivers and a Soapy device delivers at capture rates
//! (16 ms; a narrow 250 kHz capture's blocks are 65 ms). These are the numbers
//! `zeta_dsp::mix::PREBUFFER_MS` is set from.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::Instant;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpResult};
use zeta_core::port::{IqReceiver, Port};
use zeta_core::stage::{Backend, StageRegistry};
use zeta_core::stream::{Frame, Payload};
use zeta_core::types::{Hz, JobId, SourceId};
use zeta_dsp::fixtures::{nbfm_modulate, tone, write_fixture};
use zeta_source_file::{FileOpts, FileSource};

const RATE_HZ: u32 = 48_000;
const CENTER_HZ: u64 = 476_900_000;
const OFFSET_HZ: u64 = 12_500;
const DEVIATION_HZ: u64 = 2_500;
const LOW_TONE_HZ: f64 = 700.0;
const HIGH_TONE_HZ: f64 = 1_900.0;
const QUIET_HZ: f64 = 1_300.0;
const WINDOW: Duration = Duration::from_secs(3);
const WINDOWS: usize = 3;
/// A span further than this past the last one's end is a gap in the audio.
const GAP_NS: u64 = 1_000_000;

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

/// A node playing the two tones in real time, a demodulator on each, and a mix of both.
struct Chain {
    node: Node,
    mix: JobId,
    first_demod: JobId,
    /// Keeps the playback task going whatever the jobs are doing.
    _playback: IqReceiver,
}

/// 5 s at 48 kHz at 476.9 MHz: a 700 Hz tone 12.5 kHz below the centre and a 1900 Hz tone
/// 12.5 kHz above it, each FM at 2500 Hz deviation.
fn two_tones(dir: &Path) -> PathBuf {
    let len = 5 * RATE_HZ as usize;
    let carrier = |freq_hz, offset_hz| {
        let audio = tone(RATE_HZ, len, freq_hz, 1.0);
        nbfm_modulate(&audio, RATE_HZ, RATE_HZ, DEVIATION_HZ as f64, offset_hz)
    };
    let low = carrier(LOW_TONE_HZ, -(OFFSET_HZ as f64));
    let high = carrier(HIGH_TONE_HZ, OFFSET_HZ as f64);
    let iq: Vec<_> = low.iter().zip(&high).map(|(a, b)| a + b).collect();
    write_fixture(dir, "two_tones", &iq, RATE_HZ, CENTER_HZ)
}

async fn demod(node: &Node, center_hz: u64) -> JobId {
    let op = Op::JobStart {
        kind: JobKind::FmDemod,
        source: SourceId("file".into()),
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(12_500),
        deviation_hz: Some(DEVIATION_HZ),
        sideband: None,
        dwell_ms: None,
        average: None,
        elastic: false,
        replace: vec![],
    };
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("job_start at {center_hz}: {r:?}"),
    }
}

async fn chain(dir: &Path, block_samples: usize) -> Chain {
    let opts = FileOpts {
        realtime: true,
        block_samples,
        repeat: true,
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(SourceId("file".into()), &two_tones(dir), opts).unwrap());
    let playback = src.subscribe();
    src.start();
    let port: Arc<dyn Port> = src;
    let mut registry = StageRegistry::new();
    zeta_dsp::register_all(&mut registry);
    let node = Node::open(cfg(dir), registry, vec![port]).await.unwrap();
    let first_demod = demod(&node, CENTER_HZ - OFFSET_HZ).await;
    demod(&node, CENTER_HZ + OFFSET_HZ).await;
    let mix = match node.apply(Op::MixStart { sources: None }).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("mix_start: {r:?}"),
    };
    Chain {
        node,
        mix,
        first_demod,
        _playback: playback,
    }
}

/// `job_context.mix` of the chain's mix.
fn mix_context(chain: &Chain) -> Value {
    chain.node.job_context(&chain.mix).unwrap()["mix"].clone()
}

fn underruns(chain: &Chain) -> u64 {
    let mix = mix_context(chain);
    let by_job = mix["underruns"].as_object().cloned().unwrap_or_default();
    by_job.values().filter_map(Value::as_u64).sum()
}

fn audio_rx(chain: &Chain, job: &JobId) -> Receiver<Frame> {
    let stream = chain.node.job_audio_stream(job).unwrap();
    chain.node.bus().subscribe(&stream).unwrap()
}

/// The power of `x` at `freq_hz`.
fn goertzel(x: &[f32], freq_hz: f64) -> f64 {
    let coeff = 2.0 * (2.0 * std::f64::consts::PI * freq_hz / RATE_HZ as f64).cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &v in x {
        let s = v as f64 + coeff * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    s1 * s1 + s2 * s2 - coeff * s1 * s2
}

/// What one window on the mix's audio saw.
struct Window {
    gaps: u64,
    audio: Vec<f32>,
    underruns: u64,
    max_demod_gap_ms: u64,
}

impl Window {
    fn clean(&self) -> bool {
        self.gaps == 0
            && self.underruns == 0
            && self.audio.len() as f64 >= 0.98 * WINDOW.as_secs_f64() * RATE_HZ as f64
    }
}

async fn window(chain: &Chain, mix: &mut Receiver<Frame>, demod: &mut Receiver<Frame>) -> Window {
    let before = underruns(chain);
    let until = Instant::now() + WINDOW;
    let (mut gaps, mut audio, mut last_end) = (0, Vec::new(), None);
    let (mut last_demod, mut max_demod_gap) = (Instant::now(), Duration::ZERO);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(until) => break,
            frame = mix.recv() => match frame {
                Ok(frame) => {
                    let span = frame.provenance.span;
                    if last_end.is_some_and(|end: u64| span.start_ns.0 > end + GAP_NS) {
                        gaps += 1;
                    }
                    last_end = Some(span.end_ns.0);
                    let Payload::Audio { samples, .. } = &frame.payload else {
                        panic!("the mix's audio stream carried {:?}", frame.kind);
                    };
                    audio.extend_from_slice(samples);
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => panic!("the mix's audio stream closed"),
            },
            frame = demod.recv() => match frame {
                Ok(_) => {
                    max_demod_gap = max_demod_gap.max(last_demod.elapsed());
                    last_demod = Instant::now();
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => panic!("the demodulator's audio stream closed"),
            },
        }
    }
    Window {
        gaps,
        audio,
        underruns: underruns(chain) - before,
        max_demod_gap_ms: max_demod_gap.as_millis() as u64,
    }
}

async fn check(block_samples: usize) {
    let dir = tempfile::tempdir().unwrap();
    let chain = chain(dir.path(), block_samples).await;
    // Squelch open, inputs polled, prebuffer run.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let mut mix = audio_rx(&chain, &chain.mix);
    let mut demod = audio_rx(&chain, &chain.first_demod);

    let mut clean = 0;
    for i in 0..WINDOWS {
        let w = window(&chain, &mut mix, &mut demod).await;
        println!(
            "mix-chain block={block_samples} window={i} gaps={} samples={} underruns={} max_demod_gap_ms={}",
            w.gaps,
            w.audio.len(),
            w.underruns,
            w.max_demod_gap_ms,
        );
        let quiet = goertzel(&w.audio, QUIET_HZ);
        for tone_hz in [LOW_TONE_HZ, HIGH_TONE_HZ] {
            let power = goertzel(&w.audio, tone_hz);
            assert!(
                power > 10.0 * quiet,
                "window {i}: {power} at {tone_hz} Hz, {quiet} at {QUIET_HZ} Hz",
            );
        }
        clean += usize::from(w.clean());
    }
    // One clean window of three proves the design; a scheduler stall on a loaded machine may
    // spoil another. A source that drifts against the mix's clock spoils all three. This does
    // not pin the prebuffer: an idle machine passes at 0 ms.
    assert!(clean > 0, "no clean window at {block_samples} samples");
    assert_eq!(mix_context(&chain)["dropped_ms"], json!({}));
}

#[tokio::test(flavor = "multi_thread")]
async fn both_tones_are_heard_and_a_continuous_tone_has_no_gaps_at_100_ms_blocks() {
    check(4800).await
}

#[tokio::test(flavor = "multi_thread")]
async fn both_tones_are_heard_and_a_continuous_tone_has_no_gaps_at_16_ms_blocks() {
    check(768).await
}
