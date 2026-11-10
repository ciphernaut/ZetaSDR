//! Foundational types shared by every ZetaSDR crate.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Frequency or bandwidth in hertz.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    pub start_ns: Timestamp,
    pub end_ns: Timestamp,
}

macro_rules! ulid_id {
    ($name:ident) => {
        #[derive(
            Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
        )]
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
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    Iq,
    Audio,
    Video,
    Text,
    Event,
    Sink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
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

/// What a source can do. The fields after `push` came later: a record without
/// them (an older node's, or one cached before them) loads with their
/// defaults, and the schema still requires every field, since a record is
/// always written whole.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = crate::knowledge::require_all)]
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
    /// The part of a capture, centred, that the source's front-end filter
    /// passes flat, from 0 to 1: a scan advances by this much of each capture
    /// and masks the rest at the edges. Absent reads `asserted` 1.0.
    #[serde(default = "whole_capture")]
    pub usable_fraction: Claim<f64>,
    /// The sample depths the source's link can carry, each with its rate cap;
    /// empty when the source has one depth (or has not listed them).
    #[serde(default)]
    pub depths: Vec<Depth>,
}

/// [`Capabilities::usable_fraction`] when a record does not say: the whole
/// capture.
fn whole_capture() -> Claim<f64> {
    Claim::Asserted(1.0)
}

/// One sample depth a source's link can carry: `bits` per I and Q sample,
/// whether it holds every bit the ADC resolves (`lossless`), and the fastest
/// rate the link sustains at it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Depth {
    pub bits: u8,
    pub lossless: bool,
    pub max_bandwidth_hz: Claim<Hz>,
}

/// Schema of a required `Option<T>`: `T`'s schema inlined with `"null"` added
/// to its `type`. (`#[schemars(required)]` alone drops the `null`.) Use as
/// `#[schemars(required, schema_with = "crate::types::nullable::<T>")]`.
pub(crate) fn nullable<T: JsonSchema>(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let mut s = T::json_schema(g);
    let null = serde_json::Value::from("null");
    let ty = match s.get("type").cloned() {
        Some(serde_json::Value::Array(mut a)) => {
            if !a.contains(&null) {
                a.push(null);
            }
            a
        }
        Some(t) => vec![t, null],
        None => panic!("nullable::<{}>: schema has no `type`", T::schema_name()),
    };
    s.insert("type".into(), serde_json::Value::Array(ty));
    s
}

/// Where a source is, in degrees and metres.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct SourcePosition {
    pub lat: f64,
    pub lon: f64,
    /// Altitude in metres; `null` when not known.
    #[schemars(required, schema_with = "nullable::<f64>")]
    pub alt_m: Option<f64>,
    pub from: PositionFrom,
}

/// Where a [`SourcePosition`] came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PositionFrom {
    /// The source reported it itself (a KiwiSDR's status page).
    Status,
    /// The operator configured it (`--source-position`).
    Configured,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Provenance {
    pub source: SourceId,
    #[schemars(required, schema_with = "nullable::<JobId>")]
    pub job: Option<JobId>,
    #[schemars(required, schema_with = "nullable::<ChannelId>")]
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
            span: Span {
                start_ns: Timestamp(0),
                end_ns: Timestamp(1_000_000_000),
            },
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

    fn assert_all_required<T: JsonSchema>() {
        let s = serde_json::to_value(schemars::schema_for!(T)).unwrap();
        let mut props: Vec<String> = s["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let mut req: Vec<String> = s["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        props.sort();
        req.sort();
        assert_eq!(req, props);
        assert_eq!(s["additionalProperties"], serde_json::Value::Bool(false));
    }

    #[test]
    fn a_record_without_the_new_fields_loads_with_their_defaults() {
        // A record as a node before `usable_fraction` and `depths` wrote it.
        let old = serde_json::json!({
            "tune_min_hz": {"asserted": 1_000_000},
            "tune_max_hz": {"asserted": 6_000_000_000u64},
            "max_bandwidth_hz": {"measured": 30_720_000},
            "formats": ["cf32"],
            "channels": 1,
            "rx": true,
            "tx": false,
            "remote": false,
            "push": false,
        });
        let caps: Capabilities = serde_json::from_value(old).unwrap();
        assert_eq!(caps.usable_fraction, Claim::Asserted(1.0));
        assert!(caps.depths.is_empty());
        assert_eq!(caps.max_bandwidth_hz, Claim::Measured(Hz(30_720_000)));
        // Written whole, and read back the same.
        let mut caps = caps;
        caps.usable_fraction = Claim::Asserted(0.9);
        caps.depths = vec![Depth {
            bits: 12,
            lossless: true,
            max_bandwidth_hz: Claim::Measured(Hz(30_720_000)),
        }];
        let v = serde_json::to_value(&caps).unwrap();
        assert_eq!(v["usable_fraction"], serde_json::json!({"asserted": 0.9}));
        assert_eq!(
            v["depths"],
            serde_json::json!([{"bits": 12, "lossless": true, "max_bandwidth_hz": {"measured": 30_720_000}}])
        );
        assert_eq!(serde_json::from_value::<Capabilities>(v).unwrap(), caps);
    }

    #[test]
    fn schemas_require_every_non_optional_field() {
        assert_all_required::<Capabilities>();
        assert_all_required::<Depth>();
        assert_all_required::<Selection>();
        assert_all_required::<Span>();
    }
}
