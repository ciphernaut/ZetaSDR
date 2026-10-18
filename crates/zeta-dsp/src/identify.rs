//! Layer-2 decode-to-identify: run the APRS decoder and report only what
//! passes the AX.25 FCS (design 6.2).

use async_trait::async_trait;
use num_complex::Complex;
use zeta_core::classify::{Classifier, FeatureRecord, Label};
use zeta_core::types::Hz;

use crate::afsk::Afsk1200Demod;
use crate::ax25::{HdlcDeframer, parse_ax25_ui};
use crate::channel::extract_offset;
use crate::measure::measure;
use crate::nbfm::nbfm_demod;

/// Shortest block worth decoding.
const MIN_SAMPLES: usize = 1024;
/// Narrowest channel extracted.
const MIN_BW_HZ: u64 = 12_500;
/// Preferred audio rate and the lowest acceptable fallback.
const AUDIO_HZ: u64 = 48_000;
const MIN_AUDIO_HZ: u64 = 24_000;
/// FM deviation assumed for APRS.
const DEVIATION_HZ: u64 = 3_000;

/// Output rate for `rate`: 48 kHz when it divides, else the largest integer
/// divisor of `rate` in `[24 kHz, 48 kHz]`, else `rate` itself.
fn audio_rate(rate: u64) -> u64 {
    if rate.is_multiple_of(AUDIO_HZ) {
        return AUDIO_HZ;
    }
    (MIN_AUDIO_HZ..AUDIO_HZ)
        .rev()
        .find(|r| rate.is_multiple_of(*r))
        .unwrap_or(rate)
}

/// Decodes APRS from the strongest occupied band of the block.
pub struct DecodeToIdentify;

#[async_trait]
impl Classifier for DecodeToIdentify {
    fn name(&self) -> &str {
        "decode-to-identify"
    }
    fn layer(&self) -> u8 {
        2
    }
    async fn classify(
        &self,
        iq: &[Complex<f32>],
        sample_rate_hz: Hz,
        features: Option<&FeatureRecord>,
    ) -> Vec<Label> {
        let rate = sample_rate_hz.0;
        if iq.len() < MIN_SAMPLES || rate < MIN_AUDIO_HZ {
            return Vec::new();
        }
        let owned;
        let f = match features {
            Some(f) => f,
            None => {
                owned = measure(iq, rate, rate / 2);
                &owned
            }
        };
        let half = (rate / 2) as i64;
        // `center_hz` is measured with the block centre at rate/2, so negative offsets survive.
        let offset = (f.center_hz.0 as i64 - half).clamp(-half, half);
        let bw = f.bandwidth_hz.0.max(MIN_BW_HZ).min(rate);
        let Ok((chan, out_rate)) = extract_offset(iq, rate, offset, bw, audio_rate(rate)) else {
            return Vec::new();
        };
        let mut prev = Complex::new(0.0, 0.0);
        let audio = nbfm_demod(&chan, out_rate, DEVIATION_HZ, &mut prev);
        let bits = Afsk1200Demod::new(out_rate as u32).feed(&audio);
        let frames: Vec<_> = HdlcDeframer::new()
            .feed(&bits)
            .iter()
            .filter_map(|b| parse_ax25_ui(b))
            .collect();
        let Some(first) = frames.first() else {
            return Vec::new();
        };
        vec![Label {
            layer: 2,
            name: "APRS".into(),
            confidence: 1.0,
            evidence: format!(
                "{} AX.25 frames with valid FCS; first {}>{}",
                frames.len(),
                first.src,
                first.dst
            ),
        }]
    }
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::fixtures::{aprs_packet_iq, nbfm_modulate, tone};

    #[tokio::test]
    async fn identifies_aprs_in_baseband_iq() {
        let iq = aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 0.0);
        let labels = DecodeToIdentify.classify(&iq, Hz(48_000), None).await;
        assert_eq!(labels.len(), 1, "{labels:?}");
        assert_eq!(labels[0].layer, 2);
        assert_eq!(labels[0].name, "APRS");
        assert_eq!(labels[0].confidence, 1.0);
        assert!(labels[0].evidence.contains("VK3ABC>APRS"), "{labels:?}");
    }

    async fn aprs_at(offset: f64) {
        let iq = aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, offset);
        let labels = DecodeToIdentify.classify(&iq, Hz(48_000), None).await;
        assert_eq!(labels.len(), 1, "offset {offset}: {labels:?}");
        assert_eq!(labels[0].name, "APRS");
    }

    #[tokio::test]
    async fn identifies_aprs_at_positive_offset() {
        aprs_at(5000.0).await;
    }

    #[tokio::test]
    async fn identifies_aprs_at_negative_offset() {
        aprs_at(-5000.0).await;
    }

    #[tokio::test]
    async fn no_label_for_fm_tone() {
        let audio = tone(48_000, 48_000, 1_000.0, 1.0);
        let iq = nbfm_modulate(&audio, 48_000, 48_000, 2_500.0, 0.0);
        assert!(
            DecodeToIdentify
                .classify(&iq, Hz(48_000), None)
                .await
                .is_empty()
        );
        assert!(
            DecodeToIdentify
                .classify(&iq[..100], Hz(48_000), None)
                .await
                .is_empty()
        );
    }
}
