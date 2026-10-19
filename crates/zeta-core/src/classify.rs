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

/// A band's layer-1 measurement: its features and the label read from them.
/// [`ClassifierSet::run_all`] hands the first one a classifier produces to
/// the classifiers after it, so the band is measured once per run.
#[derive(Clone, Debug, PartialEq)]
pub struct Measurement {
    pub features: FeatureRecord,
    pub label: Label,
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

    /// [`Classifier::classify`] given `measured`, an earlier classifier's
    /// measurement of the same band, and returning the classifier's own
    /// measurement when it made one. The default ignores `measured` and
    /// measures nothing; a classifier that measures the band overrides this
    /// to return it, and one that needs a measurement overrides it to use
    /// `measured` instead of measuring again.
    async fn classify_measured(
        &self,
        iq: &[Complex<f32>],
        sample_rate_hz: Hz,
        center_hz: Hz,
        band: Band,
        features: Option<&FeatureRecord>,
        _measured: Option<&Measurement>,
    ) -> (Vec<Label>, Option<Measurement>) {
        (
            self.classify(iq, sample_rate_hz, center_hz, band, features)
                .await,
            None,
        )
    }
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
    /// The first [`Measurement`] a classifier returns is passed to every
    /// classifier after it.
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
        let mut measured: Option<Measurement> = None;
        for c in ordered {
            let (l, m) = c
                .classify_measured(
                    iq,
                    sample_rate_hz,
                    center_hz,
                    band,
                    features,
                    measured.as_ref(),
                )
                .await;
            labels.extend(l);
            if measured.is_none() {
                measured = m;
            }
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

    /// Layer 1 that measures (returning `marker` as its record), and layer 2
    /// that records the measurement it was handed.
    struct Measures(FeatureRecord);
    struct Uses(Mutex<Vec<Option<Measurement>>>);

    fn label(layer: u8, name: &str) -> Label {
        Label {
            layer,
            name: name.into(),
            confidence: 1.0,
            evidence: String::new(),
        }
    }

    #[async_trait]
    impl Classifier for Measures {
        fn name(&self) -> &str {
            "measures"
        }
        fn layer(&self) -> u8 {
            1
        }
        async fn classify(
            &self,
            _: &[Complex<f32>],
            _: Hz,
            _: Hz,
            _: Band,
            _: Option<&FeatureRecord>,
        ) -> Vec<Label> {
            vec![label(1, "wfm")]
        }
        async fn classify_measured(
            &self,
            _: &[Complex<f32>],
            _: Hz,
            _: Hz,
            _: Band,
            _: Option<&FeatureRecord>,
            _: Option<&Measurement>,
        ) -> (Vec<Label>, Option<Measurement>) {
            let l = label(1, "wfm");
            (
                vec![l.clone()],
                Some(Measurement {
                    features: self.0.clone(),
                    label: l,
                }),
            )
        }
    }

    #[async_trait]
    impl Classifier for Uses {
        fn name(&self) -> &str {
            "uses"
        }
        fn layer(&self) -> u8 {
            2
        }
        async fn classify(
            &self,
            _: &[Complex<f32>],
            _: Hz,
            _: Hz,
            _: Band,
            _: Option<&FeatureRecord>,
        ) -> Vec<Label> {
            Vec::new()
        }
        async fn classify_measured(
            &self,
            _: &[Complex<f32>],
            _: Hz,
            _: Hz,
            _: Band,
            _: Option<&FeatureRecord>,
            measured: Option<&Measurement>,
        ) -> (Vec<Label>, Option<Measurement>) {
            self.0.lock().unwrap().push(measured.cloned());
            (Vec::new(), None)
        }
    }

    #[tokio::test]
    async fn run_all_hands_the_layer1_measurement_to_later_layers() {
        let marker = FeatureRecord {
            bandwidth_hz: Hz(123_457),
            center_hz: Hz(104_900_000),
            symbol_rate_baud: None,
            snr_db: 12.5,
            burst: false,
        };
        let uses = Arc::new(Uses(Mutex::new(Vec::new())));
        let mut set = ClassifierSet::default();
        // Registered before layer 1: run_all orders by layer, not registration.
        set.register(uses.clone());
        set.register(Arc::new(Measures(marker.clone())));
        set.register(Arc::new(StubClassifier));
        let labels = set
            .run_all(&[], Hz(240_000), Hz(0), Band::whole(Hz(240_000)), None)
            .await;
        assert_eq!(labels, [label(1, "wfm")]);
        let seen = uses.0.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let m = seen[0].as_ref().expect("layer 2 was handed a measurement");
        assert_eq!(m.features, marker);
        assert_eq!(m.label.name, "wfm");
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
