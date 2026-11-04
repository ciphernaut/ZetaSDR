//! Test-only fixture helpers, behind feature `test-fixtures`: SigMF writing, speech-like
//! audio and real noise. Every generator of [`crate::synth`] is re-exported here, so
//! `zeta_dsp::fixtures::aprs_packet_iq` and friends keep working.

use std::f64::consts::PI;
use std::path::{Path, PathBuf};

use num_complex::Complex;
use zeta_core::capture::{CaptureMeta, write_cf32};
use zeta_core::types::{Hz, Timestamp};

pub use crate::synth::*;

/// Speech-like test audio: 300 ms bursts of a 3-tone complex (220, 440 and
/// 1 320 Hz, equal weight) with a 4 Hz amplitude tremolo, separated by 300 ms of
/// silence, starting with a burst; peak amplitude 0.5.
pub fn speech_like(rate_hz: u32, seconds: f32) -> Vec<f32> {
    let len = (rate_hz as f64 * seconds as f64) as usize;
    let period = rate_hz as usize * 600 / 1000;
    let burst = rate_hz as usize * 300 / 1000;
    (0..len)
        .map(|n| {
            if n % period >= burst {
                return 0.0;
            }
            let t = n as f64 / rate_hz as f64;
            let tones: f64 = [220.0, 440.0, 1320.0]
                .iter()
                .map(|f| (2.0 * PI * f * t).sin())
                .sum::<f64>()
                / 3.0;
            let tremolo = 0.75 + 0.25 * (2.0 * PI * 4.0 * t).sin();
            (0.5 * tremolo * tones) as f32
        })
        .collect()
}

/// Write `iq` as a cf32_le SigMF pair `<stem>.sigmf-{data,meta}` in `dir`
/// (start time 0, no annotations); returns the meta path. Panics on I/O error.
pub fn write_fixture(
    dir: &Path,
    stem: &str,
    iq: &[Complex<f32>],
    rate_hz: u32,
    center_hz: u64,
) -> PathBuf {
    let meta = CaptureMeta {
        sample_rate_hz: Hz(rate_hz as u64),
        center_hz: Hz(center_hz),
        start: Timestamp(0),
        annotations: Vec::new(),
    };
    write_cf32(dir, stem, &meta, iq).expect("write fixture")
}

/// An EV1527-style fixed-code OOK remote, synthesised (the public fixtures'
/// `ook-ev1527-synth-*`; its ID and buttons are invented). A frame is 24 bits, the
/// 20-bit `id` then the 4-bit `button`, MSB first, each bit four chips (a 0 one chip on
/// and three off, a 1 three on and one off), then the sync: one chip on and 31 off.
/// Every on run is `bias` chips short and every off run as much long, as an edge
/// threshold reads a real fob's pulses; `lead_ms` of silence comes before the first
/// frame. The carrier sits `offset_hz` from the centre at amplitude 1, each pulse rising and
/// falling over [`Ev1527Remote::EDGE_US`], and complex
/// white noise `snr_db` under that on level is added throughout.
#[derive(Clone, Debug, PartialEq)]
pub struct Ev1527Remote {
    pub id: u32,
    pub button: u8,
    pub frames: usize,
    pub chip_baud: f64,
    pub bias: f64,
    pub lead_ms: f64,
    pub offset_hz: f64,
    pub snr_db: f32,
    pub seed: u64,
}

impl Ev1527Remote {
    /// Bits of a frame's code: the 20-bit ID and the 4-bit button.
    pub const BITS: u32 = 24;
    /// Rise and fall of every pulse, in microseconds (a raised cosine), so the keyed
    /// carrier occupies tens of kHz rather than the whole capture.
    pub const EDGE_US: f64 = 20.0;

    /// The 24-bit code a frame carries: the ID, then the button nibble.
    pub fn code(&self) -> u32 {
        ((self.id & 0xF_FFFF) << 4) | u32::from(self.button & 0xF)
    }

    /// The keyed runs of every frame, `(chips, on)`, bias applied.
    fn runs(&self) -> Vec<(f64, bool)> {
        let code = self.code();
        let mut out = Vec::new();
        for _ in 0..self.frames {
            for b in (0..Self::BITS).rev() {
                let (on, off) = if (code >> b) & 1 == 1 {
                    (3.0, 1.0)
                } else {
                    (1.0, 3.0)
                };
                out.push((on - self.bias, true));
                out.push((off + self.bias, false));
            }
            out.push((1.0 - self.bias, true));
            out.push((31.0 + self.bias, false));
        }
        out
    }

    /// The capture at `rate_hz`: the lead, the frames, the noise.
    pub fn iq(&self, rate_hz: u32) -> Vec<Complex<f32>> {
        let rate = f64::from(rate_hz);
        let chip = rate / self.chip_baud;
        let lead = self.lead_ms * rate / 1000.0;
        // Run edges in samples from the start, then each sample on when it lies inside
        // an on run.
        let mut edges = Vec::new();
        let mut t = lead;
        for (chips, on) in self.runs() {
            let end = t + chips * chip;
            if on {
                edges.push((t, end));
            }
            t = end;
        }
        let n = t.ceil() as usize;
        let step = self.offset_hz / rate;
        let ramp = (Self::EDGE_US * rate / 1e6).max(1e-9);
        let mut iq = vec![Complex::new(0.0f32, 0.0); n];
        let mut k = 0;
        for (i, s) in iq.iter_mut().enumerate() {
            let x = i as f64 + 0.5;
            while k < edges.len() && edges[k].1 <= x {
                k += 1;
            }
            if k < edges.len() && edges[k].0 <= x {
                // A raised-cosine edge of `EDGE_US`, as a fob's transmitter keys.
                let into = (x - edges[k].0).min(edges[k].1 - x) / ramp;
                let a = if into >= 1.0 {
                    1.0
                } else {
                    0.5 - 0.5 * (PI * into).cos()
                };
                let (sn, cs) = (2.0 * PI * (i as f64 * step).fract()).sin_cos();
                *s = Complex::new((a * cs) as f32, (a * sn) as f32);
            }
        }
        // Noise under the on level (power 1): `awgn` over silence takes the signal as 1.
        let mut noise = vec![Complex::new(0.0f32, 0.0); n];
        crate::synth::awgn(&mut noise, self.snr_db, self.seed);
        for (s, w) in iq.iter_mut().zip(noise) {
            *s += w;
        }
        iq
    }
}

/// One public synthesised remote fixture: its file stem, the remote, and the capture.
#[derive(Clone, Debug)]
pub struct Ev1527Fixture {
    pub stem: &'static str,
    pub remote: Ev1527Remote,
    pub rate_hz: u32,
    pub center_hz: u64,
    /// The annotation's band about the centre, as a burst capture records it.
    pub band_hz: u64,
    /// `core:datetime`, in ns since the epoch.
    pub start_ns: u64,
}

/// The invented remote of the public fixtures: a 20-bit ID no real fob is known to use.
pub const SYNTH_REMOTE_ID: u32 = 0xC0DE1;

/// The public OOK fixtures in `crates/zeta-dsp/tests/data` (`gen_ook` writes them): two
/// buttons of [`SYNTH_REMOTE_ID`] at 433.92 MHz, a clean centred window at 51 200 Hz and
/// a weaker one at 62 500 Hz, 1.9 kHz off the centre, that opens on 20 ms of silence.
pub fn ev1527_fixtures() -> Vec<Ev1527Fixture> {
    vec![
        Ev1527Fixture {
            stem: "ook-ev1527-synth-1",
            remote: Ev1527Remote {
                id: SYNTH_REMOTE_ID,
                button: 0x8,
                frames: 8,
                chip_baud: 3_200.0,
                bias: 0.1,
                lead_ms: 2.0,
                offset_hz: 0.0,
                snr_db: 20.0,
                seed: 0x0E15_2701,
            },
            rate_hz: 51_200,
            center_hz: 433_920_000,
            band_hz: 50_000,
            start_ns: 1_767_225_600_000_000_000,
        },
        Ev1527Fixture {
            stem: "ook-ev1527-synth-2",
            remote: Ev1527Remote {
                id: SYNTH_REMOTE_ID,
                button: 0x4,
                frames: 8,
                chip_baud: 3_200.0,
                bias: 0.25,
                lead_ms: 20.0,
                offset_hz: 1_900.0,
                snr_db: 14.0,
                seed: 0x0E15_2702,
            },
            rate_hz: 62_500,
            center_hz: 433_920_000,
            band_hz: 60_000,
            start_ns: 1_767_225_660_000_000_000,
        },
    ]
}

/// Write fixture `f` into `dir` as a cf32_le SigMF pair. Its annotation records the
/// band's edges, the features a burst capture measures (`measure_band` over the band,
/// `zeta:features`) and what the window holds (`zeta:expected`: the remote's `id` and
/// the `codes` of its frames, as hex), so a test reads the answer from the file.
pub fn write_ev1527_fixture(dir: &Path, f: &Ev1527Fixture) -> PathBuf {
    let iq = f.remote.iq(f.rate_hz);
    let features = crate::measure::measure_band(&iq, u64::from(f.rate_hz), f.center_hz, f.band_hz);
    let meta = CaptureMeta {
        sample_rate_hz: Hz(u64::from(f.rate_hz)),
        center_hz: Hz(f.center_hz),
        start: Timestamp(f.start_ns),
        annotations: vec![serde_json::json!({
            "core:sample_start": 0,
            "core:sample_count": iq.len(),
            "core:freq_lower_edge": f.center_hz - f.band_hz / 2,
            "core:freq_upper_edge": f.center_hz + f.band_hz / 2,
            "core:description": "synthesised EV1527-style remote (invented ID), crates/zeta-dsp/examples/gen_ook.rs",
            "zeta:features": features,
            "zeta:expected": {
                "id": format!("0x{:05X}", f.remote.id),
                "codes": [format!("0x{:06X}", f.remote.code())],
            },
        })],
    };
    write_cf32(dir, f.stem, &meta, &iq).expect("write fixture")
}

/// Add real white Gaussian noise at `snr_db` below the mean power of real `x`
/// (taken as 1.0 if zero). Same deterministic LCG plus Box-Muller as [`awgn`].
pub fn awgn_real(x: &mut [f32], snr_db: f32, seed: u64) {
    if x.is_empty() {
        return;
    }
    let p: f64 = x.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / x.len() as f64;
    let p = if p > 0.0 { p } else { 1.0 };
    let sigma = (p / 10f64.powf(snr_db as f64 / 10.0)).sqrt();
    let mut state = seed;
    let mut uniform = || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((state >> 11) + 1) as f64 / (1u64 << 53) as f64
    };
    for s in x.iter_mut() {
        let (u1, u2) = (uniform(), uniform());
        *s += (sigma * (-2.0 * u1.ln()).sqrt() * (2.0 * PI * u2).cos()) as f32;
    }
}

/// Mean power in burst windows over mean power in silence windows, dB.
/// Windows are the inner 200 ms of each 300 ms burst / silence of the 600 ms period.
pub fn burst_to_silence_db(x: &[f32]) -> f64 {
    let rate = 48_000usize;
    let (period, win, margin) = (rate * 600 / 1000, rate * 200 / 1000, rate * 50 / 1000);
    let (mut pb, mut nb, mut ps, mut ns) = (0.0f64, 0usize, 0.0f64, 0usize);
    let mut start = 0;
    while start + period <= x.len() {
        for &s in &x[start + margin..start + margin + win] {
            pb += (s as f64).powi(2);
            nb += 1;
        }
        let sil = start + rate * 300 / 1000 + margin;
        for &s in &x[sil..sil + win] {
            ps += (s as f64).powi(2);
            ns += 1;
        }
        start += period;
    }
    10.0 * ((pb / nb as f64) / (ps / ns as f64)).log10()
}
