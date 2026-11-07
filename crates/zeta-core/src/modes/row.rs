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
    pub decoder: Decoder,
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
    /// `restricted` marks a row whose events carry content a deployment may show to fewer
    /// people than the spectrum (pager text); copied onto every event as `access`; `null`
    /// for the rest. Nothing gates on it yet.
    #[serde(default)]
    pub access: Option<Access>,
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

/// The channel a mode's decoder expects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    /// The span `[lo, hi]` the mode is found anywhere in (radiosondes in 400-406 MHz);
    /// `null` for none.
    #[serde(default)]
    pub range_hz: Option<[u64; 2]>,
}

impl Band {
    /// Whether a box `bandwidth_hz` wide is wide enough for this mode: its `min_hz`,
    /// else its `bandwidth_hz`, is no wider.
    pub fn fits(&self, bandwidth_hz: u64) -> bool {
        self.min_hz.unwrap_or(self.bandwidth_hz) <= bandwidth_hz
    }
}

/// The decoder: a graph template, an external program, or a frame chain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(deny_unknown_fields, rename = "ModeDecoder")]
pub enum Decoder {
    /// Today's `Graph` shape without its id; the channel node's `center_hz`,
    /// `bandwidth_hz` and `out_rate_hz` are written `"$center_hz"`, `"$bandwidth_hz"`,
    /// `"$out_rate_hz"` for the expander to fill.
    #[schemars(transform = require_all)]
    Graph {
        nodes: Vec<Node>,
        edges: Vec<Edge>,
        /// Programs a stage of the graph needs on the host, probed at startup as a
        /// program row's program is: a missing one makes the row `unavailable`.
        #[serde(default)]
        handlers: Vec<Handler>,
    },
    Program(ProgramBlock),
    /// `channel → resample → <demod> → clock_recover → slicer → frame_decode`: the
    /// demodulator and the framing `frame_decode` cuts frames by.
    Frame(FrameBlock),
}

/// Which kind of decoder a row has, as `modes_list` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecoderKind {
    Graph,
    Program,
    Frame,
}

impl Decoder {
    pub fn kind(&self) -> DecoderKind {
        match self {
            Decoder::Graph { .. } => DecoderKind::Graph,
            Decoder::Program(_) => DecoderKind::Program,
            Decoder::Frame(_) => DecoderKind::Frame,
        }
    }

    /// The programs the row needs on the host, each with where to get it: a program
    /// row's one block, a graph row's `handlers` in order; none for a plain graph row
    /// or a frame row (its chain is all in-process stages).
    pub fn handler_blocks(&self) -> Vec<(&[String], &Install)> {
        match self {
            Decoder::Program(p) => vec![(p.programs.as_slice(), &p.install)],
            Decoder::Graph { handlers, .. } => handlers
                .iter()
                .map(|h| (h.programs.as_slice(), &h.install))
                .collect(),
            Decoder::Frame(_) => vec![],
        }
    }
}

/// A program a stage of a graph row needs on the host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, rename = "ModeHandler", transform = require_all)]
pub struct Handler {
    /// Names tried on `PATH` in order (or an absolute path), the first found used.
    pub programs: Vec<String>,
    pub install: Install,
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
    /// `acars`, `rds`, or `frame` on a frame row: one CRC-clean frame of its chain);
    /// `null` for rows tried only by the `identify` job.
    #[serde(default)]
    pub quick: Option<String>,
    /// Among rows of equal score, the higher priority is tried first (0 when absent).
    #[serde(default)]
    pub priority: i32,
}

/// What counts as proof: any event, or an event whose `identify.field` is true.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename = "ModeProof")]
pub enum Proof {
    Event,
    Field,
}

/// An external decoder program.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct ProgramBlock {
    /// Names tried on `PATH` in order (or an absolute path), the first found used.
    pub programs: Vec<String>,
    /// Arguments, with `$port`, `$rate_hz`, `$center_hz`, `$center_khz` (the centre in kHz,
    /// an integer when it divides) and `$fifo` (the named pipe of `input.via: fifo`)
    /// substituted.
    pub args: Vec<String>,
    pub input: ProgramInput,
    pub output: ProgramOutput,
    pub install: Install,
    /// The demodulator between the channel and an `input.stream: audio` program;
    /// required for audio input, `null` for iq.
    #[serde(default)]
    pub demod: Option<Demod>,
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
    pub url: String,
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
}

/// Make every property of an object schema required, without a `default`: a field a row
/// file may leave out (`#[serde(default)]`) is always present, `null` when absent, in
/// what the node reports.
fn require_all(schema: &mut schemars::Schema) {
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

/// The placeholders a graph template may use, each with the dummy value it is checked with.
const GRAPH_PLACEHOLDERS: [(&str, u64); 3] = [
    ("$center_hz", 100_000_000),
    ("$bandwidth_hz", 12_500),
    ("$out_rate_hz", 48_000),
];
/// The placeholders a program's `args` may use.
const PROGRAM_PLACEHOLDERS: [&str; 5] = ["$port", "$rate_hz", "$center_hz", "$center_khz", "$fifo"];
/// The event fields the node adds after a program's `decoder.output.fields` map, exempt
/// from the rule that the map names every schema property.
pub const ENVELOPE_FIELDS: [&str; 4] = ["protocol", "summary", "t_ns", "position"];
/// The in-crate attempts `identify.quick` may name (`frame` on a frame row only).
pub const QUICK_ATTEMPTS: [&str; 5] = ["aprs", "pocsag", "acars", "rds", "frame"];
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
            && let Some(field) = bad_track_field(text)
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
/// `subject.columns[<i>].unit`, `position.alt_unit`, `access`), read from the row's JSON.
fn bad_word_field(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let word = |x: &serde_json::Value, ok: &[&str]| x.as_str().is_none_or(|w| !ok.contains(&w));
    if let Some(a) = v.get("access")
        && !a.is_null()
        && word(a, &[Access::Restricted.word()])
    {
        return Some("access".into());
    }
    if let Some(a) = v.pointer("/position/alt_unit")
        && word(a, &["m", "ft"])
    {
        return Some("position.alt_unit".into());
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
    if !DECODERS.contains(&row.id.0.as_str())
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
    if let Some([lo, hi]) = row.band.range_hz
        && lo >= hi
    {
        return Err(RowError::new(
            "band.range_hz",
            format!("[{lo}, {hi}] must be [lo, hi] with lo < hi"),
        ));
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
    }
    if let Decoder::Program(ProgramBlock {
        output: ProgramOutput {
            fields: Some(fields),
            ..
        },
        ..
    }) = &row.decoder
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
        for name in properties.keys() {
            if !ENVELOPE_FIELDS.contains(&name.as_str()) && !fields.contains_key(name) {
                return Err(RowError::new(
                    format!("event.schema.properties.{name}"),
                    "not mapped by decoder.output.fields",
                ));
            }
        }
    }
    let id = &row.identify;
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
    let frame_row = matches!(row.decoder, Decoder::Frame(_));
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
    match &row.decoder {
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
        Decoder::Program(p) => validate_program(p),
        Decoder::Frame(b) => validate_frame(b, stages),
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
        Value::String(s) if s.starts_with('$') => {
            let (_, dummy) = GRAPH_PLACEHOLDERS
                .iter()
                .find(|(p, _)| p == s)
                .ok_or_else(|| {
                    let known: Vec<&str> = GRAPH_PLACEHOLDERS.iter().map(|(p, _)| *p).collect();
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

fn validate_program(p: &ProgramBlock) -> Result<(), RowError> {
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
        let Decoder::Program(p) = &row.decoder else {
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
        assert_eq!(row.band.range_hz, Some([400_050_000, 406_000_000]));
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
        let Decoder::Program(p) = &row.decoder else {
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
        let Decoder::Program(p) = &row.decoder else {
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
        let row = parse(ADSB).unwrap();
        assert!(row.subject.as_ref().unwrap().merge.is_empty());
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(v["subject"]["merge"], serde_json::json!([]));
        let text = edited(ADSB, |v| {
            v["subject"]["merge"] = serde_json::json!(["icao"])
        });
        let row = parse(&text).unwrap();
        validate(&row, &no_stages()).unwrap();
        assert_eq!(
            row.subject.as_ref().unwrap().merge,
            vec!["icao".to_string()]
        );
        assert_eq!(parse(&serde_json::to_string(&row).unwrap()).unwrap(), row);
        let text = edited(ADSB, |v| {
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
        assert_eq!(row.decoder.kind(), DecoderKind::Frame);
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
        let blocks = row.decoder.handler_blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].0, ["c2dec"]);
        assert_eq!(blocks[0].1.debian.as_deref(), Some("codec2"));
        let Decoder::Graph { handlers, .. } = &parse(POCSAG).unwrap().decoder else {
            panic!("graph row");
        };
        assert!(handlers.is_empty(), "a row without the key has none");
        assert!(parse(POCSAG).unwrap().decoder.handler_blocks().is_empty());
        // A program row's one block.
        let adsb = parse(ADSB).unwrap();
        let blocks = adsb.decoder.handler_blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].0.first().map(String::as_str), Some("dump1090"));
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

    #[test]
    fn quick_frame_needs_a_frame_row() {
        let text = edited(POCSAG, |v| v["identify"]["quick"] = "frame".into());
        let e = validate(&parse(&text).unwrap(), &no_stages()).unwrap_err();
        assert_eq!(
            e.to_string(),
            "identify.quick: frame needs decoder.kind frame"
        );
    }
}
