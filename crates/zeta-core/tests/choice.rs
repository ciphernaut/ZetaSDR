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
