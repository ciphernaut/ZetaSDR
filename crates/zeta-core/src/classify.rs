//! The pluggable classification contract: layers 1-3 identify a signal from
//! its IQ and, optionally, features measured upstream.

use std::sync::Arc;

use async_trait::async_trait;
use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::{Baud, Hz};

/// Measurements of a detected signal, filled by the detector.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct FeatureRecord {
    pub bandwidth_hz: Hz,
    pub center_hz: Hz,
    #[schemars(required)]
    pub symbol_rate_baud: Option<Baud>,
    pub snr_db: f32,
    pub burst: bool,
}

/// One classifier verdict.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Label {
    pub layer: u8,
    pub name: String,
    pub confidence: f32,
    pub evidence: String,
}

/// The band a classifier analyses within an IQ block: its centre's offset
/// from the block centre (negative below) and its width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Band {
    pub offset_hz: i64,
    pub bandwidth_hz: Hz,
}

impl Band {
    /// The whole block at `sample_rate_hz`.
    pub fn whole(sample_rate_hz: Hz) -> Band {
        Band {
            offset_hz: 0,
            bandwidth_hz: sample_rate_hz,
        }
    }

    /// Whether this band covers the whole block: centred, at least as wide as the rate.
    pub fn is_whole(&self, sample_rate_hz: Hz) -> bool {
        self.offset_hz == 0 && self.bandwidth_hz.0 >= sample_rate_hz.0
    }
}

#[async_trait]
pub trait Classifier: Send + Sync {
    fn name(&self) -> &str;
    fn layer(&self) -> u8;
    /// Labels for `band` of `iq`, a block sampled at `sample_rate_hz` whose
    /// RF centre is `center_hz`.
    async fn classify(
        &self,
        iq: &[Complex<f32>],
        sample_rate_hz: Hz,
        center_hz: Hz,
        band: Band,
        features: Option<&FeatureRecord>,
    ) -> Vec<Label>;
}

/// The layer-3 slot, proving a SIGINT classifier can plug in; labels nothing.
pub struct StubClassifier;

#[async_trait]
impl Classifier for StubClassifier {
    fn name(&self) -> &str {
        "sigint-slot"
    }
    fn layer(&self) -> u8 {
        3
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
}

#[derive(Clone, Default)]
pub struct ClassifierSet {
    classifiers: Vec<Arc<dyn Classifier>>,
}

impl ClassifierSet {
    pub fn register(&mut self, c: Arc<dyn Classifier>) {
        self.classifiers.push(c);
    }

    /// Every classifier's labels for `band` of `iq` (a block centred at
    /// `center_hz`), lowest layer first; registration order within a layer.
    pub async fn run_all(
        &self,
        iq: &[Complex<f32>],
        sample_rate_hz: Hz,
        center_hz: Hz,
        band: Band,
        features: Option<&FeatureRecord>,
    ) -> Vec<Label> {
        let mut ordered: Vec<&Arc<dyn Classifier>> = self.classifiers.iter().collect();
        ordered.sort_by_key(|c| c.layer());
        let mut labels = Vec::new();
        for c in ordered {
            labels.extend(
                c.classify(iq, sample_rate_hz, center_hz, band, features)
                    .await,
            );
        }
        labels
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Labels `layer`/`name` and records the centre each call was given.
    struct Fixed(u8, &'static str, Mutex<Vec<Hz>>);

    fn fixed(layer: u8, name: &'static str) -> Fixed {
        Fixed(layer, name, Mutex::new(Vec::new()))
    }

    #[async_trait]
    impl Classifier for Fixed {
        fn name(&self) -> &str {
            self.1
        }
        fn layer(&self) -> u8 {
            self.0
        }
        async fn classify(
            &self,
            _: &[Complex<f32>],
            _: Hz,
            center_hz: Hz,
            _: Band,
            _: Option<&FeatureRecord>,
        ) -> Vec<Label> {
            self.2.lock().unwrap().push(center_hz);
            vec![Label {
                layer: self.0,
                name: self.1.into(),
                confidence: 1.0,
                evidence: String::new(),
            }]
        }
    }

    #[tokio::test]
    async fn run_all_orders_by_layer() {
        let mut set = ClassifierSet::default();
        set.register(Arc::new(fixed(3, "c")));
        set.register(Arc::new(fixed(1, "a")));
        set.register(Arc::new(StubClassifier));
        set.register(Arc::new(fixed(2, "b")));
        let names: Vec<String> = set
            .run_all(&[], Hz(48_000), Hz(0), Band::whole(Hz(48_000)), None)
            .await
            .into_iter()
            .map(|l| l.name)
            .collect();
        assert_eq!(names, ["a", "b", "c"]);
    }

    #[tokio::test]
    async fn run_all_forwards_center_hz() {
        let (a, b) = (Arc::new(fixed(1, "a")), Arc::new(fixed(2, "b")));
        let mut set = ClassifierSet::default();
        set.register(a.clone());
        set.register(b.clone());
        set.run_all(
            &[],
            Hz(48_000),
            Hz(104_900_000),
            Band::whole(Hz(48_000)),
            None,
        )
        .await;
        assert_eq!(*a.2.lock().unwrap(), [Hz(104_900_000)]);
        assert_eq!(*b.2.lock().unwrap(), [Hz(104_900_000)]);
    }
}
