//! Knowledge providers: what should be at a frequency, and what a signal looks like.

use std::path::Path;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::{Baud, Hz};

/// A band plan entry: what a region allocates `[min_hz, max_hz]` to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Allocation {
    pub provider: String,
    pub region: String,
    pub min_hz: Hz,
    pub max_hz: Hz,
    pub service: String,
    #[schemars(required)]
    pub note: Option<String>,
}

/// What a known signal looks like and which decoder handles it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
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
}

#[async_trait]
pub trait KnowledgeProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn allocations(&self, region: &str, center_hz: Hz, bandwidth_hz: Hz) -> Vec<Allocation>;
    async fn signatures(&self, center_hz: Hz, bandwidth_hz: Hz) -> Vec<Signature>;
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
fn overlaps(min: Hz, max: Hz, center: Hz, bandwidth: Hz) -> bool {
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
            .filter(|s| overlaps(s.min_hz, s.max_hz, center_hz, bandwidth_hz))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn builtin_signatures_include_aprs_at_144_800() {
        let k = StaticKnowledge::builtin();
        let sigs = k.signatures(Hz(144_800_000), Hz(25_000)).await;
        let aprs = sigs
            .iter()
            .find(|s| s.name == "APRS")
            .expect("APRS present");
        assert_eq!(aprs.decoder.as_deref(), Some("aprs"));
        assert!(k.signatures(Hz(100_000_000), Hz(25_000)).await.is_empty());
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
