//! Foundational types shared by every ZetaSDR crate.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Frequency or bandwidth in hertz.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Hz(pub u64);

/// Symbol rate in baud.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Baud(pub u32);

/// Nanoseconds since the UNIX epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Timestamp(pub u64);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Span {
    pub start: Timestamp,
    pub end: Timestamp,
}

macro_rules! ulid_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
        pub struct $name(pub String);

        impl $name {
            /// Generate a fresh ULID-backed id.
            pub fn new() -> Self {
                Self(ulid::Ulid::generate().to_string())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

ulid_id!(SourceId);
ulid_id!(JobId);
ulid_id!(StreamId);
ulid_id!(ChannelId);

/// Time-frequency rectangle on a source.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Selection {
    pub source: SourceId,
    pub span: Span,
    pub center_hz: Hz,
    pub bandwidth_hz: Hz,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum StreamKind {
    Iq,
    Audio,
    Video,
    Text,
    Event,
    Sink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SampleFormat {
    Ci8,
    Ci16,
    Cf32,
}

/// A value tagged with its origin: stated by a driver or measured on hardware.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Claim<T> {
    Asserted(T),
    Measured(T),
}

impl<T> Claim<T> {
    pub fn value(&self) -> &T {
        match self {
            Claim::Asserted(v) | Claim::Measured(v) => v,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Capabilities {
    pub tune_min_hz: Claim<Hz>,
    pub tune_max_hz: Claim<Hz>,
    pub max_bandwidth_hz: Claim<Hz>,
    pub formats: Vec<SampleFormat>,
    pub channels: u16,
    pub rx: bool,
    pub tx: bool,
    pub remote: bool,
    pub push: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Provenance {
    pub source: SourceId,
    pub job: Option<JobId>,
    pub channel: Option<ChannelId>,
    pub span: Span,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_roundtrips_json_with_hz_fields() {
        let s = Selection {
            source: SourceId("hackrf0".into()),
            span: Span { start: Timestamp(0), end: Timestamp(1_000_000_000) },
            center_hz: Hz(101_500_000),
            bandwidth_hz: Hz(200_000),
        };
        let j = serde_json::to_string(&s).unwrap();
        assert!(j.contains("\"center_hz\":101500000"));
        assert_eq!(serde_json::from_str::<Selection>(&j).unwrap(), s);
    }

    #[test]
    fn claim_exposes_value_and_tags_origin() {
        let c = Claim::Asserted(Hz(1));
        assert_eq!(c.value(), &Hz(1));
        assert!(serde_json::to_string(&c).unwrap().contains("asserted"));
    }

    #[test]
    fn ids_are_unique() {
        assert_ne!(JobId::new(), JobId::new());
    }

    #[test]
    fn capabilities_schema_forbids_extra_fields() {
        let s = serde_json::to_value(schemars::schema_for!(Capabilities)).unwrap();
        assert_eq!(s["additionalProperties"], serde_json::Value::Bool(false));
    }
}
