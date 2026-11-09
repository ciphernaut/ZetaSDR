//! The video chain's CPU cost at 30.72 Msps: a synthesised PAL card, FM-modulated, through
//! the `channel` stage's direct path (a 24 MHz band), `video_fm_demod` and
//! `composite_decode`, in 64 k blocks on one thread. Shared by the `bench_video` example
//! and the ignored `bench_video` test.

use std::sync::Arc;

use num_complex::Complex;
use zeta_dsp::channel::{DirectChannel, extract_direct};
use zeta_dsp::composite::{CompositeDecoder, CompositeParams};
use zeta_dsp::synth::{Impairments, SynthSpec, synthesise};
use zeta_dsp::video_fm::{VideoFmDemod, VideoFmParams, VideoStandard};

/// The rung a 24 MHz band puts a B210 capture on.
pub const RATE_HZ: u32 = 30_720_000;
/// The `video_decode` row's band.
pub const BAND_HZ: u64 = 24_000_000;
/// Blocks the chain is fed in.
pub const BLOCK: usize = 65_536;
const CENTER_HZ: u64 = 1_300_000_000;

/// One case: the card's deviation and where the carrier sits from the capture's centre.
#[derive(Clone, Copy, Debug)]
pub struct Case {
    pub deviation_hz: f64,
    pub offset_hz: i64,
}

/// CPU-seconds per signal-second of each stage, and the fields decoded.
#[derive(Clone, Copy, Debug)]
pub struct Cost {
    pub channel: f64,
    pub demod: f64,
    pub composite: f64,
    pub fields: usize,
}

/// This thread's CPU time, seconds: `/proc/thread-self/schedstat`'s time on the CPU, or the
/// wall clock where that is unreadable.
fn cpu_s(wall: &std::time::Instant) -> f64 {
    std::fs::read_to_string("/proc/thread-self/schedstat")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<u64>().ok())
        .map_or_else(|| wall.elapsed().as_secs_f64(), |ns| ns as f64 * 1e-9)
}

/// Run `case` over `seconds` of signal.
pub fn run(case: Case, seconds: f64) -> Cost {
    let fields = (seconds * 50.0).round() as u32;
    let spec = SynthSpec::Video {
        standard: VideoStandard::Pal,
        deviation_hz: case.deviation_hz,
        subcarrier_hz: None,
        rate_hz: RATE_HZ,
        fields,
        preemphasis: true,
    };
    let imp = Impairments {
        snr_db: Some(25.0),
        offset_hz: case.offset_hz as f64,
        ..Impairments::default()
    };
    let iq: Vec<Complex<f32>> = synthesise(&spec, &imp, RATE_HZ, CENTER_HZ);
    let blocks: Vec<Arc<[Complex<f32>]>> = iq.chunks(BLOCK).map(Arc::from).collect();
    drop(iq);
    let rate = u64::from(RATE_HZ);
    let mut chan = DirectChannel::new();
    let mut demod = VideoFmDemod::new(&VideoFmParams {
        deviation_hz: case.deviation_hz as u64,
        deemphasis: "ccir405".into(),
        composite_max_hz: 16_000_000,
        standard: None,
    })
    .expect("demod params");
    let mut dec = CompositeDecoder::new(&CompositeParams::default());
    let wall = std::time::Instant::now();
    let (mut c, mut d, mut v, mut n, mut got) = (0.0, 0.0, 0.0, 0usize, 0usize);
    for b in &blocks {
        let t0 = cpu_s(&wall);
        let (y, r) =
            extract_direct(&mut chan, b, rate, case.offset_hz, BAND_HZ, rate).expect("channel");
        let t1 = cpu_s(&wall);
        let (x, comp) = demod.process(&y, r).expect("demod");
        let t2 = cpu_s(&wall);
        let t_ns = (n as u128 * 1_000_000_000 / u128::from(rate)) as u64;
        got += dec.push(&x, comp, t_ns).len();
        let t3 = cpu_s(&wall);
        (c, d, v) = (c + t1 - t0, d + t2 - t1, v + t3 - t2);
        n += b.len();
    }
    let signal_s = n as f64 / rate as f64;
    Cost {
        channel: c / signal_s,
        demod: d / signal_s,
        composite: v / signal_s,
        fields: got,
    }
}

/// The cases the bench prints: 8 and 1.5 MHz deviation, the carrier on the capture's
/// centre and 1 MHz off it.
pub fn cases() -> Vec<Case> {
    let mut out = Vec::new();
    for deviation_hz in [8.0e6, 1.5e6] {
        for offset_hz in [0, 1_000_000] {
            out.push(Case {
                deviation_hz,
                offset_hz,
            });
        }
    }
    out
}
