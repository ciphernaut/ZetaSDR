//! Layer-1 rules for wideband FM (with 19 kHz pilot evidence), AM and SSB.
#![cfg(feature = "test-fixtures")]

use num_complex::Complex;
use zeta_core::classify::Label;
use zeta_dsp::fixtures::{am_modulate, awgn, nbfm_modulate, ssb_modulate, tone, wfm_modulate};
use zeta_dsp::measure::{envelope_cv, layer1_label, measure, pilot_line_db};

const CENTER: u64 = 100_000_000;
const WFM_RATE: u32 = 240_000;

/// `measure` + `layer1_label`, printing the measured features.
fn label(what: &str, iq: &[Complex<f32>], rate_hz: u32) -> Label {
    let f = measure(iq, rate_hz as u64, CENTER);
    let l = layer1_label(&f, iq, rate_hz as u64);
    eprintln!(
        "{what}: bandwidth {} Hz, snr {:.1} dB, cv {:.3}, pilot {:?} -> {} {} ({})",
        f.bandwidth_hz.0,
        f.snr_db,
        envelope_cv(iq),
        pilot_line_db(iq, rate_hz as u64),
        l.name,
        l.confidence,
        l.evidence
    );
    l
}

/// One second of broadcast-style FM at 240 kHz: a 1 kHz tone at `amp` (0.9 is
/// 67.5 kHz of the 75 kHz deviation), with or without the pilot, at 20 dB SNR.
fn wfm_fixture(amp: f32, pilot: bool) -> Vec<Complex<f32>> {
    let audio = tone(48_000, 48_000, 1_000.0, amp);
    let mut iq = wfm_modulate(&audio, 48_000, WFM_RATE, 75_000.0, 0.0, pilot);
    awgn(&mut iq, 20.0, 21);
    iq
}

#[test]
fn wfm_fixture_with_pilot_is_wfm_0_9() {
    let l = label("wfm pilot", &wfm_fixture(0.9, true), WFM_RATE);
    assert_eq!(l.name, "wfm", "{l:?}");
    assert_eq!(l.confidence, 0.9, "{l:?}");
    assert!(l.evidence.contains("19 kHz pilot"), "{}", l.evidence);
}

#[test]
fn wfm_fixture_without_pilot_is_wfm_0_6() {
    let l = label("wfm no pilot", &wfm_fixture(0.9, false), WFM_RATE);
    assert_eq!(l.name, "wfm", "{l:?}");
    assert_eq!(l.confidence, 0.6, "{l:?}");
    assert!(!l.evidence.contains("pilot"), "{}", l.evidence);
}

#[test]
fn wfm_fixture_quiet_audio_is_wfm() {
    // 37.5 kHz deviation: about 80 kHz occupied, over the 60 kHz floor.
    let l = label("wfm quiet", &wfm_fixture(0.5, true), WFM_RATE);
    assert_eq!(l.name, "wfm", "{l:?}");
    assert!(l.evidence.contains("19 kHz pilot"), "{}", l.evidence);
}

#[test]
fn nbfm_fixture_is_still_fm_not_wfm() {
    // The plan-2 12.5 kHz channel fixture: 1 kHz tone, 2.5 kHz deviation.
    let audio = tone(48_000, 48_000, 1_000.0, 1.0);
    let mut iq = nbfm_modulate(&audio, 48_000, 48_000, 2_500.0, 0.0);
    awgn(&mut iq, 20.0, 3);
    let l = label("nbfm", &iq, 48_000);
    assert_eq!(l.name, "fm", "{l:?}");
    assert_eq!(l.confidence, 0.6, "{l:?}");
    assert!(!l.evidence.contains("wideband"), "{}", l.evidence);
}

#[test]
fn am_fixture_is_am() {
    let audio = tone(48_000, 48_000, 1_000.0, 1.0);
    let mut iq = am_modulate(&audio, 48_000, 48_000, 0.5, 0.0);
    awgn(&mut iq, 20.0, 7);
    let l = label("am", &iq, 48_000);
    assert_eq!(l.name, "am", "{l:?}");
    assert_eq!(l.confidence, 0.6, "{l:?}");
    assert!(l.evidence.contains("carrier line"), "{}", l.evidence);
}

#[test]
fn ssb_fixture_is_ssb_with_sideband_evidence() {
    // Voice-like: the 700 + 1900 Hz two-tone pair, 700 Hz louder (7.4 dB, as
    // speech energy falls with frequency; equal tones would be two dominant
    // RF tones, `fsk`), plus a weak 2700 Hz component at the top of the voice
    // passband. Without it the occupied band is 1.2 kHz plus window skirts
    // (1453 Hz at 20 dB SNR), straddling the 1.5 kHz floor.
    let audio: Vec<f32> = tone(48_000, 48_000, 700.0, 0.7)
        .iter()
        .zip(tone(48_000, 48_000, 1_900.0, 0.3))
        .zip(tone(48_000, 48_000, 2_700.0, 0.15))
        .map(|((a, b), c)| a + b + c)
        .collect();
    for (usb, side) in [(true, "usb"), (false, "lsb")] {
        let mut iq = ssb_modulate(&audio, 48_000, 48_000, usb, 0.0);
        awgn(&mut iq, 20.0, 9);
        let l = label(side, &iq, 48_000);
        assert_eq!(l.name, "ssb", "{l:?}");
        assert_eq!(l.confidence, 0.6, "{l:?}");
        assert!(l.evidence.contains(side), "{}", l.evidence);
        // The dial hint lands near the suppressed carrier at the block centre.
        let dial: u64 = l
            .evidence
            .split("dial \u{2248} ")
            .nth(1)
            .and_then(|r| r.split(' ').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no dial hint: {}", l.evidence));
        assert!(dial.abs_diff(CENTER) <= 500, "dial {dial}: {}", l.evidence);
        assert!(
            l.evidence.contains(&format!("Hz ({side})")),
            "{}",
            l.evidence
        );
    }
}

/// The wfm fixture through a two-path channel (an `echo`× copy two samples
/// late, 8.3 us at 240 kHz) at 15 dB SNR: the multipath ripples the envelope
/// well past the clean fixture's cv, as on a real station. (A 0.3x one-sample
/// echo only reaches cv 0.16 here: the paths stay nearly in phase.)
fn wfm_multipath_fixture(pilot: bool, echo: f32) -> Vec<Complex<f32>> {
    let audio = tone(48_000, 48_000, 1_000.0, 0.9);
    let clean = wfm_modulate(&audio, 48_000, WFM_RATE, 75_000.0, 0.0, pilot);
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
    iq
}

#[test]
fn wfm_with_multipath_and_noise_is_still_wfm_by_pilot() {
    let iq = wfm_multipath_fixture(true, 0.5);
    let cv = envelope_cv(&iq);
    eprintln!("multipath wfm cv {cv:.3}");
    assert!((0.38..=0.45).contains(&cv), "cv {cv}");
    let l = label("wfm multipath pilot", &iq, WFM_RATE);
    assert_eq!(l.name, "wfm", "{l:?}");
    assert_eq!(l.confidence, 0.9, "{l:?}");
    assert!(l.evidence.contains("19 kHz pilot"), "{}", l.evidence);
}

#[test]
fn wfm_without_pilot_under_multipath_is_wfm_0_6() {
    // A 0.45x echo puts cv above the old 0.36 limit and below the 0.45 one,
    // so the test proves both the loosening and the margin left.
    let iq = wfm_multipath_fixture(false, 0.45);
    let cv = envelope_cv(&iq);
    eprintln!("multipath wfm (no pilot) cv {cv:.3}");
    assert!((0.36..0.45).contains(&cv), "cv {cv}");
    let l = label("wfm multipath no pilot", &iq, WFM_RATE);
    assert_eq!(l.name, "wfm", "{l:?}");
    assert_eq!(l.confidence, 0.6, "{l:?}");
    assert!(!l.evidence.contains("pilot"), "{}", l.evidence);
}

#[test]
fn am_fixture_low_index_is_am() {
    // Below AM_MIN_CV (0.25): m 0.2 and 0.3 give cv of about m / sqrt(2).
    for m in [0.2f32, 0.3] {
        let audio = tone(48_000, 48_000, 1_000.0, 1.0);
        let mut iq = am_modulate(&audio, 48_000, 48_000, m, 0.0);
        awgn(&mut iq, 20.0, 7);
        let l = label(&format!("am m {m}"), &iq, 48_000);
        assert_eq!(l.name, "am", "m {m}: {l:?}");
        assert_eq!(l.confidence, 0.6, "{l:?}");
        assert!(l.evidence.contains("carrier line"), "{}", l.evidence);
        assert!(l.evidence.contains("phase rms"), "{}", l.evidence);
    }
}

#[test]
fn centred_carrier_is_still_carrier() {
    // A bare carrier at the block centre has a carrier line; its noise
    // envelope must not pass for low-index AM.
    for (snr, seed) in [(20.0f32, 11u64), (12.0, 12)] {
        let mut iq = vec![Complex::new(1.0f32, 0.0); 48_000];
        awgn(&mut iq, snr, seed);
        let l = label(&format!("carrier {snr} dB"), &iq, 48_000);
        assert_eq!(l.name, "carrier", "snr {snr}: {l:?}");
    }
}

/// A dense train of 2-FSK packets (50 ms at 4800 Bd every 60 ms) has two envelope
/// levels and runs that are whole multiples of its 10 ms gap, but it carries tones: it
/// labels `fsk` and its rate is not the packet train's 100 Bd.
#[test]
fn a_dense_fsk_packet_train_is_fsk_not_ook_runs() {
    let spec = zeta_dsp::synth::SynthSpec::Fsk {
        symbol_rate_baud: 4_800,
        deviation_hz: 2_400.0,
        order: 2,
        bits: "14ed58c6d3ff2d75a5650138a8d447f00ec47321d420c5d08bec3850735c".into(),
        repeat: 16,
        gap_ms: 10,
    };
    let mut iq = zeta_dsp::synth::synthesise(&spec, &Default::default(), 48_000, CENTER);
    awgn(&mut iq, 20.0, 41);
    let f = measure(&iq, 48_000, CENTER);
    let l = label("fsk packet train", &iq, 48_000);
    assert_eq!(l.name, "fsk", "{l:?}");
    assert_ne!(f.symbol_rate_baud.map(|b| b.0), Some(100), "{f:?}");
}

/// The real EV1527 remote's windows (433.88 MHz, chip 313 us, 3200 chips/s): B210 at
/// 51 200 and 61 440 Hz, HackRF at 62 500 Hz.
const OOK_REMOTES: [&str; 5] = [
    "ook-remote-433882k-1",
    "ook-remote-433882k-2",
    "ook-remote-b210-1",
    "ook-remote-b210-2",
    "ook-remote-hackrf-1",
];
/// The remote's chip rate: the reciprocal of its shortest on or off run.
const OOK_REMOTE_BAUD: f64 = 3_200.0;
/// How far a measured chip rate may stray from [`OOK_REMOTE_BAUD`], as a fraction.
const OOK_REMOTE_BAUD_TOLERANCE: f64 = 0.02;

/// A fixture window as a burst capture measures it (`measure_band` over the capture's
/// band, its annotation's edges) and as `classify` labels it (`measure_selection` on
/// the same band): the features and the layer-1 label.
fn ook_remote(name: &str) -> (zeta_core::classify::FeatureRecord, Label) {
    let meta = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(format!("{name}.sigmf-meta"));
    let (m, iq) = zeta_core::capture::read_cf32(&meta).expect("fixture");
    let a = &m.annotations[0];
    let edge = |k: &str| a[k].as_u64().expect("annotation edge");
    let band = edge("core:freq_upper_edge") - edge("core:freq_lower_edge");
    let rate = m.sample_rate_hz.0;
    let f = zeta_dsp::measure::measure_band(&iq, rate, m.center_hz.0, band);
    let (_, l) = zeta_dsp::measure::measure_selection(
        &iq,
        rate,
        m.center_hz.0,
        zeta_core::classify::Band {
            offset_hz: 0,
            bandwidth_hz: zeta_core::types::Hz(band),
        },
    )
    .expect("a measurement");
    eprintln!("{name}: {f:?} -> {} ({})", l.name, l.evidence);
    (f, l)
}

/// Pulse-width OOK (a 0 is one chip on and three off, a 1 three on and one off) has its
/// strongest keying lines at the bit rate (800 bit/s) and its harmonics; the chip rate
/// is the run lengths' unit, and every window measures it.
#[test]
fn a_real_ook_remote_measures_its_chip_rate() {
    for name in OOK_REMOTES {
        let (f, _) = ook_remote(name);
        let baud = f.symbol_rate_baud.map(|b| b.0 as f64);
        assert!(
            baud.is_some_and(
                |b| (b - OOK_REMOTE_BAUD).abs() <= OOK_REMOTE_BAUD * OOK_REMOTE_BAUD_TOLERANCE
            ),
            "{name}: {f:?}"
        );
    }
}

/// The remote's envelope sits at two levels, its off level the noise floor: layer 1
/// labels every window `ook`, not `fsk` or `fm`.
#[test]
fn a_real_ook_remote_labels_ook() {
    for name in OOK_REMOTES {
        let (_, l) = ook_remote(name);
        assert_eq!(l.name, "ook", "{name}: {l:?}");
    }
}
