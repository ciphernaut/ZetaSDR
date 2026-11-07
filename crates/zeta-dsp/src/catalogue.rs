//! The iNTERCEPT signal catalogue (`data/signals.json`, Apache-2.0, see
//! `data/NOTICE`): 594 known signals with their frequency ranges, bandwidth
//! and modulations, served as a knowledge provider and used by a layer-2
//! classifier that names a measured signal.

use std::sync::OnceLock;

use async_trait::async_trait;
use num_complex::Complex;
use serde::Deserialize;
use zeta_core::classify::{Band, Classifier, FeatureRecord, Label, Measurement, Tried};
use zeta_core::knowledge::{Allocation, KnowledgeProvider, Signature};
use zeta_core::modes::{ModeRegistry, ModeState, ModesHandle};
use zeta_core::types::Hz;

use crate::measure::measure_selection;

/// One catalogue signal. Entries without a bandwidth range or modulations are
/// kept; the corresponding scoring terms are then not applied.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogueEntry {
    pub id: String,
    pub name: String,
    /// Inclusive `(min, max)` ranges the signal is found in.
    pub frequency_ranges: Vec<(Hz, Hz)>,
    pub bandwidth_min_hz: Option<Hz>,
    pub bandwidth_max_hz: Option<Hz>,
    /// As the catalogue spells them (`"WFM"`, `"NFM"`, `"USB"`, ...).
    pub modulations: Vec<String>,
    pub regions: Vec<String>,
    /// The signal's sigidwiki page, or empty.
    pub url: String,
}

#[derive(Deserialize)]
struct RawRange {
    min_hz: u64,
    max_hz: u64,
}

#[derive(Deserialize)]
struct RawEntry {
    id: String,
    name: String,
    #[serde(default)]
    frequency_ranges: Vec<RawRange>,
    #[serde(default)]
    bandwidth_range: Option<RawRange>,
    #[serde(default)]
    modulations: Vec<String>,
    #[serde(default)]
    regions: Vec<String>,
    #[serde(default)]
    sigidwiki_url: Option<String>,
}

impl From<RawEntry> for CatalogueEntry {
    fn from(r: RawEntry) -> Self {
        CatalogueEntry {
            id: r.id,
            name: r.name,
            frequency_ranges: r
                .frequency_ranges
                .iter()
                .map(|f| (Hz(f.min_hz), Hz(f.max_hz)))
                .collect(),
            bandwidth_min_hz: r.bandwidth_range.as_ref().map(|b| Hz(b.min_hz)),
            bandwidth_max_hz: r.bandwidth_range.as_ref().map(|b| Hz(b.max_hz)),
            modulations: r.modulations,
            regions: r.regions,
            url: r.sigidwiki_url.unwrap_or_default(),
        }
    }
}

/// The catalogue, parsed once from the embedded `data/signals.json`.
pub fn catalogue() -> &'static [CatalogueEntry] {
    static CATALOGUE: OnceLock<Vec<CatalogueEntry>> = OnceLock::new();
    CATALOGUE.get_or_init(|| {
        let raw: Vec<RawEntry> = serde_json::from_str(include_str!("../data/signals.json"))
            .expect("data/signals.json is valid");
        raw.into_iter().map(CatalogueEntry::from).collect()
    })
}

/// Whether `[min, max]` overlaps `[center - bw/2, center + bw/2]`.
fn overlaps(min: Hz, max: Hz, center: Hz, bandwidth: Hz) -> bool {
    let half = bandwidth.0 / 2;
    min.0 <= center.0.saturating_add(half) && max.0 >= center.0.saturating_sub(half)
}

/// The catalogue as a knowledge provider: one signature per frequency range
/// overlapping the queried band; no allocations. A signature's `decoder` is the
/// mode that proves the entry: the `ready` or `unavailable` row of `modes` (the rows
/// it holds at the lookup) whose `catalogue` lists the entry's id (`None` when no row
/// does).
pub struct CatalogueKnowledge {
    modes: ModesHandle,
}

impl CatalogueKnowledge {
    pub fn new(modes: ModesHandle) -> CatalogueKnowledge {
        CatalogueKnowledge { modes }
    }
}

/// The id of the row of `modes` that proves catalogue entry `entry_id`.
fn decoder(modes: Option<&ModeRegistry>, entry_id: &str) -> Option<String> {
    modes?
        .all()
        .iter()
        .filter(|m| matches!(m.state, ModeState::Ready | ModeState::Unavailable))
        .find(|m| m.row.catalogue.iter().any(|c| c == entry_id))
        .map(|m| m.row.id.0.clone())
}

#[async_trait]
impl KnowledgeProvider for CatalogueKnowledge {
    fn name(&self) -> &str {
        "catalogue"
    }

    async fn allocations(
        &self,
        _region: &str,
        _center_hz: Hz,
        _bandwidth_hz: Hz,
    ) -> Vec<Allocation> {
        Vec::new()
    }

    async fn signatures(&self, center_hz: Hz, bandwidth_hz: Hz) -> Vec<Signature> {
        // One registry for the whole lookup.
        let modes = self.modes.get();
        let modes = modes.as_deref();
        catalogue()
            .iter()
            .flat_map(|e| {
                e.frequency_ranges
                    .iter()
                    .filter(move |&&(lo, hi)| overlaps(lo, hi, center_hz, bandwidth_hz))
                    .map(move |&(min_hz, max_hz)| Signature {
                        name: e.name.clone(),
                        min_hz,
                        max_hz,
                        bandwidth_hz: e.bandwidth_max_hz.unwrap_or(Hz(0)),
                        modulation: e.modulations.first().cloned().unwrap_or_default(),
                        symbol_rate_baud: None,
                        decoder: decoder(modes, &e.id),
                        order: None,
                        tone_spacing_hz: None,
                        burst: None,
                        sync: None,
                        encoding: None,
                        crc: None,
                        confidence: None,
                        tier: None,
                    })
            })
            .collect()
    }
}

/// Catalogue modulations a layer-1 label stands for.
fn modulations_for(label: &str) -> &'static [&'static str] {
    match label {
        "wfm" => &["WFM", "FM"],
        "fm" => &["FM", "NFM", "NBFM"],
        "am" => &["AM"],
        "ssb" => &["SSB", "USB", "LSB"],
        "fsk" => &["FSK", "AFSK", "GFSK", "2-FSK"],
        "carrier" => &["CW"],
        _ => &[],
    }
}

/// An entry whose bandwidth range tops this is wideband: its bandwidth must
/// match, so a narrow channel inside its allocation is not taken for it.
const WIDEBAND_HZ: u64 = 50_000;
/// Scores, in tenths: frequency, bandwidth, modulation.
const FREQ_SCORE: u32 = 4;
const BW_SCORE: u32 = 3;
const MOD_SCORE: u32 = 3;
/// Most labels returned.
const MAX_LABELS: usize = 3;

/// Outcome of one scoring term.
#[derive(Clone, Copy, PartialEq)]
enum Term {
    Ok,
    Off,
    NotApplicable,
}

impl Term {
    fn word(self) -> &'static str {
        match self {
            Term::Ok => "ok",
            Term::Off => "off",
            Term::NotApplicable => "n/a",
        }
    }
}

/// Score of `e` (in tenths) for a signal centred at `center_hz`, `bandwidth_hz`
/// wide and labelled `layer1`, with the bandwidth and modulation terms; `None`
/// when it does not qualify.
fn score(
    e: &CatalogueEntry,
    center_hz: u64,
    bandwidth_hz: u64,
    layer1: &str,
) -> Option<(u32, Term, Term)> {
    if !e
        .frequency_ranges
        .iter()
        .any(|&(lo, hi)| (lo.0..=hi.0).contains(&center_hz))
    {
        return None;
    }
    let bw = match (e.bandwidth_min_hz, e.bandwidth_max_hz) {
        (Some(lo), Some(hi)) => {
            let (lo, hi) = (lo.0 as f64 * 0.5, hi.0 as f64 * 1.5);
            if (lo..=hi).contains(&(bandwidth_hz as f64)) {
                Term::Ok
            } else {
                Term::Off
            }
        }
        _ => Term::NotApplicable,
    };
    let wanted = modulations_for(layer1);
    let modulation = if e.modulations.is_empty() || wanted.is_empty() {
        Term::NotApplicable
    } else if e
        .modulations
        .iter()
        .any(|m| wanted.iter().any(|w| m.eq_ignore_ascii_case(w)))
    {
        Term::Ok
    } else {
        Term::Off
    };
    // A broad entry needs its bandwidth to match; so does a wideband
    // measurement against an entry whose bandwidth it is off (a 150 kHz
    // broadcast station is not a 2 kHz tone entry sharing its band).
    if (e.bandwidth_max_hz.is_some_and(|hi| hi.0 > WIDEBAND_HZ) && bw != Term::Ok)
        || (bandwidth_hz > WIDEBAND_HZ && bw == Term::Off)
    {
        return None;
    }
    let s = FREQ_SCORE
        + if bw == Term::Ok { BW_SCORE } else { 0 }
        + if modulation == Term::Ok { MOD_SCORE } else { 0 };
    (s > FREQ_SCORE).then_some((s, bw, modulation))
}

/// Catalogue entry `entry_id`'s score (0-1) for a signal centred at `center_hz`,
/// `bandwidth_hz` wide and labelled `layer1`, as [`catalogue_labels`] scores it; 0
/// for an entry that does not qualify or is not in the catalogue.
pub fn catalogue_score(entry_id: &str, center_hz: u64, bandwidth_hz: u64, layer1: &str) -> f32 {
    catalogue()
        .iter()
        .find(|e| e.id == entry_id)
        .and_then(|e| score(e, center_hz, bandwidth_hz, layer1))
        .map_or(0.0, |(s, _, _)| (s as f32 / 10.0).min(1.0))
}

/// Width of the narrowest frequency range of `e` holding `center_hz`, the
/// tie-break among equal scores: the more specific allocation first.
fn narrowest_range(e: &CatalogueEntry, center_hz: u64) -> u64 {
    e.frequency_ranges
        .iter()
        .filter(|&&(lo, hi)| (lo.0..=hi.0).contains(&center_hz))
        .map(|&(lo, hi)| hi.0 - lo.0)
        .min()
        .unwrap_or(u64::MAX)
}

/// Up to [`MAX_LABELS`] layer-2 labels naming catalogue entries for a signal
/// centred at `center_hz`, `bandwidth_hz` wide and labelled `layer1` by layer
/// 1, highest score first; among equal scores, the entry whose narrowest
/// frequency range holding `center_hz` is narrower first (then catalogue
/// order). Score: 0.4 when a frequency range holds `center_hz` (required);
/// 0.3 when `bandwidth_hz` lies in half the entry's minimum to 1.5 × its
/// maximum bandwidth (required when that maximum is above 50 kHz, and when
/// `bandwidth_hz` is above 50 kHz and the entry gives a bandwidth); 0.3 when
/// `layer1` maps to one of the entry's modulations. At least one of the last
/// two must hold.
pub fn catalogue_labels(center_hz: u64, bandwidth_hz: u64, layer1: &str) -> Vec<Label> {
    let mut scored: Vec<(u32, &CatalogueEntry, Term, Term)> = catalogue()
        .iter()
        .filter_map(|e| score(e, center_hz, bandwidth_hz, layer1).map(|(s, b, m)| (s, e, b, m)))
        .collect();
    scored.sort_by_key(|s| (std::cmp::Reverse(s.0), narrowest_range(s.1, center_hz)));
    scored
        .into_iter()
        .take(MAX_LABELS)
        .map(|(s, e, bw, m)| Label {
            layer: 2,
            name: e.name.clone(),
            confidence: (s as f32 / 10.0).min(1.0),
            evidence: format!(
                "catalogue {}: freq in range, bandwidth {}, modulation {} (measured {center_hz} Hz, {bandwidth_hz} Hz wide, layer-1 {layer1})",
                e.id,
                bw.word(),
                m.word()
            ),
        })
        .collect()
}

/// Layer-2 classifier naming signals from the catalogue: takes the band's
/// centre, bandwidth and layer-1 label from an earlier classifier's
/// [`Measurement`] when [`Classifier::classify_measured`] is handed one, and
/// otherwise measures the band with [`measure_selection`]; with `features`,
/// their centre and bandwidth are used instead of the measured ones. Scores
/// every entry with [`catalogue_labels`]. Nothing for a band layer 1 calls
/// noise.
pub struct CatalogueClassifier;

#[async_trait]
impl Classifier for CatalogueClassifier {
    fn name(&self) -> &str {
        "catalogue"
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
        features: Option<&FeatureRecord>,
        measured: Option<&Measurement>,
        _allocations: &[Allocation],
    ) -> (Vec<Label>, Option<Measurement>, Vec<Tried>) {
        let own;
        let m = match measured {
            Some(m) => m,
            None => match measure_selection(iq, sample_rate_hz.0, center_hz.0, band) {
                Some((features, label)) => {
                    own = Measurement { features, label };
                    &own
                }
                None => return (Vec::new(), None, Vec::new()),
            },
        };
        if m.label.name == "noise" {
            return (Vec::new(), None, Vec::new());
        }
        let f = features.unwrap_or(&m.features);
        (
            catalogue_labels(f.center_hz.0, f.bandwidth_hz.0, &m.label.name),
            None,
            Vec::new(),
        )
    }
}
