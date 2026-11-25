//! The choice at step 1: `classify` answers one ranked job from layer 1's measurement, a
//! quick proof and the rows, with its evidence, runner-up and the rows that could prove
//! otherwise. Each test runs a node over a synthesised capture played in real time.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use num_complex::Complex;
use zeta_core::capture::{CaptureMeta, write_cf32};
use zeta_core::choice::{Choice, NotTried, Settled, choose_now};
use zeta_core::classify::{Classification, FeatureRecord, Label, Measurement};
use zeta_core::modes::ModeRegistry;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{Op, OpResult};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, StageRegistry};
use zeta_core::types::{Hz, Selection, SourceId, Span, Timestamp};
use zeta_dsp::synth::{
    am_modulate, awgn, nbfm_modulate, pocsag_packet_iq, ssb_modulate, tone, wfm_stereo_modulate,
};
use zeta_source_file::{FileOpts, FileSource};

const RATE_HZ: u32 = 48_000;
/// A well-known POCSAG centre, inside the catalogue's pager and DMR ranges.
const POCSAG_HZ: u64 = 148_637_500;
const SPAN_NS: u64 = 2_000_000_000;

/// What `classify` answered: the labels, layer 1's features and the choice.
struct Answer {
    labels: Vec<Label>,
    features: Option<FeatureRecord>,
    choice: Choice,
}

/// One second-scale burst of three tones under a tremolo, then silence: voice enough
/// for layer 1 (zeta-dsp's `speech_like`).
fn speech_like(rate_hz: u32, seconds: f32) -> Vec<f32> {
    use std::f64::consts::PI;
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

/// A node over `iq` at `rate_hz` centred on `center_hz`, with the DSP stages and
/// classifiers, once its ring holds the first [`SPAN_NS`].
async fn node_over(dir: &Path, iq: &[Complex<f32>], rate_hz: u32, center_hz: u64) -> Node {
    let meta = CaptureMeta {
        sample_rate_hz: Hz(rate_hz as u64),
        center_hz: Hz(center_hz),
        start: Timestamp(0),
        annotations: Vec::new(),
    };
    let meta = write_cf32(dir, "fixture", &meta, iq).expect("write fixture");
    let opts = FileOpts {
        realtime: true,
        block_samples: (rate_hz / 10) as usize,
        repeat: true,
        tune_range: None,
    };
    let source = SourceId("rx".into());
    let src = Arc::new(FileSource::open(source.clone(), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    let mut reg = StageRegistry::new();
    zeta_dsp::register_all(&mut reg);
    let cfg = NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        // Long enough that the first 2 s are still held while the test classifies.
        ring_seconds: 20.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
        owns_state_dir: true,
        bank_factory: None,
        modes_dir: None,
        handlers_dir: None,
    };
    let node = Node::open(cfg, reg, vec![port]).await.unwrap();
    for c in zeta_dsp::classifiers(node.modes_handle()) {
        node.add_classifier(c);
    }
    src.start();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let latest = node.ring(&source).and_then(|r| r.read().unwrap().latest());
        if latest.is_some_and(|t| t.0 >= SPAN_NS) {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "ring never filled");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    node
}

/// `classify` over the first [`SPAN_NS`] of the source, band `center_hz ± bandwidth_hz/2`.
async fn classify(node: &Node, center_hz: u64, bandwidth_hz: u64) -> Answer {
    let selection = Selection {
        source: SourceId("rx".into()),
        span: Span {
            start_ns: Timestamp(0),
            end_ns: Timestamp(SPAN_NS),
        },
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(bandwidth_hz),
    };
    match node.apply(Op::Classify { selection }).await {
        Ok(OpResult::Classification {
            labels,
            features,
            choice,
            ..
        }) => {
            let choice = *choice.expect("classify answers a choice");
            eprintln!("labels {labels:?}\nfeatures {features:?}\nchoice {choice:?}");
            Answer {
                labels,
                features: features.map(|f| *f),
                choice,
            }
        }
        other => panic!("expected a classification, got {other:?}"),
    }
}

/// The choice's job, which must be there.
fn job(c: &Choice) -> &zeta_core::choice::ChoiceJob {
    c.job.as_ref().unwrap_or_else(|| panic!("no job: {c:?}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cb_voice_answers_fm_demod_with_the_plans_deviation() {
    let plan = &zeta_core::plans::UHF_CB_AU;
    let (_, channel_hz) = zeta_core::plans::channels(plan)
        .into_iter()
        .find(|&(n, _)| n == 10)
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut iq = nbfm_modulate(&speech_like(RATE_HZ, 3.0), RATE_HZ, RATE_HZ, 2_500.0, 700.0);
    awgn(&mut iq, 25.0, 5);
    let node = node_over(dir.path(), &iq, RATE_HZ, channel_hz).await;
    let a = classify(&node, channel_hz, 20_000).await;
    let c = &a.choice;
    let f = a.features.expect("layer 1 measured");
    let j = job(c);
    assert_eq!(j.kind, "fm_demod", "{c:?}");
    assert_eq!(c.row.as_deref(), Some("fm_demod"), "{c:?}");
    assert_eq!(c.settled, Settled::BestGuess, "{c:?}");
    assert_eq!(c.step, 1, "{c:?}");
    assert_eq!(j.params["deviation_hz"], 2_500, "{c:?}");
    // The measured centre, never the channel's.
    assert_eq!(j.center_hz, f.center_hz, "{c:?}");
    assert_ne!(j.center_hz.0, channel_hz, "{c:?}");
    assert!(
        j.center_hz.0.abs_diff(channel_hz + 700) <= 300,
        "{c:?} {f:?}"
    );
    assert!(
        c.evidence
            .iter()
            .any(|e| e == "plan: uhf_cb_au deviation 2500"),
        "{c:?}"
    );
    assert!(
        c.evidence.iter().any(|e| e.starts_with("layer 1: ")),
        "{c:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pocsag_burst_answers_pocsag_decode_proved() {
    let dir = tempfile::tempdir().unwrap();
    let iq = pocsag_packet_iq(1234567, 3, "HELLO ZETA", 1200, RATE_HZ, 0.0);
    let node = node_over(dir.path(), &iq, RATE_HZ, POCSAG_HZ).await;
    let a = classify(&node, POCSAG_HZ, 12_500).await;
    let c = &a.choice;
    let j = job(c);
    assert_eq!(j.kind, "pocsag_decode", "{c:?}");
    assert_eq!(c.row.as_deref(), Some("pocsag_decode"), "{c:?}");
    assert_eq!(c.settled, Settled::Proved, "{c:?}");
    assert_eq!(c.step, 1, "{c:?}");
    // The row's own width, on the measured centre.
    assert_eq!(j.bandwidth_hz, Hz(12_500), "{c:?}");
    assert_eq!(
        j.center_hz,
        a.features.expect("measured").center_hz,
        "{c:?}"
    );
    assert!(
        c.evidence
            .iter()
            .any(|e| e.starts_with("decode pocsag_decode: ")),
        "{c:?}"
    );
    assert!(!c.could_prove.iter().any(|r| r == "pocsag_decode"), "{c:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broadcast_station_answers_wfm_demod() {
    const WFM_RATE_HZ: u32 = 240_000;
    const STATION_HZ: u64 = 104_900_000;
    let dir = tempfile::tempdir().unwrap();
    let n = WFM_RATE_HZ as usize * 2;
    // The same programme on both channels, as layer 1's broadcast fixture: 1 kHz at 0.9.
    let audio = tone(WFM_RATE_HZ, n, 1_000.0, 0.9);
    let mut iq = wfm_stereo_modulate(&audio, &audio, WFM_RATE_HZ, 75_000.0, 0.1);
    awgn(&mut iq, 20.0, 21);
    let node = node_over(dir.path(), &iq, WFM_RATE_HZ, STATION_HZ).await;
    let a = classify(&node, STATION_HZ, 240_000).await;
    let c = &a.choice;
    let j = job(c);
    assert_eq!(j.kind, "wfm_demod", "{c:?}");
    assert_eq!(c.settled, Settled::BestGuess, "{c:?}");
    // Never under the row's floor.
    assert!(j.bandwidth_hz.0 >= 150_000, "{c:?}");
    assert_eq!(j.params["deviation_hz"], 75_000, "{c:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_am_carrier_answers_am_demod() {
    const AM_HZ: u64 = 127_000_000;
    let dir = tempfile::tempdir().unwrap();
    // Layer 1's AM fixture: a 1 kHz tone at index 0.5.
    let audio = tone(RATE_HZ, RATE_HZ as usize * 2, 1_000.0, 1.0);
    let mut iq = am_modulate(&audio, RATE_HZ, RATE_HZ, 0.5, 0.0);
    awgn(&mut iq, 20.0, 7);
    let node = node_over(dir.path(), &iq, RATE_HZ, AM_HZ).await;
    let a = classify(&node, AM_HZ, 20_000).await;
    let c = &a.choice;
    let j = job(c);
    assert_eq!(j.kind, "am_demod", "{c:?}");
    assert_eq!(c.settled, Settled::BestGuess, "{c:?}");
    assert!(j.bandwidth_hz.0 >= 5_000, "{c:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usb_answers_ssb_demod_on_the_dial() {
    const SSB_HZ: u64 = 14_200_000;
    // The voice-like fixture of zeta-dsp's layer-1 tests: 700 + 1900 Hz and a weak 2700 Hz.
    let audio: Vec<f32> = tone(RATE_HZ, RATE_HZ as usize, 700.0, 0.7)
        .iter()
        .zip(tone(RATE_HZ, RATE_HZ as usize, 1_900.0, 0.3))
        .zip(tone(RATE_HZ, RATE_HZ as usize, 2_700.0, 0.15))
        .map(|((a, b), c)| a + b + c)
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let mut iq = ssb_modulate(&audio, RATE_HZ, RATE_HZ, true, 0.0);
    awgn(&mut iq, 20.0, 9);
    let node = node_over(dir.path(), &iq, RATE_HZ, SSB_HZ).await;
    let a = classify(&node, SSB_HZ, 48_000).await;
    let c = &a.choice;
    let j = job(c);
    let ssb = a.features.and_then(|f| f.ssb).expect("an ssb feature");
    assert_eq!(j.kind, "ssb_demod", "{c:?}");
    assert_eq!(j.center_hz, ssb.dial_hz, "{c:?}");
    assert_eq!(j.params["sideband"], "usb", "{c:?}");
    assert_eq!(j.bandwidth_hz, Hz(6_000), "{c:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn noise_answers_cannot_tell() {
    let dir = tempfile::tempdir().unwrap();
    let mut iq = vec![Complex::new(0.0, 0.0); RATE_HZ as usize * 3];
    awgn(&mut iq, 0.0, 11);
    let node = node_over(dir.path(), &iq, RATE_HZ, POCSAG_HZ).await;
    let a = classify(&node, POCSAG_HZ, 12_500).await;
    let c = &a.choice;
    assert_eq!(c.settled, Settled::CannotTell, "{c:?}");
    assert!(c.job.is_none(), "{c:?}");
    assert!(c.row.is_none(), "{c:?}");
    assert_eq!(
        c.reason.as_deref(),
        Some("the band holds noise only"),
        "{c:?}"
    );
    // Written as `null`, so a client reads one shape.
    let v = serde_json::to_value(c).unwrap();
    assert!(v["job"].is_null(), "{v}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dmr_burst_answers_fm_demod_with_dmr_in_could_prove() {
    const DMR_HZ: u64 = 439_100_000;
    let dir = tempfile::tempdir().unwrap();
    let params = serde_json::json!({
        "family": "dmr", "color_code": 5, "src": 9990001, "dst": 9990, "superframes": 6
    });
    let (spec, imp) = zeta_dsp::synth::parse_params(params.as_object().unwrap().clone()).unwrap();
    let mut iq = zeta_dsp::synth::synthesise(&spec, &imp, RATE_HZ, DMR_HZ);
    awgn(&mut iq, 25.0, 13);
    let node = node_over(dir.path(), &iq, RATE_HZ, DMR_HZ).await;
    let a = classify(&node, DMR_HZ, 20_000).await;
    let c = &a.choice;
    let j = job(c);
    assert_eq!(j.kind, "fm_demod", "{c:?}");
    assert_eq!(c.settled, Settled::BestGuess, "{c:?}");
    // The node's DMR stage is registered, so the row is ready and a later step may prove it.
    assert!(c.could_prove.iter().any(|r| r == "dmr_decode"), "{c:?}");
    assert!(
        !c.not_tried
            .iter()
            .any(|n| n.row.as_deref() == Some("dmr_decode")),
        "{c:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_range_only_catalogue_match_never_overturns_the_label() {
    let dir = tempfile::tempdir().unwrap();
    // AM of a comb of tones up to 2.4 kHz: about 5 kHz occupied, the width of the
    // catalogue's narrow two-way entries on the frequency.
    let n = RATE_HZ as usize * 2;
    let mut audio = vec![0.0_f32; n];
    for k in 1..=6 {
        for (a, t) in audio
            .iter_mut()
            .zip(tone(RATE_HZ, n, 400.0 * k as f64, 1.0 / 6.0))
        {
            *a += t;
        }
    }
    let mut iq = am_modulate(&audio, RATE_HZ, RATE_HZ, 0.8, 0.0);
    awgn(&mut iq, 20.0, 7);
    let node = node_over(dir.path(), &iq, RATE_HZ, POCSAG_HZ).await;
    let a = classify(&node, POCSAG_HZ, 20_000).await;
    // The catalogue names a signal of the range on its frequency and width alone.
    let range_only: Vec<&Label> = a
        .labels
        .iter()
        .filter(|l| l.evidence.starts_with("catalogue ") && l.evidence.contains("modulation off"))
        .collect();
    assert!(!range_only.is_empty(), "{:?}", a.labels);
    let c = &a.choice;
    let j = job(c);
    assert_eq!(j.kind, "am_demod", "{c:?}");
    assert_eq!(c.settled, Settled::BestGuess, "{c:?}");
    // It only ranks the rows that list it: the demodulator lists none, so the choice's
    // evidence names none.
    assert!(
        !c.evidence.iter().any(|e| e.starts_with("catalogue: ")),
        "{c:?}"
    );
}

/// A DMR channel's centre, inside the catalogue's DMR range and off any plan channel.
const DMR_HZ: u64 = 439_100_000;

/// Layer 1's measurement of a band `label`, `bandwidth_hz` wide at `center_hz`.
fn measured(label: &str, center_hz: u64, bandwidth_hz: u64) -> Classification {
    let features = FeatureRecord {
        bandwidth_hz: Hz(bandwidth_hz),
        center_hz: Hz(center_hz),
        symbol_rate_baud: None,
        snr_db: 20.0,
        burst: false,
        duty: None,
        bursts: None,
        burst_ms: None,
        period_ms: None,
        order: None,
        tone_spacing_hz: None,
        symbol_rate_line: None,
        power99_hz: None,
        audio_rms_hz: None,
        ssb: None,
    };
    let label = Label {
        layer: 1,
        name: label.into(),
        confidence: 0.6,
        evidence: String::new(),
    };
    Classification {
        measurement: Some(Measurement { features, label }),
        ..Classification::default()
    }
}

/// A box `bandwidth_hz` wide at `center_hz` on `rx`.
fn box_at(center_hz: u64, bandwidth_hz: u64) -> Selection {
    Selection {
        source: SourceId("rx".into()),
        span: Span {
            start_ns: Timestamp(0),
            end_ns: Timestamp(SPAN_NS),
        },
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(bandwidth_hz),
    }
}

/// The embedded rows, with every DSP stage registered (so the in-process decoders are
/// ready), and the rows of `modes_dir`.
fn registry(modes_dir: Option<&Path>) -> ModeRegistry {
    let mut reg = StageRegistry::new();
    zeta_dsp::register_all(&mut reg);
    ModeRegistry::load(&reg, modes_dir)
}

/// A decoder row is tried at its own band, not the box's: a 12 kHz box on a 9 kHz FM
/// signal lists the 12.5 kHz DMR row, and is not told the box is too narrow for it.
#[test]
fn a_box_narrower_than_a_row_still_lists_it() {
    let modes = registry(None);
    let c = measured("fm", DMR_HZ, 9_000);
    let sel = box_at(DMR_HZ, 12_000);
    let capture = Some((Hz(DMR_HZ), Hz(RATE_HZ as u64)));
    let choice = choose_now(&modes, modes.weights(), &c, &sel, "AU", capture);
    eprintln!("{choice:?}");
    assert!(
        choice.could_prove.iter().any(|r| r == "dmr_decode"),
        "{choice:?}"
    );
    assert!(
        !choice
            .not_tried
            .iter()
            .any(|n| n.row.as_deref() == Some("dmr_decode")),
        "{choice:?}"
    );
}

/// A row whose own band reaches past the capture is not tried, with the capture's
/// reason; a narrower row on the same centre still is.
#[test]
fn a_row_the_capture_does_not_hold_is_not_tried_with_the_reason() {
    // An operator's copy of the M17 row, 8 kHz wide: inside the capture where the
    // 12.5 kHz rows are not.
    let dir = tempfile::tempdir().unwrap();
    let embedded = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modes/m17_decode.json");
    let mut row: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(embedded).unwrap()).unwrap();
    row["id"] = "m17_narrow".into();
    row["band"]["bandwidth_hz"] = 8_000.into();
    std::fs::write(dir.path().join("m17_narrow.json"), row.to_string()).unwrap();
    let modes = registry(Some(dir.path()));
    assert!(modes.get("m17_narrow").is_some_and(|m| m.startable()));
    // The signal 5 kHz inside the capture's upper edge.
    let ring_center = DMR_HZ;
    let edge = ring_center + RATE_HZ as u64 / 2;
    let center = edge - 5_000;
    let c = measured("fm", center, 7_000);
    let sel = box_at(center, 8_000);
    let capture = Some((Hz(ring_center), Hz(RATE_HZ as u64)));
    let choice = choose_now(&modes, modes.weights(), &c, &sel, "AU", capture);
    eprintln!("{choice:?}");
    assert!(
        choice.could_prove.iter().any(|r| r == "m17_narrow"),
        "{choice:?}"
    );
    for wide in ["dmr_decode", "p25_decode", "m17_decode"] {
        assert!(!choice.could_prove.iter().any(|r| r == wide), "{choice:?}");
        let note = NotTried {
            row: Some(wide.into()),
            reason: format!("not tried: the capture does not hold {wide}'s 12500 Hz here"),
        };
        assert!(choice.not_tried.contains(&note), "{wide}: {choice:?}");
    }
    // With no capture to test against, every row is listed.
    let free = choose_now(&modes, modes.weights(), &c, &sel, "AU", None);
    assert!(
        free.could_prove.iter().any(|r| r == "dmr_decode"),
        "{free:?}"
    );
}

/// `measured`, with `power99_hz` set.
fn measured_p99(label: &str, center_hz: u64, bandwidth_hz: u64, power99_hz: u64) -> Classification {
    let mut c = measured(label, center_hz, bandwidth_hz);
    c.measurement.as_mut().unwrap().features.power99_hz = Some(Hz(power99_hz));
    c
}

/// `unknown` waives the label term: the ready rows whose width window holds the occupied
/// width, `power99_hz` or the box are listed, to be tried.
#[test]
fn unknown_lists_the_ready_rows_its_width_fits() {
    let modes = registry(None);
    let c = measured_p99("unknown", DMR_HZ, 3_000, 9_000);
    let sel = box_at(DMR_HZ, 3_000);
    let choice = choose_now(&modes, modes.weights(), &c, &sel, "AU", None);
    for row in ["dmr_decode", "p25_decode", "m17_decode"] {
        assert!(
            choice.could_prove.iter().any(|r| r == row),
            "{row}: {choice:?}"
        );
    }
    for row in ["rds_decode", "adsb_decode"] {
        assert!(
            !choice.could_prove.iter().any(|r| r == row),
            "{row}: {choice:?}"
        );
    }
}

/// A row listed for `unknown` is tried, never started on this evidence.
#[test]
fn unknown_never_starts_an_unproved_decoder() {
    let modes = registry(None);
    let c = measured_p99("unknown", DMR_HZ, 3_000, 9_000);
    let sel = box_at(DMR_HZ, 3_000);
    let choice = choose_now(&modes, modes.weights(), &c, &sel, "AU", None);
    assert!(choice.job.is_none() && choice.row.is_none(), "{choice:?}");
    assert_eq!(choice.settled, Settled::CannotTell, "{choice:?}");
    assert_eq!(choice.reason.as_deref(), Some("no row serves unknown"));
    assert!(choice.runner_up.is_none(), "{choice:?}");
    assert!(!choice.could_prove.is_empty(), "{choice:?}");
}

/// Any other label keeps its label test, even at a width that fits the 12.5 kHz rows.
#[test]
fn a_label_other_than_unknown_keeps_its_label_test() {
    let modes = registry(None);
    let c = measured_p99("carrier", DMR_HZ, 12_000, 12_000);
    let sel = box_at(DMR_HZ, 12_000);
    let choice = choose_now(&modes, modes.weights(), &c, &sel, "AU", None);
    for row in ["dmr_decode", "p25_decode", "m17_decode"] {
        assert!(
            !choice.could_prove.iter().any(|r| r == row),
            "{row}: {choice:?}"
        );
    }
    // The same measurement read `unknown` lists them: the width is not what keeps them out.
    let unknown = measured_p99("unknown", DMR_HZ, 12_000, 12_000);
    let listed = choose_now(&modes, modes.weights(), &unknown, &sel, "AU", None);
    assert!(
        listed.could_prove.iter().any(|r| r == "dmr_decode"),
        "{listed:?}"
    );
}

/// For `unknown` the selection's width alone may fit a row: neither the occupied width
/// nor `power99_hz` does here.
#[test]
fn unknown_lists_a_row_only_the_selections_width_fits() {
    let modes = registry(None);
    let c = measured_p99("unknown", DMR_HZ, 3_000, 3_000);
    let narrow = choose_now(
        &modes,
        modes.weights(),
        &c,
        &box_at(DMR_HZ, 3_000),
        "AU",
        None,
    );
    assert!(
        !narrow.could_prove.iter().any(|r| r == "dmr_decode"),
        "{narrow:?}"
    );
    let wide = choose_now(
        &modes,
        modes.weights(),
        &c,
        &box_at(DMR_HZ, 12_000),
        "AU",
        None,
    );
    assert!(
        wide.could_prove.iter().any(|r| r == "dmr_decode"),
        "{wide:?}"
    );
}

// Spec 49.2: the expected emission weighs against the label. A test fixture's licences
// and the embedded UHF CB plan, never a real licence.

/// A UHF CB channel (`uhf_cb_au` ch 10): an FM plan, with a `deviation_hz`.
fn cb_channel_hz() -> u64 {
    zeta_core::plans::channels(&zeta_core::plans::UHF_CB_AU)
        .into_iter()
        .find(|&(n, _)| n == 10)
        .unwrap()
        .1
}

/// An airband centre, off every channel plan.
const AIRBAND_HZ: u64 = 127_000_000;

/// A test register's transmitter licensed for `designator` at `center_hz`.
fn licence(designator: &str, center_hz: u64) -> zeta_core::knowledge::Allocation {
    let emission = zeta_core::emission::parse_emission(designator).expect("a designator");
    let half = emission.bandwidth_hz.0 / 2;
    zeta_core::knowledge::Allocation {
        provider: "register".into(),
        region: "AU".into(),
        min_hz: Hz(center_hz - half),
        max_hz: Hz(center_hz + half),
        service: "assignment".into(),
        kind: Some("assignment".into()),
        licensee: Some("Test Licensee".into()),
        site: Some("Test site".into()),
        center_hz: Some(Hz(center_hz)),
        bandwidth_hz: Some(emission.bandwidth_hz),
        role: Some("tx".into()),
        emission: Some(designator.into()),
        emission_decoded: Some(emission),
        ..Default::default()
    }
}

/// [`measured`] with the register's `allocations` for the band.
fn measured_with(
    label: &str,
    center_hz: u64,
    bandwidth_hz: u64,
    allocations: Vec<zeta_core::knowledge::Allocation>,
) -> Classification {
    let mut c = measured(label, center_hz, bandwidth_hz);
    c.allocations = allocations;
    c
}

/// The choice for `c` in a `box_hz` box on its measured centre, with no capture.
fn choose_for(modes: &ModeRegistry, c: &Classification, box_hz: u64) -> Choice {
    let center = c.measurement.as_ref().unwrap().features.center_hz.0;
    choose_now(
        modes,
        modes.weights(),
        c,
        &box_at(center, box_hz),
        "AU",
        None,
    )
}

/// An `am` read on a channel licensed for FM: the licence outweighs the label, and the
/// label's own demodulator is the runner-up. Still a best guess, never a proof.
#[test]
fn am_on_a_channel_licensed_fm_answers_fm_demod_with_am_its_runner_up() {
    let modes = registry(None);
    // Measured 400 Hz off the licence's centre: the job is on the measured one.
    let c = measured_with("am", DMR_HZ + 400, 9_000, vec![licence("11K0F3E", DMR_HZ)]);
    let choice = choose_for(&modes, &c, 12_500);
    assert_eq!(choice.row.as_deref(), Some("fm_demod"), "{choice:?}");
    assert_eq!(choice.settled, Settled::BestGuess, "{choice:?}");
    assert_eq!(job(&choice).center_hz, Hz(DMR_HZ + 400), "{choice:?}");
    let runner_up = choice.runner_up.as_ref().expect("a runner-up");
    assert_eq!(runner_up.row, "am_demod", "{choice:?}");
    assert!(
        choice
            .evidence
            .iter()
            .any(|e| e == "register: 11K0F3E expected here (layer 1 read am)"),
        "{choice:?}"
    );
    // The runner-up is the label's own: nothing expected brought it.
    assert!(
        !runner_up
            .evidence
            .iter()
            .any(|e| e.starts_with("register: ")),
        "{runner_up:?}"
    );
}

/// An `am` read on an FM plan's channel with no register: the plan outweighs the label.
#[test]
fn am_on_a_plan_fm_channel_answers_fm_demod() {
    let modes = registry(None);
    let channel = cb_channel_hz();
    let c = measured("am", channel + 300, 9_000);
    let choice = choose_for(&modes, &c, 12_500);
    assert_eq!(choice.row.as_deref(), Some("fm_demod"), "{choice:?}");
    assert_eq!(choice.settled, Settled::BestGuess, "{choice:?}");
    let j = job(&choice);
    assert_eq!(j.center_hz, Hz(channel + 300), "{choice:?}");
    assert_eq!(j.params["deviation_hz"], 2_500, "{choice:?}");
    assert!(
        choice
            .evidence
            .iter()
            .any(|e| e == "plan: uhf_cb_au is an FM plan (layer 1 read am)"),
        "{choice:?}"
    );
    assert_eq!(
        choice.runner_up.as_ref().map(|r| r.row.as_str()),
        Some("am_demod"),
        "{choice:?}"
    );
}

/// An `am` read only 5.5 kHz wide (inside `am_demod`'s width window, under `fm_demod`'s)
/// on an FM plan's channel or an FM licence: the expected width fits `fm_demod`, and the
/// measured width of a misread is what is in doubt, so the expectation still outweighs it.
#[test]
fn a_narrow_am_on_an_fm_channel_still_answers_fm_demod() {
    let modes = registry(None);
    let channel = cb_channel_hz();
    let cases = [
        measured("am", channel + 300, 5_500),
        measured_with("am", DMR_HZ, 5_500, vec![licence("11K0F3E", DMR_HZ)]),
    ];
    for c in cases {
        let choice = choose_for(&modes, &c, 12_500);
        assert_eq!(choice.row.as_deref(), Some("fm_demod"), "{choice:?}");
        assert_eq!(
            choice.runner_up.as_ref().map(|r| r.row.as_str()),
            Some("am_demod"),
            "{choice:?}"
        );
    }
}

/// The FM licence at the centre listed third: behind two transmitters elsewhere in the
/// band, of a letter no demodulator serves (`N`) or of AM, which give `am_demod` a licence
/// term of its own. Neither outweighs the licence that holds the centre.
#[test]
fn an_fm_licence_listed_behind_others_still_outweighs_am() {
    let modes = registry(None);
    for other in ["1K00N0N", "6K00A3E"] {
        let allocs = vec![
            licence(other, DMR_HZ + 50_000),
            licence(other, DMR_HZ + 75_000),
            licence("11K0F3E", DMR_HZ),
        ];
        let c = measured_with("am", DMR_HZ, 9_000, allocs);
        let choice = choose_for(&modes, &c, 12_500);
        assert_eq!(
            choice.row.as_deref(),
            Some("fm_demod"),
            "{other}: {choice:?}"
        );
        assert_eq!(
            choice.runner_up.as_ref().map(|r| r.row.as_str()),
            Some("am_demod"),
            "{other}: {choice:?}"
        );
    }
}

/// A licence at the centre that no demodulator's width fits (an FM emission 50 kHz wide)
/// brings nothing and discounts nothing: whatever `contradicted` is, the answer is the one
/// with no licence at all (where `acars_decode` is not within the margin of `am_demod`).
#[test]
fn an_expectation_no_demodulator_fits_leaves_the_label_whole() {
    let modes = registry(None);
    let sel = box_at(AIRBAND_HZ, 20_000);
    let plain = measured("am", AIRBAND_HZ, 9_000);
    let before = choose_now(&modes, modes.weights(), &plain, &sel, "AU", None);
    assert_eq!(before.row.as_deref(), Some("am_demod"), "{before:?}");
    let wide = vec![licence("50K0F3E", AIRBAND_HZ)];
    let c = measured_with("am", AIRBAND_HZ, 9_000, wide);
    for k in [0.0, 0.15, 1.0] {
        let mut w = modes.weights().clone();
        w.contradicted = k;
        let choice = choose_now(&modes, &w, &c, &sel, "AU", None);
        assert_eq!(choice, before, "contradicted {k}");
    }
}

/// An `am` read where the licence is for AM answers `am_demod`, as it did; and where one
/// licence at the centre is AM and another FM, not every expected emission contradicts
/// the label, so the label holds.
#[test]
fn am_on_a_channel_licensed_am_answers_am_demod() {
    let modes = registry(None);
    let am = measured_with(
        "am",
        AIRBAND_HZ,
        9_000,
        vec![licence("6K00A3E", AIRBAND_HZ)],
    );
    let choice = choose_for(&modes, &am, 12_500);
    assert_eq!(choice.row.as_deref(), Some("am_demod"), "{choice:?}");
    assert!(
        !choice.evidence.iter().any(|e| e.starts_with("register: ")),
        "{choice:?}"
    );
    let both = vec![
        licence("11K0F3E", AIRBAND_HZ),
        licence("6K00A3E", AIRBAND_HZ),
    ];
    let mixed = measured_with("am", AIRBAND_HZ, 9_000, both);
    let choice = choose_for(&modes, &mixed, 12_500);
    assert_eq!(choice.row.as_deref(), Some("am_demod"), "{choice:?}");
}

/// A pause (`carrier`) or a weak read (`unknown`) on an FM channel, licensed or planned,
/// answers `fm_demod` where it could not tell, sized as FM: the plan's width on the plan
/// channel, the box's on the licensed one.
#[test]
fn carrier_and_unknown_on_an_fm_channel_answer_fm_demod() {
    let modes = registry(None);
    let channel = cb_channel_hz();
    for label in ["carrier", "unknown"] {
        let cases = [
            (
                measured(label, channel + 300, 50),
                format!("plan: uhf_cb_au is an FM plan (layer 1 read {label})"),
            ),
            (
                measured_with(label, DMR_HZ, 50, vec![licence("11K0F3E", DMR_HZ)]),
                format!("register: 11K0F3E expected here (layer 1 read {label})"),
            ),
        ];
        for (c, line) in cases {
            let choice = choose_for(&modes, &c, 12_500);
            assert_eq!(choice.row.as_deref(), Some("fm_demod"), "{choice:?}");
            assert_eq!(choice.settled, Settled::BestGuess, "{choice:?}");
            assert!(choice.reason.is_none(), "{choice:?}");
            assert_eq!(job(&choice).bandwidth_hz, Hz(12_500), "{choice:?}");
            assert!(choice.evidence.contains(&line), "{line}: {choice:?}");
        }
    }
    // With no expectation they still cannot tell; nor can a signal wider than the
    // channel expected there (30 kHz on a 12.5 kHz channel).
    for label in ["carrier", "unknown"] {
        let choice = choose_for(&modes, &measured(label, DMR_HZ, 50), 12_500);
        assert_eq!(choice.settled, Settled::CannotTell, "{choice:?}");
        let wide = choose_for(&modes, &measured(label, channel + 300, 30_000), 40_000);
        assert_eq!(wide.settled, Settled::CannotTell, "{wide:?}");
    }
}

/// With nothing expected at the centre (no register, no plan, or a licence whose band
/// does not hold the centre) the new weight changes nothing: each of the earlier tests'
/// inputs answers what it did, whatever `contradicted` is.
#[test]
fn with_no_expectation_every_answer_is_as_before() {
    let modes = registry(None);
    let cases: [(&str, u64, u64, u64, Option<&str>); 6] = [
        ("fm", DMR_HZ, 9_000, 12_000, Some("fm_demod")),
        ("am", AIRBAND_HZ, 9_000, 20_000, Some("am_demod")),
        ("ssb", AIRBAND_HZ, 2_200, 20_000, Some("ssb_demod")),
        ("carrier", DMR_HZ, 50, 12_500, None),
        ("unknown", DMR_HZ, 3_000, 3_000, None),
        ("noise", DMR_HZ, 3_000, 12_500, None),
    ];
    // A licence 50 kHz away: its band does not hold the centre.
    let away = |center: u64| vec![licence("11K0F3E", center + 50_000)];
    for (label, center, width, box_hz, row) in cases {
        for allocations in [vec![], away(center)] {
            let c = measured_with(label, center, width, allocations);
            let sel = box_at(center, box_hz);
            let answers: Vec<Choice> = [0.0, 0.15, 1.0]
                .into_iter()
                .map(|k| {
                    let mut w = modes.weights().clone();
                    w.contradicted = k;
                    choose_now(&modes, &w, &c, &sel, "AU", None)
                })
                .collect();
            assert_eq!(answers[0].row.as_deref(), row, "{label}: {:?}", answers[0]);
            assert_eq!(answers[0], answers[1], "{label}");
            assert_eq!(answers[1], answers[2], "{label}");
            assert!(
                !answers[0]
                    .evidence
                    .iter()
                    .any(|e| e.starts_with("register: ") || e.ends_with(&format!("read {label})"))),
                "{label}: {:?}",
                answers[0]
            );
        }
    }
}

/// Identify's ranking (`score_rows`) does not read `contradicted`, and the rows that serve
/// the label rank the same with and without a licence of another letter (one that
/// contradicts the label); so does the choice's `could_prove`. The expectation moves only
/// the demodulators.
#[test]
fn identify_ranks_the_decoders_as_before() {
    use zeta_core::modes::score::{allocation_term, score_rows};
    let modes = registry(None);
    let am = vec![licence("6K00A3E", DMR_HZ)];
    let ranked = |w: &zeta_core::modes::weights::Weights,
                  allocs: &[zeta_core::knowledge::Allocation],
                  serving_fm: bool|
     -> Vec<(String, f32)> {
        score_rows(w, &modes, DMR_HZ, 12_000, "fm", &|_| 0.0, &|row| {
            allocation_term(row, allocs, w)
        })
        .iter()
        .filter(|s| !serving_fm || s.fits_label)
        .map(|s| (s.mode.row.id.0.clone(), s.score))
        .collect()
    };
    let weighed = |k: f32| {
        let mut w = modes.weights().clone();
        w.contradicted = k;
        ranked(&w, &am, false)
    };
    assert!(!weighed(0.0).is_empty());
    assert_eq!(weighed(0.0), weighed(0.15));
    assert_eq!(weighed(0.15), weighed(1.0));
    let w = modes.weights();
    assert!(!ranked(w, &[], true).is_empty());
    assert_eq!(ranked(w, &[], true), ranked(w, &am, true));
    let none = choose_for(&modes, &measured("fm", DMR_HZ, 12_000), 12_500);
    let licensed = choose_for(&modes, &measured_with("fm", DMR_HZ, 12_000, am), 12_500);
    assert!(!none.could_prove.is_empty(), "{none:?}");
    assert_eq!(none.could_prove, licensed.could_prove, "{licensed:?}");
}

/// Placeholder for track A's keyed-over generator (`zeta_dsp::synth::{KeyedOver,
/// keyed_over}`), which has not landed on this branch: the same fields, a carrier keyed
/// up over the whole window only. PLAN 49: delete this module, and the `use` of it in
/// the test below, when the tracks merge.
mod keyed_over_placeholder {
    use num_complex::Complex;
    use zeta_dsp::synth::{awgn, nbfm_modulate};

    pub struct KeyedOver {
        pub rate_hz: u32,
        pub seconds: f32,
        pub deviation_hz: f64,
        pub audio: f32,
        pub on: (f64, f64),
        pub snr_db: f32,
        pub seed: u64,
    }

    pub fn keyed_over(spec: &KeyedOver) -> Vec<Complex<f32>> {
        assert_eq!(
            spec.on,
            (0.0, 1.0),
            "the placeholder keys the whole window only"
        );
        assert_eq!(spec.audio, 0.0, "the placeholder holds a pause only");
        let n = (f64::from(spec.rate_hz) * f64::from(spec.seconds)).round() as usize;
        let audio = vec![0.0_f32; n];
        let mut iq = nbfm_modulate(&audio, spec.rate_hz, spec.rate_hz, spec.deviation_hz, 0.0);
        awgn(&mut iq, spec.snr_db, spec.seed);
        iq
    }
}

/// UAT 48.6 through the operation: a pause in a keyed over (the carrier up, no audio) on
/// a plan channel, in noise, chooses `fm_demod` whatever layer 1 reads it as.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "plan 49: needs track A's keyed-over generator"]
async fn a_keyed_over_in_a_pause_on_a_plan_channel_chooses_fm_demod() {
    use keyed_over_placeholder::{KeyedOver, keyed_over};
    let channel = cb_channel_hz();
    for snr_db in [36.0, 15.0] {
        let dir = tempfile::tempdir().unwrap();
        let iq = keyed_over(&KeyedOver {
            rate_hz: RATE_HZ,
            seconds: 3.0,
            deviation_hz: 2_500.0,
            audio: 0.0,
            on: (0.0, 1.0),
            snr_db,
            seed: 49,
        });
        // The file's centre 300 Hz off the channel's: the carrier sits there.
        let node = node_over(dir.path(), &iq, RATE_HZ, channel + 300).await;
        let a = classify(&node, channel, 12_500).await;
        let c = &a.choice;
        assert_eq!(job(c).kind, "fm_demod", "{snr_db} dB: {c:?}");
        assert_eq!(c.settled, Settled::BestGuess, "{snr_db} dB: {c:?}");
        let label = c.evidence[0]
            .strip_prefix("layer 1: ")
            .and_then(|l| l.split(' ').next())
            .expect("layer 1's line first");
        if !["fm", "fsk"].contains(&label) {
            let line = format!("plan: uhf_cb_au is an FM plan (layer 1 read {label})");
            assert!(c.evidence.contains(&line), "{snr_db} dB: {c:?}");
        }
    }
}
