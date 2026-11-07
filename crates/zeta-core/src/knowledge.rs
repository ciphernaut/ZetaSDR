//! Knowledge providers: what should be at a frequency, and what a signal looks like.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::emission::Emission;
use crate::reach::Reach;
use crate::types::{Baud, Hz};

/// A band plan entry: what a region allocates `[min_hz, max_hz]` to, or (from a
/// licence register) one assignment in it. The fields after `note` are optional and
/// `null` when the provider lacks them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct Allocation {
    pub provider: String,
    pub region: String,
    pub min_hz: Hz,
    pub max_hz: Hz,
    pub service: String,
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub note: Option<String>,
    /// `plan` (a band plan service) or `assignment` (a licensed device).
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub kind: Option<String>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub licensee: Option<String>,
    /// The assignment's site name.
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub site: Option<String>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub lat: Option<f64>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub lon: Option<f64>,
    /// From the station to the site; `null` without a station.
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub distance_km: Option<f64>,
    /// From the station to the site, degrees clockwise from true north.
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub bearing_deg: Option<f64>,
    /// Transmitter power in watts, converted from the register's unit.
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub power_w: Option<f64>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<Hz>")]
    pub center_hz: Option<Hz>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<Hz>")]
    pub bandwidth_hz: Option<Hz>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub licence_no: Option<String>,
    /// `tx` or `rx`.
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub role: Option<String>,
    /// The emission designator as the register stores it, trimmed.
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub emission: Option<String>,
    /// `emission` decoded; `null` for a designator the parser cannot read.
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<Emission>")]
    pub emission_decoded: Option<Emission>,
    /// The provider's data as of this time (its last sync), when it says.
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub as_of: Option<String>,
}

/// Every property is always written (`null` when absent), so the schema requires
/// them all; the `#[serde(default)]` fields still load from files that lack them.
pub(crate) fn require_all(schema: &mut schemars::Schema) {
    let props: Vec<serde_json::Value> = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .map(|p| p.keys().map(|k| k.as_str().into()).collect())
        .unwrap_or_default();
    schema.insert("required".into(), props.into());
}

/// What a known signal looks like and which decoder handles it. The fields after
/// `decoder` are `null` for the seed and the catalogue; a learned signature
/// ([`crate::learned::LearnedKnowledge`]) fills those its record holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct Signature {
    pub name: String,
    pub min_hz: Hz,
    pub max_hz: Hz,
    pub bandwidth_hz: Hz,
    pub modulation: String,
    #[schemars(required)]
    pub symbol_rate_baud: Option<Baud>,
    #[schemars(required)]
    pub decoder: Option<String>,
    /// Modulation order (2 or 4 tones, or PSK points).
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<u8>")]
    pub order: Option<u8>,
    /// Spacing of adjacent FSK tones.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<Hz>")]
    pub tone_spacing_hz: Option<Hz>,
    /// The burst shape (duty, burst length, period).
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<crate::signatures::Burst>")]
    pub burst: Option<crate::signatures::Burst>,
    /// The framing's sync word, hex.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<String>")]
    pub sync: Option<String>,
    /// The framing's line code (`nrz`, `nrzi`, `manchester`, ...).
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<String>")]
    pub encoding: Option<String>,
    /// The framing's CRC.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<String>")]
    pub crc: Option<String>,
    /// How sure the signature is (a learned record's confidence).
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<f32>")]
    pub confidence: Option<f32>,
    /// Where it comes from: `learned` for an accepted learned signature.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<String>")]
    pub tier: Option<String>,
}

#[async_trait]
pub trait KnowledgeProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn allocations(&self, region: &str, center_hz: Hz, bandwidth_hz: Hz) -> Vec<Allocation>;
    async fn signatures(&self, center_hz: Hz, bandwidth_hz: Hz) -> Vec<Signature>;
    /// How far from the station this provider searched for the band, when it
    /// applies a reach.
    async fn reach(&self, _region: &str, _center_hz: Hz, _bandwidth_hz: Hz) -> Option<Reach> {
        None
    }
}

/// The region a register is asked about when the node has no station.
pub const DEFAULT_REGION: &str = "AU";
/// How many assignments `identify` and `classify` keep, nearest first.
pub const NEAREST_ASSIGNMENTS: usize = 5;

/// What `identify`, `classify` and the watch report for a band: every provider's
/// assignments (`kind` `assignment`) in `region`, nearest first (one without a distance
/// last; the providers' order otherwise, which puts transmitters before receivers), at
/// most [`NEAREST_ASSIGNMENTS`], then every plan row (`kind` `plan`, or none: the seed
/// and static entries) in provider order, uncapped. One call per provider: a lookup
/// against a dead service may take seconds.
pub async fn nearest_allocations(
    providers: &[Arc<dyn KnowledgeProvider>],
    region: &str,
    center_hz: Hz,
    bandwidth_hz: Hz,
) -> Vec<Allocation> {
    let (mut assignments, mut plans) = (Vec::new(), Vec::new());
    for p in providers {
        for a in p.allocations(region, center_hz, bandwidth_hz).await {
            match a.kind.as_deref() == Some("assignment") {
                true => assignments.push(a),
                false => plans.push(a),
            }
        }
    }
    assignments.sort_by(|a, b| match (a.distance_km, b.distance_km) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    assignments.truncate(NEAREST_ASSIGNMENTS);
    assignments.extend(plans);
    assignments
}

/// Providers backed by JSON data held in memory.
pub struct StaticKnowledge {
    signatures: Vec<Signature>,
    allocations: Vec<Allocation>,
}

fn invalid(e: serde_json::Error) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, e)
}

/// Whether `[min, max]` overlaps `[center - bw/2, center + bw/2]`.
pub(crate) fn overlaps(min: Hz, max: Hz, center: Hz, bandwidth: Hz) -> bool {
    let half = bandwidth.0 / 2;
    min.0 <= center.0.saturating_add(half) && max.0 >= center.0.saturating_sub(half)
}

impl StaticKnowledge {
    pub fn from_files(
        signatures: &Path,
        allocations: Option<&Path>,
    ) -> Result<Self, std::io::Error> {
        let signatures = serde_json::from_slice(&std::fs::read(signatures)?).map_err(invalid)?;
        let allocations = match allocations {
            Some(p) => serde_json::from_slice(&std::fs::read(p)?).map_err(invalid)?,
            None => Vec::new(),
        };
        Ok(Self {
            signatures,
            allocations,
        })
    }

    /// The seed signatures embedded in the binary, with no allocations.
    pub fn builtin() -> Self {
        let signatures = serde_json::from_str(include_str!("../data/signatures.json"))
            .expect("data/signatures.json is valid");
        Self {
            signatures,
            allocations: Vec::new(),
        }
    }
}

#[async_trait]
impl KnowledgeProvider for StaticKnowledge {
    fn name(&self) -> &str {
        "static"
    }

    async fn allocations(&self, region: &str, center_hz: Hz, bandwidth_hz: Hz) -> Vec<Allocation> {
        self.allocations
            .iter()
            .filter(|a| {
                a.region.eq_ignore_ascii_case(region)
                    && overlaps(a.min_hz, a.max_hz, center_hz, bandwidth_hz)
            })
            .cloned()
            .collect()
    }

    async fn signatures(&self, center_hz: Hz, bandwidth_hz: Hz) -> Vec<Signature> {
        self.signatures
            .iter()
            .filter(|s| {
                // A signature occupies its bandwidth around every centre in
                // `[min_hz, max_hz]`, not just that range.
                let half = s.bandwidth_hz.0 / 2;
                let (lo, hi) = (
                    s.min_hz.0.saturating_sub(half),
                    s.max_hz.0.saturating_add(half),
                );
                overlaps(Hz(lo), Hz(hi), center_hz, bandwidth_hz)
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers `rows` whatever it is asked.
    struct Fixed(Vec<Allocation>);

    #[async_trait]
    impl KnowledgeProvider for Fixed {
        fn name(&self) -> &str {
            "fixed"
        }
        async fn allocations(&self, _: &str, _: Hz, _: Hz) -> Vec<Allocation> {
            self.0.clone()
        }
        async fn signatures(&self, _: Hz, _: Hz) -> Vec<Signature> {
            Vec::new()
        }
    }

    fn row(kind: Option<&str>, service: &str, distance_km: Option<f64>) -> Allocation {
        Allocation {
            kind: kind.map(Into::into),
            service: service.into(),
            distance_km,
            ..Allocation::default()
        }
    }

    #[tokio::test]
    async fn nearest_allocations_keep_five_assignments_then_every_plan_row() {
        let mut first = vec![row(Some("plan"), "p1", None)];
        for d in [9.0, 3.0, 7.0, 1.0] {
            first.push(row(Some("assignment"), &format!("a{d}"), Some(d)));
        }
        let second = vec![
            row(Some("assignment"), "a-none", None),
            row(None, "seed", None),
            row(Some("assignment"), "a5", Some(5.0)),
            row(Some("assignment"), "a2", Some(2.0)),
            row(Some("plan"), "p2", None),
        ];
        let providers: Vec<Arc<dyn KnowledgeProvider>> =
            vec![Arc::new(Fixed(first)), Arc::new(Fixed(second))];
        let got = nearest_allocations(&providers, "AU", Hz(1), Hz(1)).await;
        let services: Vec<&str> = got.iter().map(|a| a.service.as_str()).collect();
        // The five nearest assignments (the distance-less and the farthest dropped),
        // then the plan rows, a kind-less one among them, in provider order.
        assert_eq!(services, ["a1", "a2", "a3", "a5", "a7", "p1", "seed", "p2"]);
    }

    #[tokio::test]
    async fn builtin_signatures_include_aprs_at_144_800() {
        let k = StaticKnowledge::builtin();
        let sigs = k.signatures(Hz(144_800_000), Hz(25_000)).await;
        let aprs = sigs
            .iter()
            .find(|s| s.name == "APRS")
            .expect("APRS present");
        assert_eq!(aprs.decoder.as_deref(), Some("aprs_decode"));
        assert!(k.signatures(Hz(100_000_000), Hz(25_000)).await.is_empty());
    }

    #[tokio::test]
    async fn lookup_near_aprs_within_its_bandwidth_matches() {
        let k = StaticKnowledge::builtin();
        let sigs = k.signatures(Hz(144_805_000), Hz(1_000)).await;
        assert!(sigs.iter().any(|s| s.name == "APRS"), "{sigs:?}");
        // Beyond APRS's 12.5 kHz (6.25 kHz either side of 144.800 MHz).
        let far = k.signatures(Hz(144_810_000), Hz(1_000)).await;
        assert!(!far.iter().any(|s| s.name == "APRS"), "{far:?}");
    }

    #[tokio::test]
    async fn allocations_filter_by_region_and_overlap() {
        let dir = tempfile::tempdir().unwrap();
        let (sp, ap) = (dir.path().join("s.json"), dir.path().join("a.json"));
        std::fs::write(&sp, "[]").unwrap();
        std::fs::write(
            &ap,
            r#"[{"provider":"p","region":"AU","min_hz":144000000,"max_hz":148000000,"service":"amateur","note":null}]"#,
        )
        .unwrap();
        let k = StaticKnowledge::from_files(&sp, Some(&ap)).unwrap();
        assert_eq!(
            k.allocations("au", Hz(145_000_000), Hz(1_000)).await.len(),
            1
        );
        assert!(
            k.allocations("EU", Hz(145_000_000), Hz(1_000))
                .await
                .is_empty()
        );
        assert!(
            k.allocations("AU", Hz(100_000_000), Hz(1_000))
                .await
                .is_empty()
        );
    }
}
