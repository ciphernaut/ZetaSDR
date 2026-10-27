//! The mode registry: every decoder mode the node knows, one row each.
//!
//! The embedded rows (`modes/*.json` in the repo, compiled in) are read first, then the
//! operator directory (`--modes-dir`, default `$XDG_CONFIG_HOME/zetasdr/modes`, else
//! `~/.config/zetasdr/modes`); an operator row with an embedded row's `id` replaces it.
//! Loading never fails: a row that does not parse or validate is kept as `broken` with
//! its reason (and an operator file that fails leaves the row it meant to replace in
//! force), so a bad row never stops the node.

mod row;
pub mod summary;
pub mod template;

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::stage::StageRegistry;

pub use row::{
    Band, Decoder, DecoderKind, Event, Family, Identify, InputStream, InputVia, Install, ModeId,
    ModeRow, OutputVia, Parser, ProgramBlock, ProgramInput, ProgramOutput, Proof, QUICK_ATTEMPTS,
    RowError, RowTest, SampleFormat, TestSpec, is_mode_id, parse, validate,
};

/// Where a loaded row stands: `ready` (parsed, validated and, for a program row, its
/// program found), `broken` (the row does not parse or validate; `reason` names the
/// field) or `unavailable` (a program row whose program is not on this host; `reason`
/// says which).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModeState {
    Ready,
    Broken,
    Unavailable,
}

/// One row as the registry holds it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
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
    /// The program a program row runs; `null` until probed, and for a graph row.
    #[schemars(required, schema_with = "crate::types::nullable::<PathBuf>")]
    #[serde(deserialize_with = "nullable")]
    pub program: Option<PathBuf>,
    /// When the row's check last passed with `--strict` on this host; `null` if never.
    #[schemars(required, schema_with = "crate::types::nullable::<u64>")]
    #[serde(deserialize_with = "nullable")]
    pub checked: Option<u64>,
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
const EMBEDDED: [(&str, &str); 5] = [
    (
        "modes/acars_decode.json",
        include_str!("../../../../modes/acars_decode.json"),
    ),
    (
        "modes/adsb_decode.json",
        include_str!("../../../../modes/adsb_decode.json"),
    ),
    (
        "modes/aprs_decode.json",
        include_str!("../../../../modes/aprs_decode.json"),
    ),
    (
        "modes/pocsag_decode.json",
        include_str!("../../../../modes/pocsag_decode.json"),
    ),
    (
        "modes/rds_decode.json",
        include_str!("../../../../modes/rds_decode.json"),
    ),
];

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

/// Every mode the node knows, sorted by id.
#[derive(Clone, Debug, Default)]
pub struct ModeRegistry {
    modes: Vec<LoadedMode>,
}

impl ModeRegistry {
    /// The embedded rows: `(file name, contents)`.
    pub fn embedded() -> Vec<(String, &'static str)> {
        EMBEDDED
            .iter()
            .map(|(f, text)| ((*f).to_string(), *text))
            .collect()
    }

    /// Load the embedded rows, then every `*.json` file of `operator_dir` (by file name;
    /// a missing or unreadable directory reads as empty), validating each against
    /// `stages`. A valid operator row replaces the row with its id; a broken one is
    /// listed beside it (see [`LoadedMode::row`]). Never fails.
    pub fn load(stages: &StageRegistry, operator_dir: Option<&Path>) -> ModeRegistry {
        let mut reg = ModeRegistry::default();
        for (file, text) in EMBEDDED {
            reg.add(file.to_string(), text, stages);
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
        let loaded = LoadedMode {
            row,
            state: ModeState::Ready,
            reason: None,
            file,
            program: None,
            checked: None,
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
        });
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

    /// Record the program a program row runs after probing: found, the row is ready;
    /// not found (`None`), it is unavailable with `reason`. A broken row stays broken.
    pub fn set_program(&mut self, id: &str, program: Option<PathBuf>, reason: Option<String>) {
        let Some(m) = self.modes.iter_mut().find(|m| m.row.id.0 == id) else {
            return;
        };
        if m.state == ModeState::Broken {
            return;
        }
        m.state = match program {
            Some(_) => ModeState::Ready,
            None => ModeState::Unavailable,
        };
        m.program = program;
        m.reason = reason;
    }

    /// One stderr line per broken row: `zeta: mode <id> (<file>): <field>: <error>`.
    pub fn startup_lines(&self) -> Vec<String> {
        self.modes
            .iter()
            .filter(|m| m.state == ModeState::Broken)
            .map(|m| {
                let id = m.row.id.0.split('@').next().unwrap_or_default();
                let reason = m.reason.as_deref().unwrap_or_default();
                format!("zeta: mode {id} ({}): {reason}", m.file)
            })
            .collect()
    }
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
        },
        modulation: vec![],
        label: String::new(),
        protocol: String::new(),
        decoder: Decoder::Graph {
            nodes: vec![],
            edges: vec![],
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
        },
        catalogue: vec![],
        test: None,
        notes: None,
    }
}
