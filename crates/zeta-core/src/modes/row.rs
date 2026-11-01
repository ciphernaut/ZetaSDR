//! One mode's row: the schema of a `modes/*.json` file and its validation.
//!
//! A row names a decoder mode (`job_start --kind <id>`): its band, the layer-1 labels it
//! presents as, the decoder (a graph template or an external program), its event schema
//! and one-line summary, and the decode-to-identify rule. Every struct rejects unknown
//! fields; [`parse`] names the offending field, [`validate`] checks what serde cannot.

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
}

/// What a mode's events are about: the event field naming it, the table columns the
/// page shows per subject and how the waterfall draws it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
}

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

/// The decoder: a graph template, or an external program.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(deny_unknown_fields, rename = "ModeDecoder")]
pub enum Decoder {
    /// Today's `Graph` shape without its id; the channel node's `center_hz`,
    /// `bandwidth_hz` and `out_rate_hz` are written `"$center_hz"`, `"$bandwidth_hz"`,
    /// `"$out_rate_hz"` for the expander to fill.
    Graph {
        nodes: Vec<Node>,
        edges: Vec<Edge>,
    },
    Program(ProgramBlock),
}

/// Which kind of decoder a row has, as `modes_list` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecoderKind {
    Graph,
    Program,
}

impl Decoder {
    pub fn kind(&self) -> DecoderKind {
        match self {
            Decoder::Graph { .. } => DecoderKind::Graph,
            Decoder::Program(_) => DecoderKind::Program,
        }
    }
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
    /// `acars`, `rds`); `null` for rows tried only by the `identify` job.
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
    /// Arguments, with `$port`, `$rate_hz`, `$center_hz` and `$fifo` (the named pipe of
    /// `input.via: fifo`) substituted.
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
const PROGRAM_PLACEHOLDERS: [&str; 4] = ["$port", "$rate_hz", "$center_hz", "$fifo"];
/// The in-crate attempts `identify.quick` may name.
pub const QUICK_ATTEMPTS: [&str; 4] = ["aprs", "pocsag", "acars", "rds"];
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
        let named = ["unknown field `", "missing field `"]
            .iter()
            .find_map(|p| msg.split_once(p))
            .and_then(|(_, rest)| rest.split_once('`'))
            .map(|(name, _)| name.to_string());
        RowError::new(named.unwrap_or_else(|| "row".into()), msg)
    })
}

/// The field whose value is not one of its allowed words (`subject.waterfall`,
/// `subject.columns[<i>].unit`, `position.alt_unit`), read from the row's JSON.
fn bad_word_field(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let word = |x: &serde_json::Value, ok: &[&str]| x.as_str().is_none_or(|w| !ok.contains(&w));
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
        if sub.key.is_empty() {
            return Err(RowError::new("subject.key", "must not be empty"));
        }
        if sub.name.is_empty() {
            return Err(RowError::new("subject.name", "must not be empty"));
        }
        named.push(("subject.key".into(), sub.key.clone()));
        for (i, c) in sub.columns.iter().enumerate() {
            named.push((format!("subject.columns[{i}].field"), c.field.clone()));
        }
        named.extend(sub.heading.clone().map(|h| ("subject.heading".into(), h)));
        named.extend(sub.label.clone().map(|l| ("subject.label".into(), l)));
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
    match &row.decoder {
        Decoder::Graph { nodes, edges } => validate_graph(nodes, edges, stages),
        Decoder::Program(p) => validate_program(p),
    }
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
}
