//! AM demodulation: magnitude, DC removal, 5 kHz low-pass, 48 kHz audio, slow AGC.
#![cfg(feature = "test-fixtures")]

use num_complex::Complex;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::ops::Sideband;
use zeta_core::port::IqBlock;
use zeta_core::stage::{Stage, StageError};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};
use zeta_dsp::am::{AmDemod, AmDemodStage, AmParams, SsbDemod, SsbParams, am_demod};
use zeta_dsp::fft::{Window, power_spectrum_db};
use zeta_dsp::fixtures::{am_modulate, awgn, ssb_modulate, tone};

#[test]
fn am_demod_is_the_magnitude() {
    let m = am_demod(&[Complex::new(3.0, 4.0), Complex::new(0.0, -2.0)]);
    assert_eq!(m, vec![5.0, 2.0]);
}

#[test]
fn am_demod_recovers_tone() {
    let audio = tone(48_000, 48_000, 1_000.0, 1.0);
    let iq = am_modulate(&audio, 48_000, 48_000, 0.5, 0.0);
    let mut d = AmDemod::new(&AmParams {
        audio_rate_hz: 48_000,
        squelch_db: None,
        squelch: true,
    })
    .unwrap();
    let mut out = Vec::new();
    for b in iq.chunks(480) {
        out.extend(d.process(b, 48_000).unwrap());
    }
    assert!(out.len().abs_diff(48_000) <= 2, "len {}", out.len());
    assert!(out.iter().all(|s| s.abs() <= 1.0));
    let tail = &out[9_600..9_600 + 38_400];
    let rms = (tail.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / tail.len() as f64).sqrt();
    assert!((rms - 0.2).abs() <= 0.05, "rms {rms}");
    // 4800-point spectrum: 10 Hz bins, 1 kHz on bin 100 above centre.
    let cx: Vec<Complex<f32>> = tail[..4_800]
        .iter()
        .map(|&s| Complex::new(s, 0.0))
        .collect();
    let db = power_spectrum_db(&cx, 4_800, Window::Hann);
    let (peak_bin, _) = db
        .iter()
        .enumerate()
        .skip(2_400)
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .unwrap();
    assert_eq!(peak_bin, 2_400 + 100, "peak at bin {peak_bin}");
}

#[tokio::test]
async fn am_stage_rejects_zero_rate() {
    let bus = Arc::new(StreamBus::new(4));
    let (_tx, rx) = broadcast::channel(4);
    let r = AmDemodStage::new()
        .run(
            serde_json::json!({"audio_rate_hz": 0}),
            vec![rx],
            vec![StreamId::new()],
            bus,
            CancellationToken::new(),
            zeta_core::stage::StageContext::new(zeta_core::types::SourceId("s".into())),
        )
        .await;
    match r {
        Err(StageError::Failed(m)) => assert!(m.contains("audio_rate_hz"), "{m}"),
        r => panic!("expected Failed, got {r:?}"),
    }
}

#[test]
fn am_and_ssb_survive_a_nan_sample() {
    let audio = tone(48_000, 48_000, 1_000.0, 1.0);
    let am = am_modulate(&audio, 48_000, 48_000, 0.5, 0.0);
    let ssb = ssb_modulate(&audio, 48_000, 48_000, true, 0.0);
    let mut am_d = AmDemod::new(&AmParams {
        audio_rate_hz: 48_000,
        squelch_db: None,
        squelch: true,
    })
    .unwrap();
    let mut ssb_d = SsbDemod::new(&SsbParams {
        sideband: Sideband::Usb,
        audio_rate_hz: 48_000,
        audio_bandwidth_hz: 2_700,
    })
    .unwrap();
    type Run<'a> = Box<dyn FnMut(&[Complex<f32>]) -> Vec<f32> + 'a>;
    let runs: [(&str, &[Complex<f32>], Run); 2] = [
        ("am", &am, Box::new(|b| am_d.process(b, 48_000).unwrap())),
        ("ssb", &ssb, Box::new(|b| ssb_d.process(b, 48_000).unwrap())),
    ];
    for (what, iq, mut demod) in runs {
        // 480-sample blocks; one NaN in the middle of block 50.
        let mut iq = iq.to_vec();
        iq[50 * 480 + 240] = Complex::new(f32::NAN, f32::NAN);
        let blocks: Vec<Vec<f32>> = iq.chunks(480).map(&mut demod).collect();
        for (k, out) in blocks.iter().enumerate().skip(50).take(2) {
            assert!(!out.is_empty(), "{what} block {k} empty");
            assert!(
                out.iter().all(|s| s.is_finite() && s.abs() <= 1.0),
                "{what} block {k}: {out:?}"
            );
        }
    }
}

/// The audio frame count and the `squelch` events of an `am_demod` stage fed `blocks`.
async fn am_stage_run(blocks: Vec<Vec<Complex<f32>>>) -> (usize, Vec<serde_json::Value>) {
    let prov = Provenance {
        source: SourceId("s".into()),
        job: None,
        channel: None,
        span: Span {
            start_ns: Timestamp(0),
            end_ns: Timestamp(1),
        },
    };
    let bus = Arc::new(StreamBus::new(1_024));
    let (out, ev) = (
        bus.register(StreamKind::Audio, prov.clone()),
        bus.register(StreamKind::Event, prov.clone()),
    );
    let (mut rx_out, mut rx_ev) = (bus.subscribe(&out).unwrap(), bus.subscribe(&ev).unwrap());
    let (tx, rx) = broadcast::channel(1_024);
    for (i, b) in blocks.into_iter().enumerate() {
        let iq = IqBlock {
            t0: Timestamp(i as u64 * 20_000_000),
            sample_rate_hz: Hz(48_000),
            center_hz: Hz(100_000_000),
            samples: b.into(),
        };
        tx.send(Frame::new(StreamId::new(), prov.clone(), Payload::Iq(iq)))
            .unwrap();
    }
    drop(tx);
    AmDemodStage::new()
        .run(
            serde_json::json!({"audio_rate_hz": 48_000}),
            vec![rx],
            vec![out, ev],
            bus,
            CancellationToken::new(),
            zeta_core::stage::StageContext::new(SourceId("s".into())),
        )
        .await
        .unwrap();
    let mut audio = 0;
    while rx_out.try_recv().is_ok() {
        audio += 1;
    }
    let mut events = Vec::new();
    while let Ok(f) = rx_ev.try_recv() {
        let Payload::Event(v) = f.payload else {
            panic!("expected an event")
        };
        events.push(v);
    }
    (audio, events)
}

#[tokio::test]
async fn am_squelch_closes_on_noise_and_opens_on_a_carrier() {
    let noise = |seed: u64| {
        let mut x = vec![Complex::new(0.0f32, 0.0); 960];
        awgn(&mut x, 60.0, seed);
        x
    };
    let (audio, events) = am_stage_run((0..50).map(noise).collect()).await;
    assert_eq!(audio, 0);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["protocol"], "squelch");
    assert_eq!(events[0]["open"], false);
    // 2 s of noise, then a -40 dBFS AM tone.
    let am = am_modulate(&tone(48_000, 9_600, 1_000.0, 1.0), 48_000, 48_000, 0.3, 0.0);
    let mut blocks: Vec<_> = (0..100).map(noise).collect();
    blocks.extend(am.chunks(960).map(|c| c.iter().map(|x| x * 0.01).collect()));
    let (audio, events) = am_stage_run(blocks).await;
    assert!(audio >= 9, "{audio} audio frames");
    let opens: Vec<_> = events.iter().map(|e| e["open"].clone()).collect();
    assert_eq!(opens, [false, true], "{events:?}");
}
