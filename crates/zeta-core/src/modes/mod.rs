//! The mode registry: every decoder mode the node knows, one row each.
//!
//! The embedded rows (`modes/*.json` in the repo, compiled in) are read first, then the
//! operator directory (`--modes-dir`, default `$XDG_CONFIG_HOME/zetasdr/modes`, else
//! `~/.config/zetasdr/modes`); an operator row with an embedded row's `id` replaces it.
//! Loading never fails: a row that does not parse or validate is kept as `broken` with
//! its reason (and an operator file that fails leaves the row it meant to replace in
//! force), so a bad row never stops the node.
//!
//! Handler files ([`handler_file`]) are read the same way, the embedded ones
//! (`handlers/*.json`) then the operator's (`--handlers-dir`, default `handlers` beside
//! the default modes directory), an operator file replacing the embedded one of its id;
//! each adds a handler to every mode it lists ([`LoadedMode::handlers`], the row's own
//! first). A file or entry that does not load is listed broken and never fails the load.
//!
//! A node holds its registry in a [`ModesHandle`], which `reload` replaces whole (an
//! accepted signature's row written, say): every reader takes the registry from the
//! handle at each use, so it sees the old rows or the new ones, never a mix.

pub mod derive;
pub mod handler_file;
pub mod handlers;
pub mod policy;
mod row;
pub mod score;
pub mod summary;
pub mod template;
pub mod weights;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::stage::StageRegistry;
use crate::types::StreamKind;

pub use row::{
    Access, AltUnit, Band, BuiltinBlock, CenterRule, Container, Content, DEMOD_KINDS, Decoder,
    DecoderKind, Demod, Derive, ENVELOPE_FIELDS, Event, Extractor, FRAME_PARSERS, Family,
    FieldPath, FrameBlock, FrameDemod, Handler, HandlerBlock, IDENTITY_REGISTERS, IDENTITY_STRIPS,
    Identify, Identity, InputStream, InputVia, Install, ModeId, ModeRow, OutputVia,
    PacketParserKind, Parser, PositionMap, ProgramBlock, ProgramInput, ProgramOutput,
    ProgramPackets, Proof, QUICK_ATTEMPTS, Ranges, RideStream, Rides, Role, RowError, RowTest,
    SampleFormat, Station, Subject, SubjectColumn, SubjectUnit, SubjectWaterfall, TestSpec,
    VersionProbe, WidthRule, center_khz_text, is_mode_id, parse, validate,
};
pub(crate) use row::{FRAME_DECODE_STAGE, require_all};

pub use handler_file::{BUILTIN, HandlerBlockSpec, HandlerEntry, HandlerFile, NO_DECODER};

/// The reason of the stand-in own handler of a row with no decoder of its own.
pub const STAND_IN_REASON: &str = "the row has no decoder of its own";

/// The reason of a row with no decoder of its own while no handler for it is ready, before
/// each listed handler's own (see [`LoadedMode::reason`]).
pub const NO_DECODER_REASON: &str = "no decoder of its own, and no handler for it is ready";

/// Where a loaded row stands: `ready` (parsed, validated and, for a program row or a
/// graph row with `handlers`, its required programs found; an optional one missing is
/// listed in `missing`), `broken` (the row does not parse or validate; `reason` names the
/// field) or `unavailable` (a row whose program, or one of whose required handlers, is not
/// on this host; `reason` says which).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModeState {
    Ready,
    Broken,
    Unavailable,
}

/// One row as the registry holds it. (`missing` may be left out of what is read, but is
/// always written: the schema requires every property.)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct LoadedMode {
    /// The row. A broken file's row is what parsed, or a placeholder carrying only
    /// its id when nothing did; a broken file that meant to replace a row is listed
    /// under `<id>@<file name>`, the row it meant to replace staying in force.
    pub row: ModeRow,
    pub state: ModeState,
    /// Why the row is not ready (`<field>: <error>` for a broken one); `null` when ready.
    /// A row with no decoder of its own is never ready itself: [`NO_DECODER_REASON`],
    /// then `; <id>: <reason>` for each handler file's handler of it in order, kept up
    /// to date as each handler's probe answer is recorded.
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    #[serde(deserialize_with = "nullable")]
    pub reason: Option<String>,
    /// The file the row came from: `modes/<id>.json` for an embedded row, the path for
    /// an operator row.
    pub file: String,
    /// The program a program row runs (a graph row's first handler); `null` until
    /// probed, and for a row with no handlers.
    #[schemars(required, schema_with = "crate::types::nullable::<PathBuf>")]
    #[serde(deserialize_with = "nullable")]
    pub program: Option<PathBuf>,
    /// When `zeta modes check` last passed this row with handlers on this host (UNIX
    /// seconds, from `modes-checked.json` in the state directory); `null` if never.
    #[schemars(required, schema_with = "crate::types::nullable::<u64>")]
    #[serde(deserialize_with = "nullable")]
    pub checked: Option<u64>,
    /// The optional handlers not found on this host, one line each,
    /// `<program>: missing (<hint>; <url>) — <what is lost>`: the row runs without them.
    /// Empty until probed, and when every handler is found.
    #[serde(default)]
    pub missing: Vec<String>,
    /// The stream kinds a job of the row emits (see [`template::outputs`]); empty for a
    /// broken row.
    pub outputs: Vec<StreamKind>,
    /// The voices a job of the row has beside its main audio stream (see
    /// [`template::voices`]): empty for a row with one voice, and for a broken row.
    #[serde(default)]
    pub voices: Vec<String>,
    /// Every handler of the mode: the row's own first (whose state, reason, program,
    /// missing and checked are the row's above; for a row with no decoder of its own the
    /// stand-in [`NO_DECODER`], never ready), then each handler file's entry for the
    /// mode, by file id. Reported by the node without the stand-in.
    #[serde(skip)]
    #[schemars(skip)]
    pub handlers: Vec<ModeHandler>,
}

impl LoadedMode {
    /// Whether a start that names no handler can run the mode: one of its handlers is
    /// ready, the row's own or a handler file's (the state `modes_list` reports). Every
    /// caller that starts a decoder with `handler: null` (identify, a watch, a set, a
    /// plan) asks this, never the row's own `state` alone.
    pub fn startable(&self) -> bool {
        self.state == ModeState::Ready || self.handlers.iter().any(|h| h.state == ModeState::Ready)
    }

    /// The id of the row's own handler: `builtin` for a graph or frame row, its first
    /// program for a program row, [`NO_DECODER`] for a row with no decoder of its own.
    pub fn own_handler(&self) -> String {
        handler_file::own_handler_id(&self.row)
    }

    /// What the row is for: [`ModeRow::role`].
    pub fn role(&self) -> Role {
        self.row.role()
    }

    /// Whether the row has a decoder of its own (`decoder` not `null`).
    pub fn has_decoder(&self) -> bool {
        self.row.decoder.is_some()
    }

    /// The handlers the node reports (`modes_list`, `modes_show`, the ids a refusal
    /// lists): [`LoadedMode::handlers`] without the stand-in of a row with no decoder.
    pub fn listed_handlers(&self) -> impl Iterator<Item = &ModeHandler> {
        let skip = usize::from(!self.has_decoder());
        self.handlers.iter().skip(skip)
    }

    /// The mode's handler `id` (`None`: the row's own); `None` when it has none of that
    /// id. (A mode listed without its handlers still has the row's own.)
    pub fn handler(&self, id: Option<&str>) -> Option<ModeHandler> {
        let own = self.own_handler();
        let id = id.unwrap_or(&own);
        match self.handlers.iter().find(|h| h.id == id) {
            Some(h) => Some(h.clone()),
            None => (id == own).then(|| own_handler(self)),
        }
    }
}

/// One handler of a mode: the row's own decoder, or a handler file's entry for the mode.
#[derive(Clone, Debug, PartialEq)]
pub struct ModeHandler {
    /// `builtin` for a graph or frame row's own decoder, a program row's first program
    /// for its own, the file's `id` for a handler file's.
    pub id: String,
    /// For people: `built-in`, the program's name, or the file's `name`.
    pub name: String,
    /// Whether it runs inside the node (a graph or frame row's own decoder, helper
    /// programs or not) rather than as an external program.
    pub in_process: bool,
    /// The stream kinds a job run by it emits: the row's own handler's are the row's
    /// (see [`template::outputs`]), a file entry's are events, and packets with a
    /// `packets` block; empty when broken.
    pub outputs: Vec<StreamKind>,
    pub state: ModeState,
    /// Why it is not ready (`<field>: <error>` when broken, the probe's line when its
    /// program is missing); `None` when ready.
    pub reason: Option<String>,
    /// The program found for it; `None` until probed, and in process.
    pub program: Option<PathBuf>,
    /// The version its program reported to the probe, when its block names a least
    /// version ([`handlers::check_version`]); `None` otherwise, and when it was not read.
    pub version: Option<String>,
    /// The program the probe found, when it found one: what a reload compares to carry
    /// the probe of a block with a least version ([`ModeRegistry::carry_programs`]).
    pub stamp: Option<handlers::ProgramStamp>,
    /// The optional helper programs not found (the row's own handler only).
    pub missing: Vec<String>,
    /// When `zeta modes check` last passed it: for the row's own handler `modes check
    /// <mode>`, for a handler file's `modes check <mode> --handler <id>` (recorded as
    /// `<mode>@<id>`, see [`ModeRegistry::load_checked`]).
    pub checked: Option<u64>,
    /// The file it comes from: the row's for the row's own, the handler file's otherwise.
    pub file: String,
}

impl ModeHandler {
    /// `row`'s own handler as a ready row has it, before any probe: what a graph of the
    /// row alone (no registry) is expanded for. A row with no decoder has the stand-in
    /// ([`ModeHandler::stand_in`]).
    pub fn of_row(row: &ModeRow) -> ModeHandler {
        if row.decoder.is_none() {
            return ModeHandler::stand_in(String::new());
        }
        let id = handler_file::own_handler_id(row);
        let in_process = !matches!(row.decoder, Some(Decoder::Program(_)));
        ModeHandler {
            name: match in_process {
                true => "built-in".into(),
                false => id.clone(),
            },
            id,
            in_process,
            outputs: Vec::new(),
            state: ModeState::Ready,
            reason: None,
            program: None,
            version: None,
            stamp: None,
            missing: Vec::new(),
            checked: None,
            file: String::new(),
        }
    }

    /// The own handler of a row with no decoder of its own (from `file`): [`NO_DECODER`],
    /// not in process, giving nothing, `unavailable` with [`STAND_IN_REASON`]. It keeps
    /// the row's own place first in [`LoadedMode::handlers`], and is never ready.
    pub fn stand_in(file: String) -> ModeHandler {
        ModeHandler {
            id: NO_DECODER.into(),
            name: NO_DECODER.into(),
            in_process: false,
            outputs: Vec::new(),
            state: ModeState::Unavailable,
            reason: Some(STAND_IN_REASON.into()),
            program: None,
            version: None,
            stamp: None,
            missing: Vec::new(),
            checked: None,
            file,
        }
    }
}

/// One handler file as the registry holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedHandlerFile {
    /// `handlers/<name>.json` for an embedded file, the path for an operator file.
    pub file: String,
    /// The file's `id`; for a broken file, the id its raw JSON names, else its name.
    pub id: String,
    /// `ready` when it parsed and its id is a handler id (its entries may still be
    /// broken: see `entries`), else `broken`.
    pub state: ModeState,
    /// Why the file is broken (`<field>: <error>`); `None` when ready.
    pub reason: Option<String>,
    /// The file as parsed; `None` when broken.
    pub handler: Option<HandlerFile>,
    /// Each mode entry of a ready file: `None` when it loaded, else why not (`no mode of
    /// that id`, the row's own handler's id, or `<field>: <error>`).
    pub entries: BTreeMap<String, Option<String>>,
}

impl LoadedHandlerFile {
    /// The entries that did not load, `<mode>: <reason>` each, by mode id.
    pub fn broken_entries(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter_map(|(mode, r)| r.as_ref().map(|r| format!("{mode}: {r}")))
            .collect()
    }
}

/// Deserialize a required `Option<T>`: a missing key is an error, `null` is `None`.
pub(crate) fn nullable<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    <Option<T> as Deserialize>::deserialize(d)
}

/// Schema of a required `Option<T>` for a string enum `T`: its schema with `"null"`
/// added to both `type` and `enum`.
pub(crate) fn nullable_enum<T: JsonSchema>(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let mut s = crate::types::nullable::<T>(g);
    if let Some(serde_json::Value::Array(values)) = s.get_mut("enum") {
        values.push(serde_json::Value::Null);
    }
    s
}

/// The embedded rows, one file each under `modes/` in the repo.
const EMBEDDED: [(&str, &str); 32] = [
    (
        "modes/acars_decode.json",
        include_str!("../../../../modes/acars_decode.json"),
    ),
    (
        "modes/adsb_decode.json",
        include_str!("../../../../modes/adsb_decode.json"),
    ),
    (
        "modes/ais_decode.json",
        include_str!("../../../../modes/ais_decode.json"),
    ),
    (
        "modes/am_demod.json",
        include_str!("../../../../modes/am_demod.json"),
    ),
    (
        "modes/aprs_decode.json",
        include_str!("../../../../modes/aprs_decode.json"),
    ),
    (
        "modes/ble_decode.json",
        include_str!("../../../../modes/ble_decode.json"),
    ),
    (
        "modes/dfm_decode.json",
        include_str!("../../../../modes/dfm_decode.json"),
    ),
    (
        "modes/dmr_decode.json",
        include_str!("../../../../modes/dmr_decode.json"),
    ),
    (
        "modes/dvbs_ip_decode.json",
        include_str!("../../../../modes/dvbs_ip_decode.json"),
    ),
    (
        "modes/dvbt_decode.json",
        include_str!("../../../../modes/dvbt_decode.json"),
    ),
    (
        "modes/flex_decode.json",
        include_str!("../../../../modes/flex_decode.json"),
    ),
    (
        "modes/fm_demod.json",
        include_str!("../../../../modes/fm_demod.json"),
    ),
    (
        "modes/gsmr_decode.json",
        include_str!("../../../../modes/gsmr_decode.json"),
    ),
    (
        "modes/hfdl_decode.json",
        include_str!("../../../../modes/hfdl_decode.json"),
    ),
    (
        "modes/lorawan_decode.json",
        include_str!("../../../../modes/lorawan_decode.json"),
    ),
    (
        "modes/lte_decode.json",
        include_str!("../../../../modes/lte_decode.json"),
    ),
    (
        "modes/m17_decode.json",
        include_str!("../../../../modes/m17_decode.json"),
    ),
    (
        "modes/m20_decode.json",
        include_str!("../../../../modes/m20_decode.json"),
    ),
    (
        "modes/p25_decode.json",
        include_str!("../../../../modes/p25_decode.json"),
    ),
    (
        "modes/pocsag_decode.json",
        include_str!("../../../../modes/pocsag_decode.json"),
    ),
    (
        "modes/rds_decode.json",
        include_str!("../../../../modes/rds_decode.json"),
    ),
    (
        "modes/rs41_decode.json",
        include_str!("../../../../modes/rs41_decode.json"),
    ),
    (
        "modes/ssb_demod.json",
        include_str!("../../../../modes/ssb_demod.json"),
    ),
    (
        "modes/tetra_decode.json",
        include_str!("../../../../modes/tetra_decode.json"),
    ),
    (
        "modes/thread_decode.json",
        include_str!("../../../../modes/thread_decode.json"),
    ),
    (
        "modes/vdl2_decode.json",
        include_str!("../../../../modes/vdl2_decode.json"),
    ),
    (
        "modes/video_decode.json",
        include_str!("../../../../modes/video_decode.json"),
    ),
    (
        "modes/wfm_demod.json",
        include_str!("../../../../modes/wfm_demod.json"),
    ),
    (
        "modes/wifi_decode.json",
        include_str!("../../../../modes/wifi_decode.json"),
    ),
    (
        "modes/wisun_decode.json",
        include_str!("../../../../modes/wisun_decode.json"),
    ),
    (
        "modes/zigbee_decode.json",
        include_str!("../../../../modes/zigbee_decode.json"),
    ),
    (
        "modes/zwave_decode.json",
        include_str!("../../../../modes/zwave_decode.json"),
    ),
];

/// The embedded handler files, one each under `handlers/` in the repo.
const EMBEDDED_HANDLERS: [(&str, &str); 1] = [(
    "handlers/onair.json",
    include_str!("../../../../handlers/onair.json"),
)];

/// The file in the state directory recording when `zeta modes check` last passed each
/// program row (see [`ModeRegistry::load_checked`]).
pub const MODES_CHECKED_FILE: &str = "modes-checked.json";

/// `$XDG_CONFIG_HOME/zetasdr/modes`, else `$HOME/.config/zetasdr/modes`; `None` when
/// neither variable is set.
pub fn default_operator_dir() -> Option<PathBuf> {
    let var = |k| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    var("XDG_CONFIG_HOME")
        .map(|x| x.join("zetasdr/modes"))
        .or_else(|| var("HOME").map(|h| h.join(".config/zetasdr/modes")))
}

/// `handlers` beside [`default_operator_dir`] (`$XDG_CONFIG_HOME/zetasdr/handlers`, else
/// `$HOME/.config/zetasdr/handlers`); `None` when neither variable is set.
pub fn default_handlers_dir() -> Option<PathBuf> {
    default_operator_dir().and_then(|d| d.parent().map(|p| p.join("handlers")))
}

/// The `*.json` files of `dir`, sorted; a missing or unreadable directory reads as empty.
fn json_files(dir: Option<&Path>) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = dir
        .and_then(|d| std::fs::read_dir(d).ok())
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json") && p.is_file())
        .collect();
    files.sort();
    files
}

/// The loaded mode registry, shared by the node and the stages and classifiers that
/// read its rows; empty until it is set, and replaced whole by each later `set` (a
/// reload). A reader takes [`ModesHandle::get`] at each use and keeps that registry
/// for the use, so it sees one consistent set of rows.
#[derive(Clone, Debug, Default)]
pub struct ModesHandle(Arc<RwLock<Option<Arc<ModeRegistry>>>>);

impl ModesHandle {
    /// The registry the handle holds now; `None` until one is set.
    pub fn get(&self) -> Option<Arc<ModeRegistry>> {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Hold `modes` from now on, replacing the registry held before.
    pub fn set(&self, modes: Arc<ModeRegistry>) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = Some(modes);
    }
}

impl From<Arc<ModeRegistry>> for ModesHandle {
    /// A handle holding `modes`.
    fn from(modes: Arc<ModeRegistry>) -> Self {
        let handle = ModesHandle::default();
        handle.set(modes);
        handle
    }
}

/// Every mode the node knows, sorted by id.
#[derive(Clone, Debug, Default)]
pub struct ModeRegistry {
    modes: Vec<LoadedMode>,
    handler_files: Vec<LoadedHandlerFile>,
    weights: weights::Weights,
}

impl ModeRegistry {
    /// The embedded rows: `(file name, contents)`.
    pub fn embedded() -> Vec<(String, &'static str)> {
        EMBEDDED
            .iter()
            .map(|(f, text)| ((*f).to_string(), *text))
            .collect()
    }

    /// [`ModeRegistry::load_with`] with no operator handler files.
    pub fn load(stages: &StageRegistry, operator_dir: Option<&Path>) -> ModeRegistry {
        Self::load_with(stages, operator_dir, None)
    }

    /// Load the embedded rows, then every `*.json` file of `modes_dir` (by file name;
    /// a missing or unreadable directory reads as empty), validating each against
    /// `stages`. A valid operator row replaces the row with its id; a broken one is
    /// listed beside it (see [`LoadedMode::row`]). Then the handler files, the embedded
    /// ones and those of `handlers_dir` (read the same way): a valid operator file
    /// replaces the file of its id, and each file's entries are checked against the
    /// rows they serve. Never fails.
    pub fn load_with(
        stages: &StageRegistry,
        modes_dir: Option<&Path>,
        handlers_dir: Option<&Path>,
    ) -> ModeRegistry {
        let mut reg = Self::load_rows(stages, modes_dir);
        for (file, text) in EMBEDDED_HANDLERS {
            reg.add_handler_file(file.to_string(), text);
        }
        for path in json_files(handlers_dir) {
            let file = path.display().to_string();
            match std::fs::read_to_string(&path) {
                Ok(text) => reg.add_handler_file(file, &text),
                Err(e) => reg.add_broken_handler_file(stem(&path), file, format!("file: {e}")),
            }
        }
        reg.attach_handlers();
        reg
    }

    /// The embedded rows and, in place of any embedded or operator handler files, the
    /// handler files `texts` (`(file, contents)`) in order: a later valid file replaces
    /// an earlier one of its id, as an operator file replaces an embedded one.
    pub fn load_handler_texts(stages: &StageRegistry, texts: &[(&str, &str)]) -> ModeRegistry {
        let mut reg = Self::load_rows(stages, None);
        for (file, text) in texts {
            reg.add_handler_file(file.to_string(), text);
        }
        reg.attach_handlers();
        reg
    }

    /// The rows of [`ModeRegistry::load_with`], before any handler file.
    fn load_rows(stages: &StageRegistry, operator_dir: Option<&Path>) -> ModeRegistry {
        let mut reg = ModeRegistry::default();
        for (file, text) in EMBEDDED {
            reg.add(file.to_string(), text, stages);
        }
        for path in json_files(operator_dir) {
            if path.file_name().is_some_and(|n| n == weights::WEIGHTS_FILE) {
                reg.load_weights(&path);
                continue;
            }
            let file = path.display().to_string();
            match std::fs::read_to_string(&path) {
                Ok(text) => reg.add(file, &text, stages),
                Err(e) => {
                    let e = RowError {
                        field: "file".into(),
                        error: e.to_string(),
                    };
                    reg.add_broken(&stem(&path), None, file, e);
                }
            }
        }
        reg.modes.sort_by(|a, b| a.row.id.cmp(&b.row.id));
        reg
    }

    /// Read the operator's weights file over the embedded table; a broken one is listed
    /// like a broken handler file (id `weights`) and the embedded weights stay.
    fn load_weights(&mut self, path: &Path) {
        let file = path.display().to_string();
        match std::fs::read_to_string(path)
            .map_err(|e| format!("file: {e}"))
            .and_then(|t| weights::Weights::parse(&t))
        {
            Ok(w) => self.weights = w,
            Err(reason) => self.add_broken_handler_file("weights".into(), file, reason),
        }
    }

    /// The ranking weights: the embedded table, or the operator's over it.
    pub fn weights(&self) -> &weights::Weights {
        &self.weights
    }

    /// Parse, validate and add one row file: a ready row replaces the row with its id.
    fn add(&mut self, file: String, text: &str, stages: &StageRegistry) {
        let row = match parse(text) {
            Ok(row) => row,
            Err(e) => {
                // The id the file meant, from its raw JSON, else its name.
                let id = serde_json::from_str::<serde_json::Value>(text)
                    .ok()
                    .and_then(|v| v["id"].as_str().map(String::from))
                    .unwrap_or_else(|| stem(Path::new(&file)));
                return self.add_broken(&id, None, file, e);
            }
        };
        if let Err(e) = validate(&row, stages) {
            let id = row.id.0.clone();
            return self.add_broken(&id, Some(row), file, e);
        }
        let outputs = template::outputs(&row, stages);
        let voices = template::voices(&row, stages);
        // A row with no decoder of its own is never ready itself: a handler is.
        let (state, reason) = match row.decoder {
            Some(_) => (ModeState::Ready, None),
            None => (ModeState::Unavailable, Some(NO_DECODER_REASON.to_string())),
        };
        let loaded = LoadedMode {
            row,
            state,
            reason,
            file,
            program: None,
            checked: None,
            missing: Vec::new(),
            outputs,
            voices,
            handlers: Vec::new(),
        };
        match self.modes.iter_mut().find(|m| m.row.id == loaded.row.id) {
            Some(m) => *m = loaded,
            None => self.modes.push(loaded),
        }
    }

    /// List a broken file under `id`, or under `<id>@<file name>` when a row already
    /// has that id (which stays in force).
    fn add_broken(&mut self, id: &str, row: Option<ModeRow>, file: String, e: RowError) {
        let id = match self.modes.iter().any(|m| m.row.id.0 == id) {
            true => {
                let name = Path::new(&file)
                    .file_name()
                    .map_or_else(|| file.clone(), |n| n.to_string_lossy().into_owned());
                format!("{id}@{name}")
            }
            false => id.to_string(),
        };
        let mut row = row.unwrap_or_else(|| placeholder(&id));
        row.id = ModeId(id);
        self.modes.push(LoadedMode {
            row,
            state: ModeState::Broken,
            reason: Some(e.to_string()),
            file,
            program: None,
            checked: None,
            missing: Vec::new(),
            outputs: Vec::new(),
            voices: Vec::new(),
            handlers: Vec::new(),
        });
    }

    /// Parse one handler file: a valid one replaces the valid file of its id, its
    /// entries that did not parse recorded in its `entries`; a broken one is listed
    /// beside it (and leaves that one in force).
    fn add_handler_file(&mut self, file: String, text: &str) {
        let (h, unparsed) = match handler_file::parse_handler_file(text) {
            Ok(parsed) => parsed,
            Err(e) => {
                let id = serde_json::from_str::<serde_json::Value>(text)
                    .ok()
                    .and_then(|v| v["id"].as_str().map(String::from))
                    .unwrap_or_else(|| stem(Path::new(&file)));
                return self.add_broken_handler_file(id, file, e.to_string());
            }
        };
        let loaded = LoadedHandlerFile {
            file,
            id: h.id.clone(),
            state: ModeState::Ready,
            reason: None,
            handler: Some(h),
            entries: unparsed
                .into_iter()
                .map(|(mode, e)| (mode, Some(e.to_string())))
                .collect(),
        };
        let same = |f: &&mut LoadedHandlerFile| f.id == loaded.id && f.state == ModeState::Ready;
        match self.handler_files.iter_mut().find(same) {
            Some(f) => *f = loaded,
            None => self.handler_files.push(loaded),
        }
    }

    fn add_broken_handler_file(&mut self, id: String, file: String, reason: String) {
        self.handler_files.push(LoadedHandlerFile {
            file,
            id,
            state: ModeState::Broken,
            reason: Some(reason),
            handler: None,
            entries: BTreeMap::new(),
        });
    }

    /// Give every mode its handlers: the row's own, then each ready handler file's entry
    /// for it (by file id), recording in each file why an entry did not load. An entry
    /// that fails validation is listed `broken` among the mode's handlers; one naming no
    /// mode, a broken row, or the row's own handler's id is not listed. The entries that
    /// did not parse keep the reasons [`ModeRegistry::add_handler_file`] recorded.
    fn attach_handlers(&mut self) {
        for m in &mut self.modes {
            m.handlers = vec![own_handler(m)];
        }
        for f in &mut self.handler_files {
            let Some(h) = &f.handler else {
                continue;
            };
            f.entries.retain(|mode, _| !h.modes.contains_key(mode));
            for (mode, entry) in &h.modes {
                let reason = match self.modes.iter_mut().find(|m| m.row.id.0 == *mode) {
                    None => Some("no mode of that id".to_string()),
                    Some(m) if m.state == ModeState::Broken => {
                        Some("the mode's row is broken".to_string())
                    }
                    Some(m) if h.id == m.handlers[0].id => {
                        Some(format!("id {} is the row's own handler", h.id))
                    }
                    Some(m) => {
                        let reason = handler_file::validate_entry(h, entry, &m.row)
                            .err()
                            .map(|e| e.to_string());
                        let (state, outputs) = match (&reason, &entry.block.packets) {
                            (Some(_), _) => (ModeState::Broken, vec![]),
                            (None, Some(_)) => (
                                ModeState::Ready,
                                vec![StreamKind::Event, StreamKind::Packet],
                            ),
                            (None, None) => (ModeState::Ready, vec![StreamKind::Event]),
                        };
                        m.handlers.push(ModeHandler {
                            id: h.id.clone(),
                            name: h.name.clone(),
                            in_process: false,
                            outputs,
                            state,
                            reason: reason.clone(),
                            program: None,
                            version: None,
                            stamp: None,
                            missing: vec![],
                            checked: None,
                            file: f.file.clone(),
                        });
                        reason
                    }
                };
                f.entries.insert(mode.clone(), reason);
            }
        }
        for m in &mut self.modes {
            m.handlers[1..].sort_by(|a, b| a.id.cmp(&b.id));
            no_decoder_reason(m);
        }
    }

    /// Every handler file, embedded then operator, in the order read.
    pub fn handler_files(&self) -> &[LoadedHandlerFile] {
        &self.handler_files
    }

    /// Handler `handler` of mode `mode`.
    pub fn handler(&self, mode: &str, handler: &str) -> Option<&ModeHandler> {
        self.get(mode)?.handlers.iter().find(|h| h.id == handler)
    }

    /// The program block that runs `handler` for `mode`: the row's own (a program
    /// row's decoder), or a file's entry ([`HandlerEntry::program_block`]). `None` for an
    /// in-process handler, a broken one, and one the mode does not have.
    pub fn program_block(&self, mode: &str, handler: &str) -> Option<ProgramBlock> {
        let m = self.get(mode)?;
        let i = m.handlers.iter().position(|h| h.id == handler)?;
        if m.handlers[i].state == ModeState::Broken {
            return None;
        }
        if i == 0 {
            return match &m.row.decoder {
                Some(Decoder::Program(p)) => Some(p.clone()),
                _ => None,
            };
        }
        let h = self
            .handler_files
            .iter()
            .filter(|f| f.id == handler && f.state == ModeState::Ready)
            .find_map(|f| f.handler.as_ref())?;
        Some(h.modes.get(mode)?.program_block(h))
    }

    /// The event fields `handler` adds for `mode` beside the row's: a handler file entry's
    /// `extra`, each read from a program's line as `output.fields` reads; empty for the
    /// row's own handler, a broken one, and one the mode does not have.
    pub fn handler_extra(&self, mode: &str, handler: &str) -> BTreeMap<String, FieldPath> {
        self.handler_entry(mode, handler)
            .map(|e| e.extra.clone())
            .unwrap_or_default()
    }

    /// The event fields `handler` sets to a constant for `mode`, after the field map: a
    /// handler file entry's `const`; empty under [`ModeRegistry::handler_extra`]'s
    /// conditions.
    pub fn handler_consts(&self, mode: &str, handler: &str) -> BTreeMap<String, serde_json::Value> {
        self.handler_entry(mode, handler)
            .map(|e| e.consts.clone())
            .unwrap_or_default()
    }

    /// The handler file entry that serves `mode` as `handler`: `None` for the row's own
    /// handler, a broken one, and one the mode does not have.
    fn handler_entry(&self, mode: &str, handler: &str) -> Option<&HandlerEntry> {
        let files = self.handler_files.iter().filter(|f| f.id == handler);
        let own = self.get(mode).is_some_and(|m| m.own_handler() == handler);
        let entry = files
            .filter(|f| f.state == ModeState::Ready && !own)
            .find_map(|f| f.handler.as_ref()?.modes.get(mode));
        match (entry, self.handler(mode, handler)) {
            (Some(e), Some(h)) if h.state != ModeState::Broken => Some(e),
            _ => None,
        }
    }

    /// Record what probing found for `mode`'s handler `handler`: its program, and when
    /// it is missing `reason`, which makes the handler unavailable (else it is ready).
    /// The row's own handler is [`ModeRegistry::set_program`]'s. A broken handler stays
    /// broken.
    pub fn set_handler_program(
        &mut self,
        mode: &str,
        handler: &str,
        program: Option<PathBuf>,
        reason: Option<String>,
    ) {
        let Some(m) = self.modes.iter_mut().find(|m| m.row.id.0 == mode) else {
            return;
        };
        match m.handlers.iter().position(|h| h.id == handler) {
            Some(0) => self.set_program(mode, program, reason),
            Some(i) => {
                let h = &mut m.handlers[i];
                if h.state == ModeState::Broken {
                    return;
                }
                h.state = match reason {
                    None => ModeState::Ready,
                    Some(_) => ModeState::Unavailable,
                };
                h.program = program;
                h.reason = reason;
                no_decoder_reason(m);
            }
            None => {}
        }
    }

    /// The row with id `id`.
    /// The demodulator kind `m` runs when it is a demodulator row: a built-in's own row,
    /// or a variant whose decoder is a `builtin` of a demodulator kind.
    fn demod_kind(m: &LoadedMode) -> Option<&str> {
        match &m.row.decoder {
            Some(Decoder::Builtin(b)) => DEMOD_KINDS.iter().copied().find(|k| *k == b.builtin),
            _ => None,
        }
    }

    /// The rows that ride with `demod` (a demodulator row, a variant included): every
    /// row whose `rides.with` names the built-in `demod` runs, ready or not.
    pub fn riders_of(&self, demod: &LoadedMode) -> Vec<&LoadedMode> {
        let Some(kind) = Self::demod_kind(demod) else {
            return Vec::new();
        };
        self.modes
            .iter()
            .filter(|m| {
                m.row
                    .rides
                    .as_ref()
                    .is_some_and(|r| r.with.iter().any(|w| w == kind))
            })
            .collect()
    }

    /// The demodulator rows `rider` rides with: every row (the built-in's and each
    /// variant's) whose built-in is in `rider`'s `rides.with`, ready or not. Empty for
    /// a row that does not ride.
    pub fn ridden_by(&self, rider: &LoadedMode) -> Vec<&LoadedMode> {
        let Some(r) = &rider.row.rides else {
            return Vec::new();
        };
        self.modes
            .iter()
            .filter(|m| Self::demod_kind(m).is_some_and(|k| r.with.iter().any(|w| w == k)))
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<&LoadedMode> {
        self.modes.iter().find(|m| m.row.id.0 == id)
    }

    /// The ready rows.
    pub fn ready(&self) -> impl Iterator<Item = &LoadedMode> {
        self.modes.iter().filter(|m| m.state == ModeState::Ready)
    }

    /// Every row, sorted by id.
    pub fn all(&self) -> &[LoadedMode] {
        &self.modes
    }

    /// Every row's id, sorted.
    pub fn ids(&self) -> Vec<String> {
        self.modes.iter().map(|m| m.row.id.0.clone()).collect()
    }

    /// Record what probing found for a row with handlers: its program (a program row's
    /// program, a graph row's first handler, `None` when that one is missing) and, when
    /// a required handler is missing, `reason`, which makes the row unavailable (else it
    /// is ready). It starts the row's probe over: `missing` is cleared, for
    /// [`ModeRegistry::set_missing`] to fill. A broken row stays broken, and a row with
    /// no decoder of its own has no program of its own: it is left as it is.
    pub fn set_program(&mut self, id: &str, program: Option<PathBuf>, reason: Option<String>) {
        let Some(m) = self.modes.iter_mut().find(|m| m.row.id.0 == id) else {
            return;
        };
        if m.state == ModeState::Broken || !m.has_decoder() {
            return;
        }
        m.state = match reason {
            None => ModeState::Ready,
            Some(_) => ModeState::Unavailable,
        };
        m.program = program;
        m.reason = reason;
        m.missing.clear();
        sync_own(m);
    }

    /// Add `line` to a row's `missing` (an optional handler probing did not find), after
    /// its [`ModeRegistry::set_program`]. The row's state is left as it is; a broken
    /// row's list stays empty.
    pub fn set_missing(&mut self, id: &str, line: String) {
        if let Some(m) = self
            .modes
            .iter_mut()
            .find(|m| m.row.id.0 == id && m.state != ModeState::Broken)
        {
            m.missing.push(line);
            sync_own(m);
        }
    }

    /// Record the version `handler` of `mode` reported to the probe (`None` when its
    /// block names none, or it was not read) and the program's `stamp` (`None` when
    /// missing), after [`ModeRegistry::set_handler_program`] (or
    /// [`ModeRegistry::set_program`] for the row's own handler). A broken handler is left
    /// as it is.
    pub fn set_version_read(
        &mut self,
        mode: &str,
        handler: &str,
        version: Option<String>,
        stamp: Option<handlers::ProgramStamp>,
    ) {
        let Some(m) = self.modes.iter_mut().find(|m| m.row.id.0 == mode) else {
            return;
        };
        if let Some(h) = m
            .handlers
            .iter_mut()
            .find(|h| h.id == handler && h.state != ModeState::Broken)
        {
            h.version = version;
            h.stamp = stamp;
        }
    }

    /// Carry what probing found from `old` (the registry this one replaces) to each
    /// row with handlers (a program row, a graph row naming `handlers`) of the same id:
    /// its `program`, `state`, `reason` and `missing`, and its own handler's `version`
    /// and `stamp`, so a reload keeps the probe's answer without probing again. Only where
    /// both rows have the same decoder block (an edited block's answer may no longer
    /// hold: such a row stays as loaded, its programs looked up at launch) and neither is
    /// broken. Likewise each handler file's handler of a mode: its `program`, `state`,
    /// `reason`, `version` and `stamp`, where `old` has that handler for the mode with the
    /// same program block and neither is broken. A block naming a least version
    /// (`version`) is carried only while the program found now is the one probed, by
    /// path, size and modified time ([`handlers::ProgramStamp`]): another is probed
    /// again.
    pub fn carry_programs(&mut self, old: &ModeRegistry) {
        // The stamp found now of each program list with a version floor, looked up once.
        let mut found: Vec<(Vec<String>, Option<handlers::ProgramStamp>)> = Vec::new();
        let mut same_program = |programs: &[String], was: &Option<handlers::ProgramStamp>| {
            let now = match found.iter().find(|(p, _)| p == programs) {
                Some((_, s)) => s.clone(),
                None => {
                    let s = handlers::ProgramStamp::found(programs);
                    found.push((programs.to_vec(), s.clone()));
                    s
                }
            };
            now == *was
        };
        for m in &mut self.modes {
            if m.state == ModeState::Broken || m.row.handler_blocks().is_empty() {
                continue;
            }
            let Some(o) = old.get(&m.row.id.0) else {
                continue;
            };
            if o.state == ModeState::Broken || o.row.decoder != m.row.decoder {
                continue;
            }
            let was = o.handlers.first().and_then(|h| h.stamp.clone());
            if let Some(Decoder::Program(p)) = &m.row.decoder
                && p.version.is_some()
                && !same_program(&p.programs, &was)
            {
                continue;
            }
            m.program = o.program.clone();
            m.state = o.state;
            m.reason = o.reason.clone();
            m.missing = o.missing.clone();
            sync_own(m);
            if let (Some(h), Some(oh)) = (m.handlers.first_mut(), o.handlers.first()) {
                h.version = oh.version.clone();
                h.stamp = oh.stamp.clone();
            }
        }
        // `program_block` is `None` for a broken handler, old or new.
        let mut carried = Vec::new();
        for m in &self.modes {
            let mode = &m.row.id.0;
            for h in m.handlers.iter().skip(1) {
                let block = self.program_block(mode, &h.id);
                if block.is_none() || block != old.program_block(mode, &h.id) {
                    continue;
                }
                let Some(o) = old.handler(mode, &h.id) else {
                    continue;
                };
                if let Some(b) = &block
                    && b.version.is_some()
                    && !same_program(&b.programs, &o.stamp)
                {
                    continue;
                }
                carried.push((mode.clone(), o.clone()));
            }
        }
        for (mode, o) in carried {
            let Some(m) = self.modes.iter_mut().find(|m| m.row.id.0 == mode) else {
                continue;
            };
            if let Some(h) = m.handlers.iter_mut().skip(1).find(|h| h.id == o.id) {
                h.program = o.program;
                h.state = o.state;
                h.reason = o.reason;
                h.version = o.version;
                h.stamp = o.stamp;
            }
            no_decoder_reason(m);
        }
    }

    /// Read `path` (`modes-checked.json` in the state directory: an object of UNIX
    /// seconds by mode id, and by `<mode id>@<handler id>` for a handler file's handler of
    /// the mode) into each row's `checked` (its own handler's) and each other handler's. A
    /// missing, unreadable or malformed file, and keys nothing has, are ignored: `checked`
    /// stays `None`.
    pub fn load_checked(&mut self, path: &Path) {
        let checked = read_checked(path);
        for m in &mut self.modes {
            m.checked = checked.get(&m.row.id.0).copied();
            sync_own(m);
            let mode = &m.row.id.0;
            for h in m.handlers.iter_mut().skip(1) {
                h.checked = checked.get(&format!("{mode}@{}", h.id)).copied();
            }
        }
    }

    /// Record in `path` (see [`ModeRegistry::load_checked`]) that the check of `id` (a mode
    /// id, or `<mode id>@<handler id>`) passed at `at` (UNIX seconds), keeping the other
    /// entries.
    pub fn record_checked(path: &Path, id: &str, at: u64) -> std::io::Result<()> {
        let mut checked = read_checked(path);
        checked.insert(id.to_string(), at);
        let json = serde_json::to_vec_pretty(&checked).map_err(std::io::Error::other)?;
        // Write beside it, then rename: a reader sees the old file or the new one whole.
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }

    /// One stderr line per broken row, `zeta: mode <id> (<file>): <field>: <error>`,
    /// then one per broken handler file, `zeta: handler <id> (<file>): <field>: <error>`,
    /// and one per entry of a handler file that did not load, `zeta: handler <id>
    /// (<file>): <mode>: <reason>`.
    pub fn startup_lines(&self) -> Vec<String> {
        let rows = self
            .modes
            .iter()
            .filter(|m| m.state == ModeState::Broken)
            .map(|m| {
                let id = m.row.id.0.split('@').next().unwrap_or_default();
                let reason = m.reason.as_deref().unwrap_or_default();
                format!("zeta: mode {id} ({}): {reason}", m.file)
            });
        let files = self.handler_files.iter().flat_map(|f| {
            let reasons = match &f.reason {
                Some(r) => vec![r.clone()],
                None => f.broken_entries(),
            };
            reasons
                .into_iter()
                .map(move |r| format!("zeta: handler {} ({}): {r}", f.id, f.file))
        });
        rows.chain(files).collect()
    }
}

/// The row's own handler of `m`, from the row as loaded: the stand-in for a row with no
/// decoder of its own.
fn own_handler(m: &LoadedMode) -> ModeHandler {
    if !m.has_decoder() {
        return ModeHandler::stand_in(m.file.clone());
    }
    let id = handler_file::own_handler_id(&m.row);
    let in_process = !matches!(m.row.decoder, Some(Decoder::Program(_)));
    ModeHandler {
        name: match in_process {
            true => "built-in".into(),
            false => id.clone(),
        },
        id,
        in_process,
        outputs: m.outputs.clone(),
        state: m.state,
        reason: m.reason.clone(),
        program: m.program.clone(),
        version: None,
        stamp: None,
        missing: m.missing.clone(),
        checked: m.checked,
        file: m.file.clone(),
    }
}

/// The reason of `m`, a row with no decoder of its own that is not broken, from its
/// handlers as they stand: [`NO_DECODER_REASON`], then `; <id>: <reason>` for each
/// handler file's handler in order (its state's word when it has no reason). Called each
/// time a handler's answer changes, so the probe's missing program is in it; any other
/// row is left as it is.
fn no_decoder_reason(m: &mut LoadedMode) {
    if m.has_decoder() || m.state == ModeState::Broken {
        return;
    }
    let mut reason = NO_DECODER_REASON.to_string();
    for h in m.listed_handlers() {
        let why = h.reason.as_deref().unwrap_or(match h.state {
            ModeState::Ready => "ready",
            ModeState::Broken => "broken",
            ModeState::Unavailable => "unavailable",
        });
        reason.push_str(&format!("; {}: {why}", h.id));
    }
    m.reason = Some(reason);
}

/// Copy the row's probe answer and checked time into its own handler, `handlers[0]`;
/// the stand-in of a row with no decoder keeps its own.
fn sync_own(m: &mut LoadedMode) {
    if !m.has_decoder() {
        return;
    }
    let (state, reason, program) = (m.state, m.reason.clone(), m.program.clone());
    let (missing, checked) = (m.missing.clone(), m.checked);
    if let Some(h) = m.handlers.first_mut() {
        h.state = state;
        h.reason = reason;
        h.program = program;
        h.missing = missing;
        h.checked = checked;
    }
}

/// The UNIX seconds by mode id in `path`; empty when it cannot be read or parsed.
fn read_checked(path: &Path) -> std::collections::BTreeMap<String, u64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// A file's name without its extension.
fn stem(path: &Path) -> String {
    path.file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned())
}

/// The row listed for a broken file nothing of which parsed: its id, the rest empty.
fn placeholder(id: &str) -> ModeRow {
    ModeRow {
        id: ModeId(id.into()),
        name: id.into(),
        family: Family::General,
        band: Band {
            bandwidth_hz: 0,
            centers_hz: vec![],
            min_hz: None,
            range_hz: None,
            width_rule: None,
        },
        modulation: vec![],
        label: String::new(),
        protocol: String::new(),
        decoder: Some(Decoder::Graph {
            nodes: vec![],
            edges: vec![],
            handlers: vec![],
        }),
        event: Event {
            schema: serde_json::json!({}),
            summary: String::new(),
        },
        identify: Identify {
            proof: Proof::Event,
            field: None,
            min_events: 0,
            dwell_ms: 0,
            min_span_ms: 0,
            quick: None,
            priority: 0,
            as_asked: false,
            when_named: false,
        },
        catalogue: vec![],
        test: None,
        notes: None,
        position: None,
        time: None,
        subject: None,
        derive: vec![],
        access: None,
        content: None,
        rides: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_times_round_trip_through_the_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("modes-checked.json");
        let stages = crate::stage::StageRegistry::new();
        let mut modes = ModeRegistry::load(&stages, None);
        modes.load_checked(&path);
        assert!(modes.all().iter().all(|m| m.checked.is_none()), "no file");
        ModeRegistry::record_checked(&path, "adsb_decode", 100).unwrap();
        ModeRegistry::record_checked(&path, "nope", 7).unwrap();
        ModeRegistry::record_checked(&path, "adsb_decode", 200).unwrap();
        modes.load_checked(&path);
        let seen: Vec<_> = modes
            .all()
            .iter()
            .filter_map(|m| m.checked.map(|t| (m.row.id.0.clone(), t)))
            .collect();
        assert_eq!(seen, [("adsb_decode".to_string(), 200)]);
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["modes-checked.json"], "no temp file left behind");
        std::fs::write(&path, "garbage").unwrap();
        modes.load_checked(&path);
        assert!(modes.all().iter().all(|m| m.checked.is_none()), "bad file");
    }

    /// An optional handler not found is listed in `missing` and leaves the row ready;
    /// a required one missing still makes it unavailable. (The program row validates
    /// with no stages, so it stands in for a graph row with handlers here.)
    #[test]
    fn a_missing_optional_handler_leaves_the_row_ready() {
        let stages = crate::stage::StageRegistry::new();
        let mut modes = ModeRegistry::load(&stages, None);
        let id = "adsb_decode";
        modes.set_program(id, Some("/stand-in".into()), None);
        let line = "zeta-mbe: missing (bash tools/x.sh; https://example.invalid) — voice silent";
        modes.set_missing(id, line.into());
        let m = modes.get(id).unwrap();
        assert_eq!(m.state, ModeState::Ready);
        assert_eq!(m.reason, None);
        assert_eq!(m.missing, [line]);
        modes.set_program(id, None, Some("dump1090: missing".into()));
        assert_eq!(modes.get(id).unwrap().state, ModeState::Unavailable);
        // A re-probe starts from an empty list.
        assert!(modes.get(id).unwrap().missing.is_empty());
    }

    /// A handler file's handler takes its probe answer by `set_handler_program`, the row's
    /// own handler follows the row's, and a reload over the same blocks carries both.
    #[test]
    fn a_handler_files_probe_answer_is_kept_across_a_reload() {
        let stages = crate::stage::StageRegistry::new();
        let file = serde_json::json!({
            "id": "alt", "name": "Alt", "programs": ["alt-rx"],
            "install": {"url": "https://example.invalid"},
            "modes": {"adsb_decode": {
                "args": [],
                "input": {"stream": "iq", "format": "cs16", "rate_hz": 2400000, "via": "stdin"},
                "output": {"via": "stdout", "parser": "json_lines"}
            }}
        })
        .to_string();
        let load = |text: &str| ModeRegistry::load_handler_texts(&stages, &[("alt.json", text)]);
        let mut modes = load(&file);
        let (mode, alt) = ("adsb_decode", "alt");
        modes.set_program(mode, None, Some("dump1090: missing".into()));
        modes.set_handler_program(mode, alt, Some("/opt/alt-rx".into()), None);
        let own = modes.handler(mode, "dump1090").unwrap();
        assert_eq!(own.state, ModeState::Unavailable);
        assert_eq!(own.reason.as_deref(), Some("dump1090: missing"));
        let h = modes.handler(mode, alt).unwrap();
        assert_eq!(h.state, ModeState::Ready);
        assert_eq!(h.program, Some("/opt/alt-rx".into()));

        let mut again = load(&file);
        assert_eq!(again.handler(mode, alt).unwrap().program, None);
        again.carry_programs(&modes);
        assert_eq!(again.handler(mode, alt), modes.handler(mode, alt));
        assert_eq!(
            again.handler(mode, "dump1090"),
            modes.handler(mode, "dump1090")
        );

        // An edited entry's answer may no longer hold: it is not carried.
        let edited = file.replace("\"args\":[]", "\"args\":[\"-v\"]");
        assert_ne!(edited, file);
        let mut edited = load(&edited);
        edited.carry_programs(&modes);
        assert_eq!(edited.handler(mode, alt).unwrap().program, None);

        // A missing program makes the handler unavailable with the probe's line.
        modes.set_handler_program(mode, alt, None, Some("alt-rx: missing".into()));
        let h = modes.handler(mode, alt).unwrap();
        assert_eq!(h.state, ModeState::Unavailable);
        assert_eq!(h.reason.as_deref(), Some("alt-rx: missing"));
    }
}
