//! Layer-2 decode-to-identify: run the APRS decoder and report only what
//! passes the AX.25 FCS (design 6.2).

use async_trait::async_trait;
use num_complex::Complex;
use zeta_core::classify::{Band, Classifier, FeatureRecord, Label};
use zeta_core::ops::channel_rate_hz;
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
/// Lowest block rate decoded (a 12 kHz channel carries AFSK1200).
const MIN_AUDIO_HZ: u64 = 9_600;
/// FM deviation assumed for APRS.
const DEVIATION_HZ: u64 = 3_000;

/// Decodes APRS from the requested band, at least [`MIN_BW_HZ`] wide and
/// extracted at [`channel_rate_hz`]; for the whole block, from its strongest
/// occupied band instead. Upstream features are not used.
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
        _center_hz: Hz,
        band: Band,
        _features: Option<&FeatureRecord>,
    ) -> Vec<Label> {
        let rate = sample_rate_hz.0;
        if iq.len() < MIN_SAMPLES || rate < MIN_AUDIO_HZ {
            return Vec::new();
        }
        let (offset, bw) = if band.is_whole(sample_rate_hz) {
            // Measured against a block centre of `rate`, so the offset of a
            // band below centre does not clamp at 0.
            let f = measure(iq, rate, rate);
            (f.center_hz.0 as i64 - rate as i64, f.bandwidth_hz.0)
        } else {
            (band.offset_hz, band.bandwidth_hz.0)
        };
        let out_rate = channel_rate_hz(rate, bw.max(MIN_BW_HZ));
        // The channel filter refuses a band wider than its output rate.
        let bw = bw.max(MIN_BW_HZ).min(out_rate);
        let Ok((chan, out_rate)) = extract_offset(iq, rate, offset, bw, out_rate) else {
            return Vec::new();
        };
        let mut prev = Complex::new(0.0, 0.0);
        let audio = nbfm_demod(&chan, out_rate, DEVIATION_HZ, &mut prev);
        let Some(mut demod) = Afsk1200Demod::new(out_rate as u32) else {
            return Vec::new();
        };
        let bits = demod.feed(&audio);
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
        let labels = DecodeToIdentify
            .classify(
                &iq,
                Hz(48_000),
                Hz(144_800_000),
                Band::whole(Hz(48_000)),
                None,
            )
            .await;
        assert_eq!(labels.len(), 1, "{labels:?}");
        assert_eq!(labels[0].layer, 2);
        assert_eq!(labels[0].name, "APRS");
        assert_eq!(labels[0].confidence, 1.0);
        assert!(labels[0].evidence.contains("VK3ABC>APRS"), "{labels:?}");
    }

    async fn aprs_at(offset: f64) {
        let iq = aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, offset);
        let labels = DecodeToIdentify
            .classify(
                &iq,
                Hz(48_000),
                Hz(144_800_000),
                Band::whole(Hz(48_000)),
                None,
            )
            .await;
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
                .classify(
                    &iq,
                    Hz(48_000),
                    Hz(144_800_000),
                    Band::whole(Hz(48_000)),
                    None
                )
                .await
                .is_empty()
        );
        assert!(
            DecodeToIdentify
                .classify(
                    &iq[..100],
                    Hz(48_000),
                    Hz(144_800_000),
                    Band::whole(Hz(48_000)),
                    None
                )
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn identifies_aprs_in_requested_band_only() {
        // APRS 12 kHz above centre, an FM tone 10 dB stronger 12 kHz below it.
        let aprs = aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 12_000.0);
        let audio = tone(48_000, aprs.len(), 1_000.0, 1.0);
        let fm = nbfm_modulate(&audio, 48_000, 48_000, 2_500.0, -12_000.0);
        let iq: Vec<_> = aprs.iter().zip(&fm).map(|(a, t)| a + t * 3.16).collect();
        let band = |offset_hz| Band {
            offset_hz,
            bandwidth_hz: Hz(12_500),
        };
        let above = DecodeToIdentify
            .classify(&iq, Hz(48_000), Hz(144_800_000), band(12_000), None)
            .await;
        assert_eq!(above.len(), 1, "{above:?}");
        assert_eq!(above[0].name, "APRS");
        let below = DecodeToIdentify
            .classify(&iq, Hz(48_000), Hz(144_800_000), band(-12_000), None)
            .await;
        assert!(below.is_empty(), "{below:?}");
    }
}
