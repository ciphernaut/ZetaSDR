//! The `watch` job: a controller over `fft → detect` that identifies each new signal in
//! its band, keeps the proved decoder (in its capture, or on a free source) and owns
//! what it starts. Run over 1.024 MHz file sources (under the channel bank's 2 Msps
//! threshold) carrying a 1200-baud POCSAG page, an FSK burst no row proves, or noise,
//! 300 kHz above the capture's centre and repeating every 2 s.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use num_complex::Complex;
use serde_json::Value;
use tokio::sync::broadcast;
use zeta_core::capture::{CaptureMeta, write_cf32};
use zeta_core::graph::{Edge, Graph, Node as GraphNode, NodeId};
use zeta_core::job::JobState;
use zeta_core::knowledge::{Allocation, KnowledgeProvider, Signature};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpResult};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{
    Backend, PassthroughStage, Stage, StageContext, StageError, StageRegistry, StageSpec,
};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Capabilities, Hz, JobId, SourceId, StreamKind, Timestamp};
use zeta_source_file::{FileOpts, FileSource};

const RATE_HZ: u32 = 1_024_000;
/// A well-known POCSAG centre: the burst sits here, so `pocsag_decode` is tried first.
const POCSAG_HZ: u64 = 148_637_500;
const OFFSET_HZ: u64 = 300_000;
/// The capture's centre: the burst is [`OFFSET_HZ`] above it.
const CENTER_HZ: u64 = POCSAG_HZ - OFFSET_HZ;
/// One burst per this many samples (2 s).
const CYCLE: usize = 2 * RATE_HZ as usize;
/// The detector's timing for the tests' time scale: a 2 s cycle stays one signal.
const PERSIST_MS: u64 = 1_500;
const GONE_MS: u64 = 4_000;
/// The re-adoption tests' `gone_ms`: a wider margin for a re-adopted decoder's signal to be
/// seen again on a loaded host (the semantics are those of [`GONE_MS`]).
const READOPT_GONE_MS: u64 = 6_000;

/// Each node runs real-time sources and the watch timing is process-wide: one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Fixtures {
    _dir: tempfile::TempDir,
    /// The POCSAG page every 2 s.
    pocsag: PathBuf,
    /// A random 2400-baud GFSK burst every 2 s: no row proves it.
    fsk: PathBuf,
    /// The rows of the in-crate decoders with a 1500 ms dwell, for the FSK test's pace.
    quick_modes: PathBuf,
    /// Eight CRC-framed 4800-baud FSK frames every 2 s: [`FRAME_ROW`] proves them.
    framed: PathBuf,
    /// [`Fixtures::quick_modes`] and [`FRAME_ROW`].
    frame_modes: PathBuf,
    /// Three pages at [`SPAN_OFFSETS_HZ`] in a [`SPAN_LOOP`] loop, each every 2 s, the
    /// third only in the loop's seconds 6-14.
    pocsag3: PathBuf,
    /// `pocsag3`'s loop with its pages at [`KNOWN_OFFSETS_HZ`]: none on `pocsag_decode`'s
    /// listed centre, the third (the register's channel) only in the loop's seconds 6-14.
    known3: PathBuf,
    /// The page at [`OFFSET_HZ`] beside the GFSK burst at [`FSK_BESIDE_HZ`], every 2 s.
    pocsag_fsk: PathBuf,
    /// A GFSK burst about 60 kHz wide at [`FSK_BESIDE_HZ`] every 2 s, at the suite's noise.
    wide: PathBuf,
    /// The page at [`OFFSET_HZ`] every 2 s beside a continuous band of shaped noise
    /// [`NOISE_BAND_HZ`] wide at [`NOISE_OFFSET_HZ`]: it fits `pocsag_decode`'s width and
    /// decodes nothing.
    noise12k: PathBuf,
}

/// The span tests' pages, above the capture's centre.
const SPAN_OFFSETS_HZ: [u64; 3] = [200_000, 300_000, 400_000];
/// The known-rank test's pages, above the capture's centre: two unknown pages below
/// [`OFFSET_HZ`] (`pocsag_decode`'s listed centre, where no page sits) and the register's
/// channel above it, which comes on air only after the slot is taken.
const KNOWN_OFFSETS_HZ: [u64; 3] = [150_000, 200_000, 400_000];
/// The span fixture's loop (20 s): ten page cycles.
const SPAN_LOOP: usize = 10 * CYCLE;
/// The cycles of [`SPAN_LOOP`] the third page is in (seconds 6-14).
const SPAN_THIRD_CYCLES: std::ops::Range<usize> = 3..7;
/// Where `pocsag_fsk`'s GFSK burst sits above the capture's centre.
const FSK_BESIDE_HZ: u64 = 100_000;

/// The frame row of the frame-row watch test, at [`POCSAG_HZ`].
const FRAME_ROW: &str = "fsk_test_frames";
const FRAME_BAUD: u32 = 4_800;

/// The frame row's framing: a 16-bit sync, a fixed 272-bit frame, a CRC-16/CCITT.
fn frame_framing() -> Value {
    serde_json::json!({"sync": {"hex": "2dd4", "bits": 16}, "length": {"fixed_bits": 272},
        "crc": "crc16-ccitt"})
}

/// The frame row (as `tests/frame_rows.rs`) on the fixtures' signal centre, with a 4 s
/// dwell.
fn frame_row() -> Value {
    serde_json::json!({
        "id": FRAME_ROW,
        "name": "FSK test frames",
        "family": "general",
        "band": {"bandwidth_hz": 12_500, "centers_hz": [POCSAG_HZ]},
        "modulation": ["fsk", "fm"],
        "label": "FSK frames",
        "protocol": "frame",
        "decoder": {
            "kind": "frame",
            "demod": {"modulation": "fsk", "symbol_rate_baud": FRAME_BAUD, "order": 2,
                "tone_spacing_hz": null},
            "framing": frame_framing(),
            "parser": "opaque"
        },
        "event": {
            "schema": {"type": "object", "properties": {
                "protocol": {"type": "string"},
                "sync": {"type": "string"},
                "length_bytes": {"type": "integer"},
                "bytes": {"type": "string"},
                "crc_ok": {"type": "boolean"},
                "payload": {"type": "string"}
            }},
            "summary": "frame {length_bytes} B"
        },
        "identify": {"proof": "field", "field": "crc_ok", "min_events": 1, "dwell_ms": 4_000,
            "min_span_ms": 1000, "quick": "frame"},
        "catalogue": []
    })
}

fn fixtures() -> &'static Fixtures {
    static F: OnceLock<Fixtures> = OnceLock::new();
    F.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let page = zeta_dsp::synth::pocsag_packet_iq(
            1_234_567,
            3,
            "HELLO ZETA",
            1200,
            RATE_HZ,
            OFFSET_HZ as f64,
        );
        let pocsag = write(dir.path(), "pocsag", &cycle(page.clone(), 1), CENTER_HZ);
        let burst = fsk_burst(OFFSET_HZ, FSK_BAUD, FSK_DEVIATION_HZ);
        let fsk = write(dir.path(), "fsk", &cycle(burst, 2), CENTER_HZ);
        let pocsag3 = write(dir.path(), "pocsag3", &span_loop(), CENTER_HZ);
        let known3 = span_loop_at(KNOWN_OFFSETS_HZ);
        let known3 = write(dir.path(), "known3", &known3, CENTER_HZ);
        let narrow = fsk_burst(FSK_BESIDE_HZ, NARROW_FSK_BAUD, NARROW_FSK_DEVIATION_HZ);
        let beside = cycle_at(sum(&[&page, &narrow]), 4, SPAN_FSK_SNR_DB);
        let pocsag_fsk = write(dir.path(), "pocsag_fsk", &beside, CENTER_HZ);
        let burst = fsk_burst(FSK_BESIDE_HZ, WIDE_FSK_BAUD, WIDE_FSK_DEVIATION_HZ);
        let wide = write(dir.path(), "wide", &cycle(burst, 6), CENTER_HZ);
        let noise12k = write(dir.path(), "noise12k", &noise_beside(&page), CENTER_HZ);
        let quick_modes = dir.path().join("modes");
        std::fs::create_dir_all(&quick_modes).unwrap();
        for (file, text) in zeta_core::modes::ModeRegistry::embedded() {
            let mut row: Value = serde_json::from_str(text).unwrap();
            if row["decoder"]["kind"] != "graph"
                || row["band"]["bandwidth_hz"].as_u64() > Some(12_500)
            {
                continue;
            }
            row["identify"]["dwell_ms"] = 1_500.into();
            let name = Path::new(&file).file_name().unwrap().to_owned();
            std::fs::write(quick_modes.join(name), row.to_string()).unwrap();
        }
        let params = serde_json::json!({"family": "framing", "modulation": "fsk",
            "symbol_rate_baud": FRAME_BAUD, "deviation_hz": 2400.0, "frames": 8,
            "body_bytes": 32, "seed": 7, "framing": frame_framing(), "repeat": 1,
            "offset_hz": OFFSET_HZ as f64});
        let (spec, imp) =
            zeta_dsp::synth::parse_params(params.as_object().unwrap().clone()).unwrap();
        let frames = zeta_dsp::synth::synthesise(&spec, &imp, RATE_HZ, CENTER_HZ);
        let framed = write(dir.path(), "framed", &cycle(frames, 3), CENTER_HZ);
        let frame_modes = dir.path().join("frame_modes");
        std::fs::create_dir_all(&frame_modes).unwrap();
        for entry in std::fs::read_dir(&quick_modes).unwrap() {
            let p = entry.unwrap().path();
            std::fs::copy(&p, frame_modes.join(p.file_name().unwrap())).unwrap();
        }
        let row = frame_row().to_string();
        std::fs::write(frame_modes.join(format!("{FRAME_ROW}.json")), row).unwrap();
        Fixtures {
            _dir: dir,
            pocsag,
            fsk,
            quick_modes,
            framed,
            frame_modes,
            pocsag3,
            known3,
            pocsag_fsk,
            wide,
            noise12k,
        }
    })
}

/// `noise12k`'s band of shaped noise: its width and where it sits above the centre.
const NOISE_BAND_HZ: u64 = 12_000;
const NOISE_OFFSET_HZ: u64 = 350_000;
/// The noise band's power against the page's (both well over the floor).
const NOISE_BAND_POWER: f64 = 0.5;
/// The low-pass shaping the noise band, and the noise segment tiled over a cycle.
const NOISE_TAPS: usize = 1_025;
const NOISE_SEGMENT: usize = CYCLE / 8;

/// The `noise12k` cycle: `page` at its start, a continuous band of white noise through a
/// [`NOISE_BAND_HZ`]/2 low-pass mixed to [`NOISE_OFFSET_HZ`], and the span fixtures'
/// white noise over all.
fn noise_beside(page: &[Complex<f32>]) -> Vec<Complex<f32>> {
    let mut white = vec![Complex::new(0.0f32, 0.0); NOISE_SEGMENT + NOISE_TAPS];
    zeta_dsp::synth::awgn(&mut white, 0.0, 9);
    let taps = zeta_dsp::util::lowpass_taps(NOISE_BAND_HZ as f64 / 2.0, RATE_HZ as f64, NOISE_TAPS);
    // The filter's start-up dropped: the segment is the settled band.
    let band = zeta_dsp::util::fir_decimate(&white, &taps, 1).split_off(NOISE_TAPS);
    let power = band.iter().map(|s| s.norm_sqr() as f64).sum::<f64>() / band.len() as f64;
    let gain = (NOISE_BAND_POWER / power).sqrt() as f32;
    let mut out: Vec<Complex<f32>> = band.iter().cycle().take(CYCLE).map(|s| s * gain).collect();
    mix(&mut out, NOISE_OFFSET_HZ as f64);
    for (o, p) in out.iter_mut().zip(page) {
        *o += *p;
    }
    zeta_dsp::synth::awgn(&mut out, SPAN3_SNR_DB, 10);
    out
}

/// The suite's GFSK burst: 2400 baud, 3 kHz deviation.
const FSK_BAUD: u32 = 2_400;
const FSK_DEVIATION_HZ: f64 = 3_000.0;
/// `pocsag_fsk`'s GFSK burst: narrow enough (about 4 kHz) that no 12.5 kHz row fits it.
const NARROW_FSK_BAUD: u32 = 1_200;
const NARROW_FSK_DEVIATION_HZ: f64 = 1_000.0;
/// The `wide` fixture's GFSK burst: about 60 kHz (Carson), far wider than any 12.5 or
/// 25 kHz row's tolerant fit.
const WIDE_FSK_BAUD: u32 = 9_600;
const WIDE_FSK_DEVIATION_HZ: f64 = 20_000.0;
/// The span fixtures' noise, under each fixture's mean power: each page stands about
/// 34 dB over the floor in a detector bin, where the detector reads it 13-14 kHz wide.
/// (The suite's 20 dB leaves the lone page 50 dB up, its skirts read 19-25 kHz wide, and
/// no 12.5 kHz row fits it; much quieter, its two FSK tones read as two narrow runs.)
const SPAN3_SNR_DB: f32 = 8.0;
const SPAN_FSK_SNR_DB: f32 = 6.0;

/// A random one-second GFSK burst (no row proves it) `offset_hz` above the centre.
fn fsk_burst(offset_hz: u64, baud: u32, deviation_hz: f64) -> Vec<Complex<f32>> {
    let mut state = 0x5eed_u64;
    let symbols: Vec<u8> = (0..baud)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 63) as u8
        })
        .collect();
    let mut burst = zeta_dsp::synth::gfsk_modulate(&symbols, 2, baud, deviation_hz, RATE_HZ);
    mix(&mut burst, offset_hz as f64);
    burst
}

/// The sample-wise sum of `parts`, as long as the longest.
fn sum(parts: &[&[Complex<f32>]]) -> Vec<Complex<f32>> {
    let len = parts.iter().map(|p| p.len()).max().unwrap_or(0);
    let mut out = vec![Complex::new(0.0f32, 0.0); len];
    for p in parts {
        for (o, s) in out.iter_mut().zip(p.iter()) {
            *o += *s;
        }
    }
    out
}

/// The step between `pocsag3`'s page addresses: one batch of frames, so each page sits in
/// the suite page's frame and the detector reads all three alike (a page in frame 0
/// reads 3 kHz wide at the moment the detector announces it).
const SPAN_ADDRESS_STEP: u32 = 8;

/// The `pocsag3` loop: a page at each of [`SPAN_OFFSETS_HZ`] (its own address) at the
/// start of every cycle, the third only in [`SPAN_THIRD_CYCLES`], white noise over all.
fn span_loop() -> Vec<Complex<f32>> {
    span_loop_at(SPAN_OFFSETS_HZ)
}

/// [`span_loop`] with its pages at `offsets_hz`.
fn span_loop_at(offsets_hz: [u64; 3]) -> Vec<Complex<f32>> {
    let pages: Vec<Vec<Complex<f32>>> = offsets_hz
        .iter()
        .enumerate()
        .map(|(n, off)| {
            zeta_dsp::synth::pocsag_packet_iq(
                1_234_567 + SPAN_ADDRESS_STEP * n as u32,
                3,
                "HELLO ZETA",
                1200,
                RATE_HZ,
                *off as f64,
            )
        })
        .collect();
    let two = sum(&[&pages[0], &pages[1]]);
    let three = sum(&[&pages[0], &pages[1], &pages[2]]);
    let mut out = vec![Complex::new(0.0f32, 0.0); SPAN_LOOP];
    for c in 0..SPAN_LOOP / CYCLE {
        let burst = match SPAN_THIRD_CYCLES.contains(&c) {
            true => &three,
            false => &two,
        };
        assert!(burst.len() < CYCLE, "burst {} samples", burst.len());
        out[c * CYCLE..c * CYCLE + burst.len()].copy_from_slice(burst);
    }
    zeta_dsp::synth::awgn(&mut out, SPAN3_SNR_DB, 5);
    out
}

/// `burst` at the start of a [`CYCLE`], the rest silence, with white noise over all.
fn cycle(burst: Vec<Complex<f32>>, seed: u64) -> Vec<Complex<f32>> {
    cycle_at(burst, seed, 20.0)
}

/// [`cycle`] with the noise `snr_db` under the cycle's mean power.
fn cycle_at(mut burst: Vec<Complex<f32>>, seed: u64, snr_db: f32) -> Vec<Complex<f32>> {
    assert!(burst.len() < CYCLE, "burst {} samples", burst.len());
    burst.resize(CYCLE, Complex::new(0.0, 0.0));
    zeta_dsp::synth::awgn(&mut burst, snr_db, seed);
    burst
}

/// Shift `iq` up by `offset_hz`.
fn mix(iq: &mut [Complex<f32>], offset_hz: f64) {
    for (n, s) in iq.iter_mut().enumerate() {
        let cycles = offset_hz * n as f64 / RATE_HZ as f64;
        let ph = 2.0 * std::f64::consts::PI * cycles.fract();
        *s *= Complex::from_polar(1.0, ph as f32);
    }
}

fn write(dir: &Path, stem: &str, iq: &[Complex<f32>], center_hz: u64) -> PathBuf {
    let meta = CaptureMeta {
        sample_rate_hz: Hz(RATE_HZ as u64),
        center_hz: Hz(center_hz),
        start: Timestamp(0),
        annotations: Vec::new(),
    };
    write_cf32(dir, stem, &meta, iq).expect("write fixture")
}

/// Stands in for `zeta-ext`'s `sink_sondehub`: an event input and no outputs, reading
/// until its input closes.
struct FakeSink(StageSpec);

#[async_trait]
impl Stage for FakeSink {
    fn spec(&self) -> &StageSpec {
        &self.0
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    async fn run(
        &self,
        _params: Value,
        mut inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<zeta_core::types::StreamId>,
        _bus: Arc<StreamBus>,
        cancel: tokio_util::sync::CancellationToken,
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        assert!(outputs.is_empty(), "a sink has no output streams");
        let mut rx = inputs.remove(0);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                r = rx.recv() => if let Err(broadcast::error::RecvError::Closed) = r {
                    return Ok(());
                },
            }
        }
    }
}

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    r.register(Arc::new(FakeSink(StageSpec {
        kind: "sink_sondehub",
        inputs: vec![StreamKind::Event],
        outputs: vec![],
        params_schema: schemars::json_schema!({
            "type": "object",
            "required": ["callsign"],
            "properties": {"callsign": {"type": "string"}}
        }),
    })));
    zeta_dsp::register_all(&mut r);
    r
}

/// A port that is a file source but for a switch replacing its samples with noise
/// (the burst stops; the noise floor stays).
struct Muting {
    inner: Arc<FileSource>,
    tx: broadcast::Sender<IqBlock>,
    muted: Arc<AtomicBool>,
}

impl Muting {
    fn new(inner: Arc<FileSource>, muted: bool) -> Arc<Self> {
        let (tx, _) = broadcast::channel(64);
        let flag = Arc::new(AtomicBool::new(muted));
        let mut rx = inner.subscribe();
        let (out, on) = (tx.clone(), flag.clone());
        tokio::spawn(async move {
            let mut seed = 7;
            loop {
                match rx.recv().await {
                    Ok(mut b) => {
                        if on.load(Ordering::Relaxed) {
                            let mut v = vec![Complex::new(0.0f32, 0.0); b.samples.len()];
                            seed += 1;
                            zeta_dsp::synth::awgn(&mut v, 21.0, seed);
                            b.samples = v.into();
                        }
                        let _ = out.send(b);
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        Arc::new(Muting {
            inner,
            tx,
            muted: flag,
        })
    }

    fn mute(&self) {
        self.muted.store(true, Ordering::Relaxed);
    }
}

#[async_trait]
impl Port for Muting {
    fn id(&self) -> &SourceId {
        self.inner.id()
    }
    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        self.inner.tune(t).await
    }
    fn current(&self) -> Option<Tuning> {
        self.inner.current()
    }
    fn subscribe(&self) -> IqReceiver {
        self.tx.subscribe()
    }
    fn fault(&self) -> Option<String> {
        self.inner.fault()
    }
    async fn idle(&self) {
        self.inner.idle().await
    }
}

/// A looping, real-time, tunable (±2 MHz) file source `id` of `meta`, not yet started.
fn file_source(id: &str, meta: &Path) -> Arc<FileSource> {
    file_source_with(id, meta, true)
}

/// [`file_source`], tunable or not: an untunable one answers every tune with the
/// recording's own band, so a job on another band inside it decodes the burst.
fn file_source_with(id: &str, meta: &Path, tunable: bool) -> Arc<FileSource> {
    let opts = FileOpts {
        realtime: true,
        block_samples: 16_384,
        repeat: true,
        tune_range: tunable.then_some((Hz(CENTER_HZ - 2_000_000), Hz(CENTER_HZ + 2_000_000))),
    };
    Arc::new(FileSource::open(SourceId(id.into()), meta, opts).unwrap())
}

/// A node over `ports` (each source started once the node subscribed), with the node's
/// classifiers and `modes_dir`'s rows.
async fn node_over(
    dir: &Path,
    ports: Vec<(Arc<dyn Port>, Arc<FileSource>)>,
    modes_dir: Option<&Path>,
) -> Node {
    let cfg = NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        ring_seconds: 4.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory: None,
        modes_dir: modes_dir.map(Path::to_path_buf),
    };
    let srcs: Vec<Arc<FileSource>> = ports.iter().map(|(_, s)| s.clone()).collect();
    let node = Node::open(cfg, registry(), ports.into_iter().map(|(p, _)| p).collect())
        .await
        .unwrap();
    for c in zeta_dsp::classifiers(node.modes_handle()) {
        node.add_classifier(c);
    }
    for s in srcs {
        s.start();
    }
    node
}

/// A node over one source `rx` of `meta`, muted from the start when `muted`.
async fn node_on(dir: &Path, meta: &Path, muted: bool) -> (Node, Arc<Muting>) {
    let src = file_source("rx", meta);
    let port = Muting::new(src.clone(), muted);
    let node = node_over(dir, vec![(port.clone(), src)], None).await;
    (node, port)
}

fn rx() -> SourceId {
    SourceId("rx".into())
}

fn capture() -> Tuning {
    Tuning {
        center_hz: Hz(CENTER_HZ),
        bandwidth_hz: Hz(RATE_HZ as u64),
    }
}

/// The watch's graph for the capture with `detect` params `params` (persist and gone
/// timing for the tests unless given).
fn watch_graph(node: &Node, mut params: Value) -> Graph {
    let mut g = JobKind::Watch
        .expand(
            &rx(),
            capture(),
            Hz(RATE_HZ as u64),
            None,
            None,
            None,
            &node.modes(),
        )
        .unwrap();
    for (k, v) in [("persist_ms", PERSIST_MS), ("gone_ms", GONE_MS)] {
        if params.get(k).is_none() {
            params[k] = v.into();
        }
    }
    let detect = g.nodes.iter_mut().find(|n| n.kind == "detect").unwrap();
    detect.params = params;
    g
}

/// Start a watch over the whole capture of `rx` with `detect` params `params`.
async fn start_watch(node: &Node, params: Value) -> JobId {
    let g = watch_graph(node, params);
    node.job_start("watch", rx(), capture(), g, Some(Hz(RATE_HZ as u64)), false)
        .await
        .unwrap()
}

/// Every event the watch's event stream carries from now on, across restarts.
struct Events {
    seen: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Events {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Events {
    /// Collect `job`'s events on `node` (re-subscribing after a restart of the job).
    fn of(node: &Arc<Node>, job: &JobId) -> Events {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (node, job, out) = (Arc::downgrade(node), job.clone(), seen.clone());
        let task = tokio::spawn(async move {
            loop {
                let rx = node.upgrade().and_then(|n| {
                    let s = n
                        .job_outputs(&job)
                        .into_iter()
                        .find(|(k, _)| *k == StreamKind::Event)?
                        .1;
                    n.bus().subscribe(&s)
                });
                let Some(mut rx) = rx else {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                };
                loop {
                    match rx.recv().await {
                        Ok(f) => {
                            if let Payload::Event(v) = f.payload {
                                out.lock().unwrap().push(v);
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            }
        });
        Events { seen, task }
    }

    /// The `watch` events in `state` so far.
    fn watch(&self, state: &str) -> Vec<Value> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e["protocol"] == "watch" && e["state"] == state)
            .cloned()
            .collect()
    }

    fn signals(&self) -> Vec<Value> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e["protocol"] == "signal")
            .cloned()
            .collect()
    }

    fn all(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }
}

/// Poll `f` every 20 ms until it holds, panicking with `what` after `within`.
async fn until(what: &str, within: Duration, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn record(node: &Node, id: &JobId) -> Option<zeta_core::job::JobRecord> {
    node.job_list().into_iter().find(|r| &r.id == id)
}

fn state(node: &Node, id: &JobId) -> Option<JobState> {
    record(node, id).map(|r| r.state)
}

fn running(node: &Node) -> Vec<(String, String)> {
    node.job_list()
        .into_iter()
        .filter(|r| r.state == JobState::Running)
        .map(|r| (r.id.0, r.kind))
        .collect()
}

/// The decoders `watch.decoding` lists.
fn decoding(node: &Node, watch: &JobId) -> Vec<Value> {
    node.job_context(watch)
        .and_then(|c| c["watch"]["decoding"].as_array().cloned())
        .unwrap_or_default()
}

/// Wait for the watch's `decoding` event and return the decoder's id.
async fn decoded(node: &Node, events: &Events, watch: &JobId) -> JobId {
    until("a decoding event", Duration::from_secs(40), || {
        !events.watch("decoding").is_empty()
    })
    .await;
    let e = &events.watch("decoding")[0];
    let job = JobId(e["job"].as_str().unwrap().into());
    until("watch.decoding lists it", Duration::from_secs(5), || {
        decoding(node, watch)
            .iter()
            .any(|d| d["job"] == job.0.as_str())
    })
    .await;
    job
}

/// Stop every running job, then check the node holds nothing on `rx`.
async fn stop_all(node: &Node) {
    for (id, _) in running(node) {
        let _ = node.job_stop(&JobId(id)).await;
    }
    until("no lease on rx", Duration::from_secs(5), || {
        node.lease_holders(&rx()).is_empty()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_signal_is_identified_and_the_proved_decoder_kept() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({})).await;
    let events = Events::of(&node, &watch);
    let job = decoded(&node, &events, &watch).await;
    let all = events.all();
    eprintln!("{all:#?}");
    let identifying = events.watch("identifying");
    assert_eq!(identifying.len(), 1, "{all:#?}");
    let d = &events.watch("decoding")[0];
    let at = |e: &Value| all.iter().position(|x| x == e).unwrap();
    assert!(at(&identifying[0]) < at(d), "{all:#?}");
    assert_eq!(d["mode"], "pocsag_decode", "{d}");
    assert_eq!(d["placed"], "capture", "{d}");
    assert_eq!(d["source"], "rx", "{d}");
    assert!(d["signal"]["center_hz"].is_u64(), "{d}");
    assert!(
        d["summary"].as_str().unwrap().contains("pocsag_decode"),
        "{d}"
    );
    // The detector's own announcement came first, on the same stream.
    let new = events.signals();
    assert!(new.iter().any(|s| s["state"] == "new"), "{all:#?}");

    let rec = record(&node, &job).expect("decoder listed");
    assert_eq!(rec.kind, "pocsag_decode");
    assert_eq!(rec.state, JobState::Running);
    assert_eq!(rec.owner, None, "a kept decoder is an ordinary job");
    assert!(
        rec.tuning.center_hz.0.abs_diff(POCSAG_HZ) <= 6_000,
        "{:?}",
        rec.tuning
    );
    let ctx = node.job_context(&watch).unwrap();
    let d0 = &ctx["watch"]["decoding"][0];
    assert_eq!(d0["job"], job.0.as_str(), "{ctx}");
    assert_eq!(d0["mode"], "pocsag_decode", "{ctx}");
    assert_eq!(d0["source"], "rx", "{ctx}");
    assert!(d0["since_ns"].is_u64(), "{ctx}");
    assert!(ctx["watch"]["current"].is_null(), "{ctx}");
    let signals = ctx["watch"]["signals"].as_array().unwrap();
    assert_eq!(signals.len(), 1, "{ctx}");
    assert_eq!(signals[0]["handled"], true, "{ctx}");
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_signal_inside_another_jobs_band_is_ignored() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({})).await;
    let events = Events::of(&node, &watch);
    // An operator's job on the burst's band, inside the watch's capture.
    let op = Op::JobStart {
        kind: JobKind::Measure,
        source: rx(),
        center_hz: Hz(POCSAG_HZ),
        bandwidth_hz: Hz(12_500),
        deviation_hz: None,
        sideband: None,
        elastic: false,
        replace: vec![],
    };
    assert!(matches!(node.apply(op).await, Ok(OpResult::Job(_))));
    until(
        "the detector announces the burst",
        Duration::from_secs(15),
        || events.signals().iter().any(|s| s["state"] == "new"),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let all = events.all();
    assert!(events.watch("identifying").is_empty(), "{all:#?}");
    let ctx = node.job_context(&watch).unwrap();
    assert!(
        ctx["watch"]["signals"].as_array().unwrap().is_empty(),
        "{ctx}"
    );
    assert!(ctx["watch"]["current"].is_null(), "{ctx}");
    stop_all(&node).await;
}

/// Start a watch on `band` (inside the capture) with `detect` params `params`.
async fn start_watch_on(node: &Node, band: Tuning, params: Value) -> JobId {
    let mut g = JobKind::Watch
        .expand(
            &rx(),
            band,
            Hz(RATE_HZ as u64),
            None,
            None,
            None,
            &node.modes(),
        )
        .unwrap();
    let mut params = params;
    for (k, v) in [("persist_ms", PERSIST_MS), ("gone_ms", GONE_MS)] {
        if params.get(k).is_none() {
            params[k] = v.into();
        }
    }
    g.nodes
        .iter_mut()
        .find(|n| n.kind == "detect")
        .unwrap()
        .params = params;
    node.job_start("watch", rx(), band, g, Some(Hz(RATE_HZ as u64)), false)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_narrow_watch_sees_only_its_own_band_of_a_wider_capture() {
    // Seen live 2026-11-05: a 154 kHz watch in a 6 MHz capture announced signals 4 MHz away.
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let narrow = |center_hz| Tuning {
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(150_000),
    };
    // The capture held at the recording's band, as a pin or a wide job holds a radio.
    let pin = Op::SourcePin {
        source: rx(),
        center_hz: Hz(CENTER_HZ),
        bandwidth_hz: Hz(RATE_HZ as u64),
        replace: vec![],
    };
    node.apply(pin).await.unwrap();
    // On the burst: the control. 300 kHz under the centre: nothing transmits there.
    let on = start_watch_on(&node, narrow(POCSAG_HZ), serde_json::json!({})).await;
    let off = start_watch_on(&node, narrow(CENTER_HZ - OFFSET_HZ), serde_json::json!({})).await;
    let (on_events, off_events) = (Events::of(&node, &on), Events::of(&node, &off));
    until(
        "the watch on the burst announces it",
        Duration::from_secs(15),
        || on_events.signals().iter().any(|s| s["state"] == "new"),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let all = off_events.all();
    assert!(off_events.signals().is_empty(), "{all:#?}");
    assert!(off_events.watch("identifying").is_empty(), "{all:#?}");
    let new = on_events.signals();
    assert!(
        new.iter()
            .all(|s| s["center_hz"].as_u64().unwrap().abs_diff(POCSAG_HZ) <= 75_000),
        "{new:#?}"
    );
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn watch_children_run_inside_the_watch_capture() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({})).await;
    let events = Events::of(&node, &watch);
    let inside = |t: &Tuning| {
        let half = t.bandwidth_hz.0 / 2;
        t.center_hz.0 - half >= CENTER_HZ - RATE_HZ as u64 / 2
            && t.center_hz.0 + half <= CENTER_HZ + RATE_HZ as u64 / 2
    };
    let mut saw_identify = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    while events.watch("decoding").is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "no decoding event");
        assert_eq!(port.current(), Some(capture()), "the capture moved");
        let holders = node.lease_holders(&rx());
        assert!(holders.iter().all(|(_, t)| inside(t)), "{holders:?}");
        let identify = node
            .job_list()
            .into_iter()
            .find(|r| r.kind == "identify" && r.state == JobState::Running);
        if let Some(i) = identify {
            assert_eq!(i.owner.as_ref(), Some(&watch), "{i:?}");
            assert!(holders.iter().any(|(j, _)| *j == i.id), "{holders:?}");
            assert!(holders.iter().any(|(j, _)| *j == watch), "{holders:?}");
            saw_identify = true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(saw_identify, "the identify run was never seen");
    let job = decoded(&node, &events, &watch).await;
    assert_eq!(port.current(), Some(capture()), "the capture moved");
    let holders = node.lease_holders(&rx());
    let ids: Vec<&JobId> = holders.iter().map(|(j, _)| j).collect();
    assert!(ids.contains(&&watch) && ids.contains(&&job), "{holders:?}");
    assert!(holders.iter().all(|(_, t)| inside(t)), "{holders:?}");
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unknown_signal_is_retried_at_most_three_times() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(1_000, 0);
    let dir = tempfile::tempdir().unwrap();
    let src = file_source("rx", &fixtures().fsk);
    let node = node_over(
        dir.path(),
        vec![(src.clone() as Arc<dyn Port>, src)],
        Some(&fixtures().quick_modes),
    )
    .await;
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({})).await;
    let events = Events::of(&node, &watch);
    let tries = 1 + zeta_core::node::watch::RETRY_MAX as usize;
    until("every try ends unknown", Duration::from_secs(90), || {
        events.watch("unknown").len() >= tries
    })
    .await;
    // Left alone from then on while it persists.
    tokio::time::sleep(Duration::from_secs(6)).await;
    let all = events.all();
    let unknown = events.watch("unknown");
    assert_eq!(unknown.len(), tries, "{all:#?}");
    assert_eq!(events.watch("identifying").len(), tries, "{all:#?}");
    assert!(events.watch("decoding").is_empty(), "{all:#?}");
    for e in &unknown {
        assert!(e["features"]["bandwidth_hz"].is_u64(), "{e}");
        assert!(e["signal"]["center_hz"].is_u64(), "{e}");
        assert!(e["job"].is_string(), "{e}");
    }
    let ctx = node.job_context(&watch).unwrap();
    let s = &ctx["watch"]["signals"][0];
    assert_eq!(s["tries"], tries as u64, "{ctx}");
    assert_eq!(s["handled"], true, "{ctx}");
    // The identify runs and their children are gone with no trace in the history.
    let owned: Vec<_> = node
        .job_list()
        .into_iter()
        .filter(|r| r.owner.is_some())
        .collect();
    assert!(owned.is_empty(), "{owned:#?}");
    assert_eq!(running(&node), [(watch.0.clone(), "watch".to_string())]);
    stop_all(&node).await;
    zeta_core::node::watch::test_timing(0, 0);
}

/// A licence register's answer, after `delay`: `rows` when given, else one transmitter
/// licensed for 16K0F1D at the centre of whatever band it is asked about.
struct Register {
    rows: Option<Vec<Allocation>>,
    delay: Duration,
}

impl Register {
    /// One transmitter licensed for 16K0F1D at the centre asked about.
    fn licensed() -> Register {
        Register {
            rows: None,
            delay: Duration::ZERO,
        }
    }

    /// `rows`, whatever it is asked.
    fn with(rows: Vec<Allocation>) -> Register {
        Register {
            rows: Some(rows),
            delay: Duration::ZERO,
        }
    }

    /// [`Register::with`] answering after `delay`.
    fn slow(rows: Vec<Allocation>, delay: Duration) -> Register {
        Register {
            rows: Some(rows),
            delay,
        }
    }
}

/// A transmitter licensed at `center_hz` for `bw_hz` (`emission`, decoded) to
/// `licensee` at `site`, `km` from the station.
fn assignment(
    center_hz: u64,
    bw_hz: u64,
    emission: &str,
    licensee: &str,
    site: &str,
    km: f64,
) -> Allocation {
    Allocation {
        provider: "register".into(),
        region: "AU".into(),
        min_hz: Hz(center_hz - bw_hz / 2),
        max_hz: Hz(center_hz + bw_hz / 2),
        service: "assignment".into(),
        note: None,
        kind: Some("assignment".into()),
        licensee: Some(licensee.into()),
        site: Some(site.into()),
        distance_km: Some(km),
        center_hz: Some(Hz(center_hz)),
        bandwidth_hz: Some(Hz(bw_hz)),
        role: Some("tx".into()),
        emission: Some(emission.into()),
        emission_decoded: zeta_core::emission::parse_emission(emission),
        ..Allocation::default()
    }
}

#[async_trait]
impl KnowledgeProvider for Register {
    fn name(&self) -> &str {
        "register"
    }
    fn gives_assignments(&self) -> bool {
        true
    }
    async fn allocations(&self, region: &str, center_hz: Hz, _bandwidth_hz: Hz) -> Vec<Allocation> {
        tokio::time::sleep(self.delay).await;
        if let Some(rows) = &self.rows {
            return rows.clone();
        }
        vec![Allocation {
            region: region.into(),
            ..assignment(
                center_hz.0,
                16_000,
                "16K0F1D",
                "Test Telemetry Pty Ltd",
                "Mt Gravatt",
                7.6,
            )
        }]
    }
    async fn signatures(&self, _center_hz: Hz, _bandwidth_hz: Hz) -> Vec<Signature> {
        vec![]
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unknown_summary_names_the_licensee() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let src = file_source("rx", &fixtures().fsk);
    let node = node_over(
        dir.path(),
        vec![(src.clone() as Arc<dyn Port>, src)],
        Some(&fixtures().quick_modes),
    )
    .await;
    node.add_knowledge(Arc::new(Register::licensed()));
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({})).await;
    let events = Events::of(&node, &watch);
    until("an unknown", Duration::from_secs(60), || {
        !events.watch("unknown").is_empty()
    })
    .await;
    let e = &events.watch("unknown")[0];
    let summary = e["summary"].as_str().unwrap();
    // The measurement, then the register's transmitter.
    assert!(summary.contains(" kHz"), "{e}");
    let register = "; register says 16K0F1D (16 kHz FM data) Test Telemetry Pty Ltd, \
        Mt Gravatt, 8 km (identify 1 of";
    assert!(summary.contains(register), "{e}");
    let licensee = "Test Telemetry Pty Ltd";
    assert_eq!(e["allocations"][0]["licensee"], licensee, "{e}");
    // The origins ride beside the allocations: the provider asked, the site lit.
    assert_eq!(
        e["origins"]["providers"],
        serde_json::json!(["learned", "register"]),
        "{e}"
    );
    assert_eq!(
        e["origins"]["origins"][0]["allocation"]["licensee"], licensee,
        "{e}"
    );
    // The signal keeps the verdict's record and register answer.
    let ctx = node.job_context(&watch).unwrap();
    let s = &ctx["watch"]["signals"][0];
    assert!(s["features"]["bandwidth_hz"].is_u64(), "{ctx}");
    assert_eq!(s["allocations"][0]["licensee"], licensee, "{ctx}");
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_signal_stops_its_decoder() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 2_000);
    let dir = tempfile::tempdir().unwrap();
    let (node, port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({})).await;
    let events = Events::of(&node, &watch);
    let job = decoded(&node, &events, &watch).await;
    port.mute();
    until("a lost event", Duration::from_secs(20), || {
        !events.watch("lost").is_empty()
    })
    .await;
    let lost = &events.watch("lost")[0];
    assert_eq!(lost["job"], job.0.as_str(), "{lost}");
    assert_eq!(lost["mode"], "pocsag_decode", "{lost}");
    assert_eq!(state(&node, &job), Some(JobState::Stopped));
    assert!(decoding(&node, &watch).is_empty());
    assert_eq!(state(&node, &watch), Some(JobState::Running));
    stop_all(&node).await;
    zeta_core::node::watch::test_timing(0, 0);
}

/// A signal whose `gone` came while identify ran sends no other `gone`: its proved
/// decoder still counts its silence from the verdict and is dropped once silent. The
/// detector here holds a signal for a minute (`gone_ms`), so only the `gone` the test
/// injects on the watch's stream during identify can start that count.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_signal_gone_during_identify_still_times_out() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 2_000);
    let dir = tempfile::tempdir().unwrap();
    let (node, port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({"gone_ms": 60_000})).await;
    let events = Events::of(&node, &watch);
    until("an identifying event", Duration::from_secs(20), || {
        !events.watch("identifying").is_empty()
    })
    .await;
    let signal = events.watch("identifying")[0]["signal"].clone();
    let out = node
        .job_outputs(&watch)
        .into_iter()
        .find(|(k, _)| *k == StreamKind::Event)
        .unwrap()
        .1;
    let prov = node
        .bus()
        .list()
        .into_iter()
        .find(|i| i.id == out)
        .unwrap()
        .provenance;
    let gone = serde_json::json!({
        "protocol": "signal",
        "state": "gone",
        "center_hz": signal["center_hz"],
        "bandwidth_hz": signal["bandwidth_hz"],
    });
    node.bus()
        .publish(Frame::new(out, prov, Payload::Event(gone)));
    let job = decoded(&node, &events, &watch).await;
    port.mute();
    until("a lost event", Duration::from_secs(20), || {
        !events.watch("lost").is_empty()
    })
    .await;
    let lost = &events.watch("lost")[0];
    assert_eq!(lost["job"], job.0.as_str(), "{lost}");
    assert!(
        lost["summary"]
            .as_str()
            .unwrap()
            .contains("gone and silent"),
        "{lost}"
    );
    assert_eq!(state(&node, &job), Some(JobState::Stopped));
    stop_all(&node).await;
    zeta_core::node::watch::test_timing(0, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn owned_jobs_never_take_a_history_slot() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    // A full history of the operator's own stopped jobs.
    let mut mine = Vec::new();
    for _ in 0..zeta_core::node::JOB_HISTORY_MAX {
        let g = JobKind::Passthrough
            .expand(&rx(), capture(), Hz(0), None, None, None, &node.modes())
            .unwrap();
        let id = node
            .job_start("passthrough", rx(), capture(), g, None, false)
            .await
            .unwrap();
        node.job_stop(&id).await.unwrap();
        mine.push(id);
    }
    let watch = start_watch(&node, serde_json::json!({})).await;
    let events = Events::of(&node, &watch);
    let job = decoded(&node, &events, &watch).await;
    let identify = JobId(
        events.watch("identifying")[0]["job"]
            .as_str()
            .unwrap()
            .into(),
    );
    until("the identify run is gone", Duration::from_secs(5), || {
        record(&node, &identify).is_none()
    })
    .await;
    let list = node.job_list();
    for id in &mine {
        assert_eq!(state(&node, id), Some(JobState::Stopped), "{list:#?}");
    }
    let owned: Vec<_> = list.iter().filter(|r| r.owner.is_some()).collect();
    assert!(owned.is_empty(), "{owned:#?}");
    assert!(
        !dir.path()
            .join("state/jobs")
            .join(format!("{}.json", identify.0))
            .exists()
    );
    assert_eq!(state(&node, &job), Some(JobState::Running));
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_the_watch_stops_its_decoders() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({})).await;
    let events = Events::of(&node, &watch);
    let job = decoded(&node, &events, &watch).await;
    node.job_stop(&watch).await.unwrap();
    assert_eq!(state(&node, &watch), Some(JobState::Stopped));
    assert_eq!(state(&node, &job), Some(JobState::Stopped));
    assert!(running(&node).is_empty(), "{:?}", running(&node));
    assert!(node.lease_holders(&rx()).is_empty());
    assert!(decoding(&node, &watch).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn graph_put_on_the_watch_readopts_its_decoders() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({"gone_ms": READOPT_GONE_MS})).await;
    let events = Events::of(&node, &watch);
    let job = decoded(&node, &events, &watch).await;
    let mut graph = match node.apply(Op::GraphGet { job: watch.clone() }).await {
        Ok(OpResult::Graph(g)) => g,
        r => panic!("graph_get: {r:?}"),
    };
    let detect = graph.nodes.iter_mut().find(|n| n.kind == "detect").unwrap();
    detect.params["threshold_db"] = 12.into();
    let op = Op::GraphPut {
        job: watch.clone(),
        graph,
    };
    assert!(matches!(node.apply(op).await, Ok(OpResult::Ok)));
    assert_eq!(state(&node, &watch), Some(JobState::Running));
    assert_eq!(state(&node, &job), Some(JobState::Running));
    let rec = record(&node, &watch).unwrap();
    assert!(
        rec.capture_rate_hz.is_some(),
        "a detect put keeps the watch re-expandable"
    );
    // Re-adopted, and kept once its signal is seen again (past `gone_ms`).
    tokio::time::sleep(Duration::from_millis(READOPT_GONE_MS + 3_000)).await;
    let listed = decoding(&node, &watch);
    assert_eq!(listed.len(), 1, "{listed:#?}");
    assert_eq!(listed[0]["job"], job.0.as_str());
    assert_eq!(state(&node, &job), Some(JobState::Running));
    assert!(events.watch("lost").is_empty(), "{:#?}", events.all());
    // No second identify: the decoder's signal is its own.
    assert_eq!(events.watch("identifying").len(), 1, "{:#?}", events.all());
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_readopts_or_drops() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let jobs = dir.path().join("state").join("jobs");
    let file = |id: &JobId| jobs.join(format!("{}.json", id.0));
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({"gone_ms": READOPT_GONE_MS})).await;
    let events = Events::of(&node, &watch);
    let job = decoded(&node, &events, &watch).await;
    drop(events);
    // The records as a kill mid-decode leaves them: both running, the watch naming it.
    let watch_rec = std::fs::read_to_string(file(&watch)).unwrap();
    let job_rec = std::fs::read_to_string(file(&job)).unwrap();
    let v: Value = serde_json::from_str(&watch_rec).unwrap();
    assert_eq!(v["context"]["watch"]["decoding"][0]["job"], job.0.as_str());
    drop(node);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let restore = || {
        std::fs::write(file(&watch), &watch_rec).unwrap();
        std::fs::write(file(&job), &job_rec).unwrap();
    };
    restore();

    // The burst is still there: the decoder resumes and the watch keeps it.
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let events = Events::of(&node, &watch);
    assert_eq!(state(&node, &watch), Some(JobState::Running));
    assert_eq!(state(&node, &job), Some(JobState::Running));
    tokio::time::sleep(Duration::from_millis(READOPT_GONE_MS + 3_000)).await;
    let listed = decoding(&node, &watch);
    assert_eq!(listed.len(), 1, "{listed:#?}");
    assert_eq!(listed[0]["job"], job.0.as_str());
    assert_eq!(state(&node, &job), Some(JobState::Running));
    assert!(events.watch("lost").is_empty(), "{:#?}", events.all());
    drop(events);
    drop(node);
    tokio::time::sleep(Duration::from_millis(500)).await;
    restore();

    // The burst is gone: the decoder resumes, its signal never reappears, it is dropped.
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, true).await;
    let node = Arc::new(node);
    let events = Events::of(&node, &watch);
    assert_eq!(state(&node, &job), Some(JobState::Running));
    until("the decoder is dropped", Duration::from_secs(15), || {
        state(&node, &job) == Some(JobState::Stopped)
    })
    .await;
    until("watch.decoding is empty", Duration::from_secs(5), || {
        decoding(&node, &watch).is_empty()
    })
    .await;
    let lost = events.watch("lost");
    assert_eq!(lost.len(), 1, "{:#?}", events.all());
    assert_eq!(lost[0]["job"], job.0.as_str());
    assert_eq!(state(&node, &watch), Some(JobState::Running));
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn decode_on_free_moves_the_decoder_to_a_free_source() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let a = file_source("rx", &fixtures().pocsag);
    let b = file_source_with("rx2", &fixtures().pocsag, false);
    let ports: Vec<(Arc<dyn Port>, Arc<FileSource>)> = vec![(a.clone(), a), (b.clone(), b)];
    let node = node_over(dir.path(), ports, None).await;
    let node = Arc::new(node);
    let params = serde_json::json!({"decode_on": "free", "prefer": ["rx2"]});
    let watch = start_watch(&node, params).await;
    let events = Events::of(&node, &watch);
    let job = decoded(&node, &events, &watch).await;
    let d = &events.watch("decoding")[0];
    assert_eq!(d["placed"], "free", "{d}");
    assert_eq!(d["source"], "rx2", "{d}");
    let rec = record(&node, &job).unwrap();
    assert_eq!(rec.source, SourceId("rx2".into()));
    assert_eq!(rec.kind, "pocsag_decode");
    assert_eq!(rec.state, JobState::Running);
    // The child identify proved on the watch's own radio is gone; only the watch holds it.
    let holders: Vec<JobId> = node
        .lease_holders(&rx())
        .into_iter()
        .map(|(j, _)| j)
        .collect();
    assert_eq!(holders, std::slice::from_ref(&watch));
    let holders: Vec<JobId> = node
        .lease_holders(&SourceId("rx2".into()))
        .into_iter()
        .map(|(j, _)| j)
        .collect();
    assert_eq!(holders, std::slice::from_ref(&job));
    // The moved decoder decodes: its own events arrive.
    let stream = node
        .job_outputs(&job)
        .into_iter()
        .find(|(k, _)| *k == StreamKind::Event)
        .unwrap()
        .1;
    let mut rx2 = node.bus().subscribe(&stream).unwrap();
    let got = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(f) = rx2.recv().await
                && let Payload::Event(v) = f.payload
                && v["protocol"] == "pocsag"
            {
                return v;
            }
        }
    })
    .await
    .expect("a page decoded on rx2");
    assert_eq!(got["address"], 1_234_567, "{got}");
    node.job_stop(&watch).await.unwrap();
    assert_eq!(state(&node, &job), Some(JobState::Stopped));
    assert!(running(&node).is_empty(), "{:?}", running(&node));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn watch_is_neither_elastic_nor_attachable() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let op = Op::JobStart {
        kind: JobKind::Watch,
        source: rx(),
        center_hz: Hz(CENTER_HZ),
        bandwidth_hz: Hz(RATE_HZ as u64),
        deviation_hz: None,
        sideband: None,
        elastic: true,
        replace: vec![],
    };
    let e = node.apply(op).await.unwrap_err();
    assert!(e.to_string().contains("watch cannot be elastic"), "{e}");
    let op = Op::JobAttach {
        kind: JobKind::Watch,
        job: JobId::new(),
        language: None,
    };
    let e = node.apply(op).await.unwrap_err();
    assert!(e.to_string().contains("watch is not attachable"), "{e}");
    assert!(node.job_list().is_empty());
}

/// The records a watch owns that are listed.
fn owned(node: &Node, watch: &JobId) -> Vec<zeta_core::job::JobRecord> {
    node.job_list()
        .into_iter()
        .filter(|r| r.owner.as_ref() == Some(watch))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panic_in_the_loop_leaves_no_owned_job_and_no_lease() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    zeta_core::node::watch::test_fault_after("identifying", true);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({})).await;
    until("the watch fails", Duration::from_secs(30), || {
        matches!(state(&node, &watch), Some(JobState::Failed { .. }))
    })
    .await;
    let reason = match state(&node, &watch) {
        Some(JobState::Failed { reason }) => reason,
        s => panic!("{s:?}"),
    };
    assert!(reason.contains("panic"), "{reason}");
    until("nothing runs", Duration::from_secs(15), || {
        running(&node).is_empty()
    })
    .await;
    until("no lease", Duration::from_secs(5), || {
        node.lease_holders(&rx()).is_empty()
    })
    .await;
    // Past the identify run's dwell: nothing it would have proved is left running.
    tokio::time::sleep(Duration::from_secs(7)).await;
    assert!(running(&node).is_empty(), "{:?}", running(&node));
    assert!(node.lease_holders(&rx()).is_empty());
    assert!(
        owned(&node, &watch).is_empty(),
        "{:#?}",
        owned(&node, &watch)
    );
    zeta_core::node::watch::test_fault_after("", false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_watch_tears_down_its_decoders() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    zeta_core::node::watch::test_fault_after("decoding", false);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({})).await;
    let events = Events::of(&node, &watch);
    until("a decoding event", Duration::from_secs(40), || {
        !events.watch("decoding").is_empty()
    })
    .await;
    let job = JobId(events.watch("decoding")[0]["job"].as_str().unwrap().into());
    until("the watch fails", Duration::from_secs(10), || {
        matches!(state(&node, &watch), Some(JobState::Failed { .. }))
    })
    .await;
    assert_eq!(
        state(&node, &watch),
        Some(JobState::Failed {
            reason: "watch: test failure".into()
        })
    );
    until("the decoder stops", Duration::from_secs(10), || {
        state(&node, &job) == Some(JobState::Stopped)
    })
    .await;
    assert!(running(&node).is_empty(), "{:?}", running(&node));
    until("no lease", Duration::from_secs(5), || {
        node.lease_holders(&rx()).is_empty()
    })
    .await;
    assert!(decoding(&node, &watch).is_empty());
    zeta_core::node::watch::test_fault_after("", false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn graph_put_with_bad_detect_params_is_refused() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({})).await;
    let events = Events::of(&node, &watch);
    let job = decoded(&node, &events, &watch).await;
    let stream = node.job_outputs(&watch);
    let graph = match node.apply(Op::GraphGet { job: watch.clone() }).await {
        Ok(OpResult::Graph(g)) => g,
        r => panic!("graph_get: {r:?}"),
    };
    for (key, value) in [
        ("edge_mask", serde_json::json!(0.6)),
        ("width_hz", serde_json::json!([30000, 5000])),
    ] {
        let mut bad = graph.clone();
        let detect = bad.nodes.iter_mut().find(|n| n.kind == "detect").unwrap();
        detect.params[key] = value;
        let op = Op::GraphPut {
            job: watch.clone(),
            graph: bad,
        };
        let e = node.apply(op).await.unwrap_err();
        assert!(e.to_string().contains(key), "{key}: {e}");
    }
    // Nothing restarted: the same run, its graph and its decoder.
    assert_eq!(node.job_outputs(&watch), stream);
    assert_eq!(record(&node, &watch).unwrap().graph, graph);
    assert_eq!(state(&node, &watch), Some(JobState::Running));
    assert_eq!(state(&node, &job), Some(JobState::Running));
    assert_eq!(decoding(&node, &watch)[0]["job"], job.0.as_str());
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watch_with_sondehub_appends_the_sink() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let sondehub = serde_json::json!({
        "callsign": "TEST", "lat": -27.5, "lon": 153.0, "alt_m": 10, "batch_ms": 5000,
        "url": "http://127.0.0.1:1/"
    });
    let watch = start_watch(&node, serde_json::json!({ "sondehub": sondehub })).await;
    let events = Events::of(&node, &watch);
    let job = decoded(&node, &events, &watch).await;
    let graph = match node.apply(Op::GraphGet { job: job.clone() }).await {
        Ok(OpResult::Graph(g)) => g,
        r => panic!("graph_get: {r:?}"),
    };
    let sink = graph
        .nodes
        .iter()
        .find(|n| n.kind == "sink_sondehub")
        .unwrap_or_else(|| panic!("no sink in {graph:#?}"));
    assert_eq!(sink.params, sondehub);
    let edge = graph.edges.iter().find(|e| e.to == sink.id).expect("fed");
    assert_eq!(edge.kind, StreamKind::Event);
    let from = graph.nodes.iter().find(|n| n.id == edge.from).unwrap();
    assert_eq!(from.kind, "pocsag_decode", "{graph:#?}");
    assert_eq!(state(&node, &job), Some(JobState::Running));
    // The sink adds no stream: the decoder's own event output is still the job's.
    assert!(
        node.job_outputs(&job)
            .iter()
            .any(|(k, _)| *k == StreamKind::Event)
    );
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watch_without_sondehub_appends_nothing() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({})).await;
    let events = Events::of(&node, &watch);
    let job = decoded(&node, &events, &watch).await;
    let rec = record(&node, &job).unwrap();
    assert!(rec.graph.nodes.iter().all(|n| n.kind != "sink_sondehub"));
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stage_with_no_outputs_launches_and_stops_cleanly() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let n = |id: &str, kind: &str, params: Value| GraphNode {
        id: NodeId(id.into()),
        kind: kind.into(),
        params,
    };
    let graph = Graph {
        id: zeta_core::graph::GraphId("sink".into()),
        nodes: vec![
            n("in", "passthrough", serde_json::json!({"kind": "event"})),
            n("out", "sink_sondehub", serde_json::json!({"callsign": "T"})),
        ],
        edges: vec![Edge {
            from: NodeId("in".into()),
            to: NodeId("out".into()),
            kind: StreamKind::Event,
        }],
    };
    // A built-in kind's graph is re-expanded at launch; a put one is launched as stored.
    let first = JobKind::Passthrough
        .expand(
            &rx(),
            capture(),
            Hz(RATE_HZ as u64),
            None,
            None,
            None,
            &node.modes(),
        )
        .unwrap();
    let job = node
        .job_start(
            "passthrough",
            rx(),
            capture(),
            first,
            Some(Hz(RATE_HZ as u64)),
            false,
        )
        .await
        .unwrap();
    node.job_graph_put(&job, graph).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(state(&node, &job), Some(JobState::Running));
    assert!(
        node.debug_stage_count() >= 2,
        "{}",
        node.debug_stage_count()
    );
    // The edge into the sink is still an output of the job, so its events stay tailable.
    assert!(
        node.job_outputs(&job)
            .iter()
            .any(|(k, _)| *k == StreamKind::Event)
    );
    node.job_stop(&job).await.unwrap();
    until("no stages", Duration::from_secs(5), || {
        node.debug_stage_count() == 0
    })
    .await;
    assert_eq!(state(&node, &job), Some(JobState::Stopped));
    stop_all(&node).await;
}

fn sondehub_params() -> Value {
    serde_json::json!({
        "callsign": "TEST", "lat": -27.5, "lon": 153.0, "alt_m": 10, "batch_ms": 5000,
        "url": "http://127.0.0.1:1/"
    })
}

/// `prefer` names this node's sources: an unknown id is refused at start and at a put,
/// naming its index, before anything starts or restarts.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watch_preferring_an_unknown_source_is_refused() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let g = watch_graph(&node, serde_json::json!({"prefer": ["rx", "b210"]}));
    let e = node
        .job_start("watch", rx(), capture(), g, Some(Hz(RATE_HZ as u64)), false)
        .await
        .unwrap_err();
    assert!(
        e.to_string().contains("prefer[1]: unknown source b210"),
        "{e}"
    );
    assert!(running(&node).is_empty(), "{:?}", running(&node));
    let watch = start_watch(&node, serde_json::json!({"prefer": ["rx"]})).await;
    let stream = node.job_outputs(&watch);
    let mut graph = record(&node, &watch).unwrap().graph;
    let detect = graph.nodes.iter_mut().find(|n| n.kind == "detect").unwrap();
    detect.params["prefer"] = serde_json::json!(["b210"]);
    let e = node
        .apply(Op::GraphPut {
            job: watch.clone(),
            graph,
        })
        .await
        .unwrap_err();
    assert!(
        e.to_string().contains("prefer[0]: unknown source b210"),
        "{e}"
    );
    assert_eq!(node.job_outputs(&watch), stream, "nothing restarted");
    assert_eq!(state(&node, &watch), Some(JobState::Running));
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watch_with_a_bad_sondehub_is_refused() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    // At start: refused naming the field, nothing started.
    let bad = serde_json::json!({ "sondehub": { "callsign": 5 } });
    let g = watch_graph(&node, bad);
    let e = node
        .job_start("watch", rx(), capture(), g, Some(Hz(RATE_HZ as u64)), false)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("sondehub.callsign"), "{e}");
    assert!(running(&node).is_empty(), "{:?}", running(&node));
    // At a put: refused before anything restarts.
    let watch = start_watch(&node, serde_json::json!({})).await;
    let stream = node.job_outputs(&watch);
    let mut graph = record(&node, &watch).unwrap().graph;
    let detect = graph.nodes.iter_mut().find(|n| n.kind == "detect").unwrap();
    detect.params["sondehub"] = serde_json::json!({ "lat": 1 });
    let e = node
        .apply(Op::GraphPut {
            job: watch.clone(),
            graph: graph.clone(),
        })
        .await
        .unwrap_err();
    assert!(e.to_string().contains("sondehub"), "{e}");
    assert_eq!(node.job_outputs(&watch), stream, "nothing restarted");
    assert_eq!(state(&node, &watch), Some(JobState::Running));
    stop_all(&node).await;
    // Without the sink stage registered the same params are refused as such.
    let mut stages = StageRegistry::new();
    stages.register(Arc::new(PassthroughStage::new()));
    zeta_dsp::register_all(&mut stages);
    let mut g = graph;
    g.nodes
        .iter_mut()
        .find(|n| n.kind == "detect")
        .unwrap()
        .params["sondehub"] = sondehub_params();
    let e = zeta_core::graph::validate(&g, &stages).unwrap_err();
    assert!(
        e.to_string()
            .contains("sondehub: sink_sondehub stage not registered"),
        "{e}"
    );
    assert!(zeta_core::graph::validate(&g, &registry()).is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_sink_restart_keeps_the_decoder_without_the_sink() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    zeta_core::node::watch::test_fail_sink_launch(1);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({ "sondehub": sondehub_params() })).await;
    let events = Events::of(&node, &watch);
    let job = decoded(&node, &events, &watch).await;
    let d = &events.watch("decoding")[0];
    let summary = d["summary"].as_str().unwrap();
    assert!(summary.contains("sondehub sink refused"), "{summary}");
    assert!(summary.contains("sink restart failed"), "{summary}");
    assert_eq!(state(&node, &job), Some(JobState::Running));
    let rec = record(&node, &job).unwrap();
    assert!(rec.graph.nodes.iter().all(|n| n.kind != "sink_sondehub"));
    let listed = decoding(&node, &watch);
    assert!(listed[0]["sink_error"].is_string(), "{listed:?}");
    assert!(events.watch("lost").is_empty(), "{:#?}", events.all());
    zeta_core::node::watch::test_fail_sink_launch(0);
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sondehub_sink_without_a_station_keeps_the_decoder() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    assert!(node.station().is_none());
    // A callsign only: the position would come from the station, and there is none.
    let sondehub = serde_json::json!({ "callsign": "TEST" });
    let watch = start_watch(&node, serde_json::json!({ "sondehub": sondehub })).await;
    let events = Events::of(&node, &watch);
    let job = decoded(&node, &events, &watch).await;
    let d = &events.watch("decoding")[0];
    let summary = d["summary"].as_str().unwrap();
    assert!(summary.contains("sondehub sink refused"), "{summary}");
    assert!(summary.contains("no station set"), "{summary}");
    assert_eq!(state(&node, &job), Some(JobState::Running));
    let rec = record(&node, &job).unwrap();
    assert!(rec.graph.nodes.iter().all(|n| n.kind != "sink_sondehub"));
    let listed = decoding(&node, &watch);
    let err = listed[0]["sink_error"].as_str().unwrap_or_default();
    assert!(
        err.starts_with("sink.params.lat: no station set"),
        "{listed:?}"
    );
    assert!(events.watch("lost").is_empty(), "{:#?}", events.all());
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_decoder_that_cannot_be_relaunched_is_lost_not_decoding() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    zeta_core::node::watch::test_fail_sink_launch(2);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({ "sondehub": sondehub_params() })).await;
    let events = Events::of(&node, &watch);
    until("a lost event", Duration::from_secs(40), || {
        !events.watch("lost").is_empty()
    })
    .await;
    let lost = &events.watch("lost")[0];
    assert!(
        lost["summary"]
            .as_str()
            .unwrap()
            .contains("relaunch without it failed"),
        "{lost}"
    );
    assert!(events.watch("decoding").is_empty(), "{:#?}", events.all());
    assert!(decoding(&node, &watch).is_empty());
    let job = JobId(lost["job"].as_str().unwrap().into());
    assert_ne!(state(&node, &job), Some(JobState::Running));
    zeta_core::node::watch::test_fail_sink_launch(0);
    stop_all(&node).await;
}

/// A `replace` of the watch that takes the gate of the radio while the sink restart of a
/// proved decoder waits for it: the restart is cancelled with the watch (a task finishes
/// its launch and stops the decoder), so the replace completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replace_of_the_watch_completes_while_a_sink_restart_is_pending() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({ "sondehub": sondehub_params() })).await;
    until("the identify child runs", Duration::from_secs(40), || {
        owned(&node, &watch)
            .iter()
            .any(|r| r.kind == "pocsag_decode" && r.state == JobState::Running)
    })
    .await;
    let child = owned(&node, &watch)
        .into_iter()
        .find(|r| r.kind == "pocsag_decode")
        .unwrap()
        .id;
    // Held from before the proof: the decoder's relaunch with the sink queues on it.
    let gate = node.debug_gate(&rx()).await;
    let op = Op::JobStart {
        kind: JobKind::Measure,
        source: rx(),
        center_hz: Hz(POCSAG_HZ),
        bandwidth_hz: Hz(12_500),
        deviation_hz: None,
        sideband: None,
        elastic: false,
        replace: vec![watch.clone()],
    };
    let replace = node.apply(op);
    tokio::pin!(replace);
    // Queued on the gate before the sink restart is: the proof comes, the decoder is
    // stopped for the restart and its launch waits on the gate.
    let early = tokio::time::timeout(Duration::from_millis(300), &mut replace).await;
    assert!(early.is_err(), "the replace ran under the gate: {early:?}");
    until("the decoder is stopped", Duration::from_secs(30), || {
        state(&node, &child) != Some(JobState::Running)
    })
    .await;
    assert_eq!(state(&node, &watch), Some(JobState::Running));
    drop(gate);
    let measure = match tokio::time::timeout(Duration::from_secs(10), replace).await {
        Ok(Ok(OpResult::Job(j))) => j,
        r => panic!(
            "replace of the watch: {r:?} {:?} {:?}",
            node.job_list()
                .iter()
                .map(|r| (r.kind.clone(), r.state.clone(), r.owner.clone()))
                .collect::<Vec<_>>(),
            node.lease_holders(&rx())
        ),
    };
    assert_eq!(state(&node, &watch), Some(JobState::Stopped));
    // The cancelled restart's task may still be finishing the decoder's launch (it takes
    // the lease) before it stops it: wait for the end state, which a leaked lease never
    // reaches.
    let only_measure = [(measure.0.clone(), "measure".to_string())];
    let holders = || -> Vec<JobId> {
        node.lease_holders(&rx())
            .into_iter()
            .map(|(j, _)| j)
            .collect()
    };
    until(
        "only the measure job runs and holds the radio",
        Duration::from_secs(10),
        || running(&node) == only_measure && holders() == [measure.clone()],
    )
    .await;
    stop_all(&node).await;
}

/// A fault of the watch's own port fails the watch under the source's name and leaves its
/// decoders to fail on their own with the same prefix, not stopped by the watch: the
/// node's recovery relaunches them with the watch, whose next run re-adopts them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watch_whose_port_faults_leaves_its_decoders_to_fail_on_their_own() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let src = file_source("rx", &fixtures().pocsag);
    let port = zeta_core::test_fixtures::FaultablePort::new(src.clone());
    let node = node_over(dir.path(), vec![(port.clone(), src)], None).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({})).await;
    let events = Events::of(&node, &watch);
    let job = decoded(&node, &events, &watch).await;
    port.set_fault(Some("device unplugged".into()));
    let failed = Some(JobState::Failed {
        reason: "source rx: device unplugged".into(),
    });
    until("the watch fails", Duration::from_secs(10), || {
        matches!(state(&node, &watch), Some(JobState::Failed { .. }))
    })
    .await;
    assert_eq!(state(&node, &watch), failed);
    until("the decoder ends", Duration::from_secs(10), || {
        state(&node, &job) != Some(JobState::Running)
    })
    .await;
    assert_eq!(state(&node, &job), failed);
    assert!(events.watch("lost").is_empty(), "{:#?}", events.all());
    assert!(
        decoding(&node, &watch)
            .iter()
            .any(|d| d["job"] == job.0.as_str()),
        "{:?}",
        node.job_context(&watch)
    );
    until("no lease", Duration::from_secs(5), || {
        node.lease_holders(&rx()).is_empty()
    })
    .await;
}

/// A node over the FSK fixture with the quick rows: every identify ends unknown.
async fn fsk_node(dir: &Path) -> Arc<Node> {
    let src = file_source("rx", &fixtures().fsk);
    let node = node_over(
        dir,
        vec![(src.clone() as Arc<dyn Port>, src)],
        Some(&fixtures().quick_modes),
    )
    .await;
    Arc::new(node)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn on_unknown_capture_captures_once_per_signal() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(1_000, 0);
    let dir = tempfile::tempdir().unwrap();
    let node = fsk_node(dir.path()).await;
    let params = serde_json::json!({"on_unknown": "capture", "capture": {"bursts": 2}});
    let watch = start_watch(&node, params).await;
    let events = Events::of(&node, &watch);
    until("a captured event", Duration::from_secs(60), || {
        !events.watch("captured").is_empty()
    })
    .await;
    // A retry identifies again; it starts no second capture.
    until("a second unknown", Duration::from_secs(60), || {
        events.watch("unknown").len() >= 2
    })
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let all = events.all();
    let at = |e: &Value| all.iter().position(|x| x == e).unwrap();
    let (identifying, unknown) = (events.watch("identifying"), events.watch("unknown"));
    let (capturing, captured) = (events.watch("capturing"), events.watch("captured"));
    assert_eq!(capturing.len(), 1, "{all:#?}");
    assert_eq!(captured.len(), 1, "{all:#?}");
    assert!(at(&identifying[0]) < at(&unknown[0]), "{all:#?}");
    assert!(at(&unknown[0]) < at(&capturing[0]), "{all:#?}");
    assert!(at(&capturing[0]) < at(&captured[0]), "{all:#?}");
    let child = capturing[0]["job"].as_str().unwrap().to_string();
    assert!(capturing[0]["signal"]["center_hz"].is_u64(), "{all:#?}");
    let c = &captured[0];
    assert_eq!(c["signal"], capturing[0]["signal"], "{c}");
    let ids: Vec<String> = c["captures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 2, "{c}");
    assert!(c["summary"].as_str().unwrap().contains("captured"), "{c}");
    // The child inherits the watch's unknown verdict: nothing classified.
    assert!(c.get("classified").is_none(), "{c}");
    for id in &ids {
        let rec = node.bursts().get(id).expect("in the store");
        assert_eq!(rec.watch, Some(watch.clone()), "{rec:?}");
        assert_eq!(rec.job, JobId(child.clone()), "{rec:?}");
        assert_eq!(
            rec.signal.as_ref().unwrap()["center_hz"],
            capturing[0]["signal"]["center_hz"],
            "{rec:?}"
        );
        assert!(rec.bandwidth_hz.0 >= 12_500, "{rec:?}");
    }
    let ctx = node.job_context(&watch).unwrap();
    let s = &ctx["watch"]["signals"][0];
    assert_eq!(s["captures"].as_array().unwrap().len(), 2, "{ctx}");
    assert_eq!(s["captured"], true, "{ctx}");
    // The child is gone with no trace in the history.
    assert!(record(&node, &JobId(child)).is_none());
    stop_all(&node).await;
    zeta_core::node::watch::test_timing(0, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watch_capture_child_proposes_once() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(1_000, 0);
    let dir = tempfile::tempdir().unwrap();
    let node = fsk_node(dir.path()).await;
    let params = serde_json::json!({"on_unknown": "capture", "capture": {"bursts": 2}});
    let watch = start_watch(&node, params).await;
    let events = Events::of(&node, &watch);
    until("a captured event", Duration::from_secs(60), || {
        !events.watch("captured").is_empty()
    })
    .await;
    let child = JobId(
        events.watch("capturing")[0]["job"]
            .as_str()
            .unwrap()
            .to_string(),
    );
    until("one proposal", Duration::from_secs(10), || {
        node.signatures().list(None).len() == 1
    })
    .await;
    let sig = node.signatures().list(None).remove(0);
    assert_eq!(sig.provenance.jobs, [child, watch.clone()], "{sig:?}");
    assert_eq!(sig.provenance.proposed_by, "node", "{sig:?}");
    let captures = events.watch("captured")[0]["captures"].clone();
    assert_eq!(
        serde_json::json!(sig.provenance.captures),
        captures,
        "{sig:?}"
    );
    // The signal's retries identify again; they propose nothing more.
    until("a second unknown", Duration::from_secs(60), || {
        events.watch("unknown").len() >= 2
    })
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(node.signatures().list(None).len(), 1);
    stop_all(&node).await;
    zeta_core::node::watch::test_timing(0, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bad_capture_block_is_refused_naming_the_field() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let bad = serde_json::json!({"on_unknown": "capture", "capture": {"bursts": "x"}});
    let g = watch_graph(&node, bad);
    let e = node
        .job_start("watch", rx(), capture(), g, Some(Hz(RATE_HZ as u64)), false)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("capture.bursts"), "{e}");
    assert!(running(&node).is_empty(), "{:?}", running(&node));
    // The stage's own check: a zero is named too.
    let zero = serde_json::json!({"capture": {"window_ms": 0}});
    let g = watch_graph(&node, zero);
    let e = node
        .job_start("watch", rx(), capture(), g, Some(Hz(RATE_HZ as u64)), false)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("capture.window_ms"), "{e}");
    // At a put: refused before anything restarts.
    let watch = start_watch(&node, serde_json::json!({})).await;
    let stream = node.job_outputs(&watch);
    let mut graph = record(&node, &watch).unwrap().graph;
    let detect = graph.nodes.iter_mut().find(|n| n.kind == "detect").unwrap();
    detect.params["capture"] = serde_json::json!({"bursts": "x"});
    let e = node
        .apply(Op::GraphPut {
            job: watch.clone(),
            graph,
        })
        .await
        .unwrap_err();
    assert!(e.to_string().contains("capture.bursts"), "{e}");
    assert_eq!(node.job_outputs(&watch), stream, "nothing restarted");
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_the_watch_stops_a_running_capture_child() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let node = fsk_node(dir.path()).await;
    let params = serde_json::json!({"on_unknown": "capture", "capture": {"bursts": 32}});
    let watch = start_watch(&node, params).await;
    let events = Events::of(&node, &watch);
    until("a capturing event", Duration::from_secs(60), || {
        !events.watch("capturing").is_empty()
    })
    .await;
    let child = JobId(
        events.watch("capturing")[0]["job"]
            .as_str()
            .unwrap()
            .to_string(),
    );
    assert_eq!(state(&node, &child), Some(JobState::Running));
    node.job_stop(&watch).await.unwrap();
    assert_eq!(state(&node, &watch), Some(JobState::Stopped));
    until("the child ends", Duration::from_secs(5), || {
        state(&node, &child) != Some(JobState::Running)
    })
    .await;
    assert!(running(&node).is_empty(), "{:?}", running(&node));
    until("no lease on rx", Duration::from_secs(5), || {
        node.lease_holders(&rx()).is_empty()
    })
    .await;
    assert!(events.watch("captured").is_empty(), "{:#?}", events.all());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watch_graphs_fft_edge_is_unfed_but_its_signal_events_are_fed() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let feed = node.bus().feed_of(&rx()).expect("feed");
    let mut feed_rx = node.bus().subscribe(&feed).unwrap();
    let watch = start_watch(&node, serde_json::json!({})).await;
    let end = tokio::time::Instant::now() + Duration::from_secs(8);
    let mut signal = false;
    while let Ok(r) = tokio::time::timeout_at(end, feed_rx.recv()).await {
        let Ok(f) = r else { continue };
        if let Payload::Event(ref v) = f.payload {
            assert!(v.get("q").is_none(), "an fft row reached the feed: {v}");
            if v["protocol"] == "signal" && f.provenance.job.as_ref() == Some(&watch) {
                signal = true;
            }
        }
    }
    assert!(signal, "no signal event from the watch in the feed");
    stop_all(&node).await;
}

/// Accept, straight into the node's store, a signature of the fixture `meta`'s signal
/// (the band at [`OFFSET_HZ`], 12.5 kHz wide) from layer 1's measurement of its burst
/// (the cycle's first second); features only (confidence 0.3), as the node's own
/// proposals are.
async fn accept_signature(node: &Node, meta: &Path) -> zeta_core::signatures::SignatureRecord {
    use zeta_core::classify::{Band, ClassifierSet};
    use zeta_core::signatures::NewSignature;
    let (m, iq) = zeta_core::capture::read_cf32(meta).unwrap();
    let burst = &iq[..RATE_HZ as usize];
    let mut set = ClassifierSet::default();
    for c in zeta_dsp::classifiers(node.modes_handle()) {
        set.register(c);
    }
    let band = Band {
        offset_hz: OFFSET_HZ as i64,
        bandwidth_hz: Hz(12_500),
    };
    let measured = set
        .measure(burst, m.sample_rate_hz, m.center_hz, band)
        .await
        .expect("layer 1 measures the band");
    let new = NewSignature {
        name: Some("test link".into()),
        features: measured.features,
        modulation: measured.label.name,
        framing: None,
        crc_frames: 0,
        evidence: "test".into(),
        source: rx(),
        station: None,
        captures: vec![],
        jobs: vec![],
        allocation: None,
        proposed_by: "cli".into(),
    };
    let store = node.signatures();
    let rec = store.propose(new, 1).unwrap();
    store.accept(&rec.id, None, "cli", 2).unwrap()
}

/// `on_unknown: capture` over a signal an accepted signature knows: every identify run
/// ends unknown but names the learned match, so the signal is neither captured nor
/// proposed again, and its retries go on.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_watch_does_not_capture_a_learned_signal() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(1_000, 0);
    let dir = tempfile::tempdir().unwrap();
    let node = fsk_node(dir.path()).await;
    let sig = accept_signature(&node, &fixtures().fsk).await;
    assert!((sig.confidence - 0.3).abs() < 1e-6, "{sig:?}");
    let params = serde_json::json!({"on_unknown": "capture", "capture": {"bursts": 2}});
    let watch = start_watch(&node, params).await;
    let events = Events::of(&node, &watch);
    // The retry identifies again: the retry schedule is kept.
    until("a second unknown", Duration::from_secs(90), || {
        events.watch("unknown").len() >= 2
    })
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let all = events.all();
    for e in events.watch("unknown") {
        let summary = e["summary"].as_str().unwrap();
        assert!(summary.starts_with("learned test link at "), "{e}");
    }
    assert!(events.watch("capturing").is_empty(), "{all:#?}");
    assert!(events.watch("captured").is_empty(), "{all:#?}");
    let ctx = node.job_context(&watch).unwrap();
    let s = &ctx["watch"]["signals"][0];
    assert_ne!(s["captured"], true, "{ctx}");
    // No proposal beside the accepted record.
    let sigs = node.signatures().list(None);
    assert_eq!(sigs.len(), 1, "{sigs:#?}");
    assert_eq!(sigs[0].id, sig.id);
    assert!(sigs[0].matches >= 2, "{:?}", sigs[0]);
    stop_all(&node).await;
    zeta_core::node::watch::test_timing(0, 0);
}

/// A frame row's decoder keeps publishing `timing` (and forwarded `bits`) events on
/// noise: only events in the row's own protocol (`frame`) count as its signal, so once
/// the signal is gone and the frames stop it is dropped after the silence.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_frame_row_decoder_is_dropped_when_its_frames_stop() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 2_000);
    let dir = tempfile::tempdir().unwrap();
    let src = file_source("rx", &fixtures().framed);
    let port = Muting::new(src.clone(), false);
    let node = node_over(
        dir.path(),
        vec![(port.clone() as Arc<dyn Port>, src)],
        Some(&fixtures().frame_modes),
    )
    .await;
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({})).await;
    let events = Events::of(&node, &watch);
    let job = decoded(&node, &events, &watch).await;
    let decoding = &events.watch("decoding")[0];
    assert_eq!(decoding["mode"], FRAME_ROW, "{decoding}");
    port.mute();
    until("a lost event", Duration::from_secs(30), || {
        !events.watch("lost").is_empty()
    })
    .await;
    let lost = &events.watch("lost")[0];
    assert_eq!(lost["job"], job.0.as_str(), "{lost}");
    assert_eq!(lost["mode"], FRAME_ROW, "{lost}");
    assert_eq!(state(&node, &job), Some(JobState::Stopped));
    stop_all(&node).await;
    zeta_core::node::watch::test_timing(0, 0);
}

/// The watch's band in the `modes` refusal test: narrower than `rds_decode`'s 200 kHz.
const NARROW_BAND_HZ: u64 = 100_000;
/// The id of the broken operator row the `modes` refusal test loads.
const BROKEN_ROW: &str = "pocsag_broken";

/// `node`'s watch graph `graph` with `modes` set on its `detect` node.
fn with_modes(graph: &Graph, modes: Value) -> Graph {
    let mut g = graph.clone();
    let detect = g.nodes.iter_mut().find(|n| n.kind == "detect").unwrap();
    detect.params["modes"] = modes;
    g
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn graph_put_with_a_bad_modes_list_is_refused() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    // An operator row with a bad field: listed `broken`.
    let modes_dir = dir.path().join("modes");
    std::fs::create_dir_all(&modes_dir).unwrap();
    let (_, text) = zeta_core::modes::ModeRegistry::embedded()
        .into_iter()
        .find(|(f, _)| f.ends_with("pocsag_decode.json"))
        .unwrap();
    let mut row: Value = serde_json::from_str(text).unwrap();
    row["id"] = BROKEN_ROW.into();
    row["band"]["bandwidth_hz"] = "x".into();
    std::fs::write(
        modes_dir.join(format!("{BROKEN_ROW}.json")),
        row.to_string(),
    )
    .unwrap();
    let src = file_source("rx", &fixtures().pocsag);
    let port = Muting::new(src.clone(), false);
    let node = node_over(
        dir.path(),
        vec![(port.clone() as Arc<dyn Port>, src)],
        Some(&modes_dir),
    )
    .await;
    let node = Arc::new(node);
    let band = Tuning {
        center_hz: Hz(POCSAG_HZ),
        bandwidth_hz: Hz(NARROW_BAND_HZ),
    };
    // Refused at the start too, starting nothing.
    let mut g = JobKind::Watch
        .expand(
            &rx(),
            band,
            Hz(RATE_HZ as u64),
            None,
            None,
            None,
            &node.modes(),
        )
        .unwrap();
    g = with_modes(&g, serde_json::json!(["no_such_mode"]));
    let e = node
        .job_start("watch", rx(), band, g, Some(Hz(RATE_HZ as u64)), false)
        .await
        .unwrap_err();
    assert!(
        e.to_string()
            .contains("modes[0]: unknown mode no_such_mode"),
        "{e}"
    );
    assert!(running(&node).is_empty(), "{:?}", running(&node));
    let watch = start_watch_on(&node, band, serde_json::json!({})).await;
    let stream = node.job_outputs(&watch);
    let graph = record(&node, &watch).unwrap().graph;
    let cases = [
        (
            serde_json::json!(["pocsag_decode", "no_such_mode"]),
            "modes[1]: unknown mode no_such_mode".to_string(),
        ),
        (
            serde_json::json!([BROKEN_ROW]),
            format!("modes[0]: {BROKEN_ROW} is broken ("),
        ),
        (
            serde_json::json!(["rds_decode"]),
            format!(
                "modes[0]: rds_decode is 200000 Hz wide; the watch's band is {NARROW_BAND_HZ} Hz"
            ),
        ),
        (serde_json::json!([]), "modes: empty list".to_string()),
    ];
    for (modes, want) in cases {
        let op = Op::GraphPut {
            job: watch.clone(),
            graph: with_modes(&graph, modes.clone()),
        };
        let e = node.apply(op).await.unwrap_err();
        assert!(e.to_string().contains(&want), "{modes}: {e}");
        // Nothing restarted: the same run and its graph.
        assert_eq!(record(&node, &watch).unwrap().graph, graph, "{modes}");
        assert_eq!(node.job_outputs(&watch), stream, "{modes}");
        assert_eq!(state(&node, &watch), Some(JobState::Running), "{modes}");
    }
    // A list of loaded rows that fit the band is taken.
    let op = Op::GraphPut {
        job: watch.clone(),
        graph: with_modes(&graph, serde_json::json!(["pocsag_decode"])),
    };
    node.apply(op).await.unwrap();
    until("the watch runs again", Duration::from_secs(5), || {
        state(&node, &watch) == Some(JobState::Running)
    })
    .await;
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_event_carries_why_and_a_decoding_event_carries_via() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 2_000);
    let dir = tempfile::tempdir().unwrap();
    let (node, port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({})).await;
    let events = Events::of(&node, &watch);
    let job = decoded(&node, &events, &watch).await;
    let decoding_event = &events.watch("decoding")[0];
    assert_eq!(decoding_event["via"], "identify", "{decoding_event}");
    assert_eq!(
        decoding_event.get("why"),
        Some(&Value::Null),
        "{decoding_event}"
    );
    assert_eq!(decoding(&node, &watch)[0]["via"], "identify");
    let identifying = &events.watch("identifying")[0];
    assert_eq!(identifying.get("via"), Some(&Value::Null), "{identifying}");
    port.mute();
    until("a lost event", Duration::from_secs(20), || {
        !events.watch("lost").is_empty()
    })
    .await;
    let lost = &events.watch("lost")[0];
    assert_eq!(lost["job"], job.0.as_str(), "{lost}");
    assert_eq!(lost["why"], "gone and silent", "{lost}");
    assert!(
        lost["summary"]
            .as_str()
            .unwrap()
            .contains("gone and silent"),
        "{lost}"
    );
    stop_all(&node).await;
    zeta_core::node::watch::test_timing(0, 0);
}

/// The process-wide silence the hold test sets: far longer than the test waits.
const LONG_SILENCE_MS: u64 = 60_000;
/// The hold the hold test's `detect` node sets.
const SHORT_HOLD_MS: u64 = 1_500;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hold_ms_on_the_detect_node_replaces_the_silence_constant() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, LONG_SILENCE_MS);
    let dir = tempfile::tempdir().unwrap();
    let (node, port) = node_on(dir.path(), &fixtures().pocsag, false).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, serde_json::json!({"hold_ms": SHORT_HOLD_MS})).await;
    let events = Events::of(&node, &watch);
    let job = decoded(&node, &events, &watch).await;
    let cap = |n: &Node| n.job_context(&watch).map(|c| c["watch"]["cap"].clone());
    until("cap counts the decoder", Duration::from_secs(5), || {
        cap(&node) == Some(serde_json::json!({"max": 16, "decoding": 1, "waiting": 0}))
    })
    .await;
    port.mute();
    // The detector's `gone` comes `GONE_MS` after the last burst; the hold has run out
    // by then. The silence constant (a minute) would keep it far longer.
    until(
        "a lost event",
        Duration::from_millis(GONE_MS + 6_000),
        || !events.watch("lost").is_empty(),
    )
    .await;
    let lost = &events.watch("lost")[0];
    assert_eq!(lost["job"], job.0.as_str(), "{lost}");
    assert_eq!(lost["why"], "gone and silent", "{lost}");
    until("cap counts nothing", Duration::from_secs(5), || {
        cap(&node) == Some(serde_json::json!({"max": 16, "decoding": 0, "waiting": 0}))
    })
    .await;
    stop_all(&node).await;
    zeta_core::node::watch::test_timing(0, 0);
}

/// The span tests' detector width window: a page (about 12 kHz) and up.
const SPAN_WIDTH_HZ: [u64; 2] = [6_000, 20_000];
/// The window of the no-fit test: wide enough down to see the narrow GFSK burst.
const SPAN_WIDE_WIDTH_HZ: [u64; 2] = [2_000, 20_000];
/// How near a directly started decoder sits to its page (about a detector bin).
const SPAN_CENTRE_TOLERANCE_HZ: u64 = 1_500;
/// `pocsag_decode`'s channel.
const POCSAG_WIDTH_HZ: u64 = 12_500;
/// Within how long a span watch over `pocsag3` decodes all three pages (the third is
/// on air 8 s of every 20).
const SPAN_ALL_THREE: Duration = Duration::from_secs(30);

/// A span watch's `detect` params: `modes` and the suite's detector timing.
fn span_params(modes: &[&str]) -> Value {
    serde_json::json!({"modes": modes, "persist_ms": PERSIST_MS, "gone_ms": GONE_MS,
        "width_hz": SPAN_WIDTH_HZ})
}

/// Whether `hz` lies within [`SPAN_CENTRE_TOLERANCE_HZ`] of the page `offset_hz` above
/// the centre.
fn on_page(hz: u64, offset_hz: u64) -> bool {
    hz.abs_diff(CENTER_HZ + offset_hz) <= SPAN_CENTRE_TOLERANCE_HZ
}

/// The decoders' job ids `watch.decoding` lists.
fn decoder_ids(node: &Node, watch: &JobId) -> Vec<JobId> {
    decoding(node, watch)
        .iter()
        .filter_map(|d| d["job"].as_str().map(|s| JobId(s.into())))
        .collect()
}

/// Wait until the watch keeps `n` decoders, checking each 250 ms that it owns no
/// record (no identify run); their ids.
async fn n_decoders_without_identify(node: &Node, watch: &JobId, n: usize) -> Vec<JobId> {
    let deadline = tokio::time::Instant::now() + SPAN_ALL_THREE;
    while decoding(node, watch).len() < n {
        let owned = owned(node, watch);
        assert!(owned.is_empty(), "{owned:#?}");
        assert!(
            tokio::time::Instant::now() < deadline,
            "{n} decoders: {:?}",
            node.job_context(watch)
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    decoder_ids(node, watch)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_span_watch_starts_a_decoder_per_signal_without_identify() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag3, false).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, span_params(&["pocsag_decode"])).await;
    let events = Events::of(&node, &watch);
    let jobs = n_decoders_without_identify(&node, &watch, 3).await;
    until("three decoding events", Duration::from_secs(5), || {
        events.watch("decoding").len() >= 3
    })
    .await;
    let all = events.all();
    assert!(events.watch("identifying").is_empty(), "{all:#?}");
    for e in events.watch("decoding") {
        assert_eq!(e["via"], "modes", "{e}");
        assert_eq!(e["mode"], "pocsag_decode", "{e}");
        assert_eq!(e["placed"], "capture", "{e}");
        let summary = e["summary"].as_str().unwrap();
        assert!(summary.ends_with("on rx (capture, modes)"), "{e}");
    }
    let mut pages = Vec::new();
    for job in &jobs {
        let rec = record(&node, job).unwrap();
        assert_eq!(rec.kind, "pocsag_decode");
        assert_eq!(rec.owner, None, "a kept decoder is an ordinary job");
        assert_eq!(
            rec.tuning.bandwidth_hz,
            Hz(POCSAG_WIDTH_HZ),
            "{:?}",
            rec.tuning
        );
        let page = SPAN_OFFSETS_HZ
            .iter()
            .position(|o| on_page(rec.tuning.center_hz.0, *o));
        assert!(page.is_some(), "{:?}", rec.tuning);
        pages.push(page);
    }
    pages.sort();
    pages.dedup();
    assert_eq!(pages.len(), 3, "one decoder per page: {pages:?}");
    // Each decoder holds its channel inside the capture: no retune.
    let holders = node.lease_holders(&rx());
    for job in &jobs {
        assert!(holders.iter().any(|(j, _)| j == job), "{holders:?}");
    }
    let ctx = node.job_context(&watch).unwrap();
    for d in ctx["watch"]["decoding"].as_array().unwrap() {
        assert_eq!(d["via"], "modes", "{ctx}");
    }
    for s in ctx["watch"]["signals"].as_array().unwrap() {
        assert_eq!(s["fits"], serde_json::json!(["pocsag_decode"]), "{ctx}");
        assert_eq!(s["waiting"], false, "{ctx}");
    }
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_signal_fitting_no_listed_mode_is_unknown_at_once() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(1_000, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag_fsk, false).await;
    let node = Arc::new(node);
    let mut params = span_params(&["pocsag_decode"]);
    params["width_hz"] = serde_json::json!(SPAN_WIDE_WIDTH_HZ);
    params["on_unknown"] = "capture".into();
    params["capture"] = serde_json::json!({"bursts": 1});
    let watch = start_watch(&node, params).await;
    let events = Events::of(&node, &watch);
    let near_burst = |hz: u64| hz.abs_diff(CENTER_HZ + FSK_BESIDE_HZ) <= 5_000;
    let burst = |e: &Value| e["signal"]["center_hz"].as_u64().is_some_and(near_burst);
    let identifies = |n: &Node| {
        n.job_list()
            .into_iter()
            .filter(|r| r.kind == "identify")
            .count()
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    loop {
        assert_eq!(identifies(&node), 0, "no identify runs");
        let unknown = events.watch("unknown").iter().filter(|e| burst(e)).count();
        let decoded = events
            .watch("decoding")
            .iter()
            .any(|e| on_page(e["signal"]["center_hz"].as_u64().unwrap_or(0), OFFSET_HZ));
        // The first unknown and a retry, and the capture's end.
        if unknown >= 2 && decoded && !events.watch("captured").is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{:#?}",
            events.all()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let all = events.all();
    assert!(events.watch("identifying").is_empty(), "{all:#?}");
    let d = &events.watch("decoding")[0];
    assert_eq!(d["via"], "modes", "{d}");
    let unknown: Vec<Value> = events
        .watch("unknown")
        .into_iter()
        .filter(|e| burst(e))
        .collect();
    for e in &unknown {
        let summary = e["summary"].as_str().unwrap();
        assert!(summary.starts_with("unknown at "), "{e}");
        assert!(summary.contains("no listed mode fits"), "{e}");
        assert!(summary.ends_with(" kHz"), "{e}");
        // No identify ran: no job, no verdict's register answer or origins.
        assert!(e["job"].is_null(), "{e}");
        assert!(e["allocations"].is_null(), "{e}");
        assert!(e["origins"].is_null(), "{e}");
    }
    // Captured once, the retry included.
    let (capturing, captured) = (events.watch("capturing"), events.watch("captured"));
    assert_eq!(capturing.len(), 1, "{all:#?}");
    assert_eq!(captured.len(), 1, "{all:#?}");
    assert!(burst(&captured[0]), "{all:#?}");
    let ids = captured[0]["captures"].as_array().unwrap();
    assert_eq!(ids.len(), 1, "{all:#?}");
    let rec = node.bursts().get(ids[0].as_str().unwrap()).expect("stored");
    assert_eq!(rec.watch, Some(watch.clone()), "{rec:?}");
    let ctx = node.job_context(&watch).unwrap();
    let s = ctx["watch"]["signals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["center_hz"].as_u64().is_some_and(near_burst))
        .cloned()
        .unwrap();
    assert_eq!(s["fits"], serde_json::json!([]), "{ctx}");
    stop_all(&node).await;
    zeta_core::node::watch::test_timing(0, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_fitting_modes_run_a_restricted_identify() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    // The span fixture's page: the suite's lone page reads too wide for a 12.5 kHz row.
    let src = file_source("rx", &fixtures().pocsag_fsk);
    let node = node_over(
        dir.path(),
        vec![(src.clone() as Arc<dyn Port>, src)],
        Some(&fixtures().quick_modes),
    )
    .await;
    let node = Arc::new(node);
    let listed = ["pocsag_decode", "aprs_decode"];
    let watch = start_watch(&node, span_params(&listed)).await;
    let events = Events::of(&node, &watch);
    until("an identifying event", Duration::from_secs(20), || {
        !events.watch("identifying").is_empty()
    })
    .await;
    let identify = JobId(
        events.watch("identifying")[0]["job"]
            .as_str()
            .unwrap()
            .into(),
    );
    let tried = Events::of(&node, &identify);
    if let Some(rec) = record(&node, &identify) {
        assert_eq!(rec.owner.as_ref(), Some(&watch), "{rec:?}");
        assert_eq!(
            rec.context["identify"]["candidates"],
            serde_json::json!(listed),
            "{rec:?}"
        );
    }
    let job = decoded(&node, &events, &watch).await;
    let all = events.all();
    let d = &events.watch("decoding")[0];
    assert_eq!(d["via"], "identify", "{d}");
    assert_eq!(d["mode"], "pocsag_decode", "{d}");
    assert_eq!(events.watch("identifying").len(), 1, "{all:#?}");
    assert_eq!(decoding(&node, &watch)[0]["via"], "identify");
    let candidates: Vec<Value> = tried
        .all()
        .into_iter()
        .filter(|e| e["protocol"] == "candidate")
        .collect();
    assert!(!candidates.is_empty(), "{:#?}", tried.all());
    for c in &candidates {
        let mode = c["mode"].as_str().unwrap();
        assert!(listed.contains(&mode), "{c}");
    }
    assert_eq!(state(&node, &job), Some(JobState::Running));
    stop_all(&node).await;
}

/// The detector's `gone_ms` of the second-new test: shorter than the 2 s page cycle, so
/// the page goes and comes back each cycle.
const FLAP_GONE_MS: u64 = 500;
/// The second-new test's `persist_ms`: shorter than one page, so each burst is announced.
const FLAP_PERSIST_MS: u64 = 500;
/// How long the second-new test watches the flapping page after its first decoder.
const FLAP_WATCH: Duration = Duration::from_secs(10);
/// How many `new`s of the page the second-new test must see to have tested anything.
const FLAP_NEWS_MIN: usize = 3;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_new_on_a_decoded_channel_starts_nothing() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    // The span fixture's page (the burst beside it is under the width window).
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag_fsk, false).await;
    let node = Arc::new(node);
    let mut params = span_params(&["pocsag_decode"]);
    params["gone_ms"] = FLAP_GONE_MS.into();
    params["persist_ms"] = FLAP_PERSIST_MS.into();
    let watch = start_watch(&node, params).await;
    let events = Events::of(&node, &watch);
    let job = decoded(&node, &events, &watch).await;
    tokio::time::sleep(FLAP_WATCH).await;
    let all = events.all();
    let news = events
        .signals()
        .iter()
        .filter(|s| s["state"] == "new")
        .filter(|s| on_page(s["center_hz"].as_u64().unwrap_or(0), OFFSET_HZ))
        .count();
    assert!(news >= FLAP_NEWS_MIN, "the page flaps: {all:#?}");
    assert_eq!(events.watch("decoding").len(), 1, "{all:#?}");
    assert_eq!(decoder_ids(&node, &watch), [job]);
    let decoders = node
        .job_list()
        .into_iter()
        .filter(|r| r.kind == "pocsag_decode")
        .count();
    assert_eq!(decoders, 1, "{:#?}", node.job_list());
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hand_started_decoder_inside_the_band_is_left_alone() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag3, false).await;
    let node = Arc::new(node);
    // The capture held at the recording's band, as a pin holds a radio.
    let pin = Op::SourcePin {
        source: rx(),
        center_hz: Hz(CENTER_HZ),
        bandwidth_hz: Hz(RATE_HZ as u64),
        replace: vec![],
    };
    node.apply(pin).await.unwrap();
    let op = Op::JobStart {
        kind: JobKind::from_name("pocsag_decode").unwrap(),
        source: rx(),
        center_hz: Hz(CENTER_HZ + OFFSET_HZ),
        bandwidth_hz: Hz(POCSAG_WIDTH_HZ),
        deviation_hz: None,
        sideband: None,
        elastic: false,
        replace: vec![],
    };
    let hand = match node.apply(op).await {
        Ok(OpResult::Job(j)) => j,
        r => panic!("job_start: {r:?}"),
    };
    let watch = start_watch(&node, span_params(&["pocsag_decode"])).await;
    let deadline = tokio::time::Instant::now() + SPAN_ALL_THREE;
    while decoding(&node, &watch).len() < 2 {
        assert!(!decoder_ids(&node, &watch).contains(&hand));
        assert!(
            tokio::time::Instant::now() < deadline,
            "{:?}",
            node.job_context(&watch)
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    // Two cycles more: the hand-started page never gets a second decoder.
    tokio::time::sleep(Duration::from_millis(2 * GONE_MS)).await;
    let ids = decoder_ids(&node, &watch);
    assert_eq!(ids.len(), 2, "{:?}", node.job_context(&watch));
    assert!(!ids.contains(&hand), "{ids:?}");
    for job in &ids {
        let rec = record(&node, job).unwrap();
        assert!(!on_page(rec.tuning.center_hz.0, OFFSET_HZ), "{rec:?}");
    }
    node.job_stop(&watch).await.unwrap();
    until("the watch's decoders stop", Duration::from_secs(10), || {
        ids.iter()
            .all(|j| state(&node, j) != Some(JobState::Running))
    })
    .await;
    assert_eq!(state(&node, &hand), Some(JobState::Running));
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_a_span_watch_stops_every_decoder() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag3, false).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, span_params(&["pocsag_decode"])).await;
    let jobs = n_decoders_without_identify(&node, &watch, 3).await;
    node.job_stop(&watch).await.unwrap();
    until("none of the three runs", Duration::from_secs(10), || {
        jobs.iter()
            .all(|j| state(&node, j) == Some(JobState::Stopped))
    })
    .await;
    assert_eq!(state(&node, &watch), Some(JobState::Stopped));
    until("no lease on rx", Duration::from_secs(5), || {
        node.lease_holders(&rx()).is_empty()
    })
    .await;
    assert!(running(&node).is_empty(), "{:?}", running(&node));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replace_of_a_span_watch_stops_its_decoders() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag3, false).await;
    let node = Arc::new(node);
    let watch = start_watch(&node, span_params(&["pocsag_decode"])).await;
    let jobs = n_decoders_without_identify(&node, &watch, 3).await;
    let op = Op::JobStart {
        kind: JobKind::Measure,
        source: rx(),
        center_hz: Hz(POCSAG_HZ),
        bandwidth_hz: Hz(POCSAG_WIDTH_HZ),
        deviation_hz: None,
        sideband: None,
        elastic: false,
        replace: vec![watch.clone()],
    };
    let measure = match node.apply(op).await {
        Ok(OpResult::Job(j)) => j,
        r => panic!("replace of the watch: {r:?}"),
    };
    until(
        "the watch and its decoders stop",
        Duration::from_secs(10),
        || {
            state(&node, &watch) == Some(JobState::Stopped)
                && jobs
                    .iter()
                    .all(|j| state(&node, j) == Some(JobState::Stopped))
        },
    )
    .await;
    let only_measure = [(measure.0.clone(), "measure".to_string())];
    until("only the measure job runs", Duration::from_secs(10), || {
        running(&node) == only_measure
    })
    .await;
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_span_watch_with_decode_on_free_starts_its_decoder_on_a_free_source() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let a = file_source("rx", &fixtures().pocsag_fsk);
    let b = file_source_with("rx2", &fixtures().pocsag_fsk, false);
    let ports: Vec<(Arc<dyn Port>, Arc<FileSource>)> = vec![(a.clone(), a), (b.clone(), b)];
    let node = node_over(dir.path(), ports, None).await;
    let node = Arc::new(node);
    let mut params = span_params(&["pocsag_decode"]);
    params["decode_on"] = "free".into();
    params["prefer"] = serde_json::json!(["rx2"]);
    let watch = start_watch(&node, params).await;
    let events = Events::of(&node, &watch);
    let jobs = n_decoders_without_identify(&node, &watch, 1).await;
    let job = jobs[0].clone();
    until("a decoding event", Duration::from_secs(5), || {
        !events.watch("decoding").is_empty()
    })
    .await;
    let d = &events.watch("decoding")[0];
    assert_eq!(d["via"], "modes", "{d}");
    assert_eq!(d["placed"], "free", "{d}");
    assert_eq!(d["source"], "rx2", "{d}");
    assert!(
        d["summary"]
            .as_str()
            .unwrap()
            .ends_with("on rx2 (free, modes)"),
        "{d}"
    );
    assert!(
        events.watch("identifying").is_empty(),
        "{:#?}",
        events.all()
    );
    let rec = record(&node, &job).unwrap();
    assert_eq!(rec.source, SourceId("rx2".into()));
    assert_eq!(rec.kind, "pocsag_decode");
    assert_eq!(rec.tuning.bandwidth_hz, Hz(POCSAG_WIDTH_HZ), "{rec:?}");
    assert!(on_page(rec.tuning.center_hz.0, OFFSET_HZ), "{rec:?}");
    let ctx = node.job_context(&watch).unwrap();
    assert_eq!(ctx["watch"]["decoding"][0]["placed"], "free", "{ctx}");
    assert_eq!(ctx["watch"]["decoding"][0]["via"], "modes", "{ctx}");
    // Only the watch holds its own radio; the decoder holds the free one.
    let holders = |s: &str| -> Vec<JobId> {
        node.lease_holders(&SourceId(s.into()))
            .into_iter()
            .map(|(j, _)| j)
            .collect()
    };
    assert_eq!(holders("rx"), std::slice::from_ref(&watch));
    assert_eq!(holders("rx2"), std::slice::from_ref(&job));
    node.job_stop(&watch).await.unwrap();
    until("the decoder stops", Duration::from_secs(10), || {
        state(&node, &job) == Some(JobState::Stopped)
    })
    .await;
    assert!(running(&node).is_empty(), "{:?}", running(&node));
}

/// A width window wide enough for the suite's loud page (read 19-25 kHz).
const LOUD_WIDTH_HZ: [u64; 2] = [6_000, 40_000];
/// A width window wide enough for the `wide` fixture's burst.
const WIDE_WIDTH_HZ: [u64; 2] = [6_000, 200_000];
/// Identify's own width fit: the most a 12.5 kHz row takes by it.
const IDENTIFY_FIT_MAX_HZ: u64 = POCSAG_WIDTH_HZ * 3 / 2;
/// The span fit's upper bound for a 12.5 kHz row.
const SPAN_FIT_MAX_HZ: u64 = 2 * POCSAG_WIDTH_HZ;

/// Publish a `detect` announcement `state` of a signal at `center_hz`, `bandwidth_hz`
/// wide, on `watch`'s event stream, as the detector would.
fn announce(node: &Node, watch: &JobId, state: &str, center_hz: u64, bandwidth_hz: u64) {
    let out = node
        .job_outputs(watch)
        .into_iter()
        .find(|(k, _)| *k == StreamKind::Event)
        .unwrap()
        .1;
    let prov = node
        .bus()
        .list()
        .into_iter()
        .find(|i| i.id == out)
        .unwrap()
        .provenance;
    let v = serde_json::json!({"protocol": "signal", "state": state, "center_hz": center_hz,
        "bandwidth_hz": bandwidth_hz, "snr_db": 30.0});
    node.bus().publish(Frame::new(out, prov, Payload::Event(v)));
}

/// The width the detector reads a strong 12.5 kHz page at (its skirts).
const PAGE_READ_WIDE_HZ: u64 = 22_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_read_wide_still_starts_its_decoder_directly() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    // Noise only: the one signal is the announcement the test makes, read as wide as the
    // detector reads the suite's loud page (19-25 kHz), wider than identify's rule takes.
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag, true).await;
    let node = Arc::new(node);
    let mut params = span_params(&["pocsag_decode"]);
    params["width_hz"] = serde_json::json!(LOUD_WIDTH_HZ);
    let watch = start_watch(&node, params).await;
    let events = Events::of(&node, &watch);
    const { assert!(PAGE_READ_WIDE_HZ > IDENTIFY_FIT_MAX_HZ) };
    until("the watch runs its loop", Duration::from_secs(5), || {
        node.job_context(&watch)
            .is_some_and(|c| c["watch"]["cap"].is_object())
    })
    .await;
    announce(&node, &watch, "new", POCSAG_HZ, PAGE_READ_WIDE_HZ);
    let jobs = n_decoders_without_identify(&node, &watch, 1).await;
    until("a decoding event", Duration::from_secs(5), || {
        !events.watch("decoding").is_empty()
    })
    .await;
    let all = events.all();
    assert!(events.watch("identifying").is_empty(), "{all:#?}");
    let d = &events.watch("decoding")[0];
    assert_eq!(d["via"], "modes", "{d}");
    assert_eq!(d["signal"]["bandwidth_hz"], PAGE_READ_WIDE_HZ, "{d}");
    let rec = record(&node, &jobs[0]).unwrap();
    assert_eq!(rec.kind, "pocsag_decode");
    assert_eq!(rec.tuning.center_hz, Hz(POCSAG_HZ), "{rec:?}");
    assert_eq!(rec.tuning.bandwidth_hz, Hz(POCSAG_WIDTH_HZ), "{rec:?}");
    let ctx = node.job_context(&watch).unwrap();
    assert_eq!(
        ctx["watch"]["signals"][0]["fits"],
        serde_json::json!(["pocsag_decode"]),
        "{ctx}"
    );
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_far_wider_signal_fits_no_listed_mode() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().wide, false).await;
    let node = Arc::new(node);
    let mut params = span_params(&["pocsag_decode", "aprs_decode"]);
    params["width_hz"] = serde_json::json!(WIDE_WIDTH_HZ);
    let watch = start_watch(&node, params).await;
    let events = Events::of(&node, &watch);
    until("an unknown", Duration::from_secs(30), || {
        !events.watch("unknown").is_empty()
    })
    .await;
    let all = events.all();
    let e = &events.watch("unknown")[0];
    let read = e["signal"]["bandwidth_hz"].as_u64().unwrap();
    assert!(read > SPAN_FIT_MAX_HZ, "{e}");
    let summary = e["summary"].as_str().unwrap();
    assert!(summary.contains("no listed mode fits"), "{e}");
    assert!(events.watch("identifying").is_empty(), "{all:#?}");
    assert!(events.watch("decoding").is_empty(), "{all:#?}");
    assert!(owned(&node, &watch).is_empty());
    stop_all(&node).await;
}

/// The cap of the cap tests: two of `pocsag3`'s three pages decode, the third waits.
const CAP: u64 = 2;

/// `watch.cap` in the watch's context.
fn cap_of(node: &Node, watch: &JobId) -> Value {
    node.job_context(watch)
        .map(|c| c["watch"]["cap"].clone())
        .unwrap_or(Value::Null)
}

/// The `watch.signals` the watch marks waiting.
fn waiting_signals(node: &Node, watch: &JobId) -> Vec<Value> {
    node.job_context(watch)
        .and_then(|c| c["watch"]["signals"].as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s["waiting"] == true)
        .collect()
}

/// The `watch.cooled` channels.
fn cooled(node: &Node, watch: &JobId) -> Vec<Value> {
    node.job_context(watch)
        .and_then(|c| c["watch"]["cooled"].as_array().cloned())
        .unwrap_or_default()
}

/// The centre a decoder runs on.
fn decoder_center(node: &Node, job: &JobId) -> u64 {
    record(node, job).map_or(0, |r| r.tuning.center_hz.0)
}

/// Wait until a span watch over `pocsag3` capped at [`CAP`] decodes two pages and the
/// third waits; the third page's centre.
async fn capped(node: &Node, events: &Events, watch: &JobId) -> u64 {
    until("two decoders and one waiting", SPAN_ALL_THREE, || {
        decoding(node, watch).len() == CAP as usize
            && events.watch("waiting").len() == 1
            && waiting_signals(node, watch).len() == 1
    })
    .await;
    let waiting = events.watch("waiting")[0].clone();
    let summary = waiting["summary"].as_str().unwrap();
    assert!(summary.starts_with("waiting at "), "{waiting}");
    assert!(summary.ends_with(": 2 of 2 decoding"), "{waiting}");
    let hz = waiting["signal"]["center_hz"].as_u64().unwrap();
    assert!(
        on_page(hz, SPAN_OFFSETS_HZ[2]),
        "the third page waits: {waiting}"
    );
    until("cap {2, 2, 1}", Duration::from_secs(5), || {
        cap_of(node, watch) == serde_json::json!({"max": CAP, "decoding": 2, "waiting": 1})
    })
    .await;
    hz
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cap_queues_and_a_freed_slot_is_taken_by_rank() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag3, false).await;
    let node = Arc::new(node);
    let mut params = span_params(&["pocsag_decode"]);
    params["max_decoders"] = CAP.into();
    let watch = start_watch(&node, params).await;
    let events = Events::of(&node, &watch);
    let third = capped(&node, &events, &watch).await;
    let first = decoder_ids(&node, &watch);
    node.job_stop(&first[0]).await.unwrap();
    until(
        "the waiting page takes the freed slot",
        Duration::from_secs(5),
        || {
            let ids = decoder_ids(&node, &watch);
            ids.len() == CAP as usize
                && !ids.contains(&first[0])
                && ids
                    .iter()
                    .any(|j| decoder_center(&node, j).abs_diff(third) <= SPAN_CENTRE_TOLERANCE_HZ)
        },
    )
    .await;
    until("cap {2, 2, 0}", Duration::from_secs(5), || {
        cap_of(&node, &watch) == serde_json::json!({"max": CAP, "decoding": 2, "waiting": 0})
    })
    .await;
    assert!(waiting_signals(&node, &watch).is_empty());
    // Never over the cap, and the one waiting event.
    assert_eq!(events.watch("decoding").len(), 3, "{:#?}", events.all());
    assert_eq!(events.watch("waiting").len(), 1, "{:#?}", events.all());
    stop_all(&node).await;
}

/// The quiet-channel test's hold and cooldown: the third page's decoder is lost soon
/// after the page's quiet spell starts (seconds 14-26 of the run), and the channel is
/// cooled past its return (a `new` about 27.5 s into the run).
const QUIET_HOLD_MS: u64 = 2_000;
const QUIET_COOLDOWN_MS: u64 = 15_000;

/// Wall-clock nanoseconds, as the watch's `until_ns`.
fn wall_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_quiet_channel_is_held_then_lost_and_cooled() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag3, false).await;
    let node = Arc::new(node);
    let mut params = span_params(&["pocsag_decode"]);
    params["hold_ms"] = QUIET_HOLD_MS.into();
    params["cooldown_ms"] = QUIET_COOLDOWN_MS.into();
    let watch = start_watch(&node, params).await;
    let events = Events::of(&node, &watch);
    let on_third = |hz: u64| on_page(hz, SPAN_OFFSETS_HZ[2]);
    let thirds = |state: &str| -> Vec<Value> {
        events
            .watch(state)
            .into_iter()
            .filter(|e| e["signal"]["center_hz"].as_u64().is_some_and(on_third))
            .collect()
    };
    until("the third page decodes", SPAN_ALL_THREE, || {
        thirds("decoding").len() == 1
    })
    .await;
    until("its decoder is lost", Duration::from_secs(20), || {
        !thirds("lost").is_empty()
    })
    .await;
    let lost = &thirds("lost")[0];
    assert_eq!(lost["why"], "gone and silent", "{lost}");
    let cool = cooled(&node, &watch);
    let channel = cool
        .iter()
        .find(|c| c["center_hz"].as_u64().is_some_and(on_third))
        .unwrap_or_else(|| panic!("cooled: {cool:#?}"))
        .clone();
    assert_eq!(channel["why"], "gone and silent", "{channel}");
    assert_eq!(channel["bandwidth_hz"], POCSAG_WIDTH_HZ, "{channel}");
    let until_ns = channel["until_ns"].as_u64().unwrap();
    // The page comes back inside the cooldown: tracked, neither started nor waiting.
    let news = || {
        events
            .signals()
            .iter()
            .filter(|s| s["state"] == "new")
            .filter(|s| s["center_hz"].as_u64().is_some_and(on_third))
            .count()
    };
    let before = news();
    until("the third page returns", Duration::from_secs(20), || {
        news() > before
    })
    .await;
    assert!(wall_ns() < until_ns, "the page returned after the cooldown");
    until("the watch tracks it", Duration::from_secs(5), || {
        node.job_context(&watch).is_some_and(|c| {
            c["watch"]["signals"].as_array().is_some_and(|s| {
                s.iter()
                    .any(|x| x["present"] == true && x["center_hz"].as_u64().is_some_and(on_third))
            })
        })
    })
    .await;
    // After the cooldown it starts again, and not before.
    until(
        "a decoder after the cooldown",
        Duration::from_millis(QUIET_COOLDOWN_MS + 10_000),
        || thirds("decoding").len() == 2,
    )
    .await;
    let again = &thirds("decoding")[1];
    assert!(again["t_ns"].as_u64().unwrap() >= until_ns, "{again}");
    assert!(events.watch("waiting").is_empty(), "{:#?}", events.all());
    stop_all(&node).await;
}

/// The unproven test's `confirm_ms`: the page's decoder publishes within it (a page every
/// 2 s, decoded at its end), the noise's never does.
const CONFIRM_MS: u64 = 5_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unproven_decoder_is_stopped_and_its_channel_cooled() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().noise12k, false).await;
    let node = Arc::new(node);
    let mut params = span_params(&["pocsag_decode"]);
    params["confirm_ms"] = CONFIRM_MS.into();
    let watch = start_watch(&node, params).await;
    let events = Events::of(&node, &watch);
    let near_noise = |hz: u64| hz.abs_diff(CENTER_HZ + NOISE_OFFSET_HZ) <= SPAN_CENTRE_TOLERANCE_HZ;
    let on_noise = |e: &Value| e["signal"]["center_hz"].as_u64().is_some_and(near_noise);
    until("the noise's decoder starts", SPAN_ALL_THREE, || {
        events
            .watch("decoding")
            .iter()
            .any(|e| on_noise(e) && e["via"] == "modes")
    })
    .await;
    until(
        "it is lost unproven",
        Duration::from_millis(CONFIRM_MS + 5_000),
        || events.watch("lost").iter().any(on_noise),
    )
    .await;
    let lost = events.watch("lost").into_iter().find(on_noise).unwrap();
    assert_eq!(lost["why"], "unproven", "{lost}");
    let summary = lost["summary"].as_str().unwrap();
    assert!(summary.contains("unproven"), "{lost}");
    let job = JobId(lost["job"].as_str().unwrap().into());
    until("its job stops", Duration::from_secs(5), || {
        state(&node, &job) == Some(JobState::Stopped)
    })
    .await;
    let cool = cooled(&node, &watch);
    let named = cool
        .iter()
        .any(|c| c["why"] == "unproven" && c["center_hz"].as_u64().is_some_and(near_noise));
    assert!(named, "{cool:#?}");
    // The page publishes: its decoder is kept past the confirm.
    let page = events
        .watch("decoding")
        .into_iter()
        .find(|e| {
            e["signal"]["center_hz"]
                .as_u64()
                .is_some_and(|hz| on_page(hz, OFFSET_HZ))
        })
        .unwrap_or_else(|| panic!("{:#?}", events.all()));
    let page_job = JobId(page["job"].as_str().unwrap().into());
    tokio::time::sleep(Duration::from_millis(CONFIRM_MS)).await;
    assert_eq!(decoder_ids(&node, &watch), std::slice::from_ref(&page_job));
    assert_eq!(state(&node, &page_job), Some(JobState::Running));
    let page_lost = events
        .watch("lost")
        .iter()
        .any(|e| e["job"] == page_job.0.as_str());
    assert!(!page_lost, "{:#?}", events.all());
    stop_all(&node).await;
}

/// The churn test's timing: each start of the noise's decoder is unproven quickly, and
/// its channel and retry come round soon after.
const CHURN_CONFIRM_MS: u64 = 1_500;
const CHURN_COOLDOWN_MS: u64 = 1_000;
const CHURN_RETRY_MS: u64 = 1_000;
/// How long the churn test watches for a start past the last try: several rounds.
const CHURN_QUIET: Duration = Duration::from_secs(10);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_carrier_no_decoder_proves_stops_being_restarted_after_its_tries() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(CHURN_RETRY_MS, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().noise12k, false).await;
    let node = Arc::new(node);
    let mut params = span_params(&["pocsag_decode"]);
    params["confirm_ms"] = CHURN_CONFIRM_MS.into();
    params["cooldown_ms"] = CHURN_COOLDOWN_MS.into();
    let watch = start_watch(&node, params).await;
    let events = Events::of(&node, &watch);
    let near_noise = |hz: u64| hz.abs_diff(CENTER_HZ + NOISE_OFFSET_HZ) <= SPAN_CENTRE_TOLERANCE_HZ;
    let on_noise = |e: &Value| e["signal"]["center_hz"].as_u64().is_some_and(near_noise);
    let count = |state: &str| events.watch(state).iter().filter(|e| on_noise(e)).count();
    // The first start and RETRY_MAX more, each lost unproven.
    let starts = zeta_core::node::watch::RETRY_MAX as usize + 1;
    until(
        "every try is lost unproven",
        Duration::from_secs(40),
        || count("lost") >= starts,
    )
    .await;
    tokio::time::sleep(CHURN_QUIET).await;
    let all = events.all();
    assert_eq!(count("decoding"), starts, "{all:#?}");
    assert_eq!(count("lost"), starts, "{all:#?}");
    for e in events.watch("lost").iter().filter(|e| on_noise(e)) {
        assert_eq!(e["why"], "unproven", "{e}");
    }
    // Still tracked and present, left alone.
    let ctx = node.job_context(&watch).unwrap();
    let noise = ctx["watch"]["signals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["center_hz"].as_u64().is_some_and(near_noise))
        .cloned()
        .unwrap_or_else(|| panic!("{ctx}"));
    assert_eq!(noise["present"], true, "{noise}");
    assert_eq!(noise["handled"], true, "{noise}");
    stop_all(&node).await;
    zeta_core::node::watch::test_timing(0, 0);
}

/// The jobs_stop test's cooldown: the stopped channels rest this long before they start
/// again.
const STOPPED_COOLDOWN_MS: u64 = 2_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn jobs_stop_of_the_decoders_frees_the_cap() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag3, false).await;
    let node = Arc::new(node);
    let mut params = span_params(&["pocsag_decode"]);
    params["max_decoders"] = CAP.into();
    params["cooldown_ms"] = STOPPED_COOLDOWN_MS.into();
    let watch = start_watch(&node, params).await;
    let events = Events::of(&node, &watch);
    let third = capped(&node, &events, &watch).await;
    let first = decoder_ids(&node, &watch);
    let op = Op::JobsStop {
        source: Some(rx()),
        kind: JobKind::from_name("pocsag_decode"),
        plan: None,
    };
    match node.apply(op).await {
        Ok(OpResult::JobsStopped { stopped, failed }) => {
            assert!(failed.is_empty(), "{failed:?}");
            for j in &first {
                assert!(stopped.contains(j), "{stopped:?}");
            }
        }
        r => panic!("jobs_stop: {r:?}"),
    }
    until("the slots fill again", Duration::from_secs(20), || {
        let ids = decoder_ids(&node, &watch);
        ids.len() == CAP as usize
            && ids
                .iter()
                .all(|j| !first.contains(j) && state(&node, j) == Some(JobState::Running))
    })
    .await;
    // The waiting page took a slot.
    let ids = decoder_ids(&node, &watch);
    let took = ids
        .iter()
        .any(|j| decoder_center(&node, j).abs_diff(third) <= SPAN_CENTRE_TOLERANCE_HZ);
    assert!(took, "{:?}", node.job_context(&watch));
    for j in &first {
        assert_eq!(state(&node, j), Some(JobState::Stopped));
    }
    let cap = cap_of(&node, &watch);
    assert_eq!(cap["decoding"], CAP, "{cap}");
    stop_all(&node).await;
}

/// The re-adoption test's hold: past the detector's `gone_ms`, and past the third page's
/// quiet spell (seconds 13-27 of a run, between its last decoded page and its next `new`).
const READOPT_HOLD_MS: u64 = 20_000;
/// How long the re-adoption test watches the resumed watch.
const READOPT_WATCH: Duration = Duration::from_secs(25);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_span_watch_is_readopted_with_the_hold() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    const { assert!(READOPT_HOLD_MS > READOPT_GONE_MS) };
    let dir = tempfile::tempdir().unwrap();
    let jobs = dir.path().join("state").join("jobs");
    let file = |id: &JobId| jobs.join(format!("{}.json", id.0));
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag3, false).await;
    let node = Arc::new(node);
    let mut params = span_params(&["pocsag_decode"]);
    params["gone_ms"] = READOPT_GONE_MS.into();
    params["hold_ms"] = READOPT_HOLD_MS.into();
    let watch = start_watch(&node, params).await;
    let ids = n_decoders_without_identify(&node, &watch, 3).await;
    // The records as a kill mid-decode leaves them.
    let recs: Vec<(PathBuf, String)> = std::iter::once(&watch)
        .chain(ids.iter())
        .map(|id| (file(id), std::fs::read_to_string(file(id)).unwrap()))
        .collect();
    let v: Value = serde_json::from_str(&recs[0].1).unwrap();
    let kept = v["context"]["watch"]["decoding"].as_array().unwrap().len();
    assert_eq!(kept, 3, "{v}");
    drop(node);
    tokio::time::sleep(Duration::from_millis(500)).await;
    for (path, text) in &recs {
        std::fs::write(path, text).unwrap();
    }
    // The recording starts over: the third page is quiet for its first 6 s, longer than
    // `gone_ms`, shorter than the hold.
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag3, false).await;
    let node = Arc::new(node);
    let events = Events::of(&node, &watch);
    assert_eq!(state(&node, &watch), Some(JobState::Running));
    tokio::time::sleep(READOPT_WATCH).await;
    let mut listed = decoder_ids(&node, &watch);
    listed.sort_by(|a, b| a.0.cmp(&b.0));
    let mut want = ids.clone();
    want.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(listed, want, "{:?}", node.job_context(&watch));
    for j in &ids {
        assert_eq!(state(&node, j), Some(JobState::Running));
    }
    assert!(events.watch("lost").is_empty(), "{:#?}", events.all());
    assert!(events.watch("decoding").is_empty(), "{:#?}", events.all());
    stop_all(&node).await;
}

/// The seeding test's register: one 12.5 kHz data transmitter licensed at the second
/// page of `pocsag3` (which is also `pocsag_decode`'s listed centre).
fn telco() -> Allocation {
    telco_at(CENTER_HZ + SPAN_OFFSETS_HZ[1])
}

/// A 12.5 kHz data transmitter licensed at `center_hz` to Telco, Mount X, 3 km away.
fn telco_at(center_hz: u64) -> Allocation {
    assignment(center_hz, 12_500, "12K5F1D", "Telco", "Mount X", 3.0)
}

/// The `watch.known` record.
fn known(node: &Node, watch: &JobId) -> Value {
    node.job_context(watch)
        .map(|c| c["watch"]["known"].clone())
        .unwrap_or_default()
}

/// The known channels' `(center_hz, from)`.
fn known_channels(node: &Node, watch: &JobId) -> Vec<(u64, String)> {
    known(node, watch)["channels"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|k| {
            (
                k["center_hz"].as_u64().unwrap_or(0),
                k["from"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The known-rank test's cooldown: the stopped decoder's channel stays out of the race
/// for the freed slot.
const RANK_COOLDOWN_MS: u64 = 30_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn known_channels_rank_first_and_name_the_licensee() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().known3, false).await;
    let licensed = CENTER_HZ + KNOWN_OFFSETS_HZ[2];
    node.add_knowledge(Arc::new(Register::with(vec![telco_at(licensed)])));
    let node = Arc::new(node);
    let mut params = span_params(&["pocsag_decode"]);
    params["max_decoders"] = 1.into();
    params["cooldown_ms"] = RANK_COOLDOWN_MS.into();
    let watch = start_watch(&node, params).await;
    let events = Events::of(&node, &watch);
    until("the known channels", Duration::from_secs(5), || {
        known_channels(&node, &watch)
            == [
                (POCSAG_HZ, "row".to_string()),
                (licensed, "register".to_string()),
            ]
    })
    .await;
    let k = known(&node, &watch);
    assert_eq!(k["providers"], 1, "{k}");
    assert_eq!(k["assignments"], 1, "{k}");
    let register = &k["channels"][1];
    assert_eq!(register["licensee"], "Telco", "{k}");
    assert_eq!(register["site"], "Mount X", "{k}");
    assert_eq!(register["mode"], "pocsag_decode", "{k}");
    assert_eq!(register["sites"], 1, "{k}");
    assert_eq!(register["bandwidth_hz"], POCSAG_WIDTH_HZ, "{k}");
    assert_eq!(k["channels"][0]["sites"], 0, "{k}");
    // An unknown page takes the one slot; the other unknown page and, once on air, the
    // register's channel wait.
    let on = |s: &Value, off: u64| s["center_hz"].as_u64().is_some_and(|hz| on_page(hz, off));
    until("one decoding and two waiting", SPAN_ALL_THREE, || {
        let waiting = waiting_signals(&node, &watch);
        decoder_ids(&node, &watch).len() == 1
            && waiting.len() == 2
            && waiting.iter().any(|s| on(s, KNOWN_OFFSETS_HZ[2]))
    })
    .await;
    let first = decoder_ids(&node, &watch);
    let running = decoder_center(&node, &first[0]);
    assert!(
        on_page(running, KNOWN_OFFSETS_HZ[0]) || on_page(running, KNOWN_OFFSETS_HZ[1]),
        "an unknown page decodes first: {running}"
    );
    // The freed slot goes to the known channel over the unknown page that waited longer.
    node.job_stop(&first[0]).await.unwrap();
    until(
        "the register's channel takes the freed slot",
        Duration::from_secs(5),
        || {
            let ids = decoder_ids(&node, &watch);
            ids.len() == 1 && on_page(decoder_center(&node, &ids[0]), KNOWN_OFFSETS_HZ[2])
        },
    )
    .await;
    let e = events
        .watch("decoding")
        .into_iter()
        .find(|e| on(&e["signal"], KNOWN_OFFSETS_HZ[2]))
        .unwrap_or_else(|| panic!("{:#?}", events.all()));
    let summary = e["summary"].as_str().unwrap();
    assert!(
        summary.contains("; register says 12K5F1D (12.5 kHz FM data) Telco, Mount X, 3 km"),
        "{e}"
    );
    // The unknown page still waits.
    until("the unknown page waits", Duration::from_secs(5), || {
        let waiting = waiting_signals(&node, &watch);
        waiting.len() == 1 && !on(&waiting[0], KNOWN_OFFSETS_HZ[2])
    })
    .await;
    stop_all(&node).await;
}

/// The seeding test's hold: the seeded decoder is lost this long after its start.
const SEED_HOLD_MS: u64 = 2_000;
/// How long the seeding test's register takes: the test's event collector subscribes
/// meanwhile, so it sees the seeded decoder's `decoding`.
const SEED_REGISTER_DELAY: Duration = Duration::from_millis(500);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn seed_known_starts_the_known_channel_before_any_signal() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    // Noise only: the detector never announces a signal.
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag3, true).await;
    node.add_knowledge(Arc::new(Register::slow(vec![telco()], SEED_REGISTER_DELAY)));
    let node = Arc::new(node);
    let mut params = span_params(&["pocsag_decode"]);
    params["seed_known"] = true.into();
    params["hold_ms"] = SEED_HOLD_MS.into();
    let watch = start_watch(&node, params).await;
    let events = Events::of(&node, &watch);
    until("a seeded decoder", Duration::from_secs(10), || {
        !events.watch("decoding").is_empty()
    })
    .await;
    let e = events.watch("decoding")[0].clone();
    assert_eq!(e["via"], "modes", "{e}");
    assert_eq!(e["mode"], "pocsag_decode", "{e}");
    let center = CENTER_HZ + SPAN_OFFSETS_HZ[1];
    assert_eq!(e["signal"]["center_hz"], center, "{e}");
    let news = || {
        events
            .signals()
            .iter()
            .filter(|s| s["state"] == "new")
            .count()
    };
    assert_eq!(news(), 0, "{:#?}", events.all());
    // The row's centre and the register's are one channel: one decoder.
    let job = JobId(e["job"].as_str().unwrap().into());
    assert_eq!(decoder_center(&node, &job), center);
    until(
        "it is lost",
        Duration::from_millis(SEED_HOLD_MS + 5_000),
        || !events.watch("lost").is_empty(),
    )
    .await;
    let lost = &events.watch("lost")[0];
    assert_eq!(lost["job"], job.0.as_str(), "{lost}");
    assert_eq!(lost["why"], "not seen again", "{lost}");
    assert_eq!(events.watch("decoding").len(), 1, "{:#?}", events.all());
    assert_eq!(news(), 0, "{:#?}", events.all());
    stop_all(&node).await;
}

/// The unreachable register's delay before it answers nothing.
const UNREACHABLE_DELAY: Duration = Duration::from_millis(50);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unreachable_register_leaves_known_from_rows_only() {
    let _one = SERIAL.lock().await;
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let (node, _port) = node_on(dir.path(), &fixtures().pocsag3, false).await;
    node.add_knowledge(Arc::new(Register::slow(Vec::new(), UNREACHABLE_DELAY)));
    let node = Arc::new(node);
    let watch = start_watch(&node, span_params(&["pocsag_decode"])).await;
    until("the row's channel only", Duration::from_secs(5), || {
        known_channels(&node, &watch) == [(POCSAG_HZ, "row".to_string())]
    })
    .await;
    let k = known(&node, &watch);
    assert_eq!(k["providers"], 1, "{k}");
    assert_eq!(k["assignments"], 0, "{k}");
    n_decoders_without_identify(&node, &watch, 2).await;
    stop_all(&node).await;
}
