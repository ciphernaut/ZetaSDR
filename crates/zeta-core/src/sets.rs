//! Named sets: one JSON file each naming the jobs of a daily setup by radio role.
//!
//! The embedded sets (`sets/*.json` in the repo, compiled in) are read first, then the
//! operator directory (`--sets-dir`, default `$XDG_CONFIG_HOME/zetasdr/sets`, else
//! `~/.config/zetasdr/sets`), with the mode registry's rules: an operator set with an
//! embedded set's `id` replaces it, and loading never fails. A file that does not parse
//! or validate is kept `broken` with its reason; one that meant to replace a set leaves
//! that set in force and is listed beside it as `<id>@<file name>`.
//!
//! A set names roles (`radios`), never sources: the request maps each role to a source.
//! Its `members` are `job_start` and `plan_start` requests without `source`, `elastic`
//! and `replace`; a `watch` member carries `params` for its `detect` node.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::modes::{ModeRegistry, is_mode_id};
use crate::ops::{JobKind, Sideband};
use crate::stage::StageRegistry;
use crate::types::Hz;

/// The embedded sets, one file each under `sets/` in the repo.
const EMBEDDED: [(&str, &str); 2] = [
    (
        "sets/airband.json",
        include_str!("../../../sets/airband.json"),
    ),
    (
        "sets/pagers_au.json",
        include_str!("../../../sets/pagers_au.json"),
    ),
];

/// A set's id: `^[a-z][a-z0-9_]*$` (see [`is_set_id`]).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct SetId(pub String);

/// Whether `id` has a set id's shape, `^[a-z][a-z0-9_]*$` (a mode id's).
pub fn is_set_id(id: &str) -> bool {
    is_mode_id(id)
}

/// One set file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct SetFile {
    pub id: SetId,
    pub name: String,
    pub notes: String,
    /// The roles in the file's order (a JSON object on the wire), each with a note for
    /// the page and the agent.
    #[serde(serialize_with = "ser_radios", deserialize_with = "de_radios")]
    #[schemars(schema_with = "radios_schema")]
    pub radios: Vec<(String, Radio)>,
    pub members: Vec<Member>,
}

/// One radio role of a set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Radio {
    /// What the role needs (`a radio that captures 130.0-137.0 MHz at once (8 MHz)`).
    pub note: String,
}

fn ser_radios<S: serde::Serializer>(radios: &[(String, Radio)], s: S) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeMap;
    let mut map = s.serialize_map(Some(radios.len()))?;
    for (role, radio) in radios {
        map.serialize_entry(role, radio)?;
    }
    map.end()
}

/// The roles object in the file's order (`serde_json` here keeps no object order, so
/// the map is read entry by entry); a role listed twice is an error.
fn de_radios<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<(String, Radio)>, D::Error> {
    struct Roles;
    impl<'de> serde::de::Visitor<'de> for Roles {
        type Value = Vec<(String, Radio)>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("an object of roles")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut roles: Vec<(String, Radio)> = Vec::new();
            while let Some((role, radio)) = map.next_entry::<String, Radio>()? {
                if roles.iter().any(|(r, _)| *r == role) {
                    return Err(serde::de::Error::custom(format!(
                        "role {role} listed twice"
                    )));
                }
                roles.push((role, radio));
            }
            Ok(roles)
        }
    }
    d.deserialize_map(Roles)
}

fn radios_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let radio = g.subschema_for::<Radio>();
    schemars::json_schema!({"type": "object", "additionalProperties": radio})
}

/// One member of a set: a `job_start` or a `plan_start` without `source`, `elastic` and
/// `replace`, on the role `radio`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub enum Member {
    JobStart {
        id: String,
        radio: String,
        kind: JobKind,
        center_hz: Hz,
        bandwidth_hz: Hz,
        #[serde(deserialize_with = "crate::modes::nullable")]
        #[schemars(required, schema_with = "crate::types::nullable::<u64>")]
        deviation_hz: Option<u64>,
        #[serde(deserialize_with = "crate::modes::nullable")]
        #[schemars(required, schema_with = "crate::modes::nullable_enum::<Sideband>")]
        sideband: Option<Sideband>,
        /// A `watch` member's params for its `detect` node; absent on any other kind.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params: Option<Value>,
    },
    PlanStart {
        id: String,
        radio: String,
        plan: String,
        #[serde(deserialize_with = "crate::modes::nullable")]
        #[schemars(required, schema_with = "crate::types::nullable::<Vec<u32>>")]
        channels: Option<Vec<u32>>,
        kind: JobKind,
        #[serde(deserialize_with = "crate::modes::nullable")]
        #[schemars(required, schema_with = "crate::types::nullable::<u64>")]
        deviation_hz: Option<u64>,
        #[serde(deserialize_with = "crate::modes::nullable")]
        #[schemars(required, schema_with = "crate::modes::nullable_enum::<Sideband>")]
        sideband: Option<Sideband>,
        #[serde(deserialize_with = "crate::modes::nullable")]
        #[schemars(required, schema_with = "crate::modes::nullable_enum::<JobKind>")]
        attach: Option<JobKind>,
        #[serde(deserialize_with = "crate::modes::nullable")]
        #[schemars(required, schema_with = "crate::types::nullable::<String>")]
        language: Option<String>,
    },
}

impl Member {
    pub fn id(&self) -> &str {
        match self {
            Member::JobStart { id, .. } | Member::PlanStart { id, .. } => id,
        }
    }

    pub fn radio(&self) -> &str {
        match self {
            Member::JobStart { radio, .. } | Member::PlanStart { radio, .. } => radio,
        }
    }

    pub fn kind(&self) -> &JobKind {
        match self {
            Member::JobStart { kind, .. } | Member::PlanStart { kind, .. } => kind,
        }
    }

    /// The op the member runs: `job_start` or `plan_start`.
    pub fn op_name(&self) -> &'static str {
        match self {
            Member::JobStart { .. } => "job_start",
            Member::PlanStart { .. } => "plan_start",
        }
    }

    /// One line for the page and the agent: `vdl2_decode at 136.975 MHz, 25 kHz wide`,
    /// `acars_decode on every channel of acars_vhf`, `watch over 148-149 MHz for
    /// pocsag_decode`.
    pub fn summary(&self) -> String {
        match self {
            Member::JobStart {
                kind: JobKind::Watch,
                center_hz,
                bandwidth_hz,
                params,
                ..
            } => {
                let low = center_hz.0.saturating_sub(bandwidth_hz.0 / 2);
                let high = center_hz.0.saturating_add(bandwidth_hz.0 / 2);
                let mut s = format!("watch over {}-{} MHz", mhz(low), mhz(high));
                let modes: Vec<&str> = params
                    .as_ref()
                    .and_then(|p| p.get("modes"))
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                if !modes.is_empty() {
                    s.push_str(&format!(" for {}", modes.join(", ")));
                }
                s
            }
            Member::JobStart {
                kind,
                center_hz,
                bandwidth_hz,
                ..
            } => format!(
                "{} at {} MHz, {} kHz wide",
                kind.name(),
                mhz(center_hz.0),
                khz(bandwidth_hz.0)
            ),
            Member::PlanStart {
                kind,
                plan,
                channels,
                attach,
                ..
            } => {
                let on = match channels {
                    None => format!("every channel of {plan}"),
                    Some(c) => {
                        let list: Vec<String> = c.iter().map(u32::to_string).collect();
                        format!("channels {} of {plan}", list.join(", "))
                    }
                };
                let mut s = format!("{} on {on}", kind.name());
                if let Some(a) = attach {
                    s.push_str(&format!(", {} attached", a.name()));
                }
                s
            }
        }
    }
}

/// `hz` in MHz with no trailing zeros (`136.975`, `148`).
fn mhz(hz: u64) -> String {
    trim(format!("{}.{:06}", hz / 1_000_000, hz % 1_000_000))
}

/// `hz` in kHz with no trailing zeros (`25`, `12.5`).
fn khz(hz: u64) -> String {
    trim(format!("{}.{:03}", hz / 1_000, hz % 1_000))
}

fn trim(s: String) -> String {
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Why a set file is broken: the offending field and what is wrong with it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {error}")]
pub struct SetError {
    pub field: String,
    pub error: String,
}

impl SetError {
    fn new(field: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            error: error.into(),
        }
    }
}

/// The field a serde error names (`unknown field `x``, `missing field `x``), else `whole`.
fn named_field(msg: &str, whole: &str) -> String {
    ["unknown field `", "missing field `"]
        .iter()
        .find_map(|p| msg.split_once(p))
        .and_then(|(_, rest)| rest.split_once('`'))
        .map_or_else(|| whole.to_string(), |(name, _)| name.to_string())
}

/// Parse a set file. A `source` or `replace` key on a member is refused naming it, read
/// from the raw JSON before the record is parsed; a member that does not parse is
/// named `members[<i>]` (with its field when serde names one).
pub fn parse(text: &str) -> Result<SetFile, SetError> {
    let raw: Value = serde_json::from_str(text).map_err(|e| SetError::new("set", e.to_string()))?;
    let members = raw.get("members").and_then(Value::as_array);
    for (i, m) in members.into_iter().flatten().enumerate() {
        if m.get("source").is_some() {
            return Err(SetError::new(
                format!("members[{i}].source"),
                "a set file names no source; the request does",
            ));
        }
        if m.get("replace").is_some() {
            return Err(SetError::new(
                format!("members[{i}].replace"),
                "never in a set file",
            ));
        }
    }
    // Parsed from the text, not from `raw`, so `radios` keeps the file's order.
    serde_json::from_str::<SetFile>(text).map_err(|e| {
        for (i, m) in members.into_iter().flatten().enumerate() {
            if let Err(me) = serde_json::from_value::<Member>(m.clone()) {
                let msg = me.to_string();
                let field = match named_field(&msg, "") {
                    f if f.is_empty() => format!("members[{i}]"),
                    f => format!("members[{i}].{f}"),
                };
                return SetError::new(field, msg);
            }
        }
        let msg = e.to_string();
        let field = if msg.contains("role ") && msg.contains("listed twice") {
            "radios".to_string()
        } else {
            named_field(&msg, "set")
        };
        SetError::new(field, msg)
    })
}

/// Check a parsed set against the node's rows and stages: the ids' shapes, each member
/// id unique and on a known role, each member's op fields as that op checks them (a
/// kind need not be a loaded row: that is a start-time refusal), and a `watch` member's
/// `params` as its `detect` node takes them (the stage's params check, then its `modes`
/// as `job_start` checks them against the member's band).
pub fn validate(
    set: &SetFile,
    modes: &ModeRegistry,
    stages: &StageRegistry,
) -> Result<(), SetError> {
    if !is_set_id(&set.id.0) {
        return Err(SetError::new(
            "id",
            format!("{:?} does not match ^[a-z][a-z0-9_]*$", set.id.0),
        ));
    }
    for (role, _) in &set.radios {
        if !is_set_id(role) {
            return Err(SetError::new(
                format!("radios.{role}"),
                "a role matches ^[a-z][a-z0-9_]*$",
            ));
        }
    }
    if set.members.is_empty() {
        return Err(SetError::new("members", "at least one member"));
    }
    for (i, m) in set.members.iter().enumerate() {
        let at = |field: &str| format!("members[{i}].{field}");
        if !is_set_id(m.id()) {
            return Err(SetError::new(
                at("id"),
                format!("{:?} does not match ^[a-z][a-z0-9_]*$", m.id()),
            ));
        }
        if set.members[..i].iter().any(|o| o.id() == m.id()) {
            return Err(SetError::new(at("id"), "listed twice"));
        }
        if !set.radios.iter().any(|(r, _)| r == m.radio()) {
            return Err(SetError::new(
                at("radio"),
                format!("unknown role {}", m.radio()),
            ));
        }
        validate_member(m, modes, stages).map_err(|e| SetError::new(at(&e.field), e.error))?;
    }
    Ok(())
}

/// One member's op fields (see [`validate`]); the field named relative to the member.
fn validate_member(
    m: &Member,
    modes: &ModeRegistry,
    stages: &StageRegistry,
) -> Result<(), SetError> {
    let kind = m.kind();
    match m {
        Member::JobStart {
            bandwidth_hz,
            deviation_hz,
            sideband,
            params,
            ..
        } => {
            crate::ops::check_startable(kind).map_err(|e| SetError::new("kind", e.to_string()))?;
            // A capture-following, stepping or trial-running kind is no set's member.
            if matches!(kind, JobKind::Spectrum | JobKind::Scan | JobKind::Identify) {
                return Err(SetError::new(
                    "kind",
                    format!("{} cannot be a set member", kind.name()),
                ));
            }
            crate::ops::check_start_fields(kind, *deviation_hz, *sideband).map_err(field)?;
            if bandwidth_hz.0 == 0 {
                return Err(SetError::new("bandwidth_hz", "at least 1 Hz"));
            }
            match (params, kind) {
                (Some(_), k) if *k != JobKind::Watch => {
                    Err(SetError::new("params", "only a watch member takes params"))
                }
                (Some(p), _) => watch_params(p, bandwidth_hz.0, modes, stages),
                (None, _) => Ok(()),
            }
        }
        Member::PlanStart {
            plan,
            channels,
            deviation_hz,
            sideband,
            attach,
            language,
            ..
        } => crate::ops::check_plan_start_fields(
            plan,
            channels.as_deref(),
            kind,
            (*deviation_hz, *sideband),
            (attach.as_ref(), language.as_deref()),
        )
        .map(|_| ())
        .map_err(field),
    }
}

/// An op's field error as the member's: `<field>: <error>`.
fn field(e: crate::ops::FieldError) -> SetError {
    SetError::new(e.field, e.error.to_string())
}

/// A `watch` member's `params`: the `detect` stage's own check (named by the first
/// key that fails on its own, so `params.hold_ms`), no `prefer` sources, then its
/// `modes` against the member's band (`params.modes[<j>]`; a row this node lacks or
/// cannot run passes, refused when the set starts).
fn watch_params(
    params: &Value,
    band_hz: u64,
    modes: &ModeRegistry,
    stages: &StageRegistry,
) -> Result<(), SetError> {
    let detect = crate::ops::DETECT_STAGE;
    if let Err(whole) = stages.check_params(detect, params) {
        let first = params.as_object().and_then(|o| {
            o.iter().find_map(|(k, v)| {
                let alone = serde_json::json!({ k.clone(): v.clone() });
                stages
                    .check_params(detect, &alone)
                    .err()
                    .map(|e| (k.clone(), e))
            })
        });
        return Err(match first {
            Some((k, e)) => {
                let e = e
                    .strip_prefix(&format!("{k}: "))
                    .map_or(e.clone(), str::to_string);
                SetError::new(format!("params.{k}"), e)
            }
            None => SetError::new("params", whole),
        });
    }
    // A set file names no source: the request maps its roles.
    if params
        .get("prefer")
        .and_then(Value::as_array)
        .is_some_and(|p| !p.is_empty())
    {
        return Err(SetError::new(
            "params.prefer",
            "a set file names no source; the request does",
        ));
    }
    let list: Option<Vec<String>> = params.get("modes").and_then(Value::as_array).map(|a| {
        a.iter()
            .filter_map(|s| s.as_str().map(String::from))
            .collect()
    });
    if let Some(list) = list {
        crate::node::watch::check_modes_list_at_load(modes, &list, band_hz)
            .map_err(|e| SetError::new("params", e))
            .map_err(|e| match e.error.split_once(": ") {
                // `modes[<j>]: ...` names its field: `params.modes[<j>]`.
                Some((f, rest)) if f.starts_with("modes[") => {
                    SetError::new(format!("params.{f}"), rest)
                }
                Some(("modes", rest)) => SetError::new("params.modes", rest),
                _ => SetError::new("params.modes", e.error),
            })?;
    }
    Ok(())
}

/// Where a loaded set stands: `ready` (parsed and validated) or `broken` (`reason`
/// names the field).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SetState {
    Ready,
    Broken,
}

/// One set as the registry holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedSet {
    /// The set. A broken file's set is what parsed, or a placeholder carrying only its
    /// id when nothing did; a broken file that meant to replace a set is listed under
    /// `<id>@<file name>`, the set it meant to replace staying in force.
    pub set: SetFile,
    pub state: SetState,
    /// Why the set is broken (`<field>: <error>`); `None` when ready.
    pub reason: Option<String>,
    /// `sets/<id>.json` for an embedded set, the path for an operator set.
    pub file: String,
}

/// Every set the node knows, sorted by id.
#[derive(Clone, Debug, Default)]
pub struct SetRegistry {
    sets: Vec<LoadedSet>,
}

impl SetRegistry {
    /// The embedded sets: `(file name, contents)`.
    pub fn embedded() -> Vec<(String, &'static str)> {
        EMBEDDED
            .iter()
            .map(|(f, text)| ((*f).to_string(), *text))
            .collect()
    }

    /// Load the embedded sets, then every `*.json` file of `operator_dir` (by file name;
    /// a missing or unreadable directory reads as empty), validating each against
    /// `modes` and `stages`. A valid operator set replaces the set with its id; a broken
    /// one is listed beside it (see [`LoadedSet::set`]). Never fails.
    pub fn load(
        modes: &ModeRegistry,
        stages: &StageRegistry,
        operator_dir: Option<&Path>,
    ) -> SetRegistry {
        let mut reg = SetRegistry::default();
        for (file, text) in EMBEDDED {
            reg.add(file.to_string(), text, modes, stages);
        }
        let mut files: Vec<PathBuf> = operator_dir
            .and_then(|d| std::fs::read_dir(d).ok())
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "json") && p.is_file())
            .collect();
        files.sort();
        for path in files {
            let file = path.display().to_string();
            match std::fs::read_to_string(&path) {
                Ok(text) => reg.add(file, &text, modes, stages),
                Err(e) => reg.add_broken(
                    &stem(&path),
                    None,
                    file,
                    SetError::new("file", e.to_string()),
                ),
            }
        }
        reg.sets.sort_by(|a, b| a.set.id.cmp(&b.set.id));
        reg
    }

    /// Parse, validate and add one set file: a ready set replaces the set with its id.
    fn add(&mut self, file: String, text: &str, modes: &ModeRegistry, stages: &StageRegistry) {
        let set = match parse(text) {
            Ok(set) => set,
            Err(e) => {
                // The id the file meant, from its raw JSON, else its name.
                let id = serde_json::from_str::<Value>(text)
                    .ok()
                    .and_then(|v| v["id"].as_str().map(String::from))
                    .unwrap_or_else(|| stem(Path::new(&file)));
                return self.add_broken(&id, None, file, e);
            }
        };
        if let Err(e) = validate(&set, modes, stages) {
            let id = set.id.0.clone();
            return self.add_broken(&id, Some(set), file, e);
        }
        let loaded = LoadedSet {
            set,
            state: SetState::Ready,
            reason: None,
            file,
        };
        match self.sets.iter_mut().find(|s| s.set.id == loaded.set.id) {
            Some(s) => *s = loaded,
            None => self.sets.push(loaded),
        }
    }

    /// List a broken file under `id`, or under `<id>@<file name>` when a set already
    /// has that id (which stays in force).
    fn add_broken(&mut self, id: &str, set: Option<SetFile>, file: String, e: SetError) {
        let id = match self.sets.iter().any(|s| s.set.id.0 == id) {
            true => {
                let name = Path::new(&file)
                    .file_name()
                    .map_or_else(|| file.clone(), |n| n.to_string_lossy().into_owned());
                format!("{id}@{name}")
            }
            false => id.to_string(),
        };
        let mut set = set.unwrap_or_else(|| SetFile {
            id: SetId(String::new()),
            name: id.clone(),
            notes: String::new(),
            radios: Vec::new(),
            members: Vec::new(),
        });
        set.id = SetId(id);
        self.sets.push(LoadedSet {
            set,
            state: SetState::Broken,
            reason: Some(e.to_string()),
            file,
        });
    }

    /// The set with id `id`.
    pub fn get(&self, id: &str) -> Option<&LoadedSet> {
        self.sets.iter().find(|s| s.set.id.0 == id)
    }

    /// Every set, sorted by id.
    pub fn all(&self) -> &[LoadedSet] {
        &self.sets
    }

    /// Every set's id, sorted.
    pub fn ids(&self) -> Vec<String> {
        self.sets.iter().map(|s| s.set.id.0.clone()).collect()
    }

    /// One stderr line per broken set: `zeta: set <id> (<file>): <field>: <error>`.
    pub fn startup_lines(&self) -> Vec<String> {
        self.sets
            .iter()
            .filter(|s| s.state == SetState::Broken)
            .map(|s| {
                let id = s.set.id.0.split('@').next().unwrap_or_default();
                let reason = s.reason.as_deref().unwrap_or_default();
                format!("zeta: set {id} ({}): {reason}", s.file)
            })
            .collect()
    }
}

/// Current on-disk format of [`SetInstance`].
pub const SETS_INSTANCE_FORMAT: u32 = 1;

/// A set's running instance, written by `set_start` to `state_dir/sets/<set id>.json`
/// and read back when the node opens: the source each role runs on and the jobs each
/// member started (an attached job beside its demodulator). One per set at a time;
/// dropped by `set_stop`, or by the next `set_start` once every job it names has ended
/// (`set_list`, a read, then shows it as not running and leaves the file).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct SetInstance {
    pub format: u32,
    pub set: String,
    pub radios: std::collections::BTreeMap<String, crate::types::SourceId>,
    pub since_ns: crate::types::Timestamp,
    pub members: Vec<InstanceMember>,
}

/// One member of a [`SetInstance`] and its jobs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct InstanceMember {
    pub id: String,
    pub jobs: Vec<crate::types::JobId>,
}

impl SetInstance {
    /// Every job the instance names, member by member.
    pub fn jobs(&self) -> impl Iterator<Item = &crate::types::JobId> {
        self.members.iter().flat_map(|m| m.jobs.iter())
    }
}

/// `$XDG_CONFIG_HOME/zetasdr/sets`, else `$HOME/.config/zetasdr/sets`; `None` when
/// neither variable is set.
pub fn default_operator_dir() -> Option<PathBuf> {
    let var = |k| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    var("XDG_CONFIG_HOME")
        .map(|x| x.join("zetasdr/sets"))
        .or_else(|| var("HOME").map(|h| h.join(".config/zetasdr/sets")))
}

/// A file's name without its extension.
fn stem(path: &Path) -> String {
    path.file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_read_in_mhz_and_khz() {
        assert_eq!(mhz(136_975_000), "136.975");
        assert_eq!(mhz(148_000_000), "148");
        assert_eq!(khz(25_000), "25");
        assert_eq!(khz(12_500), "12.5");
    }

    #[test]
    fn a_role_listed_twice_is_named() {
        let e = parse(
            r#"{"id": "x", "name": "", "notes": "", "radios": {"a": {"note": ""}, "a": {"note": ""}},
                "members": []}"#,
        )
        .unwrap_err();
        assert_eq!(e.field, "radios", "{e}");
    }
}
