//! Layer-2 decode-to-identify: run each protocol decoder (APRS, POCSAG, ACARS, RDS) over
//! the channel and report only what passes its own check, the AX.25 FCS or
//! the POCSAG BCH code (design 6.2).

use async_trait::async_trait;
use num_complex::Complex;
use zeta_core::classify::{Band, Classifier, FeatureRecord, Label};
use zeta_core::ops::channel_rate_hz;
use zeta_core::types::Hz;

use crate::acars::decode_acars_iq;
use crate::afsk::Afsk1200Demod;
use crate::ax25::{HdlcDeframer, parse_ax25_ui};
use crate::channel::extract_offset;
use crate::measure::measure;
use crate::nbfm::nbfm_demod;
use crate::pocsag::{BAUDS, decode_pocsag_iq};
use crate::rds::decode_rds_iq;

/// A requested band at least this wide is broadcast FM: only RDS is tried on it.
const RDS_MIN_BW_HZ: u64 = 150_000;

/// The same test on a whole-block band, whose width is measured: the
/// occupied bandwidth of a 75 kHz deviation station comes out near 139 kHz,
/// so the bar is lower than for a requested band.
const RDS_MIN_MEASURED_BW_HZ: u64 = 100_000;

/// Shortest block worth decoding.
const MIN_SAMPLES: usize = 1024;
/// Narrowest channel extracted.
const MIN_BW_HZ: u64 = 12_500;
/// Lowest block rate decoded (a 12 kHz channel carries AFSK1200).
const MIN_AUDIO_HZ: u64 = 9_600;
/// FM deviation assumed for APRS.
const DEVIATION_HZ: u64 = 3_000;

/// Decodes APRS, POCSAG, ACARS and RDS from the requested band, at least [`MIN_BW_HZ`] wide and
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
        let whole = band.is_whole(sample_rate_hz);
        let (offset, bw) = if whole {
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
        let wide_bw = if whole {
            RDS_MIN_MEASURED_BW_HZ
        } else {
            RDS_MIN_BW_HZ
        };
        let attempts: &[Attempt] = if bw >= wide_bw {
            &WIDE_ATTEMPTS
        } else {
            &NARROW_ATTEMPTS
        };
        attempts
            .iter()
            .filter_map(|attempt| attempt(&chan, out_rate))
            .collect()
    }
}

/// One decoder tried over the extracted channel (IQ at the given rate).
type Attempt = fn(&[Complex<f32>], u64) -> Option<Label>;

/// Decoders tried on a narrow channel, all of them; labels come back in this order.
const NARROW_ATTEMPTS: [Attempt; 3] = [aprs_attempt, pocsag_attempt, acars_attempt];

/// Decoders tried on a channel at least [`RDS_MIN_BW_HZ`] wide.
const WIDE_ATTEMPTS: [Attempt; 1] = [rds_attempt];

fn rds_attempt(chan: &[Complex<f32>], rate: u64) -> Option<Label> {
    // The first event is usually the PI alone; prefer one that has the PS.
    let events = decode_rds_iq(chan, rate);
    let first = events.iter().find(|e| e.ps.is_some()).or(events.first())?;
    let ps = first.ps.clone().unwrap_or_default();
    Some(Label {
        layer: 2,
        name: "RDS".into(),
        confidence: 1.0,
        evidence: format!("PI {}, PS {ps:?}", first.pi),
    })
}

fn acars_attempt(chan: &[Complex<f32>], rate: u64) -> Option<Label> {
    let blocks = decode_acars_iq(chan, rate);
    let first = blocks.first()?;
    Some(Label {
        layer: 2,
        name: "ACARS".into(),
        confidence: 1.0,
        evidence: format!(
            "{} blocks CRC-clean; first {} label {}",
            blocks.len(),
            first.reg,
            first.label
        ),
    })
}

fn aprs_attempt(chan: &[Complex<f32>], rate: u64) -> Option<Label> {
    let mut prev = Complex::new(0.0, 0.0);
    let audio = nbfm_demod(chan, rate, DEVIATION_HZ, &mut prev);
    let mut demod = Afsk1200Demod::new(rate as u32)?;
    let bits = demod.feed(&audio);
    let frames: Vec<_> = HdlcDeframer::new()
        .feed(&bits)
        .iter()
        .filter_map(|b| parse_ax25_ui(b))
        .collect();
    let first = frames.first()?;
    Some(Label {
        layer: 2,
        name: "APRS".into(),
        confidence: 1.0,
        evidence: format!(
            "{} AX.25 frames with valid FCS; first {}>{}",
            frames.len(),
            first.src,
            first.dst
        ),
    })
}

fn pocsag_attempt(chan: &[Complex<f32>], rate: u64) -> Option<Label> {
    let messages = decode_pocsag_iq(chan, rate);
    // The baud with the most messages is the one the transmitter used.
    let baud = BAUDS
        .iter()
        .copied()
        .max_by_key(|&b| messages.iter().filter(|m| m.baud == b).count())?;
    let at_baud: Vec<_> = messages.iter().filter(|m| m.baud == baud).collect();
    let first = at_baud.first()?;
    Some(Label {
        layer: 2,
        name: "POCSAG".into(),
        confidence: 1.0,
        evidence: format!(
            "{} messages BCH-clean at {} baud; first address {}",
            at_baud.len(),
            baud,
            first.address
        ),
    })
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::fixtures::{
        acars_packet_iq, aprs_packet_iq, nbfm_modulate, pocsag_packet_iq, rds_station_iq, tone,
    };

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

    #[tokio::test]
    async fn identifies_pocsag_in_baseband_iq() {
        let iq = pocsag_packet_iq(1234567, 3, "HELLO ZETA", 1200, 48_000, 0.0);
        let labels = DecodeToIdentify
            .classify(
                &iq,
                Hz(48_000),
                Hz(152_000_000),
                Band::whole(Hz(48_000)),
                None,
            )
            .await;
        assert_eq!(labels.len(), 1, "{labels:?}");
        assert_eq!(labels[0].layer, 2);
        assert_eq!(labels[0].name, "POCSAG");
        assert_eq!(labels[0].confidence, 1.0);
        assert!(labels[0].evidence.contains("1234567"), "{labels:?}");
        assert!(labels[0].evidence.contains("1200 baud"), "{labels:?}");
    }

    #[tokio::test]
    async fn identifies_rds_in_baseband_iq() {
        let iq = rds_station_iq(0x2AB1, "ZETA FM ", "HELLO FROM ZETASDR", 240_000);
        let labels = DecodeToIdentify
            .classify(
                &iq,
                Hz(240_000),
                Hz(100_000_000),
                Band::whole(Hz(240_000)),
                None,
            )
            .await;
        assert_eq!(labels.len(), 1, "{labels:?}");
        assert_eq!(labels[0].layer, 2);
        assert_eq!(labels[0].name, "RDS");
        assert_eq!(labels[0].confidence, 1.0);
        assert!(labels[0].evidence.contains("PI 2AB1"), "{labels:?}");
        assert!(labels[0].evidence.contains("\"ZETA FM \""), "{labels:?}");
    }

    #[tokio::test]
    async fn aprs_fixture_has_no_rds_label() {
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
        assert!(labels.iter().all(|l| l.name != "RDS"), "{labels:?}");
    }

    async fn whole(iq: &[Complex<f32>]) -> Vec<Label> {
        DecodeToIdentify
            .classify(
                iq,
                Hz(48_000),
                Hz(131_550_000),
                Band::whole(Hz(48_000)),
                None,
            )
            .await
    }

    #[tokio::test]
    async fn identifies_acars_in_baseband_iq() {
        let iq = acars_packet_iq("VH-ZET", "H1", "#M1BPOS/ZETASDR TEST", 48_000, 0.0);
        let labels = whole(&iq).await;
        assert_eq!(labels.len(), 1, "{labels:?}");
        assert_eq!(labels[0].layer, 2);
        assert_eq!(labels[0].name, "ACARS");
        assert_eq!(labels[0].confidence, 1.0);
        assert_eq!(
            labels[0].evidence,
            "1 blocks CRC-clean; first VH-ZET label H1"
        );
    }

    #[tokio::test]
    async fn aprs_and_pocsag_fixtures_have_no_acars_label_and_vice_versa() {
        let aprs = aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 0.0);
        let pocsag = pocsag_packet_iq(1234567, 3, "HELLO ZETA", 1200, 48_000, 0.0);
        let acars = acars_packet_iq("VH-ZET", "H1", "#M1BPOS/ZETASDR TEST", 48_000, 0.0);
        for iq in [&aprs, &pocsag] {
            let labels = whole(iq).await;
            assert!(labels.iter().all(|l| l.name != "ACARS"), "{labels:?}");
        }
        let labels = whole(&acars).await;
        assert!(
            labels
                .iter()
                .all(|l| l.name != "APRS" && l.name != "POCSAG"),
            "{labels:?}"
        );
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
