//! A handler file: one external program and the decoder modes it serves, each as a
//! program block (`args`, `input`, `output`, optional `packets` and `demod`) like a
//! program row's own, so the existing external stage runs it.
//!
//! Files are embedded (`handlers/*.json` in the repo) and read from the operator's
//! handlers directory (`--handlers-dir`); the registry ([`super::ModeRegistry`]) adds a
//! handler to each mode a file lists. A file that does not parse, or whose `id` is not a
//! handler id, is broken whole; a mode entry that does not parse, names no mode or fails
//! validation breaks that entry only.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::row::{
    Demod, FieldPath, Install, ModeRow, ProgramBlock, ProgramInput, ProgramOutput, ProgramPackets,
    RowError, validate_program,
};

/// The id a graph or frame row's own decoder goes by; never a handler file's.
pub const BUILTIN: &str = "builtin";

/// One handler file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct HandlerFile {
    /// The handler's id in every mode it serves: `^[a-z0-9][a-z0-9_-]*$`, never
    /// `builtin`, nor a mode's own program.
    pub id: String,
    /// The handler's name for people (`OnAir`).
    pub name: String,
    /// Names tried on `PATH` in order (or an absolute path), the first found used.
    pub programs: Vec<String>,
    pub install: Install,
    /// The modes it serves, by mode id.
    pub modes: BTreeMap<String, HandlerEntry>,
}

/// One mode a handler file serves: its program block, and the event fields the handler
/// adds beside the row's.
// Read through `EntryFields`: serde's `flatten` cannot reject an unknown key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(from = "EntryFields")]
pub struct HandlerEntry {
    #[serde(flatten)]
    pub block: HandlerBlockSpec,
    /// Event fields this handler adds that the row's schema does not have, each read as
    /// `output.fields` reads; empty for none.
    #[serde(default)]
    pub extra: BTreeMap<String, FieldPath>,
}

/// A [`HandlerEntry`] as a file writes it, every key at one level.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
struct EntryFields {
    /// As a program row's `args`, with the same placeholders.
    args: Vec<String>,
    input: ProgramInput,
    output: ProgramOutput,
    #[serde(default)]
    packets: Option<ProgramPackets>,
    #[serde(default)]
    demod: Option<Demod>,
    /// Event fields this handler adds that the row's schema does not have, each read as
    /// `output.fields` reads; empty for none.
    #[serde(default)]
    extra: BTreeMap<String, FieldPath>,
}

impl From<EntryFields> for HandlerEntry {
    fn from(e: EntryFields) -> Self {
        HandlerEntry {
            block: HandlerBlockSpec {
                args: e.args,
                input: e.input,
                output: e.output,
                packets: e.packets,
                demod: e.demod,
            },
            extra: e.extra,
        }
    }
}

/// A program block without `programs` and `install`, which the file gives once for
/// every mode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HandlerBlockSpec {
    /// As a program row's `args`, with the same placeholders.
    pub args: Vec<String>,
    pub input: ProgramInput,
    pub output: ProgramOutput,
    #[serde(default)]
    pub packets: Option<ProgramPackets>,
    #[serde(default)]
    pub demod: Option<Demod>,
}

impl HandlerEntry {
    /// The program block that runs this entry: `file`'s `programs` and `install`, the
    /// entry's `args`, `input`, `output`, `packets` and `demod`.
    pub fn program_block(&self, file: &HandlerFile) -> ProgramBlock {
        let b = &self.block;
        ProgramBlock {
            programs: file.programs.clone(),
            args: b.args.clone(),
            input: b.input.clone(),
            output: b.output.clone(),
            install: file.install.clone(),
            demod: b.demod.clone(),
            packets: b.packets.clone(),
        }
    }
}

/// `^[a-z0-9][a-z0-9_-]*$`.
pub fn is_handler_id(id: &str) -> bool {
    let mut b = id.bytes();
    let ok = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    b.next().is_some_and(ok) && b.all(|c| ok(c) || c == b'_' || c == b'-')
}

/// The id of `row`'s own handler: `builtin` for a graph or frame row, its first program
/// for a program row.
pub fn own_handler_id(row: &ModeRow) -> String {
    match &row.decoder {
        super::Decoder::Program(p) => p.programs.first().cloned().unwrap_or_default(),
        _ => BUILTIN.into(),
    }
}

/// A handler file as read before its mode entries: each entry is read on its own, so a
/// malformed one breaks only itself.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    id: String,
    name: String,
    programs: Vec<String>,
    install: Install,
    modes: BTreeMap<String, serde_json::Value>,
}

/// Parse one handler file and check what concerns the whole file: its keys, its `id`
/// and `programs`; a failure there is the file's error. Each mode entry is read on its
/// own: the file comes with the entries that parsed, and beside it, by mode id, why
/// each other one did not. A serde failure names the unknown or missing field when serde
/// says which (`handler file`, or `entry` for an entry, otherwise).
pub fn parse_handler_file(
    text: &str,
) -> Result<(HandlerFile, BTreeMap<String, RowError>), RowError> {
    let raw: RawFile = serde_json::from_str(text).map_err(|e| serde_error(&e, "handler file"))?;
    let mut modes = BTreeMap::new();
    let mut broken = BTreeMap::new();
    for (mode, value) in raw.modes {
        match serde_json::from_value::<HandlerEntry>(value) {
            Ok(entry) => {
                modes.insert(mode, entry);
            }
            Err(e) => {
                broken.insert(mode, serde_error(&e, "entry"));
            }
        }
    }
    let file = HandlerFile {
        id: raw.id,
        name: raw.name,
        programs: raw.programs,
        install: raw.install,
        modes,
    };
    if file.id == BUILTIN {
        return Err(error("id", "builtin is a row's own decoder's id"));
    }
    if !is_handler_id(&file.id) {
        return Err(error(
            "id",
            format!("{:?} must match ^[a-z0-9][a-z0-9_-]*$", file.id),
        ));
    }
    if file.programs.is_empty() {
        return Err(error(
            "programs",
            "names no program (give at least one name or absolute path)",
        ));
    }
    Ok((file, broken))
}

/// A serde failure as a [`RowError`] on the unknown or missing field it names, else on
/// `whole`.
fn serde_error(e: &serde_json::Error, whole: &str) -> RowError {
    let msg = e.to_string();
    let named = ["unknown field `", "missing field `"]
        .iter()
        .find_map(|p| msg.split_once(p))
        .and_then(|(_, rest)| rest.split_once('`'))
        .map(|(name, _)| name.to_string());
    error(named.as_deref().unwrap_or(whole), msg)
}

/// Check `file`'s entry `entry` against the row it serves: the program block as a
/// program row's is checked, `output.fields` naming only the row's event fields and
/// mapping its subject key, and `extra` naming only fields the row's event schema lacks.
/// The field is named from the entry (`output.fields`, `extra.<name>`, `args[0]`).
pub fn validate_entry(
    file: &HandlerFile,
    entry: &HandlerEntry,
    row: &ModeRow,
) -> Result<(), RowError> {
    validate_program(&entry.program_block(file)).map_err(|e| RowError {
        field: e
            .field
            .strip_prefix("decoder.")
            .map_or_else(|| e.field.clone(), String::from),
        error: e.error,
    })?;
    let empty = serde_json::Map::new();
    let properties = row.event.schema["properties"].as_object().unwrap_or(&empty);
    if let Some(fields) = &entry.block.output.fields {
        for (key, path) in fields {
            if !properties.contains_key(key) {
                return Err(error(
                    format!("output.fields.{key}"),
                    "not in the row's event schema (name it under extra)",
                ));
            }
            check_path(path, &format!("output.fields.{key}"))?;
        }
        if let Some(sub) = &row.subject
            && !fields.contains_key(&sub.key)
        {
            return Err(error(
                "output.fields",
                format!("no mapping for {} (the row's subject key)", sub.key),
            ));
        }
    }
    for (name, path) in &entry.extra {
        let at = format!("extra.{name}");
        if properties.contains_key(name) {
            return Err(error(at, "the row's event schema already has it"));
        }
        check_path(path, &at)?;
    }
    Ok(())
}

/// [`FieldPath::check`], its error named `at`.
fn check_path(path: &FieldPath, at: &str) -> Result<(), RowError> {
    path.check(at).map_err(|e| error(at, e.error))
}

fn error(field: impl Into<String>, error: impl Into<String>) -> RowError {
    RowError {
        field: field.into(),
        error: error.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(edit: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut v = serde_json::json!({
            "id": "alt",
            "name": "Alt",
            "programs": ["alt-rx"],
            "install": {"url": "https://example.invalid"},
            "modes": {
                "adsb_decode": {
                    "args": ["--rate", "$rate_hz"],
                    "input": {"stream": "iq", "format": "cs16", "rate_hz": 2400000, "via": "stdin"},
                    "output": {"via": "stdout", "parser": "json_lines", "fields": {"icao": "/icao"}},
                    "extra": {"df": "/df"}
                }
            }
        });
        edit(&mut v);
        v.to_string()
    }

    #[test]
    fn an_entry_builds_the_program_block_with_the_files_programs() {
        let f = parse_handler_file(&file(|_| {})).unwrap().0;
        let e = &f.modes["adsb_decode"];
        assert_eq!(e.extra.len(), 1);
        let b = e.program_block(&f);
        assert_eq!(b.programs, ["alt-rx"]);
        assert_eq!(b.install.url, "https://example.invalid");
        assert_eq!(b.args, ["--rate", "$rate_hz"]);
        assert_eq!(b.packets, None);
        assert_eq!(b.demod, None);
    }

    /// An entry's unknown key breaks that entry only, named under its mode; the file's
    /// own unknown key, or a missing one, breaks the file.
    #[test]
    fn an_unknown_field_in_an_entry_or_the_file_is_named() {
        let (f, broken) = parse_handler_file(&file(|v| {
            v["modes"]["ais_decode"] = v["modes"]["adsb_decode"].clone();
            v["modes"]["adsb_decode"]["bogus"] = 1.into();
        }))
        .unwrap();
        assert_eq!(f.modes.keys().collect::<Vec<_>>(), ["ais_decode"]);
        assert_eq!(broken.keys().collect::<Vec<_>>(), ["adsb_decode"]);
        let e = &broken["adsb_decode"];
        assert_eq!(e.field, "bogus", "{e}");
        assert!(e.error.contains("unknown field"), "{e}");
        let e = parse_handler_file(&file(|v| v["bogus"] = 1.into())).unwrap_err();
        assert_eq!(e.field, "bogus", "{e}");
        let e = parse_handler_file(&file(|v| {
            v.as_object_mut().unwrap().remove("install");
        }))
        .unwrap_err();
        assert_eq!(e.field, "install", "{e}");
    }

    /// The file's schema derives, an entry's keys at one level as a file writes them.
    #[test]
    fn the_handler_file_schema_lists_an_entrys_keys() {
        let s = serde_json::to_value(schemars::schema_for!(HandlerFile)).unwrap();
        let entry = s["$defs"]["HandlerEntry"]["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{s}"));
        let keys: Vec<&str> = entry.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["args", "demod", "extra", "input", "output", "packets"]
        );
    }

    #[test]
    fn a_file_id_is_a_handler_id_and_never_builtin() {
        for id in ["Alt", "-alt", "", "a b"] {
            let e = parse_handler_file(&file(|v| v["id"] = id.into())).unwrap_err();
            assert_eq!(e.field, "id", "{id}: {e}");
        }
        let e = parse_handler_file(&file(|v| v["id"] = BUILTIN.into())).unwrap_err();
        assert_eq!(e.to_string(), "id: builtin is a row's own decoder's id");
        for id in ["onair", "0x", "a_b-c"] {
            assert!(
                parse_handler_file(&file(|v| v["id"] = id.into())).is_ok(),
                "{id}"
            );
        }
        let e = parse_handler_file(&file(|v| v["programs"] = serde_json::json!([]))).unwrap_err();
        assert_eq!(e.field, "programs", "{e}");
    }

    #[test]
    fn an_entrys_block_errors_are_named_from_the_entry() {
        let row = super::super::parse(include_str!("../../../../modes/adsb_decode.json")).unwrap();
        let f = parse_handler_file(&file(|v| {
            v["modes"]["adsb_decode"]["args"] = serde_json::json!(["$nope"]);
        }))
        .unwrap()
        .0;
        let e = validate_entry(&f, &f.modes["adsb_decode"], &row).unwrap_err();
        assert_eq!(e.field, "args[0]", "{e}");
        let f = parse_handler_file(&file(|v| {
            v["modes"]["adsb_decode"]["output"]["fields"]["nope"] = "/n".into();
        }))
        .unwrap()
        .0;
        let e = validate_entry(&f, &f.modes["adsb_decode"], &row).unwrap_err();
        assert_eq!(e.field, "output.fields.nope", "{e}");
        let f = parse_handler_file(&file(|v| {
            v["modes"]["adsb_decode"]["extra"]["df"] = "df".into();
        }))
        .unwrap()
        .0;
        let e = validate_entry(&f, &f.modes["adsb_decode"], &row).unwrap_err();
        assert_eq!(e.field, "extra.df", "{e}");
        let f = parse_handler_file(&file(|_| {})).unwrap().0;
        validate_entry(&f, &f.modes["adsb_decode"], &row).unwrap();
    }
}
