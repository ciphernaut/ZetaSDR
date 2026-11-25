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

/// One keyed-over window of layer 1's tests and sweep (`tests/layer1.rs`,
/// `examples/l1_sweep.rs`): its name, the share of the window the carrier is up, the
/// channel `measure_selection` reads it through (centred) and the window.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyedOverCase {
    /// `keyed-<up|down|hole|whole>-d<share>-a<audio>-<snr>dB-<band>Hz`.
    pub name: String,
    pub share: f64,
    pub band_hz: u64,
    pub over: KeyedOver,
}

/// The rate of every keyed-over case, Hz.
pub const KEYED_OVER_RATE_HZ: u32 = 48_000;
/// The window of every keyed-over case, s.
pub const KEYED_OVER_SECONDS: f32 = 2.0;
/// The carrier-up shares of the key-up and key-down cases.
pub const KEYED_OVER_SHARES: [f64; 7] = [0.97, 0.95, 0.9, 0.75, 0.6, 0.4, 0.15];
/// The hole of the drop-out case, as a share of the window: the carrier is up but for it.
pub const KEYED_OVER_HOLE: (f64, f64) = (0.45, 0.55);
/// The carrier-to-noise ratios of the cases, dB.
pub const KEYED_OVER_SNRS_DB: [f32; 2] = [36.0, 15.0];
/// The channels, `(band, peak deviation)` in Hz: 12.5 and 25 kHz FM.
pub const KEYED_OVER_CHANNELS: [(u64, f64); 2] = [(12_500, 2_500.0), (25_000, 5_000.0)];

/// The keyed-over cases at each audio level of `audios`, each SNR of
/// [`KEYED_OVER_SNRS_DB`] and each channel of [`KEYED_OVER_CHANNELS`]: for each share `d`
/// of `shares` a key-up (the carrier up over `(1 - d, 1)`) and a key-down (`(0, d)`), or
/// one whole over for `d` of 1; and, with `hole`, the carrier up but for
/// [`KEYED_OVER_HOLE`]. A case's noise seed comes from its name.
pub fn keyed_over_cases(audios: &[f32], shares: &[f64], hole: bool) -> Vec<KeyedOverCase> {
    let mut keyings: Vec<(&str, f64, (f64, f64))> = Vec::new();
    for &d in shares {
        if d >= 1.0 {
            keyings.push(("whole", 1.0, (0.0, 1.0)));
        } else {
            keyings.push(("up", d, (1.0 - d, 1.0)));
            keyings.push(("down", d, (0.0, d)));
        }
    }
    if hole {
        let (a, b) = KEYED_OVER_HOLE;
        keyings.push(("hole", 1.0 - (b - a), (b, a)));
    }
    let mut out = Vec::new();
    for &audio in audios {
        for &snr_db in &KEYED_OVER_SNRS_DB {
            for &(band_hz, deviation_hz) in &KEYED_OVER_CHANNELS {
                for &(kind, share, on) in &keyings {
                    let name =
                        format!("keyed-{kind}-d{share:.2}-a{audio:.1}-{snr_db:.0}dB-{band_hz}Hz");
                    // FNV-1a of the name: a case's noise is the same in every list it is in.
                    let seed = name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
                        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
                    });
                    out.push(KeyedOverCase {
                        name,
                        share,
                        band_hz,
                        over: KeyedOver {
                            rate_hz: KEYED_OVER_RATE_HZ,
                            seconds: KEYED_OVER_SECONDS,
                            deviation_hz,
                            audio,
                            on,
                            snr_db,
                            seed,
                        },
                    });
                }
            }
        }
    }
    out
}

/// What a weak-signal case of layer 1 holds ([`weak_cases`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WeakSignal {
    /// An NBFM over ([`keyed_over`]) at this peak deviation, Hz, and audio level (1.0
    /// speech, 0.0 a bare carrier), the carrier up over `on` of the window.
    Fm {
        deviation_hz: f64,
        audio: f32,
        on: (f64, f64),
    },
    /// An NBFM over of voice-like audio ([`voice_like`]: noise in the speech band under a
    /// syllable envelope, no steady tone for a keying or tone rule to find) at this peak
    /// deviation, Hz, the carrier up over `on` of the window.
    Voice { deviation_hz: f64, on: (f64, f64) },
    /// Full-carrier AM at this index, up for the whole window: of a 1 kHz tone, or with
    /// `speech` of [`speech_like`] audio brought to a peak of 1. With `tilt_db`, the noise
    /// is not flat: its density rises by that many dB across the channel
    /// ([`tilted_noise`]), as under a neighbour's skirt.
    Am {
        index: f32,
        speech: bool,
        tilt_db: f32,
    },
}

/// The RMS of [`voice_like`] audio against its peak of 1: with it a 2.5 kHz peak deviation
/// is 625 Hz RMS, as [`KeyedOver`]'s speech at full level is.
pub const VOICE_RMS: f64 = 0.25;

/// Voice-like audio for FM, `seconds` at `rate_hz`: Gaussian noise band-limited to 300 to
/// 3000 Hz under a raised-cosine syllable envelope at [`KeyedOver::SYLLABLE_HZ`], scaled to
/// an RMS of [`VOICE_RMS`] and clipped at ±1 (a limiter). It has no steady tone and no
/// keying line.
pub fn voice_like(rate_hz: u32, seconds: f32, seed: u64) -> Vec<f32> {
    let n = (f64::from(rate_hz) * f64::from(seconds)).round() as usize;
    let mut x = vec![Complex::new(0.0f32, 0.0); n];
    awgn(&mut x, 0.0, seed ^ 0x5eed_701c);
    let mut planner = rustfft::FftPlanner::<f32>::new();
    planner.plan_fft_forward(n).process(&mut x);
    let bin_hz = f64::from(rate_hz) / n as f64;
    for (k, b) in x.iter_mut().enumerate() {
        let f = if 2 * k <= n { k as f64 } else { (n - k) as f64 } * bin_hz;
        if !(300.0..=3_000.0).contains(&f) {
            *b = Complex::new(0.0, 0.0);
        }
    }
    planner.plan_fft_inverse(n).process(&mut x);
    let mut audio: Vec<f64> = x
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let t = i as f64 / f64::from(rate_hz);
            f64::from(s.re) * (0.5 - 0.5 * (2.0 * PI * KeyedOver::SYLLABLE_HZ * t).cos())
        })
        .collect();
    let rms = (audio.iter().map(|a| a * a).sum::<f64>() / n.max(1) as f64).sqrt();
    if rms > 0.0 {
        audio.iter_mut().for_each(|a| *a *= VOICE_RMS / rms);
    }
    audio.iter().map(|a| a.clamp(-1.0, 1.0) as f32).collect()
}

/// `n` samples of complex Gaussian noise at `rate_hz` whose density rises by `tilt_db`,
/// evenly in dB, from the lower to the upper edge of the band `band_hz` wide about the
/// centre (and stays at the edges' levels beyond them), with a mean power of `power`
/// inside that band.
pub fn tilted_noise(
    n: usize,
    rate_hz: u32,
    band_hz: u64,
    tilt_db: f32,
    power: f64,
    seed: u64,
) -> Vec<Complex<f32>> {
    let mut x = vec![Complex::new(0.0f32, 0.0); n];
    awgn(&mut x, 0.0, seed);
    let mut planner = rustfft::FftPlanner::<f32>::new();
    planner.plan_fft_forward(n).process(&mut x);
    let (bin_hz, half) = (f64::from(rate_hz) / n as f64, band_hz as f64 / 2.0);
    let mut inside = 0.0f64;
    for (k, b) in x.iter_mut().enumerate() {
        let f = if 2 * k <= n {
            k as f64
        } else {
            k as f64 - n as f64
        } * bin_hz;
        let db = f64::from(tilt_db) * f.clamp(-half, half) / band_hz as f64;
        *b *= 10f64.powf(db / 20.0) as f32;
        if f.abs() <= half {
            inside += f64::from(b.norm_sqr());
        }
    }
    planner.plan_fft_inverse(n).process(&mut x);
    // Parseval: the band's mean power per sample is its bins' over n squared.
    let scale = (power * (n as f64).powi(2) / inside.max(f64::MIN_POSITIVE)).sqrt() / n as f64;
    x.iter().map(|s| s * scale as f32).collect()
}

/// One weak-signal window of layer 1's tests and sweep: a signal whose power stands only
/// `snr_db` over the noise inside the channel `band_hz` it is read through.
#[derive(Clone, Debug, PartialEq)]
pub struct WeakCase {
    /// `weak-<fm|carrier|am>-<what>-<snr>dB-<band>Hz-s<seed>`.
    pub name: String,
    pub signal: WeakSignal,
    pub band_hz: u64,
    /// The signal's mean power (an FM carrier's while up) over the noise's within
    /// `band_hz`, dB.
    pub snr_db: f32,
    /// The share of the window the signal is up.
    pub share: f64,
    pub seed: u64,
}

/// The in-channel SNRs of the weak FM cases, dB.
pub const WEAK_FM_SNRS_DB: [f32; 5] = [0.0, 3.0, 6.0, 9.0, 12.0];
/// The in-channel SNRs of the weak AM cases, dB.
pub const WEAK_AM_SNRS_DB: [f32; 6] = [1.0, 2.0, 3.0, 6.0, 9.0, 12.0];
/// The slope of the noise under the weak AM cases that have one, dB across the channel.
pub const WEAK_AM_TILT_DB: f32 = 6.0;
/// The channels of the weak FM cases, `(band, peak deviation)` in Hz: 12.5 and 25 kHz FM,
/// and a 12.5 kHz transmission read through a 25 kHz box.
pub const WEAK_FM_CHANNELS: [(u64, f64); 3] =
    [(12_500, 2_500.0), (25_000, 5_000.0), (25_000, 2_500.0)];
/// The noise seeds of every weak case.
pub const WEAK_SEEDS: [u64; 3] = [1, 2, 3];

/// The weak-signal cases, at [`KEYED_OVER_RATE_HZ`] over [`KEYED_OVER_SECONDS`], each seed
/// of [`WEAK_SEEDS`]:
/// - FM speech and a bare carrier, each whole, keyed up at 40 % of the window, keyed down
///   at 40 % and keyed up at 6 %, at each SNR of [`WEAK_FM_SNRS_DB`] in each channel of
///   [`WEAK_FM_CHANNELS`];
/// - FM of voice-like audio, whole and keyed up at 40 % of the window, at the same SNRs and
///   channels;
/// - AM of a tone at index 0.2, 0.5 and 0.9 and of speech at 0.9, at each SNR of
///   [`WEAK_AM_SNRS_DB`] in 12.5 and 25 kHz, and the tone at 0.2 and 0.5 and the speech
///   again over noise sloping by [`WEAK_AM_TILT_DB`] across the channel.
pub fn weak_cases() -> Vec<WeakCase> {
    let mut out = Vec::new();
    for &seed in &WEAK_SEEDS {
        for &snr_db in &WEAK_FM_SNRS_DB {
            for &(band_hz, deviation_hz) in &WEAK_FM_CHANNELS {
                for (what, audio) in [("fm", 1.0f32), ("carrier", 0.0)] {
                    for (keying, on) in [
                        ("whole", (0.0, 1.0)),
                        ("up-d0.60", (0.4, 1.0)),
                        ("down-d0.40", (0.0, 0.4)),
                        ("up-d0.94", (0.06, 1.0)),
                    ] {
                        out.push(WeakCase {
                            name: format!(
                                "weak-{what}-{keying}-dev{deviation_hz:.0}-{snr_db:.0}dB-{band_hz}Hz-s{seed}"
                            ),
                            signal: WeakSignal::Fm {
                                deviation_hz,
                                audio,
                                on,
                            },
                            band_hz,
                            snr_db,
                            share: on.1 - on.0,
                            seed,
                        });
                    }
                }
                for (keying, on) in [("whole", (0.0, 1.0)), ("up-d0.60", (0.4, 1.0))] {
                    out.push(WeakCase {
                        name: format!(
                            "weak-voice-{keying}-dev{deviation_hz:.0}-{snr_db:.0}dB-{band_hz}Hz-s{seed}"
                        ),
                        signal: WeakSignal::Voice { deviation_hz, on },
                        band_hz,
                        snr_db,
                        share: on.1 - on.0,
                        seed,
                    });
                }
            }
        }
        for &snr_db in &WEAK_AM_SNRS_DB {
            for band_hz in [12_500u64, 25_000] {
                for (what, index, speech, tilt_db) in [
                    ("tone-m0.2", 0.2f32, false, 0.0f32),
                    ("tone-m0.5", 0.5, false, 0.0),
                    ("tone-m0.9", 0.9, false, 0.0),
                    ("speech-m0.9", 0.9, true, 0.0),
                    ("tilt-tone-m0.2", 0.2, false, WEAK_AM_TILT_DB),
                    ("tilt-tone-m0.5", 0.5, false, WEAK_AM_TILT_DB),
                    ("tilt-speech-m0.9", 0.9, true, WEAK_AM_TILT_DB),
                ] {
                    out.push(WeakCase {
                        name: format!("weak-am-{what}-{snr_db:.0}dB-{band_hz}Hz-s{seed}"),
                        signal: WeakSignal::Am {
                            index,
                            speech,
                            tilt_db,
                        },
                        band_hz,
                        snr_db,
                        share: 1.0,
                        seed,
                    });
                }
            }
        }
    }
    out
}

/// The window of `case`, centred: its signal at a mean power (an FM carrier's while up) of
/// `snr_db` over the white noise within `band_hz`, so `snr_db - 10 log10(rate / band)`
/// over the whole rate.
pub fn weak_case_iq(case: &WeakCase) -> Vec<Complex<f32>> {
    let rate = KEYED_OVER_RATE_HZ;
    let over_rate_db = case.snr_db - 10.0 * (f64::from(rate) / case.band_hz as f64).log10() as f32;
    match case.signal {
        WeakSignal::Fm {
            deviation_hz,
            audio,
            on,
        } => keyed_over(&KeyedOver {
            rate_hz: rate,
            seconds: KEYED_OVER_SECONDS,
            deviation_hz,
            audio,
            on,
            snr_db: over_rate_db,
            seed: case.seed,
        }),
        WeakSignal::Voice { deviation_hz, on } => keyed_fm(
            &voice_like(rate, KEYED_OVER_SECONDS, case.seed),
            &KeyedOver {
                rate_hz: rate,
                seconds: KEYED_OVER_SECONDS,
                deviation_hz,
                audio: 1.0,
                on,
                snr_db: over_rate_db,
                seed: case.seed,
            },
        ),
        WeakSignal::Am {
            index,
            speech,
            tilt_db,
        } => {
            let audio = if speech {
                // `speech_like` peaks at 0.5.
                speech_like(rate, KEYED_OVER_SECONDS)
                    .iter()
                    .map(|a| 2.0 * a)
                    .collect()
            } else {
                let n = (f64::from(rate) * f64::from(KEYED_OVER_SECONDS)) as usize;
                tone(rate, n, 1_000.0, 1.0)
            };
            let mut iq = am_modulate(&audio, rate, rate, index, 0.0);
            if tilt_db == 0.0 {
                // `awgn` sets the noise against the mean power, the sidebands included.
                awgn(&mut iq, over_rate_db, case.seed);
            } else {
                let mean = iq.iter().map(|s| f64::from(s.norm_sqr())).sum::<f64>()
                    / iq.len().max(1) as f64;
                let power = mean / 10f64.powf(f64::from(case.snr_db) / 10.0);
                let noise = tilted_noise(iq.len(), rate, case.band_hz, tilt_db, power, case.seed);
                iq.iter_mut().zip(noise).for_each(|(s, w)| *s += w);
            }
            iq
        }
    }
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
