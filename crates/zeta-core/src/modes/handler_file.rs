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
    RowError, VersionProbe, validate_program, validate_version,
};

/// The id a graph or frame row's own decoder goes by; never a handler file's.
pub const BUILTIN: &str = "builtin";

/// The id of the stand-in own handler of a row with no decoder of its own (never ready,
/// never picked, listed or recorded); never a handler file's, nor a program row's.
pub const NO_DECODER: &str = "none";

/// One handler file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct HandlerFile {
    /// The handler's id in every mode it serves: `^[a-z0-9][a-z0-9_-]*$`, never
    /// `builtin` or `none`, nor a mode's own program.
    pub id: String,
    /// The handler's name for people (`OnAir`).
    pub name: String,
    /// Names tried on `PATH` in order (or an absolute path), the first found used.
    pub programs: Vec<String>,
    pub install: Install,
    /// The least version its program must report, as a program row's `version`; `null`
    /// for none (the program is only looked up, never run, at the probe).
    #[serde(default)]
    pub version: Option<VersionProbe>,
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
    /// Row event fields given a constant value on every event, after the field map
    /// (`{"crc_ok": true}`); empty for none. They travel beside the program block, as
    /// `extra` does ([`super::ModeRegistry::handler_consts`]).
    #[serde(default, rename = "const")]
    pub consts: BTreeMap<String, serde_json::Value>,
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
    /// Row event fields given a constant value on every event, after the field map
    /// (`{"crc_ok": true}`); empty for none.
    #[serde(default, rename = "const")]
    consts: BTreeMap<String, serde_json::Value>,
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
            consts: e.consts,
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
    /// The program block that runs this entry: `file`'s `programs`, `install` and
    /// `version`, the entry's `args`, `input`, `output`, `packets` and `demod`.
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
            version: file.version.clone(),
        }
    }
}

/// `^[a-z0-9][a-z0-9_-]*$`.
pub fn is_handler_id(id: &str) -> bool {
    let mut b = id.bytes();
    let ok = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    b.next().is_some_and(ok) && b.all(|c| ok(c) || c == b'_' || c == b'-')
}

/// The id of `row`'s own handler: `builtin` for a graph or frame row, its first program's
/// file name for a program row (`dump1090` for `/opt/x/dump1090`; the program is still
/// looked up by the whole entry), [`NO_DECODER`] for a row with no decoder of its own.
pub fn own_handler_id(row: &ModeRow) -> String {
    match &row.decoder {
        None => NO_DECODER.into(),
        Some(super::Decoder::Program(p)) => p
            .programs
            .first()
            .map(|first| {
                std::path::Path::new(first)
                    .file_name()
                    .map_or_else(|| first.clone(), |n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_default(),
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
    #[serde(default)]
    version: Option<VersionProbe>,
    modes: BTreeMap<String, serde_json::Value>,
}

/// Parse one handler file and check what concerns the whole file: its keys, its `id`,
/// `programs` and `version` block; a failure there is the file's error. Each mode entry is read on its
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
        version: raw.version,
        modes,
    };
    if file.id == BUILTIN {
        return Err(error("id", "builtin is a row's own decoder's id"));
    }
    if file.id == NO_DECODER {
        return Err(error("id", "none is reserved"));
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
    if let Some(v) = &file.version {
        validate_version(v, "version")?;
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

/// The keys the event envelope sets beside the row's [`super::ENVELOPE_FIELDS`]: the
/// `handler` that ran and a restricted row's `access`.
const ENVELOPE_EXTRA: [&str; 2] = ["handler", "access"];

/// Whether the event envelope sets `name` over the decoded fields.
fn is_envelope_key(name: &str) -> bool {
    super::ENVELOPE_FIELDS.contains(&name) || ENVELOPE_EXTRA.contains(&name)
}

/// Check `file`'s entry `entry` against the row it serves: the program block as a
/// program row's is checked, `output.fields` naming only the row's event fields and
/// mapping its subject key (and, for a row with no decoder of its own, every field of its
/// schema but the envelope's that no `const` or derive fills), and `extra` naming no
/// envelope key and only fields the row's event schema lacks.
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
        // A row with no decoder of its own has nothing else to fill its fields: the map
        // names each, as a program row's own does (a constant or a derive fills one too).
        if row.decoder.is_none() {
            let derived: Vec<&String> = row.derive.iter().flat_map(|d| d.into.values()).collect();
            let unmapped = properties.keys().find(|name| {
                !super::ENVELOPE_FIELDS.contains(&name.as_str())
                    && !fields.contains_key(*name)
                    && !entry.consts.contains_key(*name)
                    && !derived.contains(name)
            });
            if let Some(name) = unmapped {
                return Err(error(
                    "output.fields",
                    format!(
                        "{name} is not mapped, and the row has no decoder of its own to fill it"
                    ),
                ));
            }
        }
    }
    for (name, path) in &entry.extra {
        let at = format!("extra.{name}");
        if is_envelope_key(name) {
            return Err(error(at, "the event envelope sets it"));
        }
        if properties.contains_key(name) {
            return Err(error(at, "the row's event schema already has it"));
        }
        check_path(path, &at)?;
    }
    for (key, value) in &entry.consts {
        check_const(key, value, row, properties, entry)?;
    }
    Ok(())
}

/// One `const` of an entry: no key the event envelope sets, a field of the row's event
/// schema, not also read from the program, not the subject key, and a scalar the
/// schema's `type` accepts.
fn check_const(
    key: &str,
    value: &serde_json::Value,
    row: &ModeRow,
    properties: &serde_json::Map<String, serde_json::Value>,
    entry: &HandlerEntry,
) -> Result<(), RowError> {
    let at = format!("const.{key}");
    if is_envelope_key(key) {
        return Err(error(at, "the event envelope sets it"));
    }
    let Some(property) = properties.get(key) else {
        return Err(error(at, "not in the row's event schema"));
    };
    let read = entry
        .block
        .output
        .fields
        .as_ref()
        .is_some_and(|f| f.contains_key(key));
    if row.subject.as_ref().is_some_and(|s| s.key == key) {
        return Err(error(at, "the row's subject key is read from the program"));
    }
    if read || entry.extra.contains_key(key) {
        return Err(error(at, "also read from the program"));
    }
    let kind = |t: &str| match t {
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        _ => false,
    };
    let types: Vec<&str> = match &property["type"] {
        serde_json::Value::String(t) => vec![t.as_str()],
        serde_json::Value::Array(a) => a.iter().filter_map(|t| t.as_str()).collect(),
        _ => Vec::new(),
    };
    if types.is_empty() || !types.iter().any(|t| kind(t)) {
        let named = if types.is_empty() {
            "scalar".to_string()
        } else {
            types.join(" or ")
        };
        return Err(error(at, format!("not a {named}")));
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
    fn a_program_rows_own_handler_is_named_by_its_programs_file_name() {
        let text = include_str!("../../../../modes/adsb_decode.json");
        let mut row = super::super::row::parse(text).unwrap();
        assert_eq!(own_handler_id(&row), "dump1090");
        let Some(super::super::Decoder::Program(p)) = &mut row.decoder else {
            panic!("a program row")
        };
        p.programs = vec!["/opt/x/dump1090".into(), "readsb".into()];
        assert_eq!(own_handler_id(&row), "dump1090");
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
            [
                "args", "const", "demod", "extra", "input", "output", "packets"
            ]
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

    /// An `extra` field may not be one the event's envelope sets over it.
    #[test]
    fn an_extra_field_may_not_be_an_envelope_key() {
        let row = super::super::parse(include_str!("../../../../modes/adsb_decode.json")).unwrap();
        for name in [
            "protocol", "handler", "summary", "t_ns", "position", "access",
        ] {
            let f = parse_handler_file(&file(|v| {
                v["modes"]["adsb_decode"]["extra"] = serde_json::json!({ name: "/x" });
            }))
            .unwrap()
            .0;
            let e = validate_entry(&f, &f.modes["adsb_decode"], &row).unwrap_err();
            assert_eq!(
                e.to_string(),
                format!("extra.{name}: the event envelope sets it")
            );
        }
    }

    fn typed_row() -> ModeRow {
        let mut row =
            super::super::parse(include_str!("../../../../modes/adsb_decode.json")).unwrap();
        row.event.schema = serde_json::json!({
            "type": "object",
            "properties": {
                "icao": {"type": "string"},
                "address": {"type": "integer"},
                "kind": {"type": ["string", "null"]},
                "obj": {"type": "object"},
                "callsign": {"type": "string"},
                "protocol": {"type": "string"}
            }
        });
        row
    }

    /// An entry's constants travel beside its program block, as its `extra` does: the
    /// registry gives them for the mode and handler.
    #[test]
    fn an_entry_gives_constants() {
        let text = file(|v| {
            v["modes"]["adsb_decode"]["const"] = serde_json::json!({"on_ground": false});
        });
        let reg = super::super::ModeRegistry::load_handler_texts(
            &crate::stage::StageRegistry::new(),
            &[("alt.json", &text)],
        );
        let f = reg.handler_files().iter().find(|f| f.id == "alt").unwrap();
        assert_eq!(f.broken_entries(), Vec::<String>::new());
        assert_eq!(
            reg.handler_consts("adsb_decode", "alt"),
            BTreeMap::from([("on_ground".to_string(), serde_json::json!(false))])
        );
        // The row's own handler, and one the mode does not have, give none.
        assert!(reg.handler_consts("adsb_decode", "dump1090").is_empty());
        assert!(reg.handler_consts("adsb_decode", "nope").is_empty());
    }

    /// An entry's `output` is a program row's: `const` is no key of it, and the entry
    /// that writes it there does not parse.
    #[test]
    fn a_constant_under_output_breaks_the_entry() {
        let (f, broken) = parse_handler_file(&file(|v| {
            v["modes"]["adsb_decode"]["output"]["const"] = serde_json::json!({"on_ground": false});
        }))
        .unwrap();
        assert!(f.modes.is_empty());
        let e = &broken["adsb_decode"];
        assert_eq!(e.field, "const", "{e}");
        assert!(e.error.starts_with("unknown field `const`"), "{e}");
    }

    #[test]
    fn a_constant_must_fit_the_row() {
        let row = typed_row();
        let refused = |row: &ModeRow, edit: &dyn Fn(&mut serde_json::Value)| {
            let f = parse_handler_file(&file(|v| edit(&mut v["modes"]["adsb_decode"])))
                .unwrap()
                .0;
            let e = validate_entry(&f, &f.modes["adsb_decode"], row).unwrap_err();
            (e.field, e.error)
        };
        let one = |key: &'static str, value: serde_json::Value| {
            refused(&row, &move |m| {
                m["const"] = serde_json::json!({ key: value.clone() })
            })
        };
        let is = |f: &str, e: &str| (f.to_string(), e.to_string());
        // The envelope's own keys, checked first: `protocol` is in the schema too.
        for key in [
            "protocol", "handler", "summary", "t_ns", "position", "access",
        ] {
            assert_eq!(
                one(key, "x".into()),
                is(&format!("const.{key}"), "the event envelope sets it")
            );
        }
        assert_eq!(
            one("nope", 1.into()),
            is("const.nope", "not in the row's event schema")
        );
        assert_eq!(
            one("df", 1.into()),
            is("const.df", "not in the row's event schema")
        );
        assert_eq!(
            one("address", "x".into()),
            is("const.address", "not a integer")
        );
        assert_eq!(
            one("address", 1.5.into()),
            is("const.address", "not a integer")
        );
        assert_eq!(
            one("kind", 3.into()),
            is("const.kind", "not a string or null")
        );
        assert_eq!(
            one("callsign", serde_json::Value::Null),
            is("const.callsign", "not a string")
        );
        assert_eq!(
            one("callsign", serde_json::json!({"a": 1})),
            is("const.callsign", "not a string")
        );
        assert_eq!(
            one("callsign", serde_json::json!(["a"])),
            is("const.callsign", "not a string")
        );
        assert_eq!(
            one("icao", "7C6B2E".into()),
            is(
                "const.icao",
                "the row's subject key is read from the program"
            )
        );
        // Also read by the map (an `extra` name is never in the schema, so cannot be both).
        assert_eq!(
            refused(&row, &|m| {
                m["output"]["fields"]["callsign"] = "/c".into();
                m["const"] = serde_json::json!({"callsign": "x"});
            }),
            is("const.callsign", "also read from the program")
        );
        // One bad and one good entry: the good one loads.
        let (f, broken) = parse_handler_file(&file(|v| {
            v["modes"]["pocsag_decode"] = v["modes"]["adsb_decode"].clone();
            v["modes"]["pocsag_decode"]["const"] = serde_json::json!({"callsign": "ZETA"});
            v["modes"]["adsb_decode"]["const"] = serde_json::json!({"callsign": {"a": 1}});
        }))
        .unwrap();
        assert!(broken.is_empty());
        validate_entry(&f, &f.modes["pocsag_decode"], &row).unwrap();
        assert!(validate_entry(&f, &f.modes["adsb_decode"], &row).is_err());
    }
}
