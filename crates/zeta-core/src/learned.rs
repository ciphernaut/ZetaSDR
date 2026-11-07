//! Learned knowledge: the operator's accepted signatures put to use. [`LearnedKnowledge`]
//! lists them through `knowledge_lookup` (tier `learned`) and [`LearnedClassifier`] scores
//! a measured band against them in layer 2, labelling the best match `learned <name>`.
//! Both read the node's [`SignatureStore`] at each call, so an accept, a reject or a
//! supersede takes effect at once. Only accepted records are used: a proposed, rejected
//! or superseded one is never listed or scored.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use num_complex::Complex;

use crate::classify::{Band, Classifier, FeatureRecord, Label, Measurement, Tried};
use crate::knowledge::{Allocation, KnowledgeProvider, Signature, overlaps};
use crate::signatures::{SignatureRecord, SignatureStore};
use crate::types::Hz;

/// The name of the provider and of the classifier.
pub const LEARNED: &str = "learned";
/// The layer the classifier labels in.
pub const LEARNED_LAYER: u8 = 2;
/// The score for a measured centre the record's band holds (required for a label).
pub const LEARNED_CENTER_SCORE: f32 = 0.4;
/// The score for a measured bandwidth within [`LEARNED_BANDWIDTH_RANGE`] of the record's.
pub const LEARNED_BANDWIDTH_SCORE: f32 = 0.3;
/// The score for the layer-1 label equal to the record's modulation, with equal orders
/// (or either unknown).
pub const LEARNED_MODULATION_SCORE: f32 = 0.2;
/// The score for a symbol rate within [`LEARNED_RATE_TOLERANCE`] of the record's.
pub const LEARNED_RATE_SCORE: f32 = 0.1;
/// The score for a burst period within [`LEARNED_PERIOD_TOLERANCE`] of the record's.
pub const LEARNED_PERIOD_SCORE: f32 = 0.1;
/// The measured bandwidth over the record's that counts as the same width.
pub const LEARNED_BANDWIDTH_RANGE: (f64, f64) = (0.5, 1.5);
/// The relative symbol-rate difference that counts as the same rate.
pub const LEARNED_RATE_TOLERANCE: f64 = 0.05;
/// The relative burst-period difference that counts as the same period.
pub const LEARNED_PERIOD_TOLERANCE: f64 = 0.2;
/// The least score that labels a match.
pub const LEARNED_MIN_SCORE: f32 = 0.7;
/// The highest score there is (the terms add up to more).
const LEARNED_MAX_SCORE: f32 = 1.0;
/// The tier of a learned signature in `knowledge_lookup`.
pub const LEARNED_TIER: &str = "learned";
/// The least match score (see [`score`], not the label's confidence × score) of a
/// learned label that names an `identify` unknown verdict: a features-only signature
/// (confidence 0.3) names it too.
pub const LEARNED_VERDICT_MIN: f32 = 0.5;

/// The match score behind the learned label `label` given for `measured`: its record's
/// [`score`] again (`None` for another label, or a record gone from the accepted ones).
pub fn label_score(store: &SignatureStore, label: &Label, measured: &Measurement) -> Option<f32> {
    let (id, _) = learned_match(label)?;
    let r = store
        .get(id)
        .filter(|r| r.state == crate::signatures::SignatureState::Accepted)?;
    score(&r, &measured.features, &measured.label.name).map(|(s, _)| s)
}

/// A learned label's record id and signature name (from its evidence
/// `learned <id>: ...` and its name `learned <name>`); `None` for any other label.
pub fn learned_match(label: &Label) -> Option<(&str, &str)> {
    let prefix = format!("{LEARNED} ");
    let id = label.evidence.strip_prefix(&prefix)?.split(':').next()?;
    let name = label.name.strip_prefix(&prefix)?;
    Some((id, name))
}

/// The accepted signatures as a knowledge provider: no allocations; a signature per
/// accepted record whose band overlaps the queried one, with `tier: learned`.
pub struct LearnedKnowledge(pub Arc<SignatureStore>);

/// The record as a [`Signature`]: its band, waveform, burst shape, decoder and
/// confidence, and its framing's sync (hex), line code and CRC.
fn signature(r: &SignatureRecord) -> Signature {
    let framing = r.framing.as_ref();
    let text = |v: Option<&serde_json::Value>| v.and_then(|v| v.as_str()).map(str::to_string);
    Signature {
        name: r.name.clone(),
        min_hz: r.min_hz,
        max_hz: r.max_hz,
        bandwidth_hz: r.bandwidth_hz,
        modulation: r.modulation.clone(),
        symbol_rate_baud: r.symbol_rate_baud,
        decoder: r.decoder.clone(),
        order: r.order,
        tone_spacing_hz: r.tone_spacing_hz,
        burst: Some(r.burst.clone()),
        sync: text(framing.map(|f| &f["sync"]["hex"])),
        encoding: text(framing.map(|f| &f["encoding"])),
        crc: text(framing.map(|f| &f["crc"])),
        confidence: Some(r.confidence),
        tier: Some(LEARNED_TIER.into()),
    }
}

#[async_trait]
impl KnowledgeProvider for LearnedKnowledge {
    fn name(&self) -> &str {
        LEARNED
    }

    async fn allocations(&self, _: &str, _: Hz, _: Hz) -> Vec<Allocation> {
        Vec::new()
    }

    async fn signatures(&self, center_hz: Hz, bandwidth_hz: Hz) -> Vec<Signature> {
        self.0
            .accepted()
            .iter()
            .filter(|r| overlaps(r.min_hz, r.max_hz, center_hz, bandwidth_hz))
            .map(signature)
            .collect()
    }
}

/// The layer-2 scorer of the accepted signatures: see [`score`]. It needs an earlier
/// classifier's measurement and labels nothing without one.
pub struct LearnedClassifier(pub Arc<SignatureStore>);

/// Whether `measured` lies within `tolerance` of `reference` (relative to it).
fn near(measured: f64, reference: f64, tolerance: f64) -> bool {
    reference > 0.0 && (measured - reference).abs() <= tolerance * reference
}

/// The score of `r` against the measurement (`features` and the layer-1 `label`), and
/// the terms that hit in words: `centre`, `bandwidth`, the modulation, `<rate> Bd`,
/// `period`. `None` when the record's band does not hold the measured centre.
pub fn score(
    r: &SignatureRecord,
    features: &FeatureRecord,
    label: &str,
) -> Option<(f32, Vec<String>)> {
    if !r.band_holds(features.center_hz) {
        return None;
    }
    let mut score = LEARNED_CENTER_SCORE;
    let mut terms = vec!["centre".to_string()];
    let (lo, hi) = LEARNED_BANDWIDTH_RANGE;
    let ratio = features.bandwidth_hz.0 as f64 / r.bandwidth_hz.0 as f64;
    if r.bandwidth_hz.0 > 0 && (lo..=hi).contains(&ratio) {
        score += LEARNED_BANDWIDTH_SCORE;
        terms.push("bandwidth".into());
    }
    let orders = match (features.order, r.order) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    };
    if label == r.modulation && orders {
        score += LEARNED_MODULATION_SCORE;
        terms.push(r.modulation.clone());
    }
    if let (Some(m), Some(rate)) = (features.symbol_rate_baud, r.symbol_rate_baud)
        && near(m.0 as f64, rate.0 as f64, LEARNED_RATE_TOLERANCE)
    {
        score += LEARNED_RATE_SCORE;
        terms.push(format!("{} Bd", rate.0));
    }
    if let (Some(m), Some(period)) = (features.period_ms, r.burst.period_ms)
        && near(m as f64, period as f64, LEARNED_PERIOD_TOLERANCE)
    {
        score += LEARNED_PERIOD_SCORE;
        terms.push("period".into());
    }
    Some((score.min(LEARNED_MAX_SCORE), terms))
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
}

impl LearnedClassifier {
    /// The label of the best-scoring accepted record for `measured` (the newest on a
    /// tie), when its score reaches [`LEARNED_MIN_SCORE`]: `learned <name>` at the
    /// record's confidence × the score, evidence `learned <id>: <terms>`. The match is
    /// counted on the record.
    fn label(&self, measured: &Measurement) -> Option<Label> {
        let accepted = self.0.accepted();
        let (r, score, terms) = accepted
            .iter()
            .filter_map(|r| {
                score(r, &measured.features, &measured.label.name).map(|(s, t)| (r, s, t))
            })
            .fold(
                None,
                |best: Option<(&SignatureRecord, f32, Vec<String>)>, x| match &best {
                    Some(b) if b.1 >= x.1 => best,
                    _ => Some(x),
                },
            )?;
        if score < LEARNED_MIN_SCORE {
            return None;
        }
        // A count that cannot be written leaves the label standing.
        let _ = self.0.matched(&r.id, now_ns());
        Some(Label {
            layer: LEARNED_LAYER,
            name: format!("{LEARNED} {}", r.name),
            confidence: r.confidence * score,
            evidence: format!("{LEARNED} {}: {}", r.id, terms.join(", ")),
        })
    }
}

#[async_trait]
impl Classifier for LearnedClassifier {
    fn name(&self) -> &str {
        LEARNED
    }

    fn layer(&self) -> u8 {
        LEARNED_LAYER
    }

    async fn classify(
        &self,
        _iq: &[Complex<f32>],
        _sample_rate_hz: Hz,
        _center_hz: Hz,
        _band: Band,
        _features: Option<&FeatureRecord>,
    ) -> Vec<Label> {
        Vec::new()
    }

    async fn classify_measured(
        &self,
        _iq: &[Complex<f32>],
        _sample_rate_hz: Hz,
        _center_hz: Hz,
        _band: Band,
        _features: Option<&FeatureRecord>,
        measured: Option<&Measurement>,
        _allocations: &[Allocation],
    ) -> (Vec<Label>, Option<Measurement>, Vec<Tried>) {
        let labels = measured.and_then(|m| self.label(m)).into_iter().collect();
        (labels, None, Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signatures::{Burst, NewSignature};
    use crate::types::{Baud, JobId, SourceId};

    fn features(center_hz: u64, bandwidth_hz: u64) -> FeatureRecord {
        FeatureRecord {
            bandwidth_hz: Hz(bandwidth_hz),
            center_hz: Hz(center_hz),
            symbol_rate_baud: Some(Baud(4_800)),
            snr_db: 20.0,
            burst: true,
            duty: Some(0.8),
            bursts: Some(16),
            burst_ms: Some(50.0),
            period_ms: Some(60.0),
            order: Some(2),
            tone_spacing_hz: Some(Hz(4_800)),
            symbol_rate_line: None,
        }
    }

    fn accepted(store: &SignatureStore, f: FeatureRecord) -> SignatureRecord {
        let new = NewSignature {
            name: Some("link".into()),
            features: f,
            modulation: "fsk".into(),
            framing: None,
            crc_frames: 0,
            evidence: String::new(),
            source: SourceId("rx".into()),
            station: None,
            captures: vec![],
            jobs: vec![JobId("01J".into())],
            allocation: None,
            proposed_by: "cli".into(),
        };
        let r = store.propose(new, 1).unwrap();
        store.accept(&r.id, None, "cli", 2).unwrap()
    }

    #[test]
    fn every_term_scores_and_the_sum_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let store = SignatureStore::open(dir.path()).unwrap();
        let r = accepted(&store, features(433_920_000, 20_000));
        let (s, terms) = score(&r, &features(433_921_000, 18_000), "fsk").unwrap();
        assert_eq!(s, LEARNED_MAX_SCORE);
        assert_eq!(terms, ["centre", "bandwidth", "fsk", "4800 Bd", "period"]);
        // A different order misses the modulation term; a rate 10 % off, the rate's.
        let mut f = features(433_921_000, 18_000);
        f.order = Some(4);
        f.symbol_rate_baud = Some(Baud(5_280));
        f.period_ms = None;
        let (s, terms) = score(&r, &f, "fsk").unwrap();
        assert!((s - 0.7).abs() < 1e-6, "{s}");
        assert_eq!(terms, ["centre", "bandwidth"]);
        // Outside the band (widened by half the bandwidth): no score at all.
        assert!(score(&r, &features(433_960_000, 20_000), "fsk").is_none());
        // Width alone off by more than half: under the floor.
        let (s, _) = score(&r, &features(433_920_000, 100_000), "psk").unwrap();
        assert!(s < LEARNED_MIN_SCORE, "{s}");
        assert_eq!(
            r.burst,
            Burst {
                duty: Some(0.8),
                burst_ms: Some(50.0),
                period_ms: Some(60.0)
            }
        );
    }
}
