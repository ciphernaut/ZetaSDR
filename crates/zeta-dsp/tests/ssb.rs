//! SSB demodulation: Weaver-style shift, complex low-pass, real part, 48 kHz audio, AGC.
#![cfg(feature = "test-fixtures")]

use num_complex::Complex;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::ops::Sideband;
use zeta_core::stage::{Stage, StageError};
use zeta_core::stream::StreamBus;
use zeta_core::types::StreamId;
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
