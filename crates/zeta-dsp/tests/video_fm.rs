//! The `video_fm_demod` stage: the composite rate rule, the discriminator and
//! decimator, de-emphasis parsing and registration.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use num_complex::Complex;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::port::IqBlock;
use zeta_core::stage::{Stage, StageContext, StageRegistry};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};
use zeta_dsp::synth::{composite_test_card, wfm_modulate_composite};
use zeta_dsp::video_fm::VideoFmDemodStage;
use zeta_dsp::video_fm::{VideoFmDemod, VideoFmParams, VideoStandard, composite_rate_hz};

fn params(deviation_hz: u64, deemphasis: &str) -> VideoFmParams {
    VideoFmParams {
        deviation_hz,
        deemphasis: deemphasis.into(),
        composite_max_hz: 16_000_000,
        standard: None,
    }
}

/// `n` samples of a unit carrier 1 MHz above centre at `rate`.
fn carrier(n: usize, rate: f64) -> Vec<Complex<f32>> {
    (0..n)
        .map(|i| Complex::from_polar(1.0, (std::f64::consts::TAU * 1e6 * i as f64 / rate) as f32))
        .collect()
}

#[test]
fn composite_rate_is_the_largest_divisor_under_the_cap() {
    assert_eq!(composite_rate_hz(30_720_000, 16_000_000), Some(15_360_000));
    assert_eq!(composite_rate_hz(20_480_000, 16_000_000), Some(10_240_000));
    assert_eq!(composite_rate_hz(8_000_000, 16_000_000), Some(8_000_000));
    assert_eq!(composite_rate_hz(61_440_000, 16_000_000), Some(15_360_000));
    assert_eq!(composite_rate_hz(0, 16_000_000), None);
}

#[test]
fn a_modulated_card_demodulates_to_the_card() {
    let rate = 8_000_000;
    let card = composite_test_card(VideoStandard::Pal, rate, 2);
    let iq = wfm_modulate_composite(&card, rate, 1_500_000.0);
    let mut d = VideoFmDemod::new(&params(1_500_000, "none")).unwrap();
    let (out, r) = d.process(&iq, u64::from(rate)).unwrap();
    assert_eq!(r, 8_000_000);
    assert_eq!(out.len(), card.len());
    // The discriminator measures the phase step from sample n-1 to n, which the
    // modulator set from card[n-1]: one sample of delay.
    let err: f64 = (100..card.len())
        .map(|n| f64::from(out[n] - card[n - 1]).powi(2))
        .sum::<f64>()
        / (card.len() - 100) as f64;
    let rms = err.sqrt();
    assert!(rms < 0.02, "rms {rms}");
    // Unclamped: the sync tip comes out at -0.3 V.
    let lo = out[100..].iter().copied().fold(f32::MAX, f32::min);
    assert!((lo + 0.3).abs() < 0.02, "min {lo}");
}

#[test]
fn a_rate_change_rebuilds_the_decimator() {
    let mut d = VideoFmDemod::new(&params(8_000_000, "ccir405")).unwrap();
    let a = carrier(30_720, 30_720_000.0);
    let (out, r) = d.process(&a, 30_720_000).unwrap();
    assert_eq!(r, 15_360_000);
    assert_eq!(out.len(), a.len() / 2);
    let b = carrier(20_480, 20_480_000.0);
    let (out, r) = d.process(&b, 20_480_000).unwrap();
    assert_eq!(r, 10_240_000);
    assert_eq!(out.len(), b.len() / 2);
    assert!(out.iter().all(|v| v.is_finite()));
}

#[test]
fn odd_blocks_demodulate_as_the_whole() {
    let rate = 30_720_000;
    let card = composite_test_card(VideoStandard::Pal, rate, 1);
    let iq = wfm_modulate_composite(&card[..100_000], rate, 8_000_000.0);
    let mut whole = VideoFmDemod::new(&params(8_000_000, "ccir405")).unwrap();
    let (want, _) = whole.process(&iq, u64::from(rate)).unwrap();
    let mut parts = VideoFmDemod::new(&params(8_000_000, "ccir405")).unwrap();
    let mut got = Vec::new();
    for chunk in iq.chunks(4_097) {
        got.extend(parts.process(chunk, u64::from(rate)).unwrap().0);
    }
    assert_eq!(got.len(), want.len());
    let worst = got
        .iter()
        .zip(&want)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(worst < 1e-5, "worst {worst}");
}

#[test]
fn deemphasis_tau_is_accepted_and_garbage_is_not() {
    assert!(VideoFmDemod::new(&params(8_000_000, "0.5")).is_ok());
    assert!(VideoFmDemod::new(&params(8_000_000, "ccir405")).is_ok());
    match VideoFmDemod::new(&params(8_000_000, "loud")) {
        Err(e) => assert!(e.contains("deemphasis"), "{e}"),
        Ok(_) => panic!("garbage deemphasis accepted"),
    }
}

#[test]
fn the_stage_spec_is_registered() {
    let mut reg = StageRegistry::new();
    zeta_dsp::register_all(&mut reg);
    let spec = reg
        .spec("video_fm_demod")
        .expect("video_fm_demod registered");
    assert_eq!(spec.inputs, vec![StreamKind::Iq]);
    assert_eq!(spec.outputs, vec![StreamKind::Audio]);
}

#[tokio::test]
async fn the_stage_notes_its_composite_rate_and_drops() {
    let notes = Arc::new(Mutex::new(Vec::new()));
    let store = notes.clone();
    let ctx = StageContext::new(SourceId("s".into()))
        .with_note(Arc::new(move |v| store.lock().unwrap().push(v)));
    let prov = Provenance {
        source: SourceId("s".into()),
        job: None,
        channel: None,
        span: Span {
            start_ns: Timestamp(0),
            end_ns: Timestamp(1_000_000),
        },
    };
    let bus = Arc::new(StreamBus::new(16));
    let out = bus.register(StreamKind::Audio, prov.clone());
    let (tx, rx) = broadcast::channel(16);
    let stage_bus = bus.clone();
    let h = tokio::spawn(async move {
        VideoFmDemodStage::new()
            .run(
                serde_json::json!({}),
                vec![rx],
                vec![out],
                stage_bus,
                CancellationToken::new(),
                ctx,
            )
            .await
    });
    let rate = 8_000_000.0;
    tx.send(Frame::new(
        StreamId::new(),
        prov,
        Payload::Iq(IqBlock {
            t0: Timestamp(0),
            sample_rate_hz: Hz(rate as u64),
            center_hz: Hz(0),
            samples: carrier(8_000, rate).into(),
        }),
    ))
    .unwrap();
    drop(tx);
    let r = tokio::time::timeout(Duration::from_secs(5), h)
        .await
        .expect("stage ended")
        .unwrap();
    assert!(r.is_ok(), "{r:?}");
    let notes = notes.lock().unwrap();
    assert_eq!(
        notes.as_slice(),
        [serde_json::json!({"video_fm": {"composite_rate_hz": 8_000_000, "dropped": 0}})]
    );
}
