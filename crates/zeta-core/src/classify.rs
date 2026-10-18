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

#[async_trait]
pub trait Classifier: Send + Sync {
    fn name(&self) -> &str;
    fn layer(&self) -> u8;
    async fn classify(
        &self,
        iq: &[Complex<f32>],
        sample_rate_hz: Hz,
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

    /// Every classifier's labels, lowest layer first; registration order within a layer.
    pub async fn run_all(
        &self,
        iq: &[Complex<f32>],
        sample_rate_hz: Hz,
        features: Option<&FeatureRecord>,
    ) -> Vec<Label> {
        let mut ordered: Vec<&Arc<dyn Classifier>> = self.classifiers.iter().collect();
        ordered.sort_by_key(|c| c.layer());
        let mut labels = Vec::new();
        for c in ordered {
            labels.extend(c.classify(iq, sample_rate_hz, features).await);
        }
        labels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(u8, &'static str);

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
            _: Option<&FeatureRecord>,
        ) -> Vec<Label> {
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
        set.register(Arc::new(Fixed(3, "c")));
        set.register(Arc::new(Fixed(1, "a")));
        set.register(Arc::new(StubClassifier));
        set.register(Arc::new(Fixed(2, "b")));
        let names: Vec<String> = set
            .run_all(&[], Hz(48_000), None)
            .await
            .into_iter()
            .map(|l| l.name)
            .collect();
        assert_eq!(names, ["a", "b", "c"]);
    }
}
