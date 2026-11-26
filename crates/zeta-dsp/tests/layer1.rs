//! Layer-1 rules for wideband FM (with 19 kHz pilot evidence), AM and SSB, and for a
//! keyed NBFM over whatever share of the window it is up.
#![cfg(feature = "test-fixtures")]

use num_complex::Complex;
use zeta_core::classify::Label;
use zeta_dsp::fixtures::{am_modulate, awgn, nbfm_modulate, ssb_modulate, tone, wfm_modulate};
use zeta_dsp::measure::{envelope_cv, layer1_label, measure, pilot_line_db};

const CENTER: u64 = 100_000_000;
const WFM_RATE: u32 = 240_000;

/// The label production gives the whole block (`measure_selection`, as `classify`
/// runs it), asserted to be what `measure` + `layer1_label` name, printing the measured
/// features.
fn label(what: &str, iq: &[Complex<f32>], rate_hz: u32) -> Label {
    let f = measure(iq, rate_hz as u64, CENTER);
    let (mut pf, l) = zeta_dsp::measure::measure_selection(
        iq,
        rate_hz as u64,
        CENTER,
        zeta_core::classify::Band {
            offset_hz: 0,
            bandwidth_hz: zeta_core::types::Hz(rate_hz as u64),
        },
    )
    .expect("a measurement");
    // `classify` also answers the extras `measure` leaves out.
    (pf.power99_hz, pf.audio_rms_hz, pf.ssb) = (None, None, None);
    assert_eq!(pf, f, "{what}");
    assert_eq!(layer1_label(&f, iq, rate_hz as u64).name, l.name, "{what}");
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

/// EV1527-style remote windows (chip 313 us, 3200 chips/s): the real remote's
/// (433.88 MHz; B210 at 51 200 and 61 440 Hz, HackRF at 62 500 Hz; private captures,
/// skipped when absent) and the public synthesised ones (`gen_ook`, 433.92 MHz at 51 200
/// and 62 500 Hz).
const OOK_REMOTES: [&str; 7] = [
    "ook-remote-433882k-1",
    "ook-remote-433882k-2",
    "ook-remote-b210-1",
    "ook-remote-b210-2",
    "ook-remote-hackrf-1",
    "ook-ev1527-synth-1",
    "ook-ev1527-synth-2",
];
/// The remote's chip rate: the reciprocal of its shortest on or off run.
const OOK_REMOTE_BAUD: f64 = 3_200.0;
/// How far a measured chip rate may stray from [`OOK_REMOTE_BAUD`], as a fraction.
const OOK_REMOTE_BAUD_TOLERANCE: f64 = 0.02;

/// A fixture window as a burst capture measures it (`measure_band` over the capture's
/// band, its annotation's edges) and as `classify` labels it (`measure_selection` on
/// the same band): the features and the layer-1 label. `None`, with the reason printed,
/// for a private capture not in this checkout; a missing public one fails.
fn ook_remote(name: &str) -> Option<(zeta_core::classify::FeatureRecord, Label)> {
    let meta = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(format!("{name}.sigmf-meta"));
    if !meta.exists() && !name.starts_with("ook-ev1527-synth-") {
        eprintln!("skipped: {name}: a private capture, not in this checkout");
        return None;
    }
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
    Some((f, l))
}

/// Pulse-width OOK (a 0 is one chip on and three off, a 1 three on and one off) has its
/// strongest keying lines at the bit rate (800 bit/s) and its harmonics; the chip rate
/// is the run lengths' unit, and every window measures it.
#[test]
fn an_ook_remote_measures_its_chip_rate() {
    for name in OOK_REMOTES {
        let Some((f, _)) = ook_remote(name) else {
            continue;
        };
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
fn an_ook_remote_labels_ook() {
    for name in OOK_REMOTES {
        let Some((_, l)) = ook_remote(name) else {
            continue;
        };
        assert_eq!(l.name, "ook", "{name}: {l:?}");
    }
}

/// Three HackRF windows of one press of two buttons (62 500 Hz, 20-34 dB SNR, the
/// remote about 1.9 kHz over the capture centre): 431 ms, 71 ms and 471 ms, each a
/// train of whole frames. On the node they measured as bursts (duty 0.076, 1.0 and
/// 0.011), the first two at the 800 bit/s bit rate from the cyclostationary line and
/// unknown, the third `fsk` with no rate.
const OOK_SPARSE_REMOTES: [&str; 3] = [
    "ook-remote-hackrf-sparse-1",
    "ook-remote-hackrf-sparse-2",
    "ook-remote-hackrf-sparse-3",
];

/// Every sparse window, the 71 ms one too (under two frames, 90-odd on and off runs
/// of a few chips each, enough for the fit), measures the chip rate, never the
/// remote's 800 bit/s bit rate.
#[test]
fn a_sparse_ook_remote_window_measures_its_chip_rate() {
    for name in OOK_SPARSE_REMOTES {
        let Some((f, _)) = ook_remote(name) else {
            continue;
        };
        let baud = f.symbol_rate_baud.map(|b| b.0 as f64);
        assert!(
            baud.is_some_and(
                |b| (b - OOK_REMOTE_BAUD).abs() <= OOK_REMOTE_BAUD * OOK_REMOTE_BAUD_TOLERANCE
            ),
            "{name}: {f:?}"
        );
    }
}

/// Every sparse window labels `ook`: no FSK tones from the noise of its off samples.
#[test]
fn a_sparse_ook_remote_window_labels_ook() {
    for name in OOK_SPARSE_REMOTES {
        let Some((f, l)) = ook_remote(name) else {
            continue;
        };
        assert_eq!(l.name, "ook", "{name}: {f:?} {l:?}");
        assert_eq!(f.tone_spacing_hz, None, "{name}: {f:?}");
    }
}

/// The review probe's signal builder: `spec` synthesised at `rate` Hz, `snr` dB (none for
/// clean), `off` Hz from the centre.
fn probe_signal(
    spec: zeta_dsp::synth::SynthSpec,
    snr: Option<f32>,
    off: f64,
    rate: u32,
) -> Vec<Complex<f32>> {
    let imp = zeta_dsp::synth::Impairments {
        snr_db: snr,
        offset_hz: off,
        ..Default::default()
    };
    zeta_dsp::synth::synthesise(&spec, &imp, rate, 0)
}

/// Eight patterned PSK bursts, 100 ms apart, as the review probe built them.
fn psk_bursts(baud: u32, order: u8) -> zeta_dsp::synth::SynthSpec {
    zeta_dsp::synth::SynthSpec::Psk {
        symbol_rate_baud: baud,
        order,
        bits: "a5c3f0e1a5c3f0e1".into(),
        repeat: 8,
        gap_ms: 100,
    }
}

/// `iq` measured and labelled as production does it (`measure_selection` over the whole
/// block).
fn production(iq: &[Complex<f32>], rate: u64) -> (zeta_core::classify::FeatureRecord, Label) {
    zeta_dsp::measure::measure_selection(
        iq,
        rate,
        CENTER,
        zeta_core::classify::Band {
            offset_hz: 0,
            bandwidth_hz: zeta_core::types::Hz(rate),
        },
    )
    .expect("a measurement")
}

/// The review probe's false positives (2026-11-04, 41c5c07..59141c4): the run-length fit
/// read PSK bursts and POCSAG through a narrow band as on-off keying, and its `ook`
/// overrode every rule. A fit whose off runs are signal (PSK's envelope dips: 15-29 dB
/// over the noise against a remote's 1-5), whose twice-wider band holds FSK tones
/// (POCSAG: the other tone) or whose band carries a PSK order is vetoed, and the label and
/// order are the other rules'.
#[test]
fn a_vetoed_run_fit_leaves_psk_bursts_and_nbfm_pocsag_to_the_other_rules() {
    use zeta_core::classify::RateLine;
    use zeta_dsp::synth::SynthSpec;
    let pocsag = SynthSpec::Pocsag {
        address: 1_234_567,
        function: 3,
        text: "HELLO WORLD THIS IS A PAGE".into(),
        baud: 2_400,
    };
    for (what, iq, rate, label, order) in [
        (
            "bpsk bursts 2400 Bd 30 dB",
            probe_signal(psk_bursts(2_400, 2), Some(30.0), 0.0, 48_000),
            48_000,
            "fsk",
            Some(2),
        ),
        (
            "qpsk bursts 2400 Bd 30 dB",
            probe_signal(psk_bursts(2_400, 4), Some(30.0), 0.0, 48_000),
            48_000,
            "am",
            None,
        ),
        (
            "qpsk bursts 2400 Bd 8 dB",
            probe_signal(psk_bursts(2_400, 4), Some(8.0), 0.0, 48_000),
            48_000,
            "am",
            None,
        ),
        (
            "pocsag 2400 Bd over nbfm 8 dB",
            probe_signal(pocsag, Some(8.0), 0.0, 48_000),
            48_000,
            // Constant-envelope FSK 7.5 dB over the noise in its samples: its envelope CV
            // (0.26) is the noise's, which read `am` until the rules took that share out.
            "fm",
            None,
        ),
    ] {
        let (f, l) = production(&iq, rate);
        eprintln!("{what}: {f:?} -> {} ({})", l.name, l.evidence);
        assert_eq!(l.name, label, "{what}: {f:?} {l:?}");
        assert_eq!(f.order, order, "{what}: {f:?}");
        assert_ne!(f.symbol_rate_line, Some(RateLine::Runs), "{what}: {f:?}");
    }
}

/// Signals the probe found the run-length fit claiming outside the reviewer's list, none
/// of them on-off keying: QPSK bursts at 9600 Bd (the base's runs fit read 12 787 and
/// 12 048 Bd and labelled them `ook`; now vetoed, their off runs 20-23 dB over the
/// noise), and a Mode S train at 2.4 Msps whose 49 kHz measured band, narrowed to a
/// sixteenth, fitted its envelope at 2005 Bd (the narrowing now stops at an eighth).
#[test]
fn qpsk_bursts_and_a_mode_s_train_are_not_runs_of_ook() {
    use zeta_core::classify::RateLine;
    use zeta_dsp::synth::SynthSpec;
    let modes = SynthSpec::ModesIdent {
        icao: 0x7c_1234,
        callsign: "QFA1".into(),
        repeats: 20,
    };
    for (what, iq, rate) in [
        (
            "qpsk bursts 9600 Bd 15 dB",
            probe_signal(psk_bursts(9_600, 4), Some(15.0), 0.0, 96_000),
            96_000,
        ),
        (
            "qpsk bursts 9600 Bd 8 dB",
            probe_signal(psk_bursts(9_600, 4), Some(8.0), 0.0, 96_000),
            96_000,
        ),
        (
            "mode s 30 dB",
            probe_signal(modes.clone(), Some(30.0), 0.0, 2_400_000),
            2_400_000,
        ),
        (
            "mode s 8 dB",
            probe_signal(modes, Some(8.0), 0.0, 2_400_000),
            2_400_000,
        ),
    ] {
        let (f, l) = production(&iq, rate);
        eprintln!("{what}: {f:?} -> {} ({})", l.name, l.evidence);
        assert_ne!(l.name, "ook", "{what}: {f:?} {l:?}");
        assert_ne!(f.symbol_rate_line, Some(RateLine::Runs), "{what}: {f:?}");
    }
}

/// Base's time (41c5c07, debug build, the review probe on the field host) for one 1 s window
/// of bursty FSK at 2.4 Msps through `measure_selection`, ms.
const BURSTY_FSK_2M4_BASE_MS: u128 = 112;

/// The cost guard: bursty FSK at 2.4 Msps (eight 4800 Bd bursts 100 ms apart, 200 kHz
/// off the centre, 15 dB) measures within twice base's time. The head before the fix
/// took 3465 ms (the whole-window refit and the narrowing at the full rate); with each
/// band cut once at a decimated rate, the refit skipped for a gate that holds more than
/// 5 % of the window, and selections in place of sorts it takes 80 ms (debug, the field host,
/// 2026-11-04). Timing depends on the build and the host: run with `--ignored`; the bound
/// is asserted in a debug build only.
#[test]
#[ignore]
fn bursty_fsk_at_2m4_measures_within_twice_base_time() {
    let spec = zeta_dsp::synth::SynthSpec::Fsk {
        symbol_rate_baud: 4_800,
        deviation_hz: 6_000.0,
        order: 2,
        bits: "a5c3f0e1a5c3f0e1".into(),
        repeat: 8,
        gap_ms: 100,
    };
    let iq = probe_signal(spec, Some(15.0), 200_000.0, 2_400_000);
    // The best of three, so a busy host does not fail it.
    let ms = (0..3)
        .map(|_| {
            let t = std::time::Instant::now();
            let _ = production(&iq, 2_400_000);
            t.elapsed().as_millis()
        })
        .min()
        .unwrap();
    eprintln!("bursty fsk at 2.4 Msps: {ms} ms (base {BURSTY_FSK_2M4_BASE_MS} ms)");
    if cfg!(debug_assertions) {
        assert!(ms <= 2 * BURSTY_FSK_2M4_BASE_MS, "{ms} ms");
    }
}

/// A keyed-over case measured as `classify` reads a channel: `measure_selection` of its
/// band about the window's centre.
fn keyed(c: &zeta_dsp::fixtures::KeyedOverCase) -> (zeta_core::classify::FeatureRecord, Label) {
    let iq = zeta_dsp::fixtures::keyed_over(&c.over);
    zeta_dsp::measure::measure_selection(
        &iq,
        u64::from(c.over.rate_hz),
        CENTER,
        zeta_core::classify::Band {
            offset_hz: 0,
            bandwidth_hz: zeta_core::types::Hz(c.band_hz),
        },
    )
    .expect("a measurement")
}

/// Fails with every case that broke `check`, one per line, after printing each case.
fn every_keyed_over(
    cases: &[zeta_dsp::fixtures::KeyedOverCase],
    check: impl Fn(
        &zeta_dsp::fixtures::KeyedOverCase,
        &zeta_core::classify::FeatureRecord,
        &Label,
    ) -> Option<String>,
) {
    let mut failed = Vec::new();
    for c in cases {
        let (f, l) = keyed(c);
        eprintln!("{}: {} duty {:?} ({})", c.name, l.name, f.duty, l.evidence);
        if let Some(why) = check(c, &f, &l) {
            failed.push(format!("{}: {why}", c.name));
        }
    }
    assert!(
        failed.is_empty(),
        "{} of {} cases:\n{}",
        failed.len(),
        cases.len(),
        failed.join("\n")
    );
}

/// The keyed overs the label and duty tests read: speech and a quiet passage, keyed up
/// and down at every share of `KEYED_OVER_SHARES`, and the over with a hole in it.
fn speech_overs() -> Vec<zeta_dsp::fixtures::KeyedOverCase> {
    zeta_dsp::fixtures::keyed_over_cases(&[1.0, 0.1], &zeta_dsp::fixtures::KEYED_OVER_SHARES, true)
}

/// Spec 3.1: a slice of a keyed FM channel holds noise, then the key-up (or the key-down,
/// or a drop-out). Whatever share of it the carrier is up, it is FM: `fm`, or `fsk` (both
/// choose `fm_demod`), never `am`, `ook` or `unknown`.
#[test]
fn a_keyed_over_reads_fm_at_every_carrier_up_share() {
    every_keyed_over(&speech_overs(), |_, _, l| {
        (!matches!(l.name.as_str(), "fm" | "fsk")).then(|| format!("{} ({})", l.name, l.evidence))
    });
}

/// The gate finds the off part from either side: `duty` is the share the carrier is up,
/// within 0.05, a hole of 200 ms in 2 s (a share of 0.9) included.
#[test]
fn a_keyed_overs_duty_is_its_carrier_up_share() {
    // 0.05 and a rounding's slack: an f32 duty of exactly 0.05 off is within it.
    every_keyed_over(&speech_overs(), |c, f, _| match f.duty {
        Some(d) if (f64::from(d) - c.share).abs() <= 0.05 + 1e-6 => None,
        d => Some(format!("duty {d:?}, the carrier up {:.2}", c.share)),
    });
}

/// A pause between words: the carrier keyed but unmodulated reads `carrier`, as a steady
/// one does.
#[test]
fn an_unmodulated_keyed_carrier_reads_carrier() {
    let cases = zeta_dsp::fixtures::keyed_over_cases(&[0.0], &[0.9, 0.4], false);
    every_keyed_over(&cases, |_, _, l| {
        (l.name != "carrier").then(|| format!("{} ({})", l.name, l.evidence))
    });
}

/// Every envelope CV a label's evidence quotes (`cv <n>`), in order.
fn quoted_cvs(evidence: &str) -> Vec<f32> {
    evidence
        .match_indices("cv ")
        .filter(|&(i, _)| {
            evidence[..i]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_')
        })
        .filter_map(|(i, m)| {
            let rest = &evidence[i + m.len()..];
            let end = rest
                .find(|c: char| !c.is_ascii_digit() && c != '.')
                .unwrap_or(rest.len());
            rest[..end].trim_end_matches('.').parse().ok()
        })
        .collect()
}

/// Spec 3.1, first bullet: the label is read from the samples the record measured. A
/// keyed over is gated (its off part is noise, its runs sparse), so its view is its on
/// runs as received: the evidence quotes one CV, and it is the envelope CV of those runs
/// of the channel `measure_selection` cut, not the whole window's.
#[test]
fn the_label_and_the_record_describe_the_same_samples() {
    every_keyed_over(&speech_overs(), |c, _, l| {
        let rate = u64::from(c.over.rate_hz);
        let iq = zeta_dsp::fixtures::keyed_over(&c.over);
        let (chan, out_rate) = zeta_dsp::channel::extract_offset(
            &iq,
            rate,
            0,
            c.band_hz,
            zeta_core::ops::channel_rate_hz(rate, c.band_hz),
        )
        .expect("the channel");
        let b = zeta_dsp::measure::burst_runs(&chan, out_rate);
        let on: Vec<Complex<f32>> = b
            .runs
            .iter()
            .flat_map(|&(s, e)| chan[s..e].iter().copied())
            .collect();
        let view = envelope_cv(&on);
        // Quoted to three places: one value, however often it is quoted.
        let cvs = quoted_cvs(&l.evidence);
        (cvs.is_empty() || cvs.iter().any(|cv| (cv - view).abs() > 0.001))
            .then(|| format!("quotes cv {cvs:?}, the view's {view:.3}"))
    });
}

/// Spec 49.1: `ook` stays on-off keying once the label reads the measured samples. The
/// real remotes in `tests/data`, the public synthesised remotes as `gen_ook` builds them,
/// and the generator's OOK (one frame and a train of three, over the whole block and over
/// an 8 kHz band) all still read `ook`.
#[test]
fn every_ook_fixture_still_reads_ook() {
    let mut failed = Vec::new();
    for name in OOK_REMOTES.iter().chain(&OOK_SPARSE_REMOTES) {
        if let Some((_, l)) = ook_remote(name)
            && l.name != "ook"
        {
            failed.push(format!("{name}: {} ({})", l.name, l.evidence));
        }
    }
    let band = |hz: u64| zeta_core::classify::Band {
        offset_hz: 0,
        bandwidth_hz: zeta_core::types::Hz(hz),
    };
    for fx in zeta_dsp::fixtures::ev1527_fixtures() {
        let iq = fx.remote.iq(fx.rate_hz);
        let (_, l) = zeta_dsp::measure::measure_selection(
            &iq,
            u64::from(fx.rate_hz),
            fx.center_hz,
            band(fx.band_hz),
        )
        .expect("a measurement");
        eprintln!("{} generated: {} ({})", fx.stem, l.name, l.evidence);
        if l.name != "ook" {
            failed.push(format!(
                "{} generated: {} ({})",
                fx.stem, l.name, l.evidence
            ));
        }
    }
    for repeat in [1, 3] {
        let spec = zeta_dsp::synth::SynthSpec::Ook {
            symbol_rate_baud: 1200,
            bits: "c0de1a5f0e93b2d47c18e6a05bd3f2917e4c".into(),
            repeat,
            gap_ms: 50,
        };
        let iq = zeta_dsp::synth::synthesise(
            &spec,
            &zeta_dsp::synth::Impairments {
                snr_db: Some(20.0),
                ..Default::default()
            },
            48_000,
            CENTER,
        );
        for hz in [48_000, 8_000] {
            let (_, l) = zeta_dsp::measure::measure_selection(&iq, 48_000, CENTER, band(hz))
                .expect("a measurement");
            eprintln!(
                "synth ook x{repeat} in {hz} Hz: {} ({})",
                l.name, l.evidence
            );
            if l.name != "ook" {
                failed.push(format!(
                    "synth ook x{repeat} in {hz} Hz: {} ({})",
                    l.name, l.evidence
                ));
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// A layer-1 reading of `iq` (48 kS/s) over the whole block and over a 12.5 kHz band about
/// its centre, as `classify` reads a selection: `(where, features, label)`, each printed with
/// its answered deviation.
fn whole_and_channel(
    what: &str,
    iq: &[Complex<f32>],
) -> Vec<(u64, zeta_core::classify::FeatureRecord, Label)> {
    [48_000u64, 12_500]
        .into_iter()
        .map(|hz| {
            let (f, l) = zeta_dsp::measure::measure_selection(
                iq,
                48_000,
                CENTER,
                zeta_core::classify::Band {
                    offset_hz: 0,
                    bandwidth_hz: zeta_core::types::Hz(hz),
                },
            )
            .expect("a measurement");
            eprintln!(
                "{what} in {hz} Hz: {} audio_rms_hz {:?} ({})",
                l.name,
                f.audio_rms_hz.map(|h| h.0),
                l.evidence
            );
            (hz, f, l)
        })
        .collect()
}

/// The other side of rule 10's deviation floor: a bare carrier that drifts (a transmitter
/// warming up, a free-running oscillator, a chirping key-down) is still a carrier, its drift
/// not read as deviation, from 60 Hz to 3 kHz over a 2 s slice.
#[test]
fn a_drifting_bare_carrier_reads_carrier() {
    let mut failed = Vec::new();
    for drift_hz in [60.0, 150.0, 300.0, 1_000.0, 3_000.0] {
        for snr_db in [15.0f32, 36.0] {
            let what = format!("carrier drifting {drift_hz} Hz in 2 s at {snr_db} dB");
            let iq = zeta_dsp::fixtures::drifting_carrier(48_000, 2.0, drift_hz, snr_db, 61);
            for (hz, _, l) in whole_and_channel(&what, &iq) {
                if l.name != "carrier" {
                    failed.push(format!("{what} in {hz} Hz: {} ({})", l.name, l.evidence));
                }
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// Low-index AM (m 0.1, CV under rule 9's 0.1 at mid SNR) falls through to rule 10: its
/// noise-driven frequency is not deviation, so it never reads `fm`, from 3 to 30 dB. At
/// 6-11 dB in 12.5 kHz the phase alone reads 41-47 Hz; its envelope moves as much.
#[test]
fn low_index_am_never_reads_fm() {
    let mut failed = Vec::new();
    for snr_db in [
        3.0f32, 6.0, 6.5, 7.0, 8.0, 9.0, 10.0, 12.0, 15.0, 20.0, 30.0,
    ] {
        for seed in [71, 72, 73, 74] {
            let what = format!("am m 0.1 at {snr_db} dB, seed {seed}");
            let audio = tone(48_000, 96_000, 1_000.0, 1.0);
            let mut iq = am_modulate(&audio, 48_000, 48_000, 0.1, 0.0);
            awgn(&mut iq, snr_db, seed);
            for (hz, _, l) in whole_and_channel(&what, &iq) {
                if l.name == "fm" {
                    failed.push(format!("{what} in {hz} Hz: {} ({})", l.name, l.evidence));
                }
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// Rule 4 on a keyed AM over: a slice holding noise and then speech-like AM (m 1), keyed up,
/// keyed down or up for 90 %, is gated to the carrier where it is up and reads `am`, never
/// `ook`: the modulated on-part is one level, its envelope's motion the speech.
#[test]
fn a_keyed_am_over_reads_am_not_ook() {
    let mut failed = Vec::new();
    for (kind, on) in [
        ("up", (0.4, 1.0)),
        ("down", (0.0, 0.6)),
        ("up 0.9", (0.1, 1.0)),
    ] {
        for snr_db in [20.0f32, 36.0] {
            let what = format!("keyed am {kind} at {snr_db} dB");
            let audio = zeta_dsp::fixtures::speech_like(48_000, 2.0);
            let mut iq = am_modulate(&audio, 48_000, 48_000, 1.0, 0.0);
            let n = iq.len() as f64;
            for (i, s) in iq.iter_mut().enumerate() {
                let t = i as f64 / n;
                if t < on.0 || t >= on.1 {
                    *s = Complex::new(0.0, 0.0);
                }
            }
            // The noise under a carrier of power 1, over the whole window.
            let mut noise = vec![Complex::new(0.0f32, 0.0); iq.len()];
            awgn(&mut noise, snr_db, 81);
            iq.iter_mut().zip(noise).for_each(|(s, w)| *s += w);
            let (hz, f, l) = whole_and_channel(&what, &iq).remove(1);
            if l.name != "am" {
                failed.push(format!("{what} in {hz} Hz: {} ({})", l.name, l.evidence));
            }
            if !f.burst {
                failed.push(format!("{what}: not gated ({f:?})"));
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// Rule 9 weighs the phase's motion against the envelope's, both in Hz, so a rate in
/// radians per sample does not decide it (spec 49.1, last bullet). A speech over weak
/// enough that the noise puts its CV in rule 9's range (0.1 to 0.2) reads FM, whole or
/// keyed up, in a 12.5 kHz selection of a 48 kS/s and of a 192 kS/s block, and over the
/// whole of each block. In radians per sample its phase (650 Hz RMS) fell under the CV in
/// the selections and it read `am`. Low-index AM (m 0.2) still reads `am` through rule 9 at
/// both rates, over the whole block and in 12.5 kHz.
#[test]
fn rule_9_weighs_the_phase_against_the_envelope_at_48_and_192_ks() {
    let mut failed = Vec::new();
    // The SNR is over the whole rate: a 12.5 kHz selection of the 192 kS/s block holds
    // 6 dB more of it than one of the 48 kS/s block, and a whole block less.
    for (rate, band_hz, snr_db) in [
        (48_000u32, 12_500u64, 6.0f32),
        (48_000, 48_000, 12.0),
        (192_000, 12_500, 0.0),
        (192_000, 192_000, 12.0),
    ] {
        for on in [(0.0, 1.0), (0.4, 1.0)] {
            let over = zeta_dsp::fixtures::KeyedOver {
                rate_hz: rate,
                seconds: 2.0,
                deviation_hz: 2_500.0,
                audio: 1.0,
                on,
                snr_db,
                seed: 7,
            };
            let c = zeta_dsp::fixtures::KeyedOverCase {
                name: format!(
                    "over up from {} at {rate} S/s in {band_hz} Hz, {snr_db} dB",
                    on.0
                ),
                share: on.1 - on.0,
                band_hz,
                over,
            };
            let (_, l) = keyed(&c);
            eprintln!("{}: {} ({})", c.name, l.name, l.evidence);
            let cvs = quoted_cvs(&l.evidence);
            if cvs.len() != 1 || !cvs.iter().all(|cv| (0.1..0.2).contains(cv)) {
                failed.push(format!("{}: cv {cvs:?} outside rule 9's range", c.name));
            }
            if !matches!(l.name.as_str(), "fm" | "fsk") {
                failed.push(format!("{}: {} ({})", c.name, l.name, l.evidence));
            }
        }
    }
    for rate in [48_000u32, 192_000] {
        let audio = tone(rate, rate as usize, 1_000.0, 1.0);
        let mut iq = am_modulate(&audio, rate, rate, 0.2, 0.0);
        awgn(&mut iq, 20.0, 7);
        for hz in [u64::from(rate), 12_500] {
            let (_, l) = zeta_dsp::measure::measure_selection(
                &iq,
                u64::from(rate),
                CENTER,
                zeta_core::classify::Band {
                    offset_hz: 0,
                    bandwidth_hz: zeta_core::types::Hz(hz),
                },
            )
            .expect("a measurement");
            let what = format!("am m 0.2 at {rate} S/s in {hz} Hz");
            eprintln!("{what}: {} ({})", l.name, l.evidence);
            if l.name != "am" || !l.evidence.contains("phase rms") {
                failed.push(format!("{what}: {} ({})", l.name, l.evidence));
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// A weak case measured as `classify` reads a channel: `measure_selection` of its band
/// about the window's centre.
fn weak(c: &zeta_dsp::fixtures::WeakCase) -> (zeta_core::classify::FeatureRecord, Label) {
    let iq = zeta_dsp::fixtures::weak_case_iq(c);
    zeta_dsp::measure::measure_selection(
        &iq,
        u64::from(zeta_dsp::fixtures::KEYED_OVER_RATE_HZ),
        CENTER,
        zeta_core::classify::Band {
            offset_hz: 0,
            bandwidth_hz: zeta_core::types::Hz(c.band_hz),
        },
    )
    .expect("a measurement")
}

/// Fails with every weak case of `cases` that broke `check`, one per line, after printing
/// each case.
fn every_weak_case(
    cases: &[zeta_dsp::fixtures::WeakCase],
    check: impl Fn(
        &zeta_dsp::fixtures::WeakCase,
        &zeta_core::classify::FeatureRecord,
        &Label,
    ) -> Option<String>,
) {
    let mut failed = Vec::new();
    for c in cases {
        let (f, l) = weak(c);
        eprintln!("{}: {} duty {:?} ({})", c.name, l.name, f.duty, l.evidence);
        if let Some(why) = check(c, &f, &l) {
            failed.push(format!("{}: {why}", c.name));
        }
    }
    assert!(
        failed.is_empty(),
        "{} of {} cases:\n{}",
        failed.len(),
        cases.len(),
        failed.join("\n")
    );
}

/// The weak cases holding FM at this audio level (1.0 speech, 0.0 a bare carrier).
fn weak_fm(audio: f32) -> Vec<zeta_dsp::fixtures::WeakCase> {
    use zeta_dsp::fixtures::WeakSignal;
    zeta_dsp::fixtures::weak_cases()
        .into_iter()
        .filter(|c| matches!(c.signal, WeakSignal::Fm { audio: a, .. } if a == audio))
        .collect()
}

/// From this SNR inside the channel a weak FM over is told from AM and from a bare
/// carrier, dB: at 3 dB the speech's deviation stands clear of what the noise's clicks
/// leave in the discriminator's audio, and the signal is stronger than the noise its
/// envelope is read through. At 0 dB neither holds.
const WEAK_DECIDED_DB: f32 = 3.0;

/// UAT 48.6 on a weak channel: an FM voice over only 3 to 12 dB over the noise inside its
/// channel reads `fm` (or `fsk`; both choose `fm_demod`), whole, keyed up or keyed down,
/// in 12.5 and in 25 kHz. At these levels the noise alone puts the envelope CV at 0.18 to
/// 0.42, which read `am` (a carrier line and a CV of 0.25) or `unknown`. Weaker than that
/// it is not decided, and the answer is `unknown`: never a wrong demodulator.
#[test]
fn a_weak_fm_over_reads_fm_and_is_never_am() {
    every_weak_case(&weak_fm(1.0), |c, _, l| {
        let fm = matches!(l.name.as_str(), "fm" | "fsk");
        // Under the limit its deviation shows but is not clear of the noise's clicks:
        // that is not decided, and it is not an unmodulated `carrier` either.
        let ok = fm || (c.snr_db < WEAK_DECIDED_DB && l.name == "unknown");
        (!ok).then(|| format!("{} ({})", l.name, l.evidence))
    });
}

/// Rule 10 on audio no tone or keying rule can take ([`zeta_dsp::fixtures::voice_like`]:
/// noise in the speech band under syllables, 625 Hz RMS of a 2.5 kHz peak deviation): an FM
/// over of it reads `fm` from 3 dB over the noise inside its channel, whole or keyed up,
/// in 12.5 and in 25 kHz. Weaker, it reads `fm` or `unknown`: never `am`, and never
/// `carrier`, since its deviation shows.
#[test]
fn a_weak_fm_over_of_voice_like_audio_reads_fm() {
    use zeta_dsp::fixtures::WeakSignal;
    let voice: Vec<_> = zeta_dsp::fixtures::weak_cases()
        .into_iter()
        .filter(|c| matches!(c.signal, WeakSignal::Voice { flutter_hz, .. } if flutter_hz == 0.0))
        .collect();
    every_weak_case(&voice, |c, _, l| {
        let ok = l.name == "fm" || (c.snr_db < WEAK_DECIDED_DB && l.name == "unknown");
        (!ok).then(|| format!("{} ({})", l.name, l.evidence))
    });
}

/// A mobile's over flutters: its carrier fades as a Rayleigh path does at 30 Hz of Doppler
/// (4.4 dB or more under its mean 30 % of the time, 10 ms at a time), keyed up 40 % into
/// the window, its mean power 3 to 12 dB over the channel's noise. Its deep fades do reach
/// the noise, 14 to 28 times in its 1.2 s, so the gate's runs are short, but of uneven
/// length, as fades are and frames are not: the window is still gated to them (duty 0.35
/// to 0.54 of a carrier keyed for 0.6), the noise before the key-up is not read with it,
/// and it is never `am`: `fm`, or `unknown` when its fading envelope is more than the
/// noise explains.
#[test]
fn a_weak_fading_fm_over_stays_gated_and_is_never_am() {
    use zeta_dsp::fixtures::WeakSignal;
    let fading: Vec<_> = zeta_dsp::fixtures::weak_cases()
        .into_iter()
        .filter(|c| matches!(c.signal, WeakSignal::Voice { flutter_hz, .. } if flutter_hz > 0.0))
        .collect();
    every_weak_case(&fading, |c, f, l| {
        // Gated: the label read less than the window ("over <on> s of <window> s"), and
        // the duty is the carrier's share of it less its fades.
        let read: Vec<f64> = l
            .evidence
            .rsplit(" over ")
            .next()
            .map(|t| {
                t.split(" s")
                    .filter_map(|x| x.trim_start_matches(" of ").trim().parse().ok())
                    .collect()
            })
            .unwrap_or_default();
        let gated = f.burst
            && matches!(read[..], [on, window] if on < window)
            && f.duty.is_some_and(|d| (0.3..=0.65).contains(&f64::from(d)));
        let label = matches!(l.name.as_str(), "fm" | "fsk" | "unknown");
        (!(gated && label)).then(|| {
            format!(
                "{} duty {:?}, the carrier up {:.2} ({})",
                l.name, f.duty, c.share, l.evidence
            )
        })
    });
}

/// The other side: a bare carrier as weak reads `carrier`, not `fm` on the noise's phase
/// (under about 10 dB a discriminator's output is the noise's clicks) and not `am` on the
/// noise's envelope. Under 3 dB it may be `unknown`.
#[test]
fn a_weak_bare_carrier_reads_carrier() {
    every_weak_case(&weak_fm(0.0), |c, _, l| {
        let ok = l.name == "carrier" || (c.snr_db < WEAK_DECIDED_DB && l.name == "unknown");
        (!ok).then(|| format!("{} ({})", l.name, l.evidence))
    });
}

/// The slow gate: a carrier 3 dB or more over the channel's noise, keyed up or down, is
/// gated to where it is up, and `duty` is that share within 0.05. Single 1 ms chunks do
/// not tell the two levels apart there: the fast gate read a duty of 0.02 to 0.2, or 1.
/// Levels between the cases' are read too: the fast gate's runs are ragged from 4 to 6 dB.
#[test]
fn a_weak_keyed_overs_duty_is_its_carrier_up_share() {
    use zeta_dsp::fixtures::{WeakCase, WeakSignal};
    let mut cases: Vec<WeakCase> = [weak_fm(1.0), weak_fm(0.0)]
        .concat()
        .into_iter()
        .filter(|c| c.snr_db >= WEAK_DECIDED_DB)
        .collect();
    for snr_db in [3.0f32, 4.0, 5.0, 7.5, 10.0] {
        for band_hz in [12_500u64, 25_000] {
            // Keyed up, keyed down, up late, and a drop-out of a fifth of the window.
            for (audio, on, share) in [
                (1.0f32, (0.4, 1.0), 0.6),
                (0.0, (0.0, 0.4), 0.4),
                (1.0, (0.06, 1.0), 0.94),
                (1.0, (0.6, 0.4), 0.8),
            ] {
                cases.push(WeakCase {
                    name: format!("between a{audio} on {on:?} {snr_db} dB {band_hz} Hz"),
                    signal: WeakSignal::Fm {
                        deviation_hz: 2_500.0,
                        audio,
                        on,
                    },
                    band_hz,
                    snr_db,
                    share,
                    seed: 5,
                });
            }
        }
    }
    every_weak_case(&cases, |c, f, l| {
        let duty = match f.duty {
            Some(d) if (f64::from(d) - c.share).abs() <= 0.05 + 1e-6 => None,
            d => Some(format!("duty {d:?}, the carrier up {:.2}", c.share)),
        };
        // The cases between the list's levels hold their labels too.
        let audio = matches!(c.signal, WeakSignal::Fm { audio, .. } if audio > 0.0);
        let label = match (audio, l.name.as_str()) {
            (true, "fm" | "fsk") | (false, "carrier") => None,
            _ => Some(format!("{} ({})", l.name, l.evidence)),
        };
        duty.or(label)
    });
}

/// Real AM is still `am` when it is weak: its envelope moves more than the noise explains
/// (rule 7) or more than its phase does (rule 9).
/// - A tone at index 0.5 reads `am` at every level, from 1 dB.
/// - A tone at index 0.2 and speech at 0.9 read `am` from 6 dB in the channel. Under 6 dB
///   (an envelope CV of 0.14 to 0.2 under a noise share of 0.4 and more) they may read
///   `carrier` or `unknown`: the envelope's motion is within the margin of the phase's.
/// - A tone at index 0.9 reads `am` or, as it did before, `ook` (its envelope's troughs a
///   tenth of its crests, every millisecond, fit a chip).
/// - The same holds over noise that slopes by 6 dB across the channel, as under a
///   neighbour's skirt, where the noise beside the carrier is not the noise under it.
/// - None reads `fm` or `fsk`, down to 1 dB: weak AM whose envelope the noise has taken
///   is never handed to an FM demodulator.
#[test]
fn weak_am_reads_am() {
    use zeta_dsp::fixtures::{WeakCase, WeakSignal};
    let am: Vec<WeakCase> = zeta_dsp::fixtures::weak_cases()
        .into_iter()
        .filter(|c| matches!(c.signal, WeakSignal::Am { .. }))
        .collect();
    every_weak_case(&am, |c, _, l| {
        let WeakSignal::Am { index, speech, .. } = c.signal else {
            return None;
        };
        let ok = if index == 0.9 && !speech {
            matches!(l.name.as_str(), "am" | "ook")
        } else if c.snr_db >= 6.0 || index == 0.5 {
            l.name == "am"
        } else {
            matches!(l.name.as_str(), "am" | "carrier" | "unknown")
        };
        (!ok).then(|| format!("{} ({})", l.name, l.evidence))
    });
}
