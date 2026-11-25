//! The layer-1 sweep: one line per input, `<name> <label> duty=<d> occ=<hz> snr=<db>`,
//! each measured and labelled through `measure_selection`, as `classify` reads it. The
//! inputs are every fixture `tests/layer1.rs` builds (the keyed overs at every case
//! `fixtures::keyed_over_cases` lists, the whole over too), every capture in
//! `crates/zeta-dsp/tests/data/`, and each `*.sigmf-meta` in the directories given as
//! arguments. A capture is read over its first annotation's band when it has one, else
//! whole.
//!
//! Run before and after a layer-1 change and `diff` the two: a fixture whose line moves
//! has changed what layer 1 reads of it.
//!
//! `cargo run -q -p zeta-dsp --features test-fixtures --example l1_sweep [dir...]`
use std::path::{Path, PathBuf};

use num_complex::Complex;
use zeta_core::classify::Band;
use zeta_core::types::Hz;
use zeta_dsp::fixtures::{
    Impairments, KEYED_OVER_SHARES, SynthSpec, am_modulate, awgn, keyed_over, keyed_over_cases,
    nbfm_modulate, ssb_modulate, synthesise, tone, wfm_modulate,
};
use zeta_dsp::measure::measure_selection;

/// The block centre the built fixtures are measured at (`tests/layer1.rs`'s `CENTER`).
const CENTER: u64 = 100_000_000;

/// One input: its name, the block, its rate and centre, and the band read.
struct Input {
    name: String,
    iq: Vec<Complex<f32>>,
    rate_hz: u64,
    center_hz: u64,
    band: Band,
}

impl Input {
    /// `iq` at `rate_hz` about [`CENTER`], read whole.
    fn whole(name: &str, iq: Vec<Complex<f32>>, rate_hz: u32) -> Self {
        Self {
            name: name.into(),
            iq,
            rate_hz: u64::from(rate_hz),
            center_hz: CENTER,
            band: Band {
                offset_hz: 0,
                bandwidth_hz: Hz(u64::from(rate_hz)),
            },
        }
    }
}

fn main() {
    built(&mut report);
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    captures(&data, None, &mut report);
    for dir in std::env::args().skip(1) {
        let dir = PathBuf::from(dir);
        let prefix = dir
            .file_name()
            .map_or_else(|| dir.display().to_string(), |n| n.to_string_lossy().into());
        captures(&dir, Some(&prefix), &mut report);
    }
}

/// Measure `i` and print its line.
fn report(i: Input) {
    match measure_selection(&i.iq, i.rate_hz, i.center_hz, i.band) {
        Some((f, l)) => println!(
            "{} {} duty={} occ={} snr={:.1}",
            i.name,
            l.name,
            f.duty.map_or_else(|| "none".into(), |d| format!("{d:.2}")),
            f.bandwidth_hz.0,
            f.snr_db
        ),
        None => println!("{} none duty=none occ=none snr=none", i.name),
    }
}

/// Every `*.sigmf-meta` in `dir`, by name, as inputs named by their stem (after
/// `prefix/` when given), each handed to `emit`. Other files are noted on stderr and left
/// out.
fn captures(dir: &Path, prefix: Option<&str>, emit: &mut impl FnMut(Input)) {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.expect("a directory entry").path())
        .collect();
    paths.sort();
    for p in paths {
        let file = p.file_name().map(|n| n.to_string_lossy().into_owned());
        let Some(stem) = file.as_deref().and_then(|f| f.strip_suffix(".sigmf-meta")) else {
            if file.as_deref().is_some_and(|f| !f.ends_with(".sigmf-data")) {
                eprintln!("l1_sweep: {}: not a SigMF capture, left out", p.display());
            }
            continue;
        };
        let (m, iq) =
            zeta_core::capture::read_cf32(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        let (rate, center) = (m.sample_rate_hz.0, m.center_hz.0);
        let edge = |k: &str| m.annotations.first().and_then(|a| a[k].as_u64());
        let band = match (edge("core:freq_lower_edge"), edge("core:freq_upper_edge")) {
            (Some(lo), Some(hi)) if hi > lo => Band {
                offset_hz: ((lo + hi) / 2) as i64 - center as i64,
                bandwidth_hz: Hz(hi - lo),
            },
            _ => Band {
                offset_hz: 0,
                bandwidth_hz: Hz(rate),
            },
        };
        emit(Input {
            name: prefix.map_or_else(|| stem.to_string(), |d| format!("{d}/{stem}")),
            iq,
            rate_hz: rate,
            center_hz: center,
            band,
        });
    }
}

/// The fixtures `tests/layer1.rs` builds, each as its test measures it, then the keyed
/// overs, each handed to `emit` as it is built.
fn built(emit: &mut impl FnMut(Input)) {
    // Broadcast FM at 240 kHz: a 1 kHz tone, with and without the pilot, 20 dB.
    let wfm = |amp: f32, pilot: bool| {
        let audio = tone(48_000, 48_000, 1_000.0, amp);
        let mut iq = wfm_modulate(&audio, 48_000, 240_000, 75_000.0, 0.0, pilot);
        awgn(&mut iq, 20.0, 21);
        iq
    };
    emit(Input::whole("wfm-pilot", wfm(0.9, true), 240_000));
    emit(Input::whole("wfm-no-pilot", wfm(0.9, false), 240_000));
    emit(Input::whole("wfm-quiet", wfm(0.5, true), 240_000));
    // The 12.5 kHz NBFM channel: a 1 kHz tone at 2.5 kHz deviation.
    let mut iq = nbfm_modulate(
        &tone(48_000, 48_000, 1_000.0, 1.0),
        48_000,
        48_000,
        2_500.0,
        0.0,
    );
    awgn(&mut iq, 20.0, 3);
    emit(Input::whole("nbfm-tone", iq, 48_000));
    // AM at index 0.5, 0.2 and 0.3.
    for (name, m) in [("am", 0.5f32), ("am-m0.2", 0.2), ("am-m0.3", 0.3)] {
        let audio = tone(48_000, 48_000, 1_000.0, 1.0);
        let mut iq = am_modulate(&audio, 48_000, 48_000, m, 0.0);
        awgn(&mut iq, 20.0, 7);
        emit(Input::whole(name, iq, 48_000));
    }
    // SSB: 700 and 1900 Hz, and a weak 2700 Hz.
    let voice: Vec<f32> = tone(48_000, 48_000, 700.0, 0.7)
        .iter()
        .zip(tone(48_000, 48_000, 1_900.0, 0.3))
        .zip(tone(48_000, 48_000, 2_700.0, 0.15))
        .map(|((a, b), c)| a + b + c)
        .collect();
    for (name, usb) in [("ssb-usb", true), ("ssb-lsb", false)] {
        let mut iq = ssb_modulate(&voice, 48_000, 48_000, usb, 0.0);
        awgn(&mut iq, 20.0, 9);
        emit(Input::whole(name, iq, 48_000));
    }
    // Broadcast FM through two paths, 15 dB.
    for (name, pilot, echo) in [
        ("wfm-multipath-pilot", true, 0.5f32),
        ("wfm-multipath-no-pilot", false, 0.45),
    ] {
        let audio = tone(48_000, 48_000, 1_000.0, 0.9);
        let clean = wfm_modulate(&audio, 48_000, 240_000, 75_000.0, 0.0, pilot);
        let mut iq: Vec<Complex<f32>> = clean
            .iter()
            .enumerate()
            .map(|(n, &s)| {
                s + if n >= 2 {
                    clean[n - 2] * echo
                } else {
                    Complex::new(0.0, 0.0)
                }
            })
            .collect();
        awgn(&mut iq, 15.0, 23);
        emit(Input::whole(name, iq, 240_000));
    }
    // A bare centred carrier.
    for (name, snr, seed) in [("carrier-20dB", 20.0f32, 11u64), ("carrier-12dB", 12.0, 12)] {
        let mut iq = vec![Complex::new(1.0f32, 0.0); 48_000];
        awgn(&mut iq, snr, seed);
        emit(Input::whole(name, iq, 48_000));
    }
    // A dense 2-FSK packet train.
    let spec = SynthSpec::Fsk {
        symbol_rate_baud: 4_800,
        deviation_hz: 2_400.0,
        order: 2,
        bits: "14ed58c6d3ff2d75a5650138a8d447f00ec47321d420c5d08bec3850735c".into(),
        repeat: 16,
        gap_ms: 10,
    };
    let mut iq = synthesise(&spec, &Default::default(), 48_000, CENTER);
    awgn(&mut iq, 20.0, 41);
    emit(Input::whole("fsk-packet-train", iq, 48_000));
    // The review probe's signals.
    let probe = |spec: SynthSpec, snr: f32, off: f64, rate: u32| {
        let imp = Impairments {
            snr_db: Some(snr),
            offset_hz: off,
            ..Default::default()
        };
        synthesise(&spec, &imp, rate, 0)
    };
    let psk = |baud: u32, order: u8| SynthSpec::Psk {
        symbol_rate_baud: baud,
        order,
        bits: "a5c3f0e1a5c3f0e1".into(),
        repeat: 8,
        gap_ms: 100,
    };
    let pocsag = SynthSpec::Pocsag {
        address: 1_234_567,
        function: 3,
        text: "HELLO WORLD THIS IS A PAGE".into(),
        baud: 2_400,
    };
    let modes = SynthSpec::ModesIdent {
        icao: 0x7c_1234,
        callsign: "QFA1".into(),
        repeats: 20,
    };
    let bursty_fsk = SynthSpec::Fsk {
        symbol_rate_baud: 4_800,
        deviation_hz: 6_000.0,
        order: 2,
        bits: "a5c3f0e1a5c3f0e1".into(),
        repeat: 8,
        gap_ms: 100,
    };
    for (name, spec, snr, off, rate) in [
        ("bpsk-bursts-2400Bd-30dB", psk(2_400, 2), 30.0, 0.0, 48_000),
        ("qpsk-bursts-2400Bd-30dB", psk(2_400, 4), 30.0, 0.0, 48_000),
        ("qpsk-bursts-2400Bd-8dB", psk(2_400, 4), 8.0, 0.0, 48_000),
        ("pocsag-2400Bd-nbfm-8dB", pocsag, 8.0, 0.0, 48_000),
        ("qpsk-bursts-9600Bd-15dB", psk(9_600, 4), 15.0, 0.0, 96_000),
        ("qpsk-bursts-9600Bd-8dB", psk(9_600, 4), 8.0, 0.0, 96_000),
        ("modes-30dB", modes.clone(), 30.0, 0.0, 2_400_000),
        ("modes-8dB", modes, 8.0, 0.0, 2_400_000),
        (
            "bursty-fsk-2m4-15dB",
            bursty_fsk,
            15.0,
            200_000.0,
            2_400_000,
        ),
    ] {
        emit(Input::whole(name, probe(spec, snr, off, rate), rate));
    }
    // The keyed overs: speech, a quiet passage and a bare carrier, the whole over, keyed
    // up and down at every share, and with a hole.
    let shares: Vec<f64> = std::iter::once(1.0).chain(KEYED_OVER_SHARES).collect();
    for c in keyed_over_cases(&[1.0, 0.1, 0.0], &shares, true) {
        emit(Input {
            name: c.name.clone(),
            iq: keyed_over(&c.over),
            rate_hz: u64::from(c.over.rate_hz),
            center_hz: CENTER,
            band: Band {
                offset_hz: 0,
                bandwidth_hz: Hz(c.band_hz),
            },
        });
    }
}
