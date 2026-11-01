//! SSB demodulation: Weaver-style shift, complex low-pass, real part, 48 kHz audio, AGC.
#![cfg(feature = "test-fixtures")]

use num_complex::Complex;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::ops::Sideband;
use zeta_core::stage::{Stage, StageError};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, StreamId};
use zeta_dsp::am::{SsbDemod, SsbDemodStage, SsbParams};
use zeta_dsp::fft::{Window, power_spectrum_db};
use zeta_dsp::fixtures::{ssb_modulate, tone};

/// Task 3's two-tone audio: 700 Hz at 0.7 plus 1900 Hz at 0.3, one second at 48 kHz.
fn two_tones() -> Vec<f32> {
    tone(48_000, 48_000, 700.0, 0.7)
        .iter()
        .zip(tone(48_000, 48_000, 1_900.0, 0.3))
        .map(|(a, b)| a + b)
        .collect()
}

fn demod(iq: &[Complex<f32>], sideband: Sideband) -> Vec<f32> {
    let mut d = SsbDemod::new(&SsbParams {
        sideband,
        audio_rate_hz: 48_000,
        audio_bandwidth_hz: 2_800,
        denoise: false,
    })
    .unwrap();
    let mut out = Vec::new();
    for b in iq.chunks(480) {
        out.extend(d.process(b, 48_000).unwrap());
    }
    assert!(out.len().abs_diff(48_000) <= 2, "len {}", out.len());
    assert!(out.iter().all(|s| s.abs() <= 1.0));
    out
}

/// Peak level in dB within ±2 bins of `freq_hz` in a 4800-point (10 Hz bin)
/// spectrum of `audio[9600..14400]`.
fn level_db(audio: &[f32], freq_hz: f64) -> f32 {
    let cx: Vec<Complex<f32>> = audio[9_600..14_400]
        .iter()
        .map(|&s| Complex::new(s, 0.0))
        .collect();
    let db = power_spectrum_db(&cx, 4_800, Window::Hann);
    let bin = 2_400 + (freq_hz / 10.0).round() as usize;
    db[bin - 2..=bin + 2]
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max)
}

#[test]
fn ssb_demod_recovers_two_tones_usb_and_lsb() {
    let audio = two_tones();
    let expected = 20.0 * (0.7f32 / 0.3).log10();
    for (usb, sideband, wrong) in [
        (true, Sideband::Usb, Sideband::Lsb),
        (false, Sideband::Lsb, Sideband::Usb),
    ] {
        let iq = ssb_modulate(&audio, 48_000, 48_000, usb, 0.0);
        let out = demod(&iq, sideband);
        let (lo, hi) = (level_db(&out, 700.0), level_db(&out, 1_900.0));
        assert!(
            ((lo - hi) - expected).abs() <= 2.0,
            "{sideband:?}: 700 Hz {lo} dB, 1900 Hz {hi} dB, want a {expected} dB difference"
        );

        // The wrong setting: a 1200 Hz reference at 0.7 on the opposite
        // sideband sets the level (the AGC would otherwise lift the residue);
        // both tones must sit at least 30 dB below where they would be.
        let reference = tone(48_000, 48_000, 1_200.0, 0.7);
        let other = ssb_modulate(&reference, 48_000, 48_000, !usb, 0.0);
        let mixed: Vec<Complex<f32>> = iq.iter().zip(&other).map(|(a, b)| a + b).collect();
        let out = demod(&mixed, wrong);
        let r = level_db(&out, 1_200.0);
        let (lo, hi) = (level_db(&out, 700.0), level_db(&out, 1_900.0));
        assert!(
            r - lo >= 30.0,
            "{wrong:?}: 700 Hz {lo} dB vs reference {r} dB"
        );
        assert!(
            r - hi >= 30.0 + expected,
            "{wrong:?}: 1900 Hz {hi} dB vs reference {r} dB"
        );
    }
}

#[tokio::test]
async fn ssb_stage_rejects_bad_bandwidth() {
    for bw in [0u64, 499, 6_001] {
        let bus = Arc::new(StreamBus::new(4));
        let (_tx, rx) = broadcast::channel(4);
        let r = SsbDemodStage::new()
            .run(
                serde_json::json!({"sideband": "usb", "audio_rate_hz": 48_000, "audio_bandwidth_hz": bw}),
                vec![rx],
                vec![StreamId::new()],
                bus,
                CancellationToken::new(),
                zeta_core::stage::StageContext::new(zeta_core::types::SourceId("s".into())),
            )
            .await;
        match r {
            Err(StageError::Failed(m)) => assert!(m.contains("audio_bandwidth_hz"), "{m}"),
            r => panic!("expected Failed for {bw}, got {r:?}"),
        }
    }
}

#[tokio::test]
async fn ssb_denoise_frames_the_audio_and_dates_held_samples_from_their_block() {
    use zeta_core::port::IqBlock;
    use zeta_core::types::{Provenance, SourceId, Span, StreamKind, Timestamp};
    let prov = || Provenance {
        source: SourceId("s".into()),
        job: None,
        channel: None,
        span: Span {
            start_ns: Timestamp(0),
            end_ns: Timestamp(1),
        },
    };
    let bus = Arc::new(StreamBus::new(8));
    let out = bus.register(StreamKind::Audio, prov());
    let mut rx = bus.subscribe(&out).unwrap();
    let (tx, rx_in) = broadcast::channel(8);
    tokio::spawn(async move {
        SsbDemodStage::new()
            .run(
                serde_json::json!({"sideband": "usb", "audio_rate_hz": 48_000, "audio_bandwidth_hz": 2_800, "denoise": true}),
                vec![rx_in],
                vec![out],
                bus,
                CancellationToken::new(),
                zeta_core::stage::StageContext::new(zeta_core::types::SourceId("s".into())),
            )
            .await
    });
    // A 1 kHz tone in the upper sideband at baseband.
    let n = 4_800 + 100 + 380;
    let iq: Vec<Complex<f32>> = (0..n)
        .map(|i| {
            Complex::from_polar(
                0.5,
                2.0 * std::f64::consts::PI as f32 * 1_000.0 * i as f32 / 48_000.0,
            )
        })
        .collect();
    let at = |s: &[Complex<f32>], t0: u64| {
        Frame::new(
            StreamId::new(),
            prov(),
            Payload::Iq(IqBlock {
                t0: Timestamp(t0),
                sample_rate_hz: Hz(48_000),
                center_hz: Hz(7_100_000),
                samples: s.to_vec().into(),
            }),
        )
    };
    tx.send(at(&iq[..4_800], 1_000_000_000)).unwrap();
    let f = rx.recv().await.unwrap();
    let Payload::Audio { samples, .. } = &f.payload else {
        panic!()
    };
    assert_eq!(samples.len(), 4_800);
    assert_eq!(f.provenance.span.start_ns, Timestamp(1_000_000_000));
    tx.send(at(&iq[4_800..4_900], 1_100_000_000)).unwrap();
    tx.send(at(&iq[4_900..], 1_102_083_333)).unwrap();
    let f = rx.recv().await.unwrap();
    let Payload::Audio { samples, .. } = &f.payload else {
        panic!()
    };
    assert_eq!(samples.len(), 480);
    assert_eq!(f.provenance.span.start_ns, Timestamp(1_100_000_000));
    assert_eq!(f.provenance.span.end_ns, Timestamp(1_110_000_000));
}
