//! Layer-2 decode-to-identify: score the mode rows against the measured band and try the
//! best [`CLASSIFY_TRIES`] that have a quick in-process attempt (APRS, POCSAG, ACARS,
//! RDS, or a frame row's chain) over the channel, reporting only what passes its own
//! check: the AX.25 FCS, the POCSAG BCH code, the ACARS CRC-16, the RDS checkwords or
//! the frame row's CRC (design 6.2, 7.2).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use num_complex::Complex;
use zeta_core::classify::{Band, Classifier, FeatureRecord, Label, Measurement, Outcome, Tried};
use zeta_core::knowledge::Allocation;
use zeta_core::modes::score::{allocation_term, score_rows};
use zeta_core::modes::{Decoder, LoadedMode, ModeRegistry, ModeRow, ModeState};
use zeta_core::ops::channel_rate_hz;
use zeta_core::types::Hz;

use crate::acars::decode_acars_iq;
use crate::afsk::Afsk1200Demod;
use crate::ax25::{HdlcDeframer, parse_ax25_ui};
use crate::catalogue::catalogue_score;
use crate::channel::extract_offset;
use crate::measure::measure_selection;
use crate::nbfm::nbfm_demod;
use crate::pocsag::{BAUDS, decode_pocsag_iq};
use crate::rds::decode_rds_iq;

/// How many rows, best score first, `classify` considers.
pub const CLASSIFY_TRIES: usize = 5;

/// A requested band at least this wide is broadcast FM (the `rds_decode` row's
/// `band.min_hz`); a whole-block band measured at least
/// [`RDS_MIN_MEASURED_BW_HZ`] wide is read as this wide.
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

/// Decode-to-identify over the mode rows. The band is scored with
/// [`score_rows`] from layer 1's measurement when it is handed one (else it
/// measures the band itself), and the best [`CLASSIFY_TRIES`] rows are
/// considered in score order: a row whose `min_span_ms` exceeds the block is
/// `skipped_span`, one without a quick attempt `skipped_quick`, one wider
/// than the box `skipped_band`, one not ready `unavailable`; the rest run
/// their quick attempt on one channel extracted from the requested band, at
/// least [`MIN_BW_HZ`] wide at [`channel_rate_hz`] (for the whole block, the
/// measured occupied band), or, on a band over the RDS gate, for a row whose
/// own `band.bandwidth_hz` is narrower, on a channel of the row's width
/// around the measured centre: `proved` or `no_frame`. A box narrower than
/// [`MIN_BW_HZ`] is fitted as [`MIN_BW_HZ`] wide. Every row is considered;
/// a proof does not stop the sweep. Upstream features are not used. The
/// register's assignments handed to the run score [`allocation_term`].
pub struct DecodeToIdentify {
    modes: Arc<ModeRegistry>,
}

impl DecodeToIdentify {
    pub fn new(modes: Arc<ModeRegistry>) -> DecodeToIdentify {
        DecodeToIdentify { modes }
    }
}

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
        center_hz: Hz,
        band: Band,
        features: Option<&FeatureRecord>,
    ) -> Vec<Label> {
        self.classify_measured(iq, sample_rate_hz, center_hz, band, features, None, &[])
            .await
            .0
    }
    async fn classify_measured(
        &self,
        iq: &[Complex<f32>],
        sample_rate_hz: Hz,
        center_hz: Hz,
        band: Band,
        _features: Option<&FeatureRecord>,
        measured: Option<&Measurement>,
        allocations: &[Allocation],
    ) -> (Vec<Label>, Option<Measurement>, Vec<Tried>) {
        let rate = sample_rate_hz.0;
        if iq.len() < MIN_SAMPLES || rate < MIN_AUDIO_HZ {
            return (Vec::new(), None, Vec::new());
        }
        let own;
        let m = match measured {
            Some(m) => m,
            None => match measure_selection(iq, rate, center_hz.0, band) {
                Some((features, label)) => {
                    own = Measurement { features, label };
                    &own
                }
                None => return (Vec::new(), None, Vec::new()),
            },
        };
        let (m_center, m_bw, layer1) = (
            m.features.center_hz.0,
            m.features.bandwidth_hz.0,
            m.label.name.as_str(),
        );
        // The band decoded, and the box a row must fit: the requested band, or for the
        // whole block its measured occupied band, read as at least `RDS_MIN_BW_HZ`
        // once it reaches `RDS_MIN_MEASURED_BW_HZ`; either at least `MIN_BW_HZ`, as
        // the channel extracted is. `wide`: the band is over the RDS gate.
        let (offset, bw, box_hz, wide) = if band.is_whole(sample_rate_hz) {
            let wide = m_bw >= RDS_MIN_MEASURED_BW_HZ;
            let box_hz = match wide {
                true => m_bw.max(RDS_MIN_BW_HZ).min(band.bandwidth_hz.0),
                false => m_bw,
            };
            let offset = m_center as i64 - center_hz.0 as i64;
            (offset, m_bw, box_hz.max(MIN_BW_HZ), wide)
        } else {
            let bw = band.bandwidth_hz.0;
            (band.offset_hz, bw, bw.max(MIN_BW_HZ), bw >= RDS_MIN_BW_HZ)
        };
        let span_ms = iq.len() as u64 * 1000 / rate;
        let catalogue = |id: &str| catalogue_score(id, m_center, m_bw, layer1);
        let allocation = |m: &LoadedMode| allocation_term(m, allocations);
        let scored = score_rows(&self.modes, m_center, m_bw, layer1, &catalogue, &allocation);
        // Each channel is extracted once, when the first row that runs on it does.
        let mut chans: Vec<((i64, u64), Option<(Vec<Complex<f32>>, u64)>)> = Vec::new();
        let mut labels = Vec::new();
        let mut tried = Vec::new();
        for s in scored
            .into_iter()
            .filter(|s| s.in_range)
            .take(CLASSIFY_TRIES)
        {
            let row = &s.mode.row;
            let attempt = quick_attempt(row);
            let outcome = match &attempt {
                _ if row.identify.min_span_ms > span_ms => Outcome::SkippedSpan,
                None => Outcome::SkippedQuick,
                Some(_) if !row.band.fits(box_hz) => Outcome::SkippedBand,
                Some(_) if s.mode.state != ModeState::Ready => Outcome::Unavailable,
                Some(attempt) => {
                    // Under the RDS gate every row runs on the band's own channel. Over
                    // it, a row narrower than the band runs on its own width around the
                    // measured centre, not on the whole broadcast-wide band.
                    let row_bw = row.band.bandwidth_hz.max(MIN_BW_HZ);
                    let key = match wide && row_bw < bw.max(MIN_BW_HZ) {
                        true => (m_center as i64 - center_hz.0 as i64, row_bw),
                        false => (offset, bw),
                    };
                    let i = match chans.iter().position(|(k, _)| *k == key) {
                        Some(i) => i,
                        None => {
                            chans.push((key, extract(iq, rate, key.0, key.1)));
                            chans.len() - 1
                        }
                    };
                    let mut found = match &chans[i].1 {
                        Some((c, r)) => attempt(c.clone(), *r).await,
                        None => None,
                    };
                    // A box between the floor and the RDS gate may hold the signal off its
                    // centre: once more on the row's own width at the measured centre.
                    if found.is_none() && !wide && layer1 != "noise" {
                        let slice = (m_center as i64 - center_hz.0 as i64, row_bw);
                        if slice != key {
                            found = slice_attempt(iq, rate, slice, attempt).await;
                        }
                    }
                    match found {
                        Some(evidence) => {
                            labels.push(Label {
                                layer: 2,
                                name: row.label.clone(),
                                confidence: 1.0,
                                evidence: format!("decode {}: {evidence}", row.id.0),
                            });
                            Outcome::Proved
                        }
                        None => Outcome::NoFrame,
                    }
                }
            };
            tried.push(Tried {
                mode: row.id.0.clone(),
                score: s.score,
                outcome,
            });
        }
        (labels, None, tried)
    }
}

/// The channel at `offset` from the block centre, `bw` wide (at least [`MIN_BW_HZ`]),
/// and its rate; `None` when it cannot be extracted.
fn extract(
    iq: &[Complex<f32>],
    rate: u64,
    offset: i64,
    bw: u64,
) -> Option<(Vec<Complex<f32>>, u64)> {
    let out_rate = channel_rate_hz(rate, bw.max(MIN_BW_HZ));
    // The channel filter refuses a band wider than its output rate.
    let bw = bw.max(MIN_BW_HZ).min(out_rate);
    extract_offset(iq, rate, offset, bw, out_rate).ok()
}

/// `attempt` over the channel at `(offset, bandwidth)` from the block centre: its
/// evidence when a frame passes its check.
async fn slice_attempt(
    iq: &[Complex<f32>],
    rate: u64,
    (offset, bw): (i64, u64),
    attempt: &Attempt,
) -> Option<String> {
    let (chan, out_rate) = extract(iq, rate, offset, bw)?;
    attempt(chan, out_rate).await
}

/// What an [`Attempt`] returns: its evidence, once the decoder has run.
type AttemptFuture = Pin<Box<dyn Future<Output = Option<String>> + Send>>;

/// One decoder tried over the extracted channel (IQ at the given rate): its
/// evidence when a frame passes its check.
type Attempt = Arc<dyn Fn(Vec<Complex<f32>>, u64) -> AttemptFuture + Send + Sync>;

/// The in-crate attempt `row`'s `identify.quick` names (the names
/// `zeta_core::modes::QUICK_ATTEMPTS` validates): `frame` runs the frame row's own chain
/// ([`crate::frame_quick::attempt`]).
fn quick_attempt(row: &ModeRow) -> Option<Attempt> {
    let blocking = |f: fn(&[Complex<f32>], u64) -> Option<String>| -> Attempt {
        Arc::new(move |chan: Vec<Complex<f32>>, rate| Box::pin(async move { f(&chan, rate) }))
    };
    match row.identify.quick.as_deref()? {
        "aprs" => Some(blocking(aprs_attempt)),
        "pocsag" => Some(blocking(pocsag_attempt)),
        "acars" => Some(blocking(acars_attempt)),
        "rds" => Some(blocking(rds_attempt)),
        "frame" => {
            let Decoder::Frame(block) = &row.decoder else {
                return None;
            };
            let block = Arc::new(block.clone());
            Some(Arc::new(move |chan: Vec<Complex<f32>>, rate| {
                let block = block.clone();
                Box::pin(async move { crate::frame_quick::attempt(chan, rate, &block).await })
            }))
        }
        _ => None,
    }
}

fn rds_attempt(chan: &[Complex<f32>], rate: u64) -> Option<String> {
    // The first event is usually the PI alone; prefer one that has the PS.
    let events = decode_rds_iq(chan, rate);
    let first = events.iter().find(|e| e.ps.is_some()).or(events.first())?;
    let ps = first.ps.clone().unwrap_or_default();
    Some(format!("PI {}, PS {ps:?}", first.pi))
}

fn acars_attempt(chan: &[Complex<f32>], rate: u64) -> Option<String> {
    let blocks = decode_acars_iq(chan, rate);
    let first = blocks.first()?;
    Some(format!(
        "{} blocks CRC-clean; first {} label {}",
        blocks.len(),
        first.reg,
        first.label
    ))
}

fn aprs_attempt(chan: &[Complex<f32>], rate: u64) -> Option<String> {
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
    Some(format!(
        "{} AX.25 frames with valid FCS; first {}>{}",
        frames.len(),
        first.src,
        first.dst
    ))
}

fn pocsag_attempt(chan: &[Complex<f32>], rate: u64) -> Option<String> {
    let messages = decode_pocsag_iq(chan, rate);
    // The baud with the most messages is the one the transmitter used.
    let baud = BAUDS
        .iter()
        .copied()
        .max_by_key(|&b| messages.iter().filter(|m| m.baud == b).count())?;
    let at_baud: Vec<_> = messages.iter().filter(|m| m.baud == baud).collect();
    let first = at_baud.first()?;
    Some(format!(
        "{} messages BCH-clean at {} baud; first address {}",
        at_baud.len(),
        baud,
        first.address
    ))
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::fixtures::{
        acars_packet_iq, aprs_packet_iq, nbfm_modulate, pocsag_packet_iq, rds_station_iq, tone,
    };
    use zeta_core::stage::StageRegistry;

    /// Decode-to-identify over the embedded rows.
    fn d() -> DecodeToIdentify {
        let mut stages = StageRegistry::new();
        crate::register_all(&mut stages);
        DecodeToIdentify::new(Arc::new(ModeRegistry::load(&stages, None)))
    }

    #[tokio::test]
    async fn identifies_aprs_in_baseband_iq() {
        let iq = aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 0.0);
        let labels = d()
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
        let labels = d()
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
        let labels = d()
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
        let labels = d()
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

    /// The labels for `iq` as a whole 48 kHz block, padded with silence to the rows'
    /// 1 s `min_span_ms`.
    async fn whole(iq: &[Complex<f32>]) -> Vec<Label> {
        let mut iq = iq.to_vec();
        iq.resize(iq.len().max(48_000), Complex::new(0.0, 0.0));
        d().classify(
            &iq,
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
            "decode acars_decode: 1 blocks CRC-clean; first VH-ZET label H1"
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
        let labels = d()
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
            d().classify(
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
            d().classify(
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
        let above = d()
            .classify(&iq, Hz(48_000), Hz(144_800_000), band(12_000), None)
            .await;
        assert_eq!(above.len(), 1, "{above:?}");
        assert_eq!(above[0].name, "APRS");
        let below = d()
            .classify(&iq, Hz(48_000), Hz(144_800_000), band(-12_000), None)
            .await;
        assert!(below.is_empty(), "{below:?}");
    }
}
