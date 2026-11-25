//! One mode's row: the schema of a `modes/*.json` file and its validation.
//!
//! A row names a decoder mode (`job_start --kind <id>`): its band, the layer-1 labels it
//! presents as, the decoder (a graph template, an external program or a frame chain),
//! its event schema and one-line summary, and the decode-to-identify rule. Every struct
//! rejects unknown fields; [`parse`] names the offending field, [`validate`] checks what
//! serde cannot.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::graph::{Edge, Graph, GraphId, Node};
use crate::stage::StageRegistry;
use crate::stream::LinkType;

/// A mode id: lowercase snake case, the string `job_start --kind` takes.
#[derive(
    Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ModeId(pub String);

/// The group a mode is listed under; ordered as declared (the tie-break of equal scores).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[schemars(rename = "ModeFamily")]
pub enum Family {
    Tv,
    Broadcast,
    Aviation,
    Maritime,
    Satellite,
    TwoWay,
    CellularRail,
    Pagers,
    Iot,
    WeatherScience,
    AmateurDigital,
    AmateurVoice,
    HfUtility,
    General,
}

/// One decoder mode as its row file states it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct ModeRow {
    pub id: ModeId,
    /// A short human name ("POCSAG pagers").
    pub name: String,
    pub family: Family,
    pub band: Band,
    /// The layer-1 labels this mode presents as (`fm`, `fsk`, `am`, `wfm`, `ssb`, `carrier`).
    pub modulation: Vec<String>,
    /// The layer-2 label a proof produces (`APRS`, `POCSAG`, ...).
    pub label: String,
    /// The `protocol` value of the mode's events (`aprs`, `pocsag`, ...).
    pub protocol: String,
    /// The row's own decoder; `null` for a row with none, which only a handler file's
    /// entry runs (the key is required all the same).
    #[serde(deserialize_with = "super::nullable")]
    pub decoder: Option<Decoder>,
    pub event: Event,
    pub identify: Identify,
    /// The iNTERCEPT catalogue entry ids this mode proves.
    pub catalogue: Vec<String>,
    /// How the row is checked (`zeta modes check`); `null` for none.
    #[serde(default)]
    #[schemars(schema_with = "row_test_schema")]
    pub test: Option<RowTest>,
    /// Free text the generated AGENTS.md carries for the mode; `null` for none.
    #[serde(default)]
    pub notes: Option<String>,
    /// The event fields holding the fix, added to each event as `position`; `null` for
    /// a mode whose events carry none.
    #[serde(default)]
    pub position: Option<PositionMap>,
    /// The event field holding the event's own time, an ISO 8601 UTC time
    /// (`YYYY-MM-DDTHH:MM:SS[.fff]Z`) that becomes `t_ns`; `null` for none.
    #[serde(default)]
    pub time: Option<String>,
    /// What the mode's events are about (the aircraft, the sonde, the station), how the
    /// page tabulates and folds them; `null` keeps one ribbon per event.
    #[serde(default)]
    pub subject: Option<Subject>,
    /// Fields filled from another field's text by a named extractor, only on events
    /// matching `when` and only when none of the `into` targets is set; empty for none.
    #[serde(default)]
    pub derive: Vec<Derive>,
    /// `restricted` marks a row whose events carry content a deployment may show to fewer
    /// people than the spectrum (pager text); copied onto every event as `access`; `null`
    /// for the rest. Nothing gates on it yet.
    #[serde(default)]
    pub access: Option<Access>,
    /// What the row carries (`network_traffic`); `null` for the rest. Reported by
    /// `modes_list`, written into a job's context as `content` and copied to a packet
    /// capture's record. Nothing reads it yet.
    #[serde(default)]
    pub content: Option<Content>,
}

/// How widely a row's events may be shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    // No variant doc: it would turn the schema into a `oneOf` with no top-level `type`,
    // which `types::nullable` needs (ModeInfo.access is a nullable `Access`).
    Restricted,
}

impl Access {
    /// The word as the row file and the events spell it.
    pub fn word(self) -> &'static str {
        match self {
            Access::Restricted => "restricted",
        }
    }
}

/// What a row carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Content {
    // No variant doc, as on `Access`: it would turn the schema into a `oneOf`.
    NetworkTraffic,
}

impl Content {
    /// The word as the row file and the job's context spell it.
    pub fn word(self) -> &'static str {
        match self {
            Content::NetworkTraffic => "network_traffic",
        }
    }
}

/// What a mode's events are about: the event field naming it, the table columns the
/// page shows per subject and how the waterfall draws it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, rename = "ModeSubject", transform = require_all)]
pub struct Subject {
    /// The event field that names the subject (`icao`, `id`, `src`); events with it null
    /// or blank are about no subject.
    pub key: String,
    /// The plural noun for the subjects ("aircraft", "sondes").
    pub name: String,
    /// How the waterfall draws the subjects: one summary line, or each folded to a line.
    pub waterfall: SubjectWaterfall,
    /// The table columns, in order.
    pub columns: Vec<SubjectColumn>,
    /// The event field holding the subject's heading in degrees; `null` for none.
    #[serde(default)]
    pub heading: Option<String>,
    /// The event field the subject is called by (a callsign); `null` for its key.
    #[serde(default)]
    pub label: Option<String>,
    /// The most points a subject's track holds on the map, at least 4 (100 when left out):
    /// at this count the older half loses every other point, its first kept.
    #[serde(default = "default_track_points")]
    pub track_points: u32,
    /// How far in metres a fix must be from the track's last kept point to join it, counting
    /// altitude where both have one (0 when left out: every fix joins). A turn of `heading`
    /// by more than 10 degrees joins it too.
    #[serde(default)]
    pub track_min_m: f64,
    /// The event fields whose equal values mean the same subject across rows sharing
    /// `name`; empty for none.
    #[serde(default)]
    pub merge: Vec<String>,
    /// Field = value pairs; a subject whose state matches every pair takes no part in
    /// `merge` (it keeps a row of its own); empty for none.
    #[serde(default)]
    pub merge_skip: BTreeMap<String, serde_json::Value>,
    /// How a subject is matched against the licence register, so a decoded identity can
    /// confirm a lit site; `null` for a subject the register has no field for.
    #[serde(default)]
    pub identity: Option<Identity>,
    /// The mode's fixed stations: events matching it come from a ground station, not a
    /// mobile subject; `null` for a mode with none.
    #[serde(default)]
    pub station: Option<Station>,
}

/// How a mode's events from fixed ground stations are told from the rest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, rename = "ModeStation", transform = require_all)]
pub struct Station {
    /// Field = value pairs; an event matching every pair is from a station. Not empty;
    /// no value `null`.
    pub when: BTreeMap<String, serde_json::Value>,
    /// The field naming the station on its own frame (a string trimmed, a number as
    /// printed); `null` when the protocol names none: the anonymous station `"*"`.
    #[serde(default)]
    pub key: Option<String>,
    /// On any other event, the field naming the station it is addressed to; `null` for
    /// none. Not read on a station's own frame, where it names the other party.
    #[serde(default)]
    pub addressed: Option<String>,
}

impl Station {
    /// Whether `event` is from a station: an object holding every `when` field, equal.
    pub fn matches(&self, event: &serde_json::Value) -> bool {
        let Some(o) = event.as_object() else {
            return false;
        };
        self.when.iter().all(|(k, v)| o.get(k) == Some(v))
    }
}

/// How a mode's decoded subject is matched against the register (design 4.6): the event
/// field's value, after `strip`, compared trimmed and case-insensitively with the
/// register field `register` of each lit site.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, rename = "ModeIdentity", transform = require_all)]
pub struct Identity {
    /// The event field holding the identity (`src`).
    pub field: String,
    /// The `Allocation` field it is compared with: only `callsign`.
    pub register: String,
    /// What to remove before comparing: `ssid`, an AX.25 `-N` suffix; `null` for nothing.
    #[serde(default)]
    pub strip: Option<String>,
}

/// The register fields an [`Identity`] may name.
pub const IDENTITY_REGISTERS: [&str; 1] = ["callsign"];
/// The strips an [`Identity`] may name.
pub const IDENTITY_STRIPS: [&str; 1] = ["ssid"];

/// `subject.track_points` when a row leaves it out.
fn default_track_points() -> u32 {
    100
}

/// The fewest points a track may be capped at: thinning needs an older half of two.
const MIN_TRACK_POINTS: u32 = 4;

/// How the waterfall draws a mode's subjects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename = "SubjectWaterfall")]
pub enum SubjectWaterfall {
    /// One line for the job: the protocol, the count and the last label.
    Summary,
    /// One line per subject.
    Fold,
}

/// One column of a subject table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, rename = "SubjectColumn", transform = require_all)]
pub struct SubjectColumn {
    /// The event field shown.
    pub field: String,
    /// The column heading.
    pub label: String,
    /// The unit the field is in, so the page can show it as the operator reads it;
    /// `null` for text or a bare number.
    #[serde(default)]
    pub unit: Option<SubjectUnit>,
}

/// The unit of a subject column's field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "SubjectUnit")]
pub enum SubjectUnit {
    #[serde(rename = "ft")]
    Ft,
    #[serde(rename = "m")]
    M,
    #[serde(rename = "kt")]
    Kt,
    #[serde(rename = "km_h")]
    KmH,
    #[serde(rename = "m_s")]
    MS,
    #[serde(rename = "deg")]
    Deg,
}

/// The unit of a position map's altitude field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "AltUnit")]
pub enum AltUnit {
    /// Metres.
    #[default]
    #[serde(rename = "m")]
    M,
    /// Feet; `position.alt_m` is converted.
    #[serde(rename = "ft")]
    Ft,
}

/// Which event fields hold a fix: latitude and longitude in degrees, altitude in metres
/// (or in feet, by `alt_unit`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, rename = "ModePosition", transform = require_all)]
pub struct PositionMap {
    pub lat: String,
    pub lon: String,
    /// `null` for a mode whose events carry no altitude.
    #[serde(default)]
    pub alt_m: Option<String>,
    /// The unit of the `alt_m` field (`m` when left out); `position.alt_m` is metres.
    #[serde(default)]
    pub alt_unit: AltUnit,
}

/// One `derive` entry: on an event matching `when`, `extract` reads the string field
/// `from` and its outputs are written to the event fields `into` names, unless one of
/// those fields already holds a value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, rename = "ModeDerive", transform = require_all)]
pub struct Derive {
    /// Field = value pairs, every one of which an event must match for the entry to run;
    /// empty for every event.
    #[serde(default)]
    pub when: BTreeMap<String, serde_json::Value>,
    /// The event field whose string value the extractor reads.
    pub from: String,
    pub extract: Extractor,
    /// Extractor output = event field it is written to; an output left out is dropped.
    pub into: BTreeMap<String, String>,
}

/// The extractors a `derive` entry may name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename = "ModeExtractor")]
pub enum Extractor {
    // No variant doc, as on `Access`: it would turn the schema into a `oneOf`.
    AcarsPosition,
}

impl Extractor {
    /// The word as the row file spells it.
    pub fn word(self) -> &'static str {
        match self {
            Extractor::AcarsPosition => "acars_position",
        }
    }

    /// The outputs the extractor may yield, the keys `into` may name.
    pub fn outputs(self) -> &'static [&'static str] {
        match self {
            Extractor::AcarsPosition => &["lat", "lon", "alt_ft"],
        }
    }
}

/// The channel a mode's decoder expects.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, rename = "ModeBand", transform = require_all)]
pub struct Band {
    /// The channel width the decoder expects.
    pub bandwidth_hz: u64,
    /// The well-known centres; empty for "anywhere".
    pub centers_hz: Vec<u64>,
    /// The narrowest `bandwidth_hz` a job may give the mode; `null` for no minimum.
    #[serde(default)]
    pub min_hz: Option<u64>,
    /// Where identify tries the mode: one span `[lo, hi]` (radiosondes in 400-406 MHz) or
    /// a list of them (`[[lo, hi], ...]`, pagers on VHF and UHF); `null` for anywhere.
    #[serde(default)]
    #[schemars(schema_with = "ranges_schema")]
    pub range_hz: Option<Ranges>,
    /// How a choice sizes and centres this row's job from a measured signal; `null`
    /// for the rest (they take the measured band as it is).
    #[serde(default)]
    pub width_rule: Option<WidthRule>,
}

/// How a job of a demodulator row is sized and centred from a measurement.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, rename = "ModeWidthRule", transform = require_all)]
pub struct WidthRule {
    /// The measured width times this, when the measurement is trusted.
    #[serde(default)]
    pub scale: Option<f64>,
    /// The narrowest width the rule gives.
    #[serde(default)]
    pub floor_hz: Option<u64>,
    /// A fixed width, whatever was measured.
    #[serde(default)]
    pub fixed_hz: Option<u64>,
    /// Layer-1 labels whose measured width is not trusted under the floor.
    #[serde(default)]
    pub distrust_below_floor: Vec<String>,
    /// The widest `fsk` signal the row serves; `null` for no limit. Above it an `fsk`
    /// label is a data signal, not a weak FM voice whose sidebands layer 1 missed.
    #[serde(default)]
    pub voice_max_hz: Option<u64>,
    /// Where the job is centred.
    #[serde(default)]
    pub center: CenterRule,
}

/// Where a demodulator job is centred.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename = "ModeCenterRule")]
pub enum CenterRule {
    /// On the measured centre.
    #[default]
    Measured,
    /// On layer 1's SSB dial (the suppressed carrier it reads off the measured signal
    /// and its sideband); the measured centre when layer 1 read no sideband.
    Dial,
}

/// What a row is for: a decoder proves a signal by what it decodes; a demodulator
/// (a `builtin` row) turns the signal to audio and proves nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename = "ModeRole")]
pub enum Role {
    // No variant docs: a `oneOf` would lose the top-level `type` `nullable` needs.
    #[default]
    Decoder,
    Demodulator,
}

/// The built-in demodulator kinds a `builtin` row may name.
pub const DEMOD_KINDS: [&str; 4] = ["fm_demod", "wfm_demod", "am_demod", "ssb_demod"];

impl Band {
    /// Whether a box `bandwidth_hz` wide is wide enough for this mode: its `min_hz`,
    /// else its `bandwidth_hz`, is no wider.
    pub fn fits(&self, bandwidth_hz: u64) -> bool {
        self.min_hz.unwrap_or(self.bandwidth_hz) <= bandwidth_hz
    }
}

/// A row's `band.range_hz`: the spans `[lo, hi]` (inclusive) it is found anywhere in.
/// Read as one span `[lo, hi]` or a list of them; written as one span when it holds one
/// (as every row read before lists were), so a client that knows only that form still
/// reads a one-range row. [`validate`] refuses an empty list and a span with `lo >= hi`;
/// spans may overlap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ranges(pub Vec<[u64; 2]>);

impl Ranges {
    /// Whether `hz` lies in any of the spans.
    pub fn contains(&self, hz: u64) -> bool {
        self.0.iter().any(|&[lo, hi]| (lo..=hi).contains(&hz))
    }
}

impl From<[u64; 2]> for Ranges {
    fn from(span: [u64; 2]) -> Self {
        Ranges(vec![span])
    }
}

impl Serialize for Ranges {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.0.as_slice() {
            [one] => one.serialize(s),
            all => all.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Ranges {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged, expecting = "a range [lo, hi] or a list of them")]
        enum Form {
            One([u64; 2]),
            Many(Vec<[u64; 2]>),
        }
        Ok(match Form::deserialize(d)? {
            Form::One(span) => Ranges(vec![span]),
            Form::Many(spans) => Ranges(spans),
        })
    }
}

/// Schema of `band.range_hz`: one `[lo, hi]`, a non-empty list of them, or `null`.
fn ranges_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let span = g.subschema_for::<[u64; 2]>();
    schemars::json_schema!({
        "anyOf": [
            span,
            {"type": "array", "items": span, "minItems": 1},
            {"type": "null"},
        ]
    })
}

/// The decoder: a graph template, an external program, or a frame chain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(deny_unknown_fields, rename = "ModeDecoder")]
// Rows are parsed once and held in the registry: the size of the largest variant costs nothing.
#[allow(clippy::large_enum_variant)]
pub enum Decoder {
    /// Today's `Graph` shape without its id; the channel node's `center_hz`,
    /// `bandwidth_hz` and `out_rate_hz` are written `"$center_hz"`, `"$bandwidth_hz"`,
    /// `"$out_rate_hz"` for the expander to fill.
    #[schemars(transform = require_all)]
    Graph {
        nodes: Vec<Node>,
        edges: Vec<Edge>,
        /// Programs a stage of the graph needs on the host, probed at startup as a
        /// program row's program is: a missing one makes the row `unavailable`, unless
        /// it is `optional`.
        #[serde(default)]
        handlers: Vec<Handler>,
    },
    Program(ProgramBlock),
    /// `channel → resample → <demod> → clock_recover → slicer → frame_decode`: the
    /// demodulator and the framing `frame_decode` cuts frames by.
    Frame(FrameBlock),
    /// A built-in demodulator the node runs itself, with the row's `params` as its
    /// defaults.
    Builtin(BuiltinBlock),
}

/// A `builtin` decoder: the name of a built-in demodulator kind and its default
/// parameters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct BuiltinBlock {
    /// One of [`DEMOD_KINDS`].
    pub builtin: String,
    /// The kind's parameters (`deviation_hz`, `audio_lowpass_hz`, ...).
    #[serde(default)]
    pub params: serde_json::Map<String, serde_json::Value>,
}

/// Which kind of decoder a row has, as `modes_list` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecoderKind {
    Graph,
    Program,
    Frame,
    Builtin,
}

impl Decoder {
    pub fn kind(&self) -> DecoderKind {
        match self {
            Decoder::Graph { .. } => DecoderKind::Graph,
            Decoder::Program(_) => DecoderKind::Program,
            Decoder::Frame(_) => DecoderKind::Frame,
            Decoder::Builtin(_) => DecoderKind::Builtin,
        }
    }

    /// The programs the row needs on the host, each with where to get it: a program
    /// row's one block (never optional), a graph row's `handlers` in order; none for a
    /// plain graph row or a frame row (its chain is all in-process stages).
    pub fn handler_blocks(&self) -> Vec<HandlerBlock<'_>> {
        match self {
            Decoder::Program(p) => vec![HandlerBlock {
                programs: &p.programs,
                install: &p.install,
                optional: false,
            }],
            Decoder::Graph { handlers, .. } => handlers
                .iter()
                .map(|h| HandlerBlock {
                    programs: &h.programs,
                    install: &h.install,
                    optional: h.optional,
                })
                .collect(),
            Decoder::Frame(_) | Decoder::Builtin(_) => vec![],
        }
    }
}

impl ModeRow {
    /// What the row is for: a demodulator when its decoder is `builtin`.
    pub fn role(&self) -> Role {
        match self.decoder {
            Some(Decoder::Builtin(_)) => Role::Demodulator,
            _ => Role::Decoder,
        }
    }

    /// The kind of the row's own decoder; `None` for a row with none.
    pub fn decoder_kind(&self) -> Option<DecoderKind> {
        self.decoder.as_ref().map(Decoder::kind)
    }

    /// [`Decoder::handler_blocks`] of the row's own decoder; none for a row with none.
    pub fn handler_blocks(&self) -> Vec<HandlerBlock<'_>> {
        self.decoder
            .as_ref()
            .map(Decoder::handler_blocks)
            .unwrap_or_default()
    }
}

/// One program list a row needs on the host, as [`Decoder::handler_blocks`] gives it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HandlerBlock<'a> {
    pub programs: &'a [String],
    pub install: &'a Install,
    /// The row runs without it (a stage then does without what it gives).
    pub optional: bool,
}

/// A program a stage of a graph row needs on the host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, rename = "ModeHandler", transform = require_all)]
pub struct Handler {
    /// Names tried on `PATH` in order (or an absolute path), the first found used.
    pub programs: Vec<String>,
    pub install: Install,
    /// Whether the row runs without it: a missing optional handler leaves the row
    /// `ready`, listed in its `missing`, rather than `unavailable` (`false` when left
    /// out).
    #[serde(default)]
    pub optional: bool,
}

/// A frame row's decoder: the demodulator, the framing and what reads each frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct FrameBlock {
    pub demod: FrameDemod,
    /// `frame_decode`'s params (sync, length, CRC, line code, whitening, bit order),
    /// checked by that stage.
    pub framing: serde_json::Value,
    /// What reads each frame's bytes, one of [`FRAME_PARSERS`] (`opaque` when left out:
    /// the bytes as they are).
    #[serde(default = "default_parser")]
    pub parser: String,
}

/// The demodulator a frame row's chain runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, rename = "FrameDemod", transform = require_all)]
pub struct FrameDemod {
    pub modulation: crate::bits::Modulation,
    pub symbol_rate_baud: u32,
    /// Symbols' levels: 2 or 4 for fsk, 2, 4 or 8 for psk, 2 for ook (2 when left out).
    #[serde(default = "default_order")]
    pub order: u8,
    /// FSK's tone spacing; `null` for the demodulator's own estimate.
    #[serde(default)]
    pub tone_spacing_hz: Option<u64>,
}

/// `decoder.parser` when a frame row leaves it out.
fn default_parser() -> String {
    "opaque".into()
}

/// `decoder.demod.order` when a frame row leaves it out.
fn default_order() -> u8 {
    2
}

/// The mode's events: their JSON Schema and the one-line summary template.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, rename = "ModeEvent")]
pub struct Event {
    /// The event payload's JSON Schema; `properties` names every field.
    pub schema: serde_json::Value,
    /// `{field}` substituted, `{field?}` omitted with its separator when null,
    /// `{a?b}` the first of `a`, `b` that is not null; any of them may end in a number
    /// format, `:.N` or `:+.N` (`N` 0-6 decimals, `{lat?:.4}`).
    pub summary: String,
}

/// The decode-to-identify rule.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, rename = "ModeIdentify", transform = require_all)]
pub struct Identify {
    pub proof: Proof,
    /// With `proof: field`, the boolean event field that must be true.
    #[serde(default)]
    pub field: Option<String>,
    pub min_events: u32,
    /// How long the `identify` job waits for proof.
    pub dwell_ms: u64,
    /// The shortest selection `classify` tries the row on.
    pub min_span_ms: u64,
    /// The in-crate attempt `classify` runs on a buffered span (`aprs`, `pocsag`,
    /// `acars`, `rds`, `ais`, or `frame` on a frame row: one CRC-clean frame of its chain);
    /// `null` for rows tried only by the `identify` job.
    #[serde(default)]
    pub quick: Option<String>,
    /// Among rows of equal score, the higher priority is tried first (0 when absent).
    #[serde(default)]
    pub priority: i32,
    /// Tried on the `identify` band as it was asked, never re-centred on the measured
    /// centre: for a decoder that places itself inside its band and needs all of it (a
    /// first-layer measurement of an uneven wide signal can be of a part of it). `false`
    /// when absent.
    #[serde(default)]
    pub as_asked: bool,
    /// Passed over by identify's own choice of rows, classify's candidates and a watch's
    /// default modes, and by the band an identify run takes, unless the caller names the
    /// row (identify's `candidates`, a watch's `modes`, a set or plan member): for a row
    /// that is heavy to try and has nothing in the band to say it is there. A job started
    /// by hand is unchanged. `false` when absent.
    #[serde(default)]
    pub when_named: bool,
}

/// What counts as proof: any event, or an event whose `identify.field` is true.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename = "ModeProof")]
pub enum Proof {
    Event,
    Field,
    /// A demodulator row: nothing it produces proves anything.
    None,
}

/// An external decoder program.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct ProgramBlock {
    /// Names tried on `PATH` in order (or an absolute path), the first found used.
    pub programs: Vec<String>,
    /// Arguments, with `$port`, `$packet_port` (a second port zeta picks, for `packets`),
    /// `$rate_hz`, `$center_hz`, `$center_khz` (the centre in kHz, an integer when it
    /// divides), `$bandwidth_hz` (the job's band width in Hz, a whole number) and `$fifo` (the named pipe of `input.via: fifo`) substituted.
    pub args: Vec<String>,
    pub input: ProgramInput,
    pub output: ProgramOutput,
    pub install: Install,
    /// The demodulator between the channel and an `input.stream: audio` program;
    /// required for audio input, `null` for iq.
    #[serde(default)]
    pub demod: Option<Demod>,
    /// The program's binary packet output beside its events, read into the job's packet
    /// stream; `null` when it gives none.
    #[serde(default)]
    pub packets: Option<ProgramPackets>,
}

/// A program's second, binary output: whole packets, cut by `parser`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct ProgramPackets {
    /// `tcp`, or `stdout` when the events come over `tcp`.
    pub via: OutputVia,
    /// With `via: tcp`, `"$packet_port"` (the second port zeta picks, which an argument
    /// must name); `null` with `via: stdout`.
    #[serde(default)]
    pub port: Option<String>,
    pub parser: PacketParserKind,
    /// The link type the node gives a link-type number the stream carries (a pcap
    /// header's), keyed by the number in decimal as written plainly (`147`, never `0147`);
    /// a number not listed is read as itself.
    #[serde(default)]
    pub link_types: BTreeMap<String, LinkType>,
}

impl ProgramPackets {
    /// `link_types` keyed by number; a key that is not a decimal `u16` is left out (a
    /// validated row has none).
    pub fn link_type_numbers(&self) -> BTreeMap<u16, LinkType> {
        self.link_types
            .iter()
            .filter_map(|(k, v)| Some((link_type_number(k)?, *v)))
            .collect()
    }
}

/// A `link_types` key's number: decimal digits only, at most 65535.
fn link_type_number(key: &str) -> Option<u16> {
    match key.bytes().all(|b| b.is_ascii_digit()) {
        true => key.parse().ok(),
        false => None,
    }
}

/// The demodulator an audio program row's channel runs through.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(deny_unknown_fields, rename = "ModeDemod")]
pub enum Demod {
    /// Narrowband FM (`nbfm_demod`, squelch off): peak deviation and audio rate.
    Nbfm {
        deviation_hz: u64,
        audio_rate_hz: u64,
    },
}

/// What the program reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct ProgramInput {
    pub stream: InputStream,
    pub format: SampleFormat,
    /// The rate the program expects; the channel node resamples to it.
    pub rate_hz: u64,
    pub via: InputVia,
    /// `raw` samples (the default), or `wav`: a header first (PCM formats only).
    #[serde(default)]
    pub container: Container,
}

/// How the samples are framed on the program's input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename = "ProgramContainer")]
pub enum Container {
    /// Bare samples.
    #[default]
    Raw,
    /// A WAV header, then the samples.
    Wav,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InputStream {
    Iq,
    Audio,
}

/// A program's input sample format: `cu8`, `cs8`, `cs16`, `cf32` for IQ, `s16`, `f32`
/// for audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename = "ProgramSampleFormat")]
pub enum SampleFormat {
    Cu8,
    Cs8,
    Cs16,
    Cf32,
    S16,
    F32,
}

impl SampleFormat {
    /// Whether this is an IQ format (`cu8`, `cs8`, `cs16`, `cf32`), not an audio one.
    pub fn is_iq(self) -> bool {
        matches!(self, Self::Cu8 | Self::Cs8 | Self::Cs16 | Self::Cf32)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InputVia {
    Stdin,
    Fifo,
}

/// What the program writes and how zeta reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct ProgramOutput {
    pub via: OutputVia,
    /// With `via: tcp`, the port (`"$port"`, the one zeta picks); `null` otherwise.
    #[serde(default)]
    pub port: Option<String>,
    pub parser: Parser,
    /// Event field ← JSON pointer(s) into the parsed line, the first that resolves to a
    /// non-null value winning; `null` takes the whole object.
    #[serde(default)]
    pub fields: Option<BTreeMap<String, FieldPath>>,
}

/// Where in a program's parsed line an event field is read, in one of three forms:
/// - one JSON pointer;
/// - a list of pointers tried in order;
/// - an object `{"pointers": [...], "trim": "."}`: the list, with the characters of `trim`
///   stripped from both ends of a string result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum FieldPath {
    /// One JSON pointer (`/vdl2/avlc/src/addr`).
    One(String),
    /// JSON pointers tried in order, the first resolving to a non-null value winning.
    First(Vec<String>),
    /// The pointers tried in order, a string result stripped of the `trim` characters.
    Trimmed(TrimmedPaths),
}

/// The object form of [`FieldPath`]: pointers tried in order, and the characters stripped
/// from both ends of a string result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct TrimmedPaths {
    pub pointers: Vec<String>,
    pub trim: String,
}

impl FieldPath {
    /// The pointer, or the pointers in the order they are tried.
    pub fn pointers(&self) -> &[String] {
        match self {
            FieldPath::One(p) => std::slice::from_ref(p),
            FieldPath::First(ps) => ps,
            FieldPath::Trimmed(t) => &t.pointers,
        }
    }

    /// The characters stripped from both ends of a string value, for the trimmed form.
    pub fn trim(&self) -> Option<&str> {
        match self {
            FieldPath::Trimmed(t) => Some(&t.trim),
            _ => None,
        }
    }

    /// Check the path of event field `field`: a list is not empty, and every pointer
    /// starts with `/`.
    pub fn check(&self, field: &str) -> Result<(), RowError> {
        let at = format!("decoder.output.fields.{field}");
        if self.pointers().is_empty() {
            return Err(RowError::new(at, "an empty list"));
        }
        if self.pointers().iter().any(|p| !p.starts_with('/')) {
            return Err(RowError::new(at, "a JSON pointer starts with \"/\""));
        }
        if self.trim() == Some("") {
            return Err(RowError::new(at, "an empty trim"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutputVia {
    Stdout,
    Tcp,
}

/// How a program's binary packet output is cut into packets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PacketParserKind {
    Pcap,
    Beast,
}

/// How a program's output lines become events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename = "ProgramParser")]
pub enum Parser {
    JsonLines,
    Sbs,
    Nmea,
    Kiss,
    Text,
    Multimon,
}

/// Where to get a missing program.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct Install {
    /// Package names; each optional in a row file, `null` when absent.
    #[serde(default)]
    pub debian: Option<String>,
    #[serde(default)]
    pub fedora: Option<String>,
    #[serde(default)]
    pub brew: Option<String>,
    /// A snap package (`snap install <snap>`).
    #[serde(default)]
    pub snap: Option<String>,
    /// A Flatpak reference (`flatpak install <flatpak>`). An AppImage has no field: it is
    /// a download, named by `url`.
    #[serde(default)]
    pub flatpak: Option<String>,
    /// A script in this repository that builds and installs the program, run from a
    /// source checkout (`tools/install-vocoders.sh`); `null` when absent.
    #[serde(default)]
    pub script: Option<String>,
    pub url: String,
}

impl Install {
    /// How to get the program, ahead of the URL, for this host: [`Install::hint_for`] over
    /// the package managers on its `PATH` (probed once per process).
    pub fn hint(&self) -> Option<String> {
        self.hint_for(super::handlers::host_package_managers())
    }

    /// One command to get the program: `bash <script>` when the row names a script, else
    /// the install command of the first of `apt`, `dnf`, `brew`, `snap`, `flatpak` that is
    /// in `host` and has an entry in the row; when none of the host's managers has one,
    /// the first of those the row has an entry for (so the hint still names something).
    /// `None` when the row names no script and no package.
    pub fn hint_for(&self, host: &[&str]) -> Option<String> {
        if let Some(s) = &self.script {
            return Some(format!("bash {s}"));
        }
        let entries = [
            ("apt", &self.debian),
            ("dnf", &self.fedora),
            ("brew", &self.brew),
            ("snap", &self.snap),
            ("flatpak", &self.flatpak),
        ];
        let command = |(tool, pkg): &(&str, &Option<String>)| {
            pkg.as_ref().map(|p| format!("{tool} install {p}"))
        };
        entries
            .iter()
            .filter(|e| host.contains(&e.0))
            .find_map(command)
            .or_else(|| entries.iter().find_map(command))
    }
}

/// A row's check: the signal (`kind`) plus the event it must produce.
/// JSON: `{"kind": "synth"|"fixture", ..., "expect": {...}, "within_ms": N}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RowTest {
    #[serde(flatten)]
    pub kind: TestSpec,
    /// The event (or the subset of its fields) the decoder must emit.
    pub expect: serde_json::Value,
    pub within_ms: u64,
    /// The job's band for the check, when the row's own `band.bandwidth_hz` is wider than
    /// the signal to synthesise (a video row's 24 MHz band, an 8 MHz carrier).
    #[serde(default)]
    pub bandwidth_hz: Option<u64>,
}

/// Make every property of an object schema required, without a `default`: a field a row
/// file may leave out (`#[serde(default)]`) is always present, `null` when absent, in
/// what the node reports.
pub(crate) fn require_all(schema: &mut schemars::Schema) {
    let Some(props) = schema.get_mut("properties").and_then(|p| p.as_object_mut()) else {
        return;
    };
    for p in props.values_mut() {
        if let Some(p) = p.as_object_mut() {
            p.remove("default");
        }
    }
    let names: Vec<serde_json::Value> = props.keys().map(|k| k.as_str().into()).collect();
    schema.insert("required".into(), names.into());
}

/// Schema of `ModeRow.test`: an object (`kind`, the signal's parameters, `expect`,
/// `within_ms`) whose parameters depend on the mode, or `null`.
fn row_test_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["object", "null"],
        "description": "How the row is checked: `{\"kind\": \"synth\"|\"fixture\", ..., \"expect\": {...}, \"within_ms\": N}`; `null` for none."
    })
}

/// The signal a row's check runs on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TestSpec {
    /// Parameters for the registry's synthesiser.
    Synth(serde_json::Map<String, serde_json::Value>),
    /// A SigMF pair: under the repo's fixtures for embedded rows, absolute for operator rows.
    Fixture { path: String },
}

/// Why a row is broken: the offending field and what is wrong with it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {error}")]
pub struct RowError {
    pub field: String,
    pub error: String,
}

impl RowError {
    fn new(field: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            error: error.into(),
        }
    }
}

/// The integer placeholders a graph template may use, each with the dummy value it is
/// checked with; `"$audio_policy"` ([`super::template::AUDIO_POLICY_PLACEHOLDER`]) is the
/// one string placeholder, checked with the default policy's name.
const GRAPH_PLACEHOLDERS: [(&str, u64); 3] = [
    ("$center_hz", 100_000_000),
    ("$bandwidth_hz", 12_500),
    ("$out_rate_hz", 48_000),
];
/// The placeholders a program's `args` may use.
const PROGRAM_PLACEHOLDERS: [&str; 7] = [
    "$port",
    "$packet_port",
    "$rate_hz",
    "$center_hz",
    "$center_khz",
    "$bandwidth_hz",
    "$fifo",
];
/// The event fields the node adds after a program's `decoder.output.fields` map, exempt
/// from the rule that the map names every schema property.
pub const ENVELOPE_FIELDS: [&str; 4] = ["protocol", "summary", "t_ns", "position"];
/// The in-crate attempts `identify.quick` may name (`frame` on a frame row only).
pub const QUICK_ATTEMPTS: [&str; 6] = ["aprs", "pocsag", "acars", "rds", "frame", "ais"];
/// The parsers a frame row's `decoder.parser` may name.
pub const FRAME_PARSERS: &[&str] = &["opaque", "modbus_rtu", "dnp3", "iec101", "iec104"];
/// The stage a frame row's framing is checked by and runs in.
pub(crate) const FRAME_DECODE_STAGE: &str = "frame_decode";
/// Name suffixes of fields that hold an integer in their unit.
const UNIT_SUFFIXES: [&str; 4] = ["_hz", "_ms", "_ns", "_baud"];

/// Parse one row file. A serde failure names the unknown or missing field when serde
/// says which (`row` otherwise).
pub fn parse(text: &str) -> Result<ModeRow, RowError> {
    serde_json::from_str(text).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("unknown variant")
            && let Some(field) = bad_word_field(text)
        {
            return RowError::new(field, msg);
        }
        if msg.contains("invalid")
            && let Some(field) = bad_track_field(text).or_else(|| bad_handler_field(text))
        {
            return RowError::new(field, msg);
        }
        let named = ["unknown field `", "missing field `"]
            .iter()
            .find_map(|p| msg.split_once(p))
            .and_then(|(_, rest)| rest.split_once('`'))
            .map(|(name, _)| name.to_string());
        RowError::new(named.unwrap_or_else(|| "row".into()), msg)
    })
}

/// The field whose value is not one of its allowed words (`subject.waterfall`,
/// `subject.columns[<i>].unit`, `position.alt_unit`, `access`, `content`, `derive[<i>].extract`),
/// read from the row's JSON.
fn bad_word_field(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let word = |x: &serde_json::Value, ok: &[&str]| x.as_str().is_none_or(|w| !ok.contains(&w));
    if let Some(a) = v.get("access")
        && !a.is_null()
        && word(a, &[Access::Restricted.word()])
    {
        return Some("access".into());
    }
    if let Some(c) = v.get("content")
        && !c.is_null()
        && word(c, &[Content::NetworkTraffic.word()])
    {
        return Some("content".into());
    }
    if let Some(a) = v.pointer("/position/alt_unit")
        && word(a, &["m", "ft"])
    {
        return Some("position.alt_unit".into());
    }
    let extractors = [Extractor::AcarsPosition.word()];
    for (i, d) in v
        .get("derive")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .enumerate()
    {
        if let Some(x) = d.get("extract")
            && word(x, &extractors)
        {
            return Some(format!("derive[{i}].extract"));
        }
    }
    let subject = v.get("subject")?;
    if let Some(w) = subject.get("waterfall")
        && word(w, &["summary", "fold"])
    {
        return Some("subject.waterfall".into());
    }
    let units = ["ft", "m", "kt", "km_h", "m_s", "deg"];
    for (i, c) in subject.get("columns")?.as_array()?.iter().enumerate() {
        if let Some(u) = c.get("unit")
            && !u.is_null()
            && word(u, &units)
        {
            return Some(format!("subject.columns[{i}].unit"));
        }
    }
    None
}

/// The track field holding what its type cannot (`subject.track_points`,
/// `subject.track_min_m`), read from the row's JSON.
fn bad_track_field(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let subject = v.get("subject")?;
    if let Some(n) = subject.get("track_points")
        && n.as_u64().and_then(|n| u32::try_from(n).ok()).is_none()
    {
        return Some("subject.track_points".into());
    }
    if let Some(m) = subject.get("track_min_m")
        && !m.is_number()
    {
        return Some("subject.track_min_m".into());
    }
    None
}

/// The handler field holding what its type cannot (`decoder.handlers[<i>].optional`, a
/// boolean), read from the row's JSON.
fn bad_handler_field(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let handlers = v.pointer("/decoder/handlers")?.as_array()?;
    handlers.iter().enumerate().find_map(|(i, h)| {
        h.get("optional")
            .is_some_and(|o| !o.is_boolean())
            .then(|| format!("decoder.handlers[{i}].optional"))
    })
}

/// Check what serde cannot: the id's spelling, unit-suffixed fields holding integers,
/// the summary naming only schema fields, the identify rule, and the decoder (a graph
/// template type-checks against `stages` with its placeholders filled; a program's
/// placeholders, port and input are consistent).
pub fn validate(row: &ModeRow, stages: &StageRegistry) -> Result<(), RowError> {
    if !is_mode_id(&row.id.0) {
        return Err(RowError::new(
            "id",
            format!("{:?} must match ^[a-z][a-z0-9_]*$", row.id.0),
        ));
    }
    // A built-in kind's name maps to that kind, never to the row: only the five decoder
    // kinds expand from rows under their own names.
    const DECODERS: [&str; 5] = [
        "aprs_decode",
        "pocsag_decode",
        "rds_decode",
        "acars_decode",
        "adsb_decode",
    ];
    let own_builtin = matches!(&row.decoder, Some(Decoder::Builtin(b))
        if b.builtin == row.id.0 && DEMOD_KINDS.contains(&b.builtin.as_str()));
    if !DECODERS.contains(&row.id.0.as_str())
        && !own_builtin
        && crate::ops::BUILTIN_KINDS
            .iter()
            .any(|k| k.name() == row.id.0)
    {
        return Err(RowError::new(
            "id",
            format!("{} is a built-in job kind", row.id.0),
        ));
    }
    let properties = row.event.schema["properties"]
        .as_object()
        .ok_or_else(|| RowError::new("event.schema", "must be an object with `properties`"))?;
    for (name, prop) in properties {
        if has_unit_suffix(name) && !declares_integer(prop) {
            return Err(RowError::new(
                format!("event.schema.properties.{name}"),
                "a field named for its unit must be an integer",
            ));
        }
    }
    if let Some(ranges) = &row.band.range_hz {
        if ranges.0.is_empty() {
            return Err(RowError::new(
                "band.range_hz",
                "must list at least one range",
            ));
        }
        if let Some([lo, hi]) = ranges.0.iter().find(|[lo, hi]| lo >= hi) {
            return Err(RowError::new(
                "band.range_hz",
                format!("[{lo}, {hi}] must be [lo, hi] with lo < hi"),
            ));
        }
    }
    if let Some(w) = &row.band.width_rule {
        if let Some(scale) = w.scale
            && (scale.is_nan() || scale <= 0.0)
        {
            return Err(RowError::new(
                "band.width_rule.scale",
                format!("{scale} must be above 0"),
            ));
        }
        if w.fixed_hz == Some(0) {
            return Err(RowError::new("band.width_rule.fixed_hz", "must be above 0"));
        }
        if w.voice_max_hz == Some(0) {
            return Err(RowError::new(
                "band.width_rule.voice_max_hz",
                "must be above 0",
            ));
        }
        if let (Some(floor), Some(fixed)) = (w.floor_hz, w.fixed_hz)
            && floor > fixed
        {
            return Err(RowError::new(
                "band.width_rule.floor_hz",
                format!("{floor} is above fixed_hz {fixed}"),
            ));
        }
    }
    // Every event field the row names: the summary's, the position map's, the time's.
    let mut named: Vec<(String, String)> = summary_fields(&row.event.summary)?
        .into_iter()
        .map(|n| ("event.summary".to_string(), n))
        .collect();
    if let Some(p) = &row.position {
        named.push(("position.lat".into(), p.lat.clone()));
        named.push(("position.lon".into(), p.lon.clone()));
        named.extend(p.alt_m.clone().map(|a| ("position.alt_m".into(), a)));
    }
    named.extend(row.time.clone().map(|t| ("time".into(), t)));
    if let Some(sub) = &row.subject {
        if sub.key.trim().is_empty() {
            return Err(RowError::new("subject.key", "must not be empty"));
        }
        if sub.name.is_empty() {
            return Err(RowError::new("subject.name", "must not be empty"));
        }
        if sub.track_points < MIN_TRACK_POINTS {
            return Err(RowError::new(
                "subject.track_points",
                format!(
                    "{} is under the minimum of {MIN_TRACK_POINTS}",
                    sub.track_points
                ),
            ));
        }
        if sub.track_min_m < 0.0 {
            return Err(RowError::new(
                "subject.track_min_m",
                format!("{} must not be negative", sub.track_min_m),
            ));
        }
        named.push(("subject.key".into(), sub.key.clone()));
        for (i, c) in sub.columns.iter().enumerate() {
            named.push((format!("subject.columns[{i}].field"), c.field.clone()));
        }
        named.extend(sub.heading.clone().map(|h| ("subject.heading".into(), h)));
        named.extend(sub.label.clone().map(|l| ("subject.label".into(), l)));
        if let Some(id) = &sub.identity {
            if !IDENTITY_REGISTERS.contains(&id.register.as_str()) {
                return Err(RowError::new(
                    "subject.identity.register",
                    format!(
                        "{:?} is not one of {}",
                        id.register,
                        IDENTITY_REGISTERS.join(", ")
                    ),
                ));
            }
            if let Some(s) = &id.strip
                && !IDENTITY_STRIPS.contains(&s.as_str())
            {
                return Err(RowError::new(
                    "subject.identity.strip",
                    format!("{s:?} is not null or one of {}", IDENTITY_STRIPS.join(", ")),
                ));
            }
            named.push(("subject.identity.field".into(), id.field.clone()));
        }
    }
    for (field, name) in named {
        if !properties.contains_key(&name) {
            let mut known: Vec<&str> = properties.keys().map(String::as_str).collect();
            known.sort_unstable();
            return Err(RowError::new(
                field,
                format!(
                    "names {name:?}, which the event schema lacks (fields: {})",
                    known.join(", ")
                ),
            ));
        }
    }
    if let Some(sub) = &row.subject {
        for (i, m) in sub.merge.iter().enumerate() {
            if !properties.contains_key(m) && *m != sub.key {
                return Err(RowError::new(
                    format!("subject.merge[{i}]"),
                    "not in event.schema.properties",
                ));
            }
        }
        for k in sub.merge_skip.keys() {
            if !properties.contains_key(k) {
                return Err(RowError::new(
                    format!("subject.merge_skip.{k}"),
                    "not in event.schema.properties",
                ));
            }
        }
        if let Some(st) = &sub.station {
            if st.when.is_empty() {
                return Err(RowError::new("subject.station.when", "empty"));
            }
            if let Some(k) = st.when.keys().find(|k| !properties.contains_key(*k)) {
                return Err(RowError::new(
                    format!("subject.station.when.{k}"),
                    "not in event.schema.properties",
                ));
            }
            // A null pair: the page never matches one (a null field reads as absent).
            if let Some(k) = st.when.iter().find(|(_, v)| v.is_null()).map(|(k, _)| k) {
                return Err(RowError::new(format!("subject.station.when.{k}"), "null"));
            }
            for (name, f) in [("key", &st.key), ("addressed", &st.addressed)] {
                if f.as_ref().is_some_and(|f| !properties.contains_key(f)) {
                    return Err(RowError::new(
                        format!("subject.station.{name}"),
                        "not in event.schema.properties",
                    ));
                }
            }
        }
    }
    for (i, d) in row.derive.iter().enumerate() {
        let absent = |field: String| RowError::new(field, "not in event.schema.properties");
        if let Some(k) = d.when.keys().find(|k| !properties.contains_key(*k)) {
            return Err(absent(format!("derive[{i}].when.{k}")));
        }
        if !properties.contains_key(&d.from) {
            return Err(absent(format!("derive[{i}].from")));
        }
        let outputs = d.extract.outputs();
        for (key, target) in &d.into {
            if !outputs.contains(&key.as_str()) {
                return Err(RowError::new(
                    format!("derive[{i}].into.{key}"),
                    format!(
                        "not an output of {} ({})",
                        d.extract.word(),
                        outputs.join(", ")
                    ),
                ));
            }
            if !properties.contains_key(target) {
                return Err(absent(format!("derive[{i}].into.{key}")));
            }
        }
    }
    if let Some(Decoder::Program(ProgramBlock {
        output: ProgramOutput {
            fields: Some(fields),
            ..
        },
        ..
    })) = &row.decoder
    {
        for (key, path) in fields {
            if !properties.contains_key(key) {
                return Err(RowError::new(
                    format!("decoder.output.fields.{key}"),
                    "not in event.schema.properties",
                ));
            }
            path.check(key)?;
        }
        // A property a derive entry writes is mapped by it.
        let derived: Vec<&String> = row.derive.iter().flat_map(|d| d.into.values()).collect();
        for name in properties.keys() {
            if !ENVELOPE_FIELDS.contains(&name.as_str())
                && !fields.contains_key(name)
                && !derived.contains(&name)
            {
                return Err(RowError::new(
                    format!("event.schema.properties.{name}"),
                    "not mapped by decoder.output.fields",
                ));
            }
        }
    }
    let id = &row.identify;
    let builtin = match &row.decoder {
        Some(Decoder::Builtin(b)) => Some(b),
        _ => None,
    };
    if let Some(b) = builtin {
        if !DEMOD_KINDS.contains(&b.builtin.as_str()) {
            return Err(RowError::new(
                "decoder.builtin",
                format!(
                    "{:?} is not one of {} (the built-in demodulators)",
                    b.builtin,
                    DEMOD_KINDS.join(", ")
                ),
            ));
        }
        if id.proof != Proof::None {
            return Err(RowError::new(
                "identify.proof",
                "a builtin row proves nothing: none",
            ));
        }
        let want = match b.builtin.as_str() {
            "fm_demod" | "am_demod" => "squelch",
            _ => "none",
        };
        if row.protocol != want {
            return Err(RowError::new(
                "protocol",
                format!("{:?} must be {want:?} for {}", row.protocol, b.builtin),
            ));
        }
    } else if id.proof == Proof::None {
        return Err(RowError::new(
            "identify.proof",
            "none is for a builtin row (a decoder proves by an event or a field)",
        ));
    }
    if id.proof == Proof::Field && id.field.is_none() {
        return Err(RowError::new(
            "identify.field",
            "required with proof: field (the boolean event field that must be true)",
        ));
    }
    if let Some(q) = &id.quick
        && !QUICK_ATTEMPTS.contains(&q.as_str())
    {
        return Err(RowError::new(
            "identify.quick",
            format!(
                "{q:?} is not one of {} (or null)",
                QUICK_ATTEMPTS.join(", ")
            ),
        ));
    }
    if id.when_named && id.quick.is_some() {
        return Err(RowError::new(
            "identify.when_named",
            "a row tried only when named has no quick test (classify never offers it)",
        ));
    }
    let Some(decoder) = &row.decoder else {
        // A row with no decoder of its own: a handler file's program decodes it, so
        // nothing in process can try it quickly, and no synthesised capture proves it.
        if id.quick.is_some() {
            return Err(RowError::new(
                "identify.quick",
                "a row with no decoder has no quick test",
            ));
        }
        if row
            .test
            .as_ref()
            .is_some_and(|t| matches!(t.kind, TestSpec::Synth(_)))
        {
            return Err(RowError::new(
                "test.kind",
                "a row with no decoder is checked on a fixture",
            ));
        }
        return Ok(());
    };
    let frame_row = matches!(decoder, Decoder::Frame(_));
    match id.quick.as_deref() {
        Some("frame") if !frame_row => {
            return Err(RowError::new(
                "identify.quick",
                "frame needs decoder.kind frame",
            ));
        }
        Some(q) if frame_row && q != "frame" => {
            return Err(RowError::new(
                "identify.quick",
                format!("{q:?} on a frame row: frame (or null)"),
            ));
        }
        _ => {}
    }
    match decoder {
        Decoder::Graph {
            nodes,
            edges,
            handlers,
        } => {
            for (i, h) in handlers.iter().enumerate() {
                if h.programs.is_empty() {
                    return Err(RowError::new(
                        format!("decoder.handlers[{i}].programs"),
                        "names no program (give at least one name or absolute path)",
                    ));
                }
            }
            validate_graph(nodes, edges, stages)
        }
        Decoder::Program(p) => {
            validate_program(p)?;
            // Its own handler goes by its first program's file name: `none` is the
            // stand-in of a row with no decoder.
            if super::handler_file::own_handler_id(row) == super::handler_file::NO_DECODER {
                return Err(RowError::new(
                    "decoder.programs[0]",
                    "none is reserved: it names a row with no decoder of its own",
                ));
            }
            Ok(())
        }
        Decoder::Frame(b) => validate_frame(b, stages),
        Decoder::Builtin(_) => Ok(()),
    }
}

/// A frame row's demodulator, parser and framing (checked by `stages`' `frame_decode`,
/// its error's field under `decoder.framing`).
fn validate_frame(b: &FrameBlock, stages: &StageRegistry) -> Result<(), RowError> {
    let d = &b.demod;
    if d.symbol_rate_baud == 0 {
        return Err(RowError::new(
            "decoder.demod.symbol_rate_baud",
            "at least 1",
        ));
    }
    let orders = d.modulation.orders();
    if !orders.contains(&d.order) {
        let names: Vec<String> = orders.iter().map(u8::to_string).collect();
        let list = match names.split_last() {
            Some((last, [])) => last.clone(),
            Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
            None => String::new(),
        };
        return Err(RowError::new(
            "decoder.demod.order",
            format!("{} is not {list} for {}", d.order, d.modulation.name()),
        ));
    }
    if !FRAME_PARSERS.contains(&b.parser.as_str()) {
        return Err(RowError::new(
            "decoder.parser",
            format!("{:?} is not one of {}", b.parser, FRAME_PARSERS.join(", ")),
        ));
    }
    if stages.spec(FRAME_DECODE_STAGE).is_none() {
        return Err(RowError::new(
            "decoder.framing",
            format!("{FRAME_DECODE_STAGE} is not a registered stage"),
        ));
    }
    stages
        .check_params(FRAME_DECODE_STAGE, &b.framing)
        .map_err(|e| {
            // The stage names the field (`sync.bits: ...`); a serde error does not.
            let named = e.split_once(": ").filter(|(f, _)| {
                !f.is_empty()
                    && f.bytes().all(|c| {
                        c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'[' | b']')
                    })
            });
            match named {
                Some((f, rest)) => RowError::new(format!("decoder.framing.{f}"), rest),
                None => RowError::new("decoder.framing", e),
            }
        })
}

/// `^[a-z][a-z0-9_]*$`.
pub fn is_mode_id(id: &str) -> bool {
    let mut b = id.bytes();
    b.next().is_some_and(|c| c.is_ascii_lowercase())
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

fn has_unit_suffix(name: &str) -> bool {
    UNIT_SUFFIXES.iter().any(|s| name.ends_with(s))
}

/// Whether a JSON Schema's `type` is `integer`, or `integer` and `null`.
fn declares_integer(schema: &serde_json::Value) -> bool {
    match &schema["type"] {
        serde_json::Value::String(t) => t == "integer",
        serde_json::Value::Array(a) => {
            a.contains(&"integer".into()) && a.iter().all(|t| t == "integer" || t == "null")
        }
        _ => false,
    }
}

/// The field names a summary template uses: every name inside `{...}`, the
/// alternatives of `{a?b}` and the optional `{a?}` included, a number format
/// (`:.N`, `:+.N`) stripped.
fn summary_fields(summary: &str) -> Result<Vec<String>, RowError> {
    let bad = |e: &str| RowError::new("event.summary", e.to_string());
    let mut names = Vec::new();
    let mut rest = summary;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let close = after
            .find('}')
            .ok_or_else(|| bad("a `{` is never closed"))?;
        let inner = &after[..close];
        let fields = match super::summary::split_format(inner) {
            Some((fields, _)) => fields,
            None if inner.contains(':') => {
                return Err(bad(&format!(
                    "`{{{inner}}}`: a number format is `:.N` or `:+.N` with N 0-6"
                )));
            }
            None => inner,
        };
        let parts: Vec<&str> = fields.split('?').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() || inner.contains('{') {
            return Err(bad(&format!("`{{{inner}}}` names no field")));
        }
        names.extend(parts.into_iter().map(String::from));
        rest = &after[close + 1..];
    }
    Ok(names)
}

fn validate_graph(nodes: &[Node], edges: &[Edge], stages: &StageRegistry) -> Result<(), RowError> {
    let mut filled = Vec::with_capacity(nodes.len());
    for n in nodes {
        let field = format!("decoder.nodes[{}].params", n.id.0);
        let mut params = n.params.clone();
        fill(&mut params, &field)?;
        filled.push(Node {
            params,
            ..n.clone()
        });
    }
    let graph = Graph {
        id: GraphId("template".into()),
        nodes: filled,
        edges: edges.to_vec(),
    };
    crate::graph::validate(&graph, stages).map_err(|e| RowError::new("decoder", e.to_string()))
}

/// Replace each graph placeholder in `v` by its dummy value, checking that every field
/// named for its unit holds an integer, `null` or a placeholder.
fn fill(v: &mut serde_json::Value, field: &str) -> Result<(), RowError> {
    use serde_json::Value;
    match v {
        // The audio policy is a string: checked with the default policy's name.
        Value::String(s) if s == super::template::AUDIO_POLICY_PLACEHOLDER => {
            *v = crate::ops::AudioPolicy::default().name().into();
        }
        Value::String(s) if s.starts_with('$') => {
            let (_, dummy) = GRAPH_PLACEHOLDERS
                .iter()
                .find(|(p, _)| p == s)
                .ok_or_else(|| {
                    let mut known: Vec<&str> = GRAPH_PLACEHOLDERS.iter().map(|(p, _)| *p).collect();
                    known.push(super::template::AUDIO_POLICY_PLACEHOLDER);
                    RowError::new(
                        field,
                        format!("unknown placeholder {s} (known: {})", known.join(", ")),
                    )
                })?;
            *v = (*dummy).into();
        }
        Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                let f = format!("{field}.{k}");
                let unit_ok = matches!(x, Value::Null)
                    || x.is_u64()
                    || x.is_i64()
                    || x.as_str().is_some_and(|s| s.starts_with('$'));
                if has_unit_suffix(k) && !unit_ok {
                    return Err(RowError::new(
                        f,
                        "a field named for its unit must be an integer",
                    ));
                }
                fill(x, &f)?;
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter_mut().enumerate() {
                fill(x, &format!("{field}[{i}]"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn validate_program(p: &ProgramBlock) -> Result<(), RowError> {
    if p.programs.is_empty() {
        return Err(RowError::new(
            "decoder.programs",
            "names no program (give at least one name or absolute path)",
        ));
    }
    let audio = p.input.stream == InputStream::Audio;
    match (audio, &p.demod) {
        (true, None) => {
            return Err(RowError::new("decoder.demod", "audio input needs a demod"));
        }
        (false, Some(_)) => {
            return Err(RowError::new("decoder.demod", "an iq row takes no demod"));
        }
        _ => {}
    }
    use SampleFormat::{Cf32, Cs16, F32, S16};
    if p.input.container == Container::Wav {
        let (pcm, names) = match audio {
            true => (matches!(p.input.format, S16 | F32), "s16 or f32"),
            false => (matches!(p.input.format, Cs16 | Cf32), "cs16 or cf32"),
        };
        if !pcm {
            return Err(RowError::new(
                "decoder.input.container",
                format!("wav carries PCM only: format {names}"),
            ));
        }
    }
    if audio && p.input.format.is_iq() {
        return Err(RowError::new(
            "decoder.input.format",
            "an audio input takes s16 or f32",
        ));
    }
    if !audio && !p.input.format.is_iq() {
        return Err(RowError::new(
            "decoder.input.format",
            "an iq input takes cu8, cs8, cs16 or cf32",
        ));
    }
    for (i, arg) in p.args.iter().enumerate() {
        for name in placeholders(arg) {
            if !PROGRAM_PLACEHOLDERS.contains(&name) {
                return Err(RowError::new(
                    format!("decoder.args[{i}]"),
                    format!(
                        "unknown placeholder {name} (known: {})",
                        PROGRAM_PLACEHOLDERS.join(", ")
                    ),
                ));
            }
        }
    }
    if p.output.via == OutputVia::Tcp && p.output.port.is_none() {
        return Err(RowError::new(
            "decoder.output.port",
            "required with via: tcp (\"$port\" for the port zeta picks)",
        ));
    }
    let names_fifo = p.args.iter().any(|a| placeholders(a).contains(&"$fifo"));
    if p.input.via == InputVia::Fifo && !names_fifo {
        return Err(RowError::new(
            "decoder.args",
            "with input.via: fifo, an argument must name \"$fifo\" (the pipe zeta makes)",
        ));
    }
    match &p.packets {
        Some(packets) => validate_packets(packets, p.output.via, &p.args),
        None => Ok(()),
    }
}

/// A program's `packets` block beside events read `via`, run with `args`.
fn validate_packets(
    p: &ProgramPackets,
    events_via: OutputVia,
    args: &[String],
) -> Result<(), RowError> {
    match p.via {
        OutputVia::Stdout if events_via == OutputVia::Stdout => {
            return Err(RowError::new(
                "decoder.packets.via",
                "stdout already carries the events",
            ));
        }
        OutputVia::Stdout if p.port.is_some() => {
            return Err(RowError::new(
                "decoder.packets.port",
                "stdout takes no port",
            ));
        }
        OutputVia::Tcp if p.port.as_deref() != Some("$packet_port") => {
            return Err(RowError::new(
                "decoder.packets.port",
                "\"$packet_port\" with via: tcp (the second port zeta picks)",
            ));
        }
        OutputVia::Tcp
            if !args
                .iter()
                .any(|a| placeholders(a).contains(&"$packet_port")) =>
        {
            return Err(RowError::new(
                "decoder.packets.port",
                "no argument names $packet_port",
            ));
        }
        _ => {}
    }
    for key in p.link_types.keys() {
        let field = format!("decoder.packets.link_types.{key}");
        let Some(n) = link_type_number(key) else {
            return Err(RowError::new(
                field,
                "not a link-type number (decimal, 0 to 65535)",
            ));
        };
        // One spelling per number, so two keys never name one link type.
        if *key != n.to_string() {
            let twin = p
                .link_types
                .keys()
                .find(|k| *k != key && link_type_number(k) == Some(n));
            return Err(RowError::new(
                field,
                match twin {
                    Some(other) => format!("duplicates {other}"),
                    None => format!("write it {n}"),
                },
            ));
        }
    }
    Ok(())
}

/// Every `$name` in `arg` (`name` of lowercase letters, digits and `_`), `$` included.
fn placeholders(arg: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for (i, _) in arg.match_indices('$') {
        let len = arg[i + 1..]
            .bytes()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
            .count();
        out.push(&arg[i..i + 1 + len]);
    }
    out
}

/// `center_hz` in kHz as `$center_khz` is written: an integer when it divides (`8942`),
/// else up to three decimals with trailing zeros trimmed (`8942.5`).
pub fn center_khz_text(center_hz: u64) -> String {
    let (khz, rest) = (center_hz / 1000, center_hz % 1000);
    if rest == 0 {
        return khz.to_string();
    }
    let decimals = format!("{rest:03}");
    format!("{khz}.{}", decimals.trim_end_matches('0'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const POCSAG: &str = include_str!("../../../../modes/pocsag_decode.json");
    const ADSB: &str = include_str!("../../../../modes/adsb_decode.json");

    /// `text` with `edit` applied to its JSON.
    fn edited(text: &str, edit: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut v: serde_json::Value = serde_json::from_str(text).unwrap();
        edit(&mut v);
        v.to_string()
    }

    /// The stages the rows of these tests need: none (a program row has no graph, and
    /// a graph row's earlier checks fail before its graph is reached).
    fn no_stages() -> StageRegistry {
        StageRegistry::new()
    }

    /// Every embedded row parses, and the program row validates with no stages at all.
    /// (`tests/modes.rs` validates every row against zeta-dsp's stages: a unit test
    /// cannot, since zeta-dsp links its own copy of this crate.)
    #[test]
    fn embedded_rows_parse() {
        for (file, text) in super::super::ModeRegistry::embedded() {
            parse(text).unwrap_or_else(|e| panic!("{file}: {e}"));
        }
        validate(&parse(ADSB).unwrap(), &no_stages()).unwrap();
    }

    #[test]
    fn unknown_field_is_rejected_naming_it() {
        let text = edited(POCSAG, |v| v["bogus"] = 1.into());
        let e = parse(&text).unwrap_err();
        assert_eq!(e.field, "bogus", "{e}");
        assert!(
            e.to_string().starts_with("bogus: unknown field `bogus`"),
            "{e}"
        );
        let text = edited(ADSB, |v| v["decoder"]["input"]["extra"] = 1.into());
        assert_eq!(parse(&text).unwrap_err().field, "extra");
        let text = edited(POCSAG, |v| {
            v.as_object_mut().unwrap().remove("label");
        });
        assert_eq!(parse(&text).unwrap_err().field, "label");
    }

    #[test]
    fn summary_naming_a_missing_field_is_broken() {
        let row = parse(&edited(ADSB, |v| {
            v["event"]["summary"] = "{icao} {heading?}".into()
        }))
        .unwrap();
        let e = validate(&row, &no_stages()).unwrap_err();
        assert_eq!(e.field, "event.summary");
        assert!(e.error.contains("\"heading\""), "{e}");
        let row = parse(&edited(ADSB, |v| v["event"]["summary"] = "{icao".into())).unwrap();
        assert_eq!(
            validate(&row, &no_stages()).unwrap_err().field,
            "event.summary"
        );
    }

    #[test]
    fn bad_id_is_broken() {
        for id in ["Pocsag", "9pocsag", "pocsag-decode", ""] {
            let row = parse(&edited(ADSB, |v| v["id"] = id.into())).unwrap();
            let e = validate(&row, &no_stages()).unwrap_err();
            assert_eq!(e.field, "id", "{id}: {e}");
        }
    }

    #[test]
    fn identify_and_program_rules() {
        let check = |edit: fn(&mut serde_json::Value)| {
            validate(&parse(&edited(ADSB, edit)).unwrap(), &no_stages())
                .unwrap_err()
                .field
        };
        assert_eq!(
            check(|v| v["identify"]["proof"] = "field".into()),
            "identify.field"
        );
        assert_eq!(
            check(|v| v["identify"]["quick"] = "adsb".into()),
            "identify.quick"
        );
        // A row tried only when named has no quick test: classify never offers it.
        assert_eq!(
            check(|v| {
                v["identify"]["when_named"] = true.into();
                v["identify"]["quick"] = "aprs".into();
            }),
            "identify.when_named"
        );
        // Left out it is false; true alone is valid.
        let row = parse(&edited(ADSB, |_| {})).unwrap();
        assert!(!row.identify.when_named);
        let row = parse(&edited(ADSB, |v| v["identify"]["when_named"] = true.into())).unwrap();
        validate(&row, &no_stages()).unwrap();
        assert_eq!(
            check(|v| v["decoder"]["args"][0] = "--freq=$freq_hz".into()),
            "decoder.args[0]"
        );
        assert_eq!(
            check(|v| v["decoder"]["output"]["port"] = serde_json::Value::Null),
            "decoder.output.port"
        );
        assert_eq!(
            check(|v| v["decoder"]["input"]["via"] = "fifo".into()),
            "decoder.args"
        );
        let fifo = edited(ADSB, |v| {
            v["decoder"]["input"]["via"] = "fifo".into();
            v["decoder"]["args"][1] = "$fifo".into();
        });
        validate(&parse(&fifo).unwrap(), &no_stages()).unwrap();
        assert_eq!(
            check(|v| v["event"]["schema"]["properties"]["delay_ms"] =
                serde_json::json!({"type": "number"})),
            "event.schema.properties.delay_ms"
        );
    }

    /// `ADSB` as an audio program row: `s16` at 48 kHz, with `demod` when `demod` is set.
    fn audio_row(demod: bool) -> String {
        edited(ADSB, |v| {
            let input = &mut v["decoder"]["input"];
            input["stream"] = "audio".into();
            input["format"] = "s16".into();
            input["rate_hz"] = 48000.into();
            if demod {
                v["decoder"]["demod"] = serde_json::json!(
                    {"kind": "nbfm", "deviation_hz": 5000, "audio_rate_hz": 48000}
                );
            }
        })
    }

    #[test]
    fn audio_row_needs_a_demod() {
        let e = validate(&parse(&audio_row(false)).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(
            e,
            RowError::new("decoder.demod", "audio input needs a demod"),
            "{e}"
        );
        let row = parse(&audio_row(true)).unwrap();
        validate(&row, &no_stages()).unwrap();
        let Some(Decoder::Program(p)) = &row.decoder else {
            panic!("a program row");
        };
        assert_eq!(
            p.demod,
            Some(Demod::Nbfm {
                deviation_hz: 5000,
                audio_rate_hz: 48000
            })
        );
        assert_eq!(p.input.container, Container::Raw, "the default");
        // An audio row's format is an audio one.
        let text = edited(&audio_row(true), |v| {
            v["decoder"]["input"]["format"] = "cs16".into()
        });
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(e.field, "decoder.input.format", "{e}");
    }

    #[test]
    fn demod_on_an_iq_row_is_refused() {
        let nbfm =
            serde_json::json!({"kind": "nbfm", "deviation_hz": 5000, "audio_rate_hz": 48000});
        let text = edited(ADSB, |v| v["decoder"]["demod"] = nbfm);
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(
            e,
            RowError::new("decoder.demod", "an iq row takes no demod"),
            "{e}"
        );
    }

    #[test]
    fn range_must_be_ordered() {
        for range in [[406_000_000, 400_050_000], [400_000_000, 400_000_000]] {
            let text = edited(ADSB, |v| v["band"]["range_hz"] = serde_json::json!(range));
            let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
            assert_eq!(e.field, "band.range_hz", "{range:?}: {e}");
            assert!(e.error.contains("[lo, hi] with lo < hi"), "{e}");
        }
        let text = edited(ADSB, |v| {
            v["band"]["range_hz"] = serde_json::json!([400_050_000, 406_000_000])
        });
        let row = parse(&text).unwrap();
        validate(&row, &no_stages()).unwrap();
        assert_eq!(
            serde_json::to_value(&row.band).unwrap()["range_hz"],
            serde_json::json!([400_050_000, 406_000_000])
        );
        // In a list, each range is checked, and the list must not be empty.
        let reversed = serde_json::json!([[138_000_000, 175_000_000], [470_000_000, 450_000_000]]);
        let text = edited(ADSB, |v| v["band"]["range_hz"] = reversed);
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(e.field, "band.range_hz", "{e}");
        assert!(e.error.contains("[470000000, 450000000]"), "{e}");
        let text = edited(ADSB, |v| v["band"]["range_hz"] = serde_json::json!([]));
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(
            e,
            RowError::new("band.range_hz", "must list at least one range"),
            "{e}"
        );
    }

    /// `band.range_hz` is one range `[lo, hi]`, as every row before lists was, or a list
    /// of them (overlapping allowed). A row of one range writes it back as it was read,
    /// so a client that knows only the one-range form reads it unchanged.
    #[test]
    fn range_hz_is_one_range_or_a_list() {
        let read = |range: serde_json::Value| {
            let text = edited(ADSB, |v| v["band"]["range_hz"] = range);
            let row = parse(&text).unwrap();
            validate(&row, &no_stages()).unwrap();
            serde_json::to_value(&row.band).unwrap()["range_hz"].clone()
        };
        let one = serde_json::json!([400_050_000, 406_000_000]);
        assert_eq!(read(one.clone()), one);
        let two = serde_json::json!([[138_000_000, 175_000_000], [450_000_000, 470_000_000]]);
        assert_eq!(read(two.clone()), two);
        let overlapping =
            serde_json::json!([[138_000_000, 175_000_000], [170_000_000, 180_000_000]]);
        assert_eq!(read(overlapping.clone()), overlapping);
        // A list of one is one range.
        assert_eq!(read(serde_json::json!([[400_050_000, 406_000_000]])), one);
        assert_eq!(read(serde_json::Value::Null), serde_json::Value::Null);
    }

    #[test]
    fn position_names_schema_fields() {
        let with = |position: serde_json::Value, time: serde_json::Value| {
            edited(ADSB, |v| {
                v["position"] = position;
                v["time"] = time;
            })
        };
        let row = parse(&with(
            serde_json::json!({"lat": "lat", "lon": "lon", "alt_m": null}),
            serde_json::Value::Null,
        ))
        .unwrap();
        validate(&row, &no_stages()).unwrap();
        assert_eq!(
            row.position,
            Some(PositionMap {
                lat: "lat".into(),
                lon: "lon".into(),
                alt_m: None,
                alt_unit: AltUnit::M,
            })
        );
        for (position, field) in [
            (
                serde_json::json!({"lat": "latitude", "lon": "lon"}),
                "position.lat",
            ),
            (
                serde_json::json!({"lat": "lat", "lon": "longitude"}),
                "position.lon",
            ),
            (
                serde_json::json!({"lat": "lat", "lon": "lon", "alt_m": "alt"}),
                "position.alt_m",
            ),
        ] {
            let row = parse(&with(position, serde_json::Value::Null)).unwrap();
            let e = validate(&row, &no_stages()).unwrap_err();
            assert_eq!(e.field, field, "{e}");
            assert!(e.error.contains("which the event schema lacks"), "{e}");
        }
        let row = parse(&with(serde_json::Value::Null, "datetime".into())).unwrap();
        assert_eq!(validate(&row, &no_stages()).unwrap_err().field, "time");
        let text = edited(ADSB, |v| v["position"] = serde_json::json!({"lat": "lat"}));
        assert_eq!(parse(&text).unwrap_err().field, "lon");
    }

    #[test]
    fn subject_round_trips() {
        let row = parse(ADSB).unwrap();
        validate(&row, &no_stages()).unwrap();
        let sub = row.subject.clone().unwrap();
        assert_eq!(sub.key, "icao");
        assert_eq!(sub.columns[1].unit, Some(SubjectUnit::Ft));
        let again = parse(&serde_json::to_string(&row).unwrap()).unwrap();
        assert_eq!(again, row);
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(v["subject"]["columns"][1]["unit"], "ft");
        assert_eq!(v["position"]["alt_unit"], "ft");
        assert_eq!(serde_json::to_value(SubjectUnit::KmH).unwrap(), "km_h");
        assert_eq!(serde_json::to_value(SubjectUnit::MS).unwrap(), "m_s");
    }

    #[test]
    fn a_row_without_subject_loads_as_null() {
        let text = edited(ADSB, |v| {
            v.as_object_mut().unwrap().remove("subject");
            v["position"].as_object_mut().unwrap().remove("alt_unit");
        });
        let row = parse(&text).unwrap();
        assert_eq!(row.subject, None);
        assert_eq!(row.position.unwrap().alt_unit, AltUnit::M);
    }

    #[test]
    fn a_bad_waterfall_is_broken_naming_the_field() {
        let text = edited(ADSB, |v| v["subject"]["waterfall"] = "sideways".into());
        assert_eq!(parse(&text).unwrap_err().field, "subject.waterfall");
    }

    #[test]
    fn an_unknown_unit_is_broken_naming_the_field() {
        let text = edited(ADSB, |v| {
            v["subject"]["columns"][1]["unit"] = "furlong".into()
        });
        assert_eq!(parse(&text).unwrap_err().field, "subject.columns[1].unit");
    }

    #[test]
    fn a_bad_alt_unit_is_broken_naming_the_field() {
        let text = edited(ADSB, |v| v["position"]["alt_unit"] = "cubits".into());
        assert_eq!(parse(&text).unwrap_err().field, "position.alt_unit");
    }

    #[test]
    fn an_empty_key_or_name_is_broken() {
        for field in ["key", "name"] {
            let text = edited(ADSB, |v| v["subject"][field] = "".into());
            let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
            assert_eq!(e.field, format!("subject.{field}"), "{e}");
        }
    }

    #[test]
    fn subject_names_schema_fields() {
        let cases = [
            ("subject.key", "/subject/key"),
            ("subject.columns[0].field", "/subject/columns/0/field"),
            ("subject.heading", "/subject/heading"),
            ("subject.label", "/subject/label"),
        ];
        for (field, pointer) in cases {
            let text = edited(ADSB, |v| *v.pointer_mut(pointer).unwrap() = "nope".into());
            let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
            assert_eq!(e.field, field, "{e}");
            assert!(e.error.contains("which the event schema lacks"), "{e}");
        }
    }

    #[test]
    fn a_whitespace_subject_key_is_broken_naming_the_field() {
        let text = edited(ADSB, |v| v["subject"]["key"] = "  ".into());
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(e.field, "subject.key", "{e}");
        assert!(e.error.contains("must not be empty"), "{e}");
    }

    #[test]
    fn track_fields_default_and_round_trip() {
        let text = edited(ADSB, |v| {
            let sub = v["subject"].as_object_mut().unwrap();
            sub.remove("track_points");
            sub.remove("track_min_m");
        });
        let sub = parse(&text).unwrap().subject.unwrap();
        assert_eq!((sub.track_points, sub.track_min_m), (100, 0.0));
        let text = edited(ADSB, |v| {
            v["subject"]["track_points"] = 4.into();
            v["subject"]["track_min_m"] = 12.5.into();
        });
        let row = parse(&text).unwrap();
        validate(&row, &no_stages()).unwrap();
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(v["subject"]["track_points"], 4);
        assert_eq!(v["subject"]["track_min_m"], 12.5);
    }

    #[test]
    fn too_few_track_points_is_broken_naming_the_field() {
        let text = edited(ADSB, |v| v["subject"]["track_points"] = 3.into());
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(e.field, "subject.track_points", "{e}");
    }

    #[test]
    fn track_points_not_a_count_is_broken_naming_the_field() {
        for bad in [serde_json::json!(-1), serde_json::json!(4.5), "many".into()] {
            let text = edited(ADSB, |v| v["subject"]["track_points"] = bad.clone());
            assert_eq!(
                parse(&text).unwrap_err().field,
                "subject.track_points",
                "{bad}"
            );
        }
    }

    #[test]
    fn a_negative_track_min_is_broken_naming_the_field() {
        let text = edited(ADSB, |v| v["subject"]["track_min_m"] = (-1.0).into());
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(e.field, "subject.track_min_m", "{e}");
    }

    #[test]
    fn a_track_min_not_a_number_is_broken_naming_the_field() {
        let text = edited(ADSB, |v| v["subject"]["track_min_m"] = "far".into());
        assert_eq!(parse(&text).unwrap_err().field, "subject.track_min_m");
    }

    #[test]
    fn an_unknown_subject_field_is_rejected() {
        let text = edited(ADSB, |v| v["subject"]["extra"] = 1.into());
        assert_eq!(parse(&text).unwrap_err().field, "extra");
    }

    #[test]
    fn container_wav_needs_a_pcm_format() {
        let wav = |format: &str| {
            edited(&audio_row(true), |v| {
                v["decoder"]["input"]["container"] = "wav".into();
                v["decoder"]["input"]["format"] = format.into();
            })
        };
        let row = parse(&wav("s16")).unwrap();
        validate(&row, &no_stages()).unwrap();
        let Some(Decoder::Program(p)) = &row.decoder else {
            panic!("a program row");
        };
        assert_eq!(p.input.container, Container::Wav);
        validate(&parse(&wav("f32")).unwrap(), &no_stages()).unwrap();
        for format in ["cf32", "cs16"] {
            let e = validate(&parse(&wav(format)).unwrap(), &no_stages()).unwrap_err();
            assert_eq!(e.field, "decoder.input.container", "{format}: {e}");
        }
        // On an iq row, `wav` takes the PCM iq formats only.
        let iq_wav = |format: &str| {
            edited(ADSB, |v| {
                v["decoder"]["input"]["container"] = "wav".into();
                v["decoder"]["input"]["format"] = format.into();
            })
        };
        validate(&parse(&iq_wav("cs16")).unwrap(), &no_stages()).unwrap();
        validate(&parse(&iq_wav("cf32")).unwrap(), &no_stages()).unwrap();
        for format in ["cu8", "cs8"] {
            let e = validate(&parse(&iq_wav(format)).unwrap(), &no_stages()).unwrap_err();
            assert_eq!(e.field, "decoder.input.container", "{format}: {e}");
        }
    }

    #[test]
    fn graph_params_named_for_a_unit_hold_integers() {
        let row = parse(&edited(POCSAG, |v| {
            v["decoder"]["nodes"][0]["params"]["out_rate_hz"] = 48000.5.into()
        }))
        .unwrap();
        let e = validate(&row, &no_stages()).unwrap_err();
        assert_eq!(e.field, "decoder.nodes[channel].params.out_rate_hz");
        let row = parse(&edited(POCSAG, |v| {
            v["decoder"]["nodes"][0]["params"]["center_hz"] = "$centre".into()
        }))
        .unwrap();
        let e = validate(&row, &no_stages()).unwrap_err();
        assert_eq!(e.field, "decoder.nodes[channel].params.center_hz");
        // With every placeholder known, the graph itself is checked (no stages: unknown kind).
        let e = validate(&parse(POCSAG).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(e.field, "decoder");
        assert!(e.error.contains("unknown stage kind"), "{e}");
    }

    #[test]
    fn summary_fields_reads_optional_and_alternative_names() {
        assert_eq!(
            summary_fields("{ps?pi} · {rt?}").unwrap(),
            ["ps", "pi", "rt"]
        );
        assert_eq!(
            summary_fields("{icao} {callsign?} {lat?},{lon?}").unwrap(),
            ["icao", "callsign", "lat", "lon"]
        );
        assert!(summary_fields("{}").is_err());
        assert_eq!(
            summary_fields("{alt:.0} m {lat?:.4} {a?b:+.1}").unwrap(),
            ["alt", "lat", "a", "b"]
        );
        for bad in ["{a:x}", "{a:.7}", "{a:.}", "{:.1}"] {
            assert!(summary_fields(bad).is_err(), "{bad}");
        }
    }

    /// `ADSB` with a `decoder.output.fields` map over its schema: every property but the
    /// envelope's `protocol` mapped to `/<name>`, then `edit` applied to the map.
    fn with_fields(edit: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>)) -> String {
        edited(ADSB, |v| {
            let mut map = serde_json::Map::new();
            for name in v["event"]["schema"]["properties"]
                .as_object()
                .unwrap()
                .keys()
            {
                if !ENVELOPE_FIELDS.contains(&name.as_str()) {
                    map.insert(name.clone(), format!("/{name}").into());
                }
            }
            edit(&mut map);
            v["decoder"]["output"]["fields"] = map.into();
        })
    }

    /// The program block of a row.
    fn program(row: &ModeRow) -> &ProgramBlock {
        let Some(Decoder::Program(p)) = &row.decoder else {
            panic!("a program row");
        };
        p
    }

    #[test]
    fn fields_map_round_trips_and_defaults_to_null() {
        let row = parse(ADSB).unwrap();
        assert_eq!(program(&row).output.fields, None);
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(v["decoder"]["output"]["fields"], serde_json::Value::Null);
        let text = with_fields(|m| {
            m.insert("icao".into(), "/a/b".into());
            m.insert("lat".into(), serde_json::json!(["/x", "/y"]));
        });
        let row = parse(&text).unwrap();
        validate(&row, &no_stages()).unwrap();
        let fields = program(&row).output.fields.clone().unwrap();
        assert_eq!(fields["icao"], FieldPath::One("/a/b".into()));
        assert_eq!(
            fields["lat"],
            FieldPath::First(vec!["/x".into(), "/y".into()])
        );
        assert_eq!(fields["icao"].pointers(), ["/a/b".to_string()]);
        assert_eq!(
            fields["lat"].pointers(),
            ["/x".to_string(), "/y".to_string()]
        );
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(v["decoder"]["output"]["fields"]["icao"], "/a/b");
        assert_eq!(
            v["decoder"]["output"]["fields"]["lat"],
            serde_json::json!(["/x", "/y"])
        );
        assert_eq!(parse(&serde_json::to_string(&row).unwrap()).unwrap(), row);
    }

    #[test]
    fn a_trimmed_field_path_round_trips_and_needs_pointers_and_trim() {
        let p: FieldPath =
            serde_json::from_value(serde_json::json!({"pointers": ["/a/reg"], "trim": "."}))
                .unwrap();
        assert_eq!(
            p,
            FieldPath::Trimmed(TrimmedPaths {
                pointers: vec!["/a/reg".into()],
                trim: ".".into()
            })
        );
        assert_eq!(p.pointers(), ["/a/reg".to_string()]);
        assert_eq!(p.trim(), Some("."));
        p.check("reg").unwrap();
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            serde_json::json!({"pointers": ["/a/reg"], "trim": "."})
        );
        let empty: FieldPath =
            serde_json::from_value(serde_json::json!({"pointers": [], "trim": "."})).unwrap();
        assert_eq!(
            empty.check("reg").unwrap_err(),
            RowError::new("decoder.output.fields.reg", "an empty list")
        );
        let bare: FieldPath =
            serde_json::from_value(serde_json::json!({"pointers": ["/a"], "trim": ""})).unwrap();
        assert_eq!(
            bare.check("reg").unwrap_err(),
            RowError::new("decoder.output.fields.reg", "an empty trim")
        );
    }

    #[test]
    fn fields_keys_must_be_schema_properties() {
        let text = with_fields(|m| {
            m.insert("nope".into(), "/nope".into());
        });
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(
            e,
            RowError::new(
                "decoder.output.fields.nope",
                "not in event.schema.properties"
            ),
            "{e}"
        );
    }

    #[test]
    fn every_schema_property_is_mapped_except_the_envelope() {
        let text = with_fields(|m| {
            m.remove("altitude_ft");
        });
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(
            e,
            RowError::new(
                "event.schema.properties.altitude_ft",
                "not mapped by decoder.output.fields"
            ),
            "{e}"
        );
        // ADSB's schema lists `protocol`, which `with_fields` leaves unmapped.
        let text = with_fields(|m| assert!(!m.contains_key("protocol")));
        validate(&parse(&text).unwrap(), &no_stages()).unwrap();
    }

    #[test]
    fn a_pointer_must_start_with_a_slash_and_a_list_may_not_be_empty() {
        let text = with_fields(|m| {
            m.insert("icao".into(), "icao".into());
        });
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(
            e,
            RowError::new(
                "decoder.output.fields.icao",
                "a JSON pointer starts with \"/\""
            ),
            "{e}"
        );
        let text = with_fields(|m| {
            m.insert("lat".into(), serde_json::json!(["/x", "y"]));
        });
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(e.field, "decoder.output.fields.lat", "{e}");
        let text = with_fields(|m| {
            m.insert("icao".into(), serde_json::json!([]));
        });
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(
            e,
            RowError::new("decoder.output.fields.icao", "an empty list"),
            "{e}"
        );
    }

    #[test]
    fn center_khz_is_an_integer_when_it_divides() {
        assert_eq!(center_khz_text(8_942_000), "8942");
        assert_eq!(center_khz_text(8_942_500), "8942.5");
        assert_eq!(center_khz_text(136_975_000), "136975");
        assert_eq!(center_khz_text(8_942_125), "8942.125");
    }

    #[test]
    fn center_khz_is_a_known_placeholder_and_others_still_break() {
        let text = edited(ADSB, |v| v["decoder"]["args"][0] = "$center_khz".into());
        validate(&parse(&text).unwrap(), &no_stages()).unwrap();
        let text = edited(ADSB, |v| v["decoder"]["args"][0] = "$centre_khz".into());
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(e.field, "decoder.args[0]", "{e}");
        assert!(e.error.contains("$center_khz"), "{e}");
    }

    #[test]
    fn merge_round_trips_defaults_empty_and_names_schema_fields() {
        // A row that leaves `merge` out has none; the ADS-B row merges on `icao`.
        let bare = edited(ADSB, |v| {
            v["subject"].as_object_mut().unwrap().remove("merge");
        });
        let row = parse(&bare).unwrap();
        assert!(row.subject.as_ref().unwrap().merge.is_empty());
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(v["subject"]["merge"], serde_json::json!([]));
        let text = edited(&bare, |v| {
            v["subject"]["merge"] = serde_json::json!(["icao"])
        });
        let row = parse(&text).unwrap();
        validate(&row, &no_stages()).unwrap();
        assert_eq!(
            row.subject.as_ref().unwrap().merge,
            vec!["icao".to_string()]
        );
        assert_eq!(parse(&serde_json::to_string(&row).unwrap()).unwrap(), row);
        let text = edited(&bare, |v| {
            v["subject"]["merge"] = serde_json::json!(["callsign", "nope"])
        });
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(
            e,
            RowError::new("subject.merge[1]", "not in event.schema.properties"),
            "{e}"
        );
        let text = edited(ADSB, |v| {
            v["subject"]["merge"] = serde_json::json!(["nope"])
        });
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(e.field, "subject.merge[0]", "{e}");
    }

    #[test]
    fn merge_skip_round_trips_defaults_empty_and_names_schema_fields() {
        let row = parse(ADSB).unwrap();
        assert!(row.subject.as_ref().unwrap().merge_skip.is_empty());
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(v["subject"]["merge_skip"], serde_json::json!({}));
        let text = edited(ADSB, |v| {
            v["subject"]["merge_skip"] = serde_json::json!({"callsign": "Ground station"})
        });
        let row = parse(&text).unwrap();
        validate(&row, &no_stages()).unwrap();
        assert_eq!(
            row.subject.as_ref().unwrap().merge_skip,
            BTreeMap::from([("callsign".to_string(), "Ground station".into())])
        );
        assert_eq!(parse(&serde_json::to_string(&row).unwrap()).unwrap(), row);
        let text = edited(ADSB, |v| {
            v["subject"]["merge_skip"] = serde_json::json!({"addr_type": "Ground station"})
        });
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(
            e,
            RowError::new(
                "subject.merge_skip.addr_type",
                "not in event.schema.properties"
            ),
            "{e}"
        );
    }

    #[test]
    fn station_round_trips_and_defaults_null() {
        let row = parse(ADSB).unwrap();
        assert!(row.subject.as_ref().unwrap().station.is_none());
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(v["subject"]["station"], serde_json::Value::Null);
        let text = edited(ADSB, |v| {
            v["subject"]["station"] = serde_json::json!({"when": {"callsign": "x"}})
        });
        let row = parse(&text).unwrap();
        validate(&row, &no_stages()).unwrap();
        let st = row.subject.as_ref().unwrap().station.as_ref().unwrap();
        assert!(st.key.is_none() && st.addressed.is_none());
        assert_eq!(parse(&serde_json::to_string(&row).unwrap()).unwrap(), row);
    }

    #[test]
    fn station_names_schema_fields() {
        for (st, field) in [
            (
                serde_json::json!({"when": {"nope": 1}}),
                "subject.station.when.nope",
            ),
            (
                serde_json::json!({"when": {"callsign": 1}, "key": "nope"}),
                "subject.station.key",
            ),
            (
                serde_json::json!({"when": {"callsign": 1}, "addressed": "nope"}),
                "subject.station.addressed",
            ),
        ] {
            let text = edited(ADSB, |v| v["subject"]["station"] = st.clone());
            let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
            assert_eq!(
                e,
                RowError::new(field, "not in event.schema.properties"),
                "{e}"
            );
        }
    }

    #[test]
    fn an_empty_station_when_is_refused() {
        let text = edited(ADSB, |v| {
            v["subject"]["station"] = serde_json::json!({"when": {}})
        });
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(e, RowError::new("subject.station.when", "empty"), "{e}");
    }

    #[test]
    fn a_null_station_when_value_is_refused() {
        // The page reads a null field as absent, the node as equal to null: refused.
        let text = edited(ADSB, |v| {
            v["subject"]["station"] = serde_json::json!({"when": {"callsign": null}})
        });
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(
            e,
            RowError::new("subject.station.when.callsign", "null"),
            "{e}"
        );
    }

    #[test]
    fn station_matches_every_pair_present_and_equal() {
        let st = Station {
            when: BTreeMap::from([
                ("a".to_string(), serde_json::json!(true)),
                ("b".to_string(), serde_json::json!("x")),
            ]),
            key: None,
            addressed: None,
        };
        assert!(st.matches(&serde_json::json!({"a": true, "b": "x", "c": 1})));
        assert!(!st.matches(&serde_json::json!({"a": true})));
        assert!(!st.matches(&serde_json::json!({"a": "true", "b": "x"})));
        assert!(!st.matches(&serde_json::json!({"a": true, "b": "y"})));
        assert!(!st.matches(&serde_json::json!("a")));
    }

    /// `ADSB` with `text`, `downlink` and `alt_ft` in its schema (it has `lat` and `lon`)
    /// and one derive entry, `entry`.
    fn with_derive(entry: serde_json::Value) -> String {
        edited(ADSB, |v| {
            let props = &mut v["event"]["schema"]["properties"];
            props["text"] = serde_json::json!({"type": ["string", "null"]});
            props["downlink"] = serde_json::json!({"type": ["boolean", "null"]});
            props["alt_ft"] = serde_json::json!({"type": ["integer", "null"]});
            v["derive"] = serde_json::json!([entry]);
        })
    }

    /// The derive entry of the design: a downlink's text read by `acars_position`.
    fn acars_entry() -> serde_json::Value {
        serde_json::json!({
            "when": {"downlink": true}, "from": "text", "extract": "acars_position",
            "into": {"lat": "lat", "lon": "lon", "alt_ft": "alt_ft"}
        })
    }

    #[test]
    fn derive_round_trips_and_defaults_empty() {
        let row = parse(ADSB).unwrap();
        assert!(row.derive.is_empty());
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(v["derive"], serde_json::json!([]));
        let row = parse(&with_derive(acars_entry())).unwrap();
        validate(&row, &no_stages()).unwrap();
        let s = |x: &str| x.to_string();
        assert_eq!(
            row.derive,
            [Derive {
                when: BTreeMap::from([(s("downlink"), true.into())]),
                from: s("text"),
                extract: Extractor::AcarsPosition,
                into: BTreeMap::from([
                    (s("alt_ft"), s("alt_ft")),
                    (s("lat"), s("lat")),
                    (s("lon"), s("lon")),
                ]),
            }]
        );
        assert_eq!(parse(&serde_json::to_string(&row).unwrap()).unwrap(), row);
        assert_eq!(Extractor::AcarsPosition.word(), "acars_position");
        assert_eq!(Extractor::AcarsPosition.outputs(), ["lat", "lon", "alt_ft"]);
        // `when` may be left out: the entry runs on every event.
        let mut entry = acars_entry();
        entry.as_object_mut().unwrap().remove("when");
        let row = parse(&with_derive(entry)).unwrap();
        assert!(row.derive[0].when.is_empty());
        // The rest is required, and nothing else is allowed.
        for missing in ["from", "extract", "into"] {
            let mut entry = acars_entry();
            entry.as_object_mut().unwrap().remove(missing);
            assert_eq!(parse(&with_derive(entry)).unwrap_err().field, missing);
        }
        let mut entry = acars_entry();
        entry["extra"] = 1.into();
        assert_eq!(parse(&with_derive(entry)).unwrap_err().field, "extra");
    }

    #[test]
    fn an_unknown_extractor_is_broken_at_its_field() {
        let mut entry = acars_entry();
        entry["extract"] = "cubits".into();
        let e = parse(&with_derive(entry.clone())).unwrap_err();
        assert_eq!(e.field, "derive[0].extract", "{e}");
        // A row with no subject still names the field.
        let text = edited(&with_derive(entry), |v| {
            v["subject"] = serde_json::Value::Null
        });
        assert_eq!(parse(&text).unwrap_err().field, "derive[0].extract");
    }

    #[test]
    fn derive_names_schema_fields() {
        let broken = |edit: fn(&mut serde_json::Value)| {
            let mut entry = acars_entry();
            edit(&mut entry);
            validate(&parse(&with_derive(entry)).unwrap(), &no_stages()).unwrap_err()
        };
        let e = broken(|e| e["when"]["nope"] = true.into());
        assert_eq!(
            e,
            RowError::new("derive[0].when.nope", "not in event.schema.properties"),
            "{e}"
        );
        let e = broken(|e| e["from"] = "nope".into());
        assert_eq!(
            e,
            RowError::new("derive[0].from", "not in event.schema.properties"),
            "{e}"
        );
        let e = broken(|e| e["into"]["lat"] = "nope".into());
        assert_eq!(
            e,
            RowError::new("derive[0].into.lat", "not in event.schema.properties"),
            "{e}"
        );
        let e = broken(|e| e["into"]["speed"] = "lat".into());
        assert_eq!(
            e,
            RowError::new(
                "derive[0].into.speed",
                "not an output of acars_position (lat, lon, alt_ft)"
            ),
            "{e}"
        );
    }

    #[test]
    fn a_derived_property_counts_as_mapped() {
        // `lat` is in the schema, written by derive and by no pointer.
        let text = edited(&with_fields(|m| drop(m.remove("lat"))), |v| {
            v["derive"] = serde_json::json!([{
                "from": "callsign", "extract": "acars_position", "into": {"lat": "lat"}
            }]);
        });
        validate(&parse(&text).unwrap(), &no_stages()).unwrap();
        // `wind`, mapped by neither, still fails.
        let text = edited(&text, |v| {
            v["event"]["schema"]["properties"]["wind"] = serde_json::json!({"type": "number"})
        });
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(
            e,
            RowError::new(
                "event.schema.properties.wind",
                "not mapped by decoder.output.fields"
            ),
            "{e}"
        );
    }

    /// A frame row: fsk at 4800 Bd, framed by a 16-bit sync, 272 bits and a CRC.
    const FRAME: &str = r#"{
        "id": "fsk_test_frames", "name": "FSK test frames", "family": "general",
        "band": {"bandwidth_hz": 12500, "centers_hz": [433920000]},
        "modulation": ["fsk", "fm"], "label": "FSK frames", "protocol": "frame",
        "decoder": {"kind": "frame",
            "demod": {"modulation": "fsk", "symbol_rate_baud": 4800, "order": 2, "tone_spacing_hz": null},
            "framing": {"sync": {"hex": "2dd4", "bits": 16}, "length": {"fixed_bits": 272}, "crc": "crc16-ccitt"},
            "parser": "opaque"},
        "event": {"schema": {"type": "object", "properties": {"protocol": {"type": "string"},
            "length_bytes": {"type": "integer"}, "crc_ok": {"type": "boolean"}}},
            "summary": "frame {length_bytes} B"},
        "identify": {"proof": "field", "field": "crc_ok", "min_events": 1, "dwell_ms": 10000,
            "min_span_ms": 1000, "quick": "frame"},
        "catalogue": []
    }"#;

    /// A `frame_decode` stand-in whose params check refuses `sync.bits` over 64 as
    /// zeta-dsp's does, naming the field.
    struct FakeFrameDecode(crate::stage::StageSpec);

    #[async_trait::async_trait]
    impl crate::stage::Stage for FakeFrameDecode {
        fn spec(&self) -> &crate::stage::StageSpec {
            &self.0
        }
        fn backend(&self) -> crate::stage::Backend {
            crate::stage::Backend::Cpu
        }
        fn check_params(&self, params: &serde_json::Value) -> Result<(), String> {
            match params["sync"]["bits"].as_u64() {
                Some(1..=64) => Ok(()),
                _ => Err("sync.bits: 1..=64".into()),
            }
        }
        async fn run(
            &self,
            _params: serde_json::Value,
            _inputs: Vec<tokio::sync::broadcast::Receiver<crate::stream::Frame>>,
            _outputs: Vec<crate::types::StreamId>,
            _bus: std::sync::Arc<crate::stream::StreamBus>,
            _cancel: tokio_util::sync::CancellationToken,
            _ctx: crate::stage::StageContext,
        ) -> Result<(), crate::stage::StageError> {
            Ok(())
        }
    }

    fn frame_stages() -> StageRegistry {
        let mut r = StageRegistry::new();
        r.register(std::sync::Arc::new(FakeFrameDecode(
            crate::stage::StageSpec {
                kind: "frame_decode",
                inputs: vec![crate::types::StreamKind::Event],
                outputs: vec![crate::types::StreamKind::Event],
                params_schema: schemars::json_schema!({}),
            },
        )));
        r
    }

    #[test]
    fn a_frame_row_validates_its_framing_through_the_stage() {
        let row = parse(FRAME).unwrap();
        validate(&row, &frame_stages()).unwrap();
        assert_eq!(row.decoder_kind(), Some(DecoderKind::Frame));
        let e = validate(&row, &no_stages()).unwrap_err();
        assert_eq!(
            e.to_string(),
            "decoder.framing: frame_decode is not a registered stage"
        );
        let text = edited(FRAME, |v| {
            v["decoder"]["framing"]["sync"]["bits"] = 70.into()
        });
        let e = validate(&parse(&text).unwrap(), &frame_stages()).unwrap_err();
        assert_eq!(e, RowError::new("decoder.framing.sync.bits", "1..=64"));
    }

    #[test]
    fn a_frame_row_checks_its_demod_and_parser() {
        let check = |edit: fn(&mut serde_json::Value)| {
            validate(&parse(&edited(FRAME, edit)).unwrap(), &frame_stages())
                .unwrap_err()
                .to_string()
        };
        assert_eq!(
            check(|v| v["decoder"]["demod"]["order"] = 3.into()),
            "decoder.demod.order: 3 is not 2 or 4 for fsk"
        );
        assert_eq!(
            check(|v| {
                v["decoder"]["demod"]["modulation"] = "psk".into();
                v["decoder"]["demod"]["order"] = 3.into();
            }),
            "decoder.demod.order: 3 is not 2, 4 or 8 for psk"
        );
        assert_eq!(
            check(|v| v["decoder"]["demod"]["symbol_rate_baud"] = 0.into()),
            "decoder.demod.symbol_rate_baud: at least 1"
        );
        let e = check(|v| v["decoder"]["parser"] = "bogus".into());
        assert!(e.starts_with("decoder.parser: \"bogus\""), "{e}");
        let e = check(|v| v["identify"]["quick"] = "aprs".into());
        assert!(e.starts_with("identify.quick: "), "{e}");
        // `quick` may be null on a frame row.
        let text = edited(FRAME, |v| v["identify"]["quick"] = serde_json::Value::Null);
        validate(&parse(&text).unwrap(), &frame_stages()).unwrap();
        // An unknown field of the block or its demod is refused naming it.
        let text = edited(FRAME, |v| v["decoder"]["demod"]["bogus"] = 1.into());
        assert_eq!(parse(&text).unwrap_err().field, "bogus");
    }

    /// A handler block as a graph row names it.
    fn codec2_handlers() -> serde_json::Value {
        serde_json::json!([{
            "programs": ["c2dec"],
            "install": {"debian": "codec2", "url": "https://github.com/drowe67/codec2"}
        }])
    }

    #[test]
    fn a_graph_row_may_name_handlers() {
        let row = parse(&edited(POCSAG, |v| {
            v["decoder"]["handlers"] = codec2_handlers()
        }))
        .unwrap();
        // The handlers pass; with no stages the graph itself is what fails (tests/modes.rs
        // validates the row whole against zeta-dsp's stages).
        let e = validate(&row, &no_stages()).unwrap_err();
        assert_eq!(e.field, "decoder", "{e}");
        assert!(e.error.contains("unknown stage kind"), "{e}");
        let blocks = row.handler_blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].programs, ["c2dec"]);
        assert_eq!(blocks[0].install.debian.as_deref(), Some("codec2"));
        let Some(Decoder::Graph { handlers, .. }) = &parse(POCSAG).unwrap().decoder else {
            panic!("graph row");
        };
        assert!(handlers.is_empty(), "a row without the key has none");
        assert!(parse(POCSAG).unwrap().handler_blocks().is_empty());
        // A program row's one block.
        let adsb = parse(ADSB).unwrap();
        let blocks = adsb.handler_blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0].programs.first().map(String::as_str),
            Some("dump1090")
        );
    }

    #[test]
    fn an_empty_handler_program_list_is_broken_naming_the_field() {
        let empty = serde_json::json!([{"programs": [], "install": {"url": "x"}}]);
        let row = parse(&edited(POCSAG, |v| v["decoder"]["handlers"] = empty)).unwrap();
        let e = validate(&row, &no_stages()).unwrap_err();
        assert_eq!(e.field, "decoder.handlers[0].programs", "{e}");
    }

    #[test]
    fn a_handler_with_an_unknown_key_is_broken() {
        let text = edited(
            POCSAG,
            |v| {
                v["decoder"]["handlers"] =
                    serde_json::json!([{"programs": ["a"], "install": {"url": "x"}, "bogus": 1}])
            },
        );
        let e = parse(&text).unwrap_err();
        assert!(e.field.contains("bogus"), "{e}");
    }

    const M17: &str = include_str!("../../../../modes/m17_decode.json");

    #[test]
    fn an_optional_handler_parses_and_defaults_false() {
        let handlers = |row: &ModeRow| match &row.decoder {
            Some(Decoder::Graph { handlers, .. }) => handlers.clone(),
            _ => panic!("graph row"),
        };
        // The built-in M17 row's `c2dec` is optional; left out, a handler is required.
        let row = parse(M17).unwrap();
        assert!(handlers(&row)[0].optional);
        assert!(row.handler_blocks()[0].optional);
        let row = parse(&edited(M17, |v| {
            v["decoder"]["handlers"][0]
                .as_object_mut()
                .unwrap()
                .remove("optional");
        }))
        .unwrap();
        assert!(!handlers(&row)[0].optional);
        assert!(!row.handler_blocks()[0].optional);
        let e = parse(&edited(M17, |v| {
            v["decoder"]["handlers"][0]["optional"] = 1.into()
        }))
        .unwrap_err();
        assert_eq!(e.field, "decoder.handlers[0].optional", "{e}");
        // A program row's block is never optional.
        assert!(!parse(ADSB).unwrap().handler_blocks()[0].optional);
    }

    #[test]
    fn an_install_script_is_optional_in_a_row() {
        let row = parse(&edited(M17, |v| {
            v["decoder"]["handlers"][0]["install"]["script"] = "tools/install-vocoders.sh".into()
        }))
        .unwrap();
        let blocks = row.handler_blocks();
        assert_eq!(
            blocks[0].install.script.as_deref(),
            Some("tools/install-vocoders.sh")
        );
        assert_eq!(
            blocks[0].install.hint().as_deref(),
            Some("bash tools/install-vocoders.sh")
        );
        let row = parse(M17).unwrap();
        let blocks = row.handler_blocks();
        assert_eq!(blocks[0].install.script, None);
        assert_eq!(
            blocks[0].install.hint().as_deref(),
            Some("apt install codec2")
        );
    }

    fn install(v: serde_json::Value) -> Install {
        let mut base = serde_json::json!({"url": "https://example.org/x"});
        base.as_object_mut()
            .unwrap()
            .extend(v.as_object().unwrap().clone());
        serde_json::from_value(base).unwrap()
    }

    #[test]
    fn the_hint_follows_the_package_managers_the_host_has() {
        let both = install(serde_json::json!({"debian": "pkg", "flatpak": "org.example.Pkg"}));
        // Only flatpak on the host: its entry, though the row also has a Debian one.
        assert_eq!(
            both.hint_for(&["flatpak"]).as_deref(),
            Some("flatpak install org.example.Pkg")
        );
        // The fixed order, whatever order the host's list is in: apt before snap.
        let all = install(serde_json::json!({"debian": "pkg", "snap": "pkg-snap"}));
        assert_eq!(
            all.hint_for(&["snap", "apt"]).as_deref(),
            Some("apt install pkg")
        );
        assert_eq!(
            all.hint_for(&["snap"]).as_deref(),
            Some("snap install pkg-snap")
        );
        // None of the host's managers has an entry (or the host has none): today's order
        // over the row's entries.
        let row = install(serde_json::json!({"fedora": "f", "flatpak": "org.example.Pkg"}));
        assert_eq!(row.hint_for(&["apt"]).as_deref(), Some("dnf install f"));
        assert_eq!(row.hint_for(&[]).as_deref(), Some("dnf install f"));
        let row = install(serde_json::json!({"snap": "s", "flatpak": "org.example.Pkg"}));
        assert_eq!(row.hint_for(&["apt"]).as_deref(), Some("snap install s"));
        // No entry at all: no hint.
        assert_eq!(install(serde_json::json!({})).hint_for(&["apt"]), None);
    }

    #[test]
    fn an_install_script_wins_over_every_package_manager() {
        let row = install(serde_json::json!({
            "debian": "pkg", "snap": "s", "script": "tools/install.sh"
        }));
        assert_eq!(
            row.hint_for(&["apt", "snap"]).as_deref(),
            Some("bash tools/install.sh")
        );
    }

    #[test]
    fn snap_and_flatpak_are_install_fields_and_an_unknown_key_is_still_refused() {
        let row = parse(&edited(M17, |v| {
            v["decoder"]["handlers"][0]["install"]["snap"] = "codec2".into();
            v["decoder"]["handlers"][0]["install"]["flatpak"] = "org.example.Codec2".into();
        }))
        .unwrap();
        let i = &row.handler_blocks()[0].install;
        assert_eq!(i.snap.as_deref(), Some("codec2"));
        assert_eq!(i.flatpak.as_deref(), Some("org.example.Codec2"));
        let e = parse(&edited(M17, |v| {
            v["decoder"]["handlers"][0]["install"]["appimage"] = "x".into()
        }))
        .unwrap_err();
        assert_eq!(e.field, "appimage", "{e}");
    }

    #[test]
    fn the_host_probe_names_managers_in_the_fixed_order_from_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        for name in ["snap", "apt"] {
            let p = dir.path().join(name);
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Not executable: not there.
        std::fs::write(dir.path().join("dnf"), "").unwrap();
        assert_eq!(
            crate::modes::handlers::package_managers_on(Some(dir.path().as_os_str().to_owned())),
            vec!["apt", "snap"]
        );
        assert!(crate::modes::handlers::package_managers_on(None).is_empty());
    }

    /// `ADSB` (events on tcp) with a `decoder.packets` block: pcap on `$packet_port`, 147
    /// read as `mode_s`, then `edit` applied to the block.
    fn with_packets(edit: impl FnOnce(&mut serde_json::Value)) -> String {
        edited(ADSB, |v| {
            let mut p = serde_json::json!({
                "via": "tcp", "port": "$packet_port", "parser": "pcap",
                "link_types": {"147": "mode_s"}
            });
            edit(&mut p);
            v["decoder"]["packets"] = p;
            v["decoder"]["args"]
                .as_array_mut()
                .unwrap()
                .extend(["--pcap-port".into(), "$packet_port".into()]);
        })
    }

    #[test]
    fn a_packets_block_round_trips_and_defaults_to_none() {
        // The row's own Beast block left out (and its port with it).
        let row = parse(&edited(ADSB, |v| {
            v["decoder"].as_object_mut().unwrap().remove("packets");
            v["decoder"]["args"] = serde_json::json!(["--net-sbs-port", "$port"]);
        }))
        .unwrap();
        assert_eq!(program(&row).packets, None);
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(v["decoder"]["packets"], serde_json::Value::Null);
        let row = parse(&with_packets(|_| {})).unwrap();
        validate(&row, &no_stages()).unwrap();
        let p = program(&row).packets.as_ref().expect("a packets block");
        assert_eq!(p.via, OutputVia::Tcp);
        assert_eq!(p.port.as_deref(), Some("$packet_port"));
        assert_eq!(p.parser, PacketParserKind::Pcap);
        assert_eq!(
            p.link_types,
            BTreeMap::from([("147".to_string(), crate::stream::LinkType::ModeS)])
        );
        let again = parse(&serde_json::to_string(&row).unwrap()).unwrap();
        assert_eq!(again, row);
        // `link_types` may be left out.
        let row = parse(&with_packets(|p| {
            p.as_object_mut().unwrap().remove("link_types");
        }))
        .unwrap();
        assert!(
            program(&row)
                .packets
                .as_ref()
                .unwrap()
                .link_types
                .is_empty()
        );
    }

    #[test]
    fn packets_on_stdout_beside_events_on_stdout_is_refused() {
        let text = edited(&with_packets(|p| p["via"] = "stdout".into()), |v| {
            v["decoder"]["output"]["via"] = "stdout".into();
            v["decoder"]["output"]["port"] = serde_json::Value::Null;
        });
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(
            e,
            RowError::new("decoder.packets.via", "stdout already carries the events")
        );
        // Beside events on tcp, stdout is the packets' own.
        let text = with_packets(|p| {
            p["via"] = "stdout".into();
            p["port"] = serde_json::Value::Null;
        });
        validate(&parse(&text).unwrap(), &no_stages()).unwrap();
    }

    #[test]
    fn packets_via_tcp_needs_the_packet_port() {
        for port in [serde_json::Value::Null, "$port".into(), "30005".into()] {
            let text = with_packets(|p| p["port"] = port.clone());
            let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
            assert_eq!(e.field, "decoder.packets.port", "{port}: {e}");
        }
    }

    #[test]
    fn a_link_types_key_that_is_not_a_number_is_refused() {
        for key in ["usb", "-1", "65536", ""] {
            let text = with_packets(|p| p["link_types"] = serde_json::json!({key: "mode_s"}));
            let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
            assert_eq!(e.field, format!("decoder.packets.link_types.{key}"), "{e}");
        }
        let text = with_packets(|p| p["link_types"] = serde_json::json!({"195": {"other": 195}}));
        validate(&parse(&text).unwrap(), &no_stages()).unwrap();
    }

    #[test]
    fn packets_via_tcp_need_an_argument_naming_the_packet_port() {
        let text = edited(&with_packets(|_| {}), |v| {
            v["decoder"]["args"]
                .as_array_mut()
                .unwrap()
                .retain(|a| !a.as_str().unwrap().contains("$packet_port"));
        });
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(
            e,
            RowError::new("decoder.packets.port", "no argument names $packet_port")
        );
    }

    #[test]
    fn packets_via_stdout_take_no_port() {
        let text = with_packets(|p| p["via"] = "stdout".into());
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(
            e,
            RowError::new("decoder.packets.port", "stdout takes no port")
        );
    }

    #[test]
    fn a_link_types_key_not_spelled_as_its_number_is_refused() {
        let text = with_packets(|p| {
            p["link_types"] = serde_json::json!({"0147": "mode_s", "147": "user0"})
        });
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(
            e,
            RowError::new("decoder.packets.link_types.0147", "duplicates 147")
        );
        let text = with_packets(|p| p["link_types"] = serde_json::json!({"0147": "mode_s"}));
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(
            e,
            RowError::new("decoder.packets.link_types.0147", "write it 147")
        );
    }

    #[test]
    fn quick_frame_needs_a_frame_row() {
        let text = edited(POCSAG, |v| v["identify"]["quick"] = "frame".into());
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(
            e.to_string(),
            "identify.quick: frame needs decoder.kind frame"
        );
    }

    /// A program row's own handler goes by its first program's file name: `none`, the
    /// stand-in of a row with no decoder, is refused there.
    #[test]
    fn a_program_named_none_is_reserved() {
        let text = edited(ADSB, |v| {
            v["decoder"]["programs"] = serde_json::json!(["/opt/x/none", "dump1090"])
        });
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(e.field, "decoder.programs[0]", "{e}");
        let text = edited(ADSB, |v| {
            v["decoder"]["programs"] = serde_json::json!(["dump1090", "none"])
        });
        assert_eq!(validate(&parse(&text).unwrap(), &no_stages()), Ok(()));
    }
}
