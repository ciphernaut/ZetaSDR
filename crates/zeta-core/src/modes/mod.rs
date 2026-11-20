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
mod row;
pub mod score;
pub mod summary;
pub mod template;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::stage::StageRegistry;
use crate::types::StreamKind;

pub use row::{
    Access, AltUnit, Band, Container, Decoder, DecoderKind, Demod, Derive, ENVELOPE_FIELDS, Event,
    Extractor, FRAME_PARSERS, Family, FieldPath, FrameBlock, FrameDemod, Handler, HandlerBlock,
    IDENTITY_REGISTERS, IDENTITY_STRIPS, Identify, Identity, InputStream, InputVia, Install,
    ModeId, ModeRow, OutputVia, PacketParserKind, Parser, PositionMap, ProgramBlock, ProgramInput,
    ProgramOutput, ProgramPackets, Proof, QUICK_ATTEMPTS, Ranges, RowError, RowTest, SampleFormat,
    Station, Subject, SubjectColumn, SubjectUnit, SubjectWaterfall, TestSpec, center_khz_text,
    is_mode_id, parse, validate,
};
pub(crate) use row::{FRAME_DECODE_STAGE, require_all};

pub use handler_file::{BUILTIN, HandlerBlockSpec, HandlerEntry, HandlerFile};

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
    /// missing and checked are the row's above), then each handler file's entry for the
    /// mode, by file id. Not in what the node reports yet.
    #[serde(skip)]
    #[schemars(skip)]
    pub handlers: Vec<ModeHandler>,
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
    /// The optional helper programs not found (the row's own handler only).
    pub missing: Vec<String>,
    /// When `zeta modes check` last passed it (the row's own handler only).
    pub checked: Option<u64>,
    /// The file it comes from: the row's for the row's own, the handler file's otherwise.
    pub file: String,
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
const EMBEDDED: [(&str, &str); 17] = [
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
        "modes/aprs_decode.json",
        include_str!("../../../../modes/aprs_decode.json"),
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
        "modes/dvbt_decode.json",
        include_str!("../../../../modes/dvbt_decode.json"),
    ),
    (
        "modes/flex_decode.json",
        include_str!("../../../../modes/flex_decode.json"),
    ),
    (
        "modes/hfdl_decode.json",
        include_str!("../../../../modes/hfdl_decode.json"),
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
        "modes/vdl2_decode.json",
        include_str!("../../../../modes/vdl2_decode.json"),
    ),
    (
        "modes/video_decode.json",
        include_str!("../../../../modes/video_decode.json"),
    ),
];

/// The embedded handler files, one each under `handlers/` in the repo.
const EMBEDDED_HANDLERS: [(&str, &str); 0] = [];

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
        let loaded = LoadedMode {
            row,
            state: ModeState::Ready,
            reason: None,
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

    /// Parse one handler file: a valid one replaces the valid file of its id, a broken
    /// one is listed beside it (and leaves that one in force).
    fn add_handler_file(&mut self, file: String, text: &str) {
        let h = match handler_file::parse_handler_file(text) {
            Ok(h) => h,
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
            entries: BTreeMap::new(),
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
    /// mode, a broken row, or the row's own handler's id is not listed.
    fn attach_handlers(&mut self) {
        for m in &mut self.modes {
            m.handlers = vec![own_handler(m)];
        }
        for f in &mut self.handler_files {
            f.entries.clear();
            let Some(h) = &f.handler else {
                continue;
            };
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
                Decoder::Program(p) => Some(p.clone()),
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
            }
            None => {}
        }
    }

    /// The row with id `id`.
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
    /// [`ModeRegistry::set_missing`] to fill. A broken row stays broken.
    pub fn set_program(&mut self, id: &str, program: Option<PathBuf>, reason: Option<String>) {
        let Some(m) = self.modes.iter_mut().find(|m| m.row.id.0 == id) else {
            return;
        };
        if m.state == ModeState::Broken {
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

    /// Carry what probing found from `old` (the registry this one replaces) to each
    /// row with handlers (a program row, a graph row naming `handlers`) of the same id:
    /// its `program`, `state`, `reason` and `missing`, so a reload keeps the probe's answer
    /// without probing again. Only where both rows have the same decoder block (an
    /// edited block's answer may no longer hold: such a row stays as loaded, its
    /// programs looked up at launch) and neither is broken. Likewise each handler file's
    /// handler of a mode: its `program`, `state` and `reason`, where `old` has that
    /// handler for the mode with the same program block and neither is broken.
    pub fn carry_programs(&mut self, old: &ModeRegistry) {
        for m in &mut self.modes {
            if m.state == ModeState::Broken || m.row.decoder.handler_blocks().is_empty() {
                continue;
            }
            let Some(o) = old.get(&m.row.id.0) else {
                continue;
            };
            if o.state == ModeState::Broken || o.row.decoder != m.row.decoder {
                continue;
            }
            m.program = o.program.clone();
            m.state = o.state;
            m.reason = o.reason.clone();
            m.missing = o.missing.clone();
            sync_own(m);
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
                if let Some(o) = old.handler(mode, &h.id) {
                    carried.push((mode.clone(), o.clone()));
                }
            }
        }
        for (mode, o) in carried {
            let h = self
                .modes
                .iter_mut()
                .find(|m| m.row.id.0 == mode)
                .and_then(|m| m.handlers.iter_mut().skip(1).find(|h| h.id == o.id));
            if let Some(h) = h {
                h.program = o.program;
                h.state = o.state;
                h.reason = o.reason;
            }
        }
    }

    /// Read `path` (`modes-checked.json` in the state directory: an object of UNIX
    /// seconds by mode id) into each row's `checked`. A missing, unreadable or
    /// malformed file, and ids no row has, are ignored: `checked` stays `None`.
    pub fn load_checked(&mut self, path: &Path) {
        let checked = read_checked(path);
        for m in &mut self.modes {
            m.checked = checked.get(&m.row.id.0).copied();
            sync_own(m);
        }
    }

    /// Record in `path` (see [`ModeRegistry::load_checked`]) that `id`'s check passed at
    /// `at` (UNIX seconds), keeping the other entries.
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

/// The row's own handler of `m`, from the row as loaded.
fn own_handler(m: &LoadedMode) -> ModeHandler {
    let id = handler_file::own_handler_id(&m.row);
    let in_process = !matches!(m.row.decoder, Decoder::Program(_));
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
        missing: m.missing.clone(),
        checked: m.checked,
        file: m.file.clone(),
    }
}

/// Copy the row's probe answer and checked time into its own handler, `handlers[0]`.
fn sync_own(m: &mut LoadedMode) {
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
        },
        modulation: vec![],
        label: String::new(),
        protocol: String::new(),
        decoder: Decoder::Graph {
            nodes: vec![],
            edges: vec![],
            handlers: vec![],
        },
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
        },
        catalogue: vec![],
        test: None,
        notes: None,
        position: None,
        time: None,
        subject: None,
        derive: vec![],
        access: None,
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
