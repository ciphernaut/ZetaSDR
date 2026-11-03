//! A theme folder's `theme.json`: its fields, parsing and the checks serde cannot make.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::tokens::TOKENS;

/// Why a theme is broken: the offending field and what is wrong with it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {error}")]
pub struct ThemeError {
    pub field: String,
    pub error: String,
}

impl ThemeError {
    pub(crate) fn new(field: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            error: error.into(),
        }
    }
}

/// `ready` (the page can use it) or `broken` (its folder did not parse or validate;
/// `reason` names the field).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThemeState {
    Ready,
    Broken,
}

/// The page's `color-scheme` under the theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ColorScheme {
    Dark,
    Light,
}

/// The switches the page's script honours.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    /// The rail beside the frequency axis, on or off.
    pub rail: bool,
    /// The transcript ribbons' lean in degrees, -30..=30.
    pub ribbon_tilt_deg: i32,
}

/// A font file in the theme's folder, declared as an `@font-face` under the theme.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FontFace {
    pub family: String,
    /// A path relative to the theme's folder.
    pub file: String,
    /// The CSS `font-weight` range, such as `400 700`.
    pub weight: String,
}

/// A theme folder's `theme.json`. Every field is required except `fonts`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeJson {
    pub id: String,
    pub name: String,
    pub description: String,
    pub color_scheme: ColorScheme,
    pub layout: Layout,
    /// Values for the custom properties in [`TOKENS`].
    pub tokens: BTreeMap<String, String>,
    /// The waterfall's colour stops, `[t, [r, g, b]]`.
    pub ramp: Vec<(f64, [u8; 3])>,
    #[serde(default)]
    pub fonts: Vec<FontFace>,
}

/// The top-level fields of `theme.json`.
const FIELDS: [&str; 8] = [
    "id",
    "name",
    "description",
    "color_scheme",
    "layout",
    "tokens",
    "ramp",
    "fonts",
];

/// Parse a `theme.json` (a leading UTF-8 BOM is skipped). A failure names the field it
/// is in: an unknown or missing top-level field by its name, a nested one as
/// `<field>.<name>`, a value of the wrong type by its top-level field, a file that is
/// not a JSON object as `theme.json`.
pub fn parse(text: &str) -> Result<ThemeJson, ThemeError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| ThemeError::new("theme.json", e.to_string()))?;
    let Some(obj) = v.as_object() else {
        return Err(ThemeError::new("theme.json", "not a JSON object"));
    };
    if let Some(k) = obj.keys().find(|k| !FIELDS.contains(&k.as_str())) {
        return Err(ThemeError::new(
            k.clone(),
            format!("unknown field `{k}`, expected one of {}", FIELDS.join(", ")),
        ));
    }
    if let Some(k) = FIELDS[..7].iter().find(|k| !obj.contains_key(**k)) {
        return Err(ThemeError::new(*k, format!("missing field `{k}`")));
    }
    // Each field on its own first, so a failure names the field it is in.
    let field = |k: &str| obj.get(k).cloned().unwrap_or(serde_json::Value::Null);
    check::<String>("id", field("id"))?;
    check::<String>("name", field("name"))?;
    check::<String>("description", field("description"))?;
    check::<ColorScheme>("color_scheme", field("color_scheme"))?;
    check::<Layout>("layout", field("layout"))?;
    check::<BTreeMap<String, String>>("tokens", field("tokens"))?;
    check::<Vec<(f64, [u8; 3])>>("ramp", field("ramp"))?;
    if obj.contains_key("fonts") {
        check::<Vec<FontFace>>("fonts", field("fonts"))?;
    }
    serde_json::from_value(v).map_err(|e| ThemeError::new("theme.json", e.to_string()))
}

/// Deserialise one field's value as `T`, naming the field (and a nested unknown or
/// missing field under it) on failure.
fn check<T: DeserializeOwned>(field: &str, v: serde_json::Value) -> Result<(), ThemeError> {
    serde_json::from_value::<T>(v).map(drop).map_err(|e| {
        let msg = e.to_string();
        let nested = ["unknown field `", "missing field `"]
            .iter()
            .find_map(|p| msg.split_once(p))
            .and_then(|(_, rest)| rest.split_once('`'))
            .map(|(name, _)| format!("{field}.{name}"));
        ThemeError::new(nested.unwrap_or_else(|| field.to_string()), msg)
    })
}

/// `^[a-z][a-z0-9_-]{0,31}$`.
pub fn is_theme_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

/// What a CSS value written into the wrapper may not contain: anything that would end
/// its declaration or block, open markup, or load a file. `None` when it is clean.
fn bad_value(value: &str) -> Option<&'static str> {
    for (needle, what) in [(";", "`;`"), ("{", "`{`"), ("}", "`}`"), ("<", "`<`")] {
        if value.contains(needle) {
            return Some(what);
        }
    }
    value
        .to_ascii_lowercase()
        .contains("url(")
        .then_some("`url(` (urls belong in style.css)")
}

/// Check what serde cannot: the id's spelling and its folder, the layout's range, each
/// token's key and value, the ramp's stops and each font's file among `files` (the
/// paths the folder serves, relative to it).
pub fn validate(
    t: &ThemeJson,
    folder_id: &str,
    files: &BTreeSet<String>,
) -> Result<(), ThemeError> {
    if !is_theme_id(&t.id) {
        return Err(ThemeError::new(
            "id",
            format!("{:?} must match ^[a-z][a-z0-9_-]{{0,31}}$", t.id),
        ));
    }
    if t.id != folder_id {
        return Err(ThemeError::new(
            "id",
            format!("{:?} differs from its folder {folder_id:?}", t.id),
        ));
    }
    if !(-30..=30).contains(&t.layout.ribbon_tilt_deg) {
        return Err(ThemeError::new(
            "layout.ribbon_tilt_deg",
            format!("{} is outside -30..=30", t.layout.ribbon_tilt_deg),
        ));
    }
    for (key, value) in &t.tokens {
        let field = format!("tokens.{key}");
        if key == "--ribbon-tilt" {
            return Err(ThemeError::new(field, "use layout.ribbon_tilt_deg"));
        }
        if !TOKENS.contains(&key.as_str()) {
            return Err(ThemeError::new(field, "unknown token"));
        }
        if let Some(what) = bad_value(value) {
            return Err(ThemeError::new(field, format!("the value contains {what}")));
        }
    }
    validate_ramp(&t.ramp)?;
    for (i, f) in t.fonts.iter().enumerate() {
        for (name, value) in [("family", &f.family), ("weight", &f.weight)] {
            let what = bad_value(value).or_else(|| value.contains('"').then_some("`\"`"));
            if let Some(what) = what {
                return Err(ThemeError::new(
                    format!("fonts[{i}].{name}"),
                    format!("the value contains {what}"),
                ));
            }
        }
        if !files.contains(&f.file) {
            return Err(ThemeError::new(
                format!("fonts[{i}].file"),
                format!("{:?} is not a file of the theme", f.file),
            ));
        }
    }
    Ok(())
}

/// 2-16 stops, `t` strictly ascending from exactly 0 to exactly 1.
fn validate_ramp(ramp: &[(f64, [u8; 3])]) -> Result<(), ThemeError> {
    let err = |e: String| Err(ThemeError::new("ramp", e));
    if !(2..=16).contains(&ramp.len()) {
        return err(format!("{} stops, expected 2-16", ramp.len()));
    }
    if ramp[0].0 != 0.0 {
        return err(format!("the first stop's t is {}, expected 0", ramp[0].0));
    }
    let last = ramp[ramp.len() - 1].0;
    if last != 1.0 {
        return err(format!("the last stop's t is {last}, expected 1"));
    }
    if let Some(w) = ramp.windows(2).find(|w| w[1].0 <= w[0].0) {
        return err(format!("t {} after {} is not ascending", w[1].0, w[0].0));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r##"{
        "id": "mytheme",
        "name": "My theme",
        "description": "A test theme.",
        "color_scheme": "dark",
        "layout": { "rail": true, "ribbon_tilt_deg": -15 },
        "tokens": { "--accent": "#ff00ff", "--font-ui": "\"Face\", sans-serif" },
        "ramp": [[0, [0, 0, 48]], [0.35, [0, 90, 255]], [1, [255, 255, 255]]],
        "fonts": [{ "family": "Face", "file": "Face.woff2", "weight": "400 700" }]
    }"##;

    /// The fixture with `edit` applied to its JSON.
    fn edited(edit: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut v: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        edit(&mut v);
        v.to_string()
    }

    fn files() -> BTreeSet<String> {
        ["theme.json", "Face.woff2"].map(String::from).into()
    }

    /// Parse and validate `text` as the folder `mytheme`.
    fn load(text: &str) -> Result<ThemeJson, ThemeError> {
        let t = parse(text)?;
        validate(&t, "mytheme", &files())?;
        Ok(t)
    }

    /// The field `text` is refused on.
    fn refused(text: &str) -> ThemeError {
        load(text).expect_err("refused")
    }

    #[test]
    fn the_fixture_loads() {
        let t = load(FIXTURE).unwrap();
        assert_eq!(t.layout.ribbon_tilt_deg, -15);
        assert_eq!(t.ramp[1], (0.35, [0, 90, 255]));
        assert_eq!(t.fonts[0].file, "Face.woff2");
    }

    #[test]
    fn fonts_are_optional() {
        let text = edited(|v| {
            v.as_object_mut().unwrap().remove("fonts");
        });
        assert!(load(&text).unwrap().fonts.is_empty());
    }

    #[test]
    fn a_missing_field_is_named() {
        let text = edited(|v| {
            v.as_object_mut().unwrap().remove("ramp");
        });
        assert_eq!(refused(&text).field, "ramp");
    }

    #[test]
    fn an_unknown_field_is_refused() {
        let e = refused(&edited(|v| v["footprints"] = serde_json::json!({})));
        assert_eq!(e.field, "footprints", "{e}");
        let e = refused(&edited(|v| v["layout"]["corners"] = true.into()));
        assert_eq!(e.field, "layout.corners", "{e}");
    }

    #[test]
    fn a_bad_id_is_refused() {
        assert_eq!(refused(&edited(|v| v["id"] = "Bad".into())).field, "id");
        assert_eq!(refused(&edited(|v| v["id"] = "other".into())).field, "id");
    }

    #[test]
    fn a_bad_color_scheme_is_refused() {
        let e = refused(&edited(|v| v["color_scheme"] = "dim".into()));
        assert_eq!(e.field, "color_scheme", "{e}");
    }

    #[test]
    fn a_ribbon_tilt_out_of_range_is_refused() {
        let e = refused(&edited(|v| v["layout"]["ribbon_tilt_deg"] = 31.into()));
        assert_eq!(e.field, "layout.ribbon_tilt_deg", "{e}");
    }

    #[test]
    fn an_unknown_token_is_refused() {
        let e = refused(&edited(|v| v["tokens"]["--nope"] = "1px".into()));
        assert_eq!(e.field, "tokens.--nope", "{e}");
    }

    #[test]
    fn the_ribbon_tilt_token_is_refused() {
        let e = refused(&edited(|v| v["tokens"]["--ribbon-tilt"] = "5deg".into()));
        assert_eq!(e.field, "tokens.--ribbon-tilt");
        assert!(e.error.contains("use layout.ribbon_tilt_deg"), "{e}");
    }

    #[test]
    fn a_token_value_that_escapes_is_refused() {
        for bad in ["red; x", "red { x", "red } x", "<b>", "URL(x.png)"] {
            let e = refused(&edited(|v| v["tokens"]["--accent"] = bad.into()));
            assert_eq!(e.field, "tokens.--accent", "{bad}: {e}");
        }
    }

    #[test]
    fn a_long_token_value_passes() {
        let long = "a".repeat(5000);
        load(&edited(|v| v["tokens"]["--accent"] = long.into())).unwrap();
    }

    #[test]
    fn a_bad_ramp_is_refused() {
        let stop = |t: f64| serde_json::json!([t, [0, 0, 0]]);
        let cases: Vec<serde_json::Value> = vec![
            serde_json::json!([[0, [0, 0, 0]]]),
            (0..17).map(|i| stop(i as f64 / 16.0)).collect(),
            serde_json::json!([[0.1, [0, 0, 0]], [1, [0, 0, 0]]]),
            serde_json::json!([[0, [0, 0, 0]], [0.9, [0, 0, 0]]]),
            serde_json::json!([
                [0, [0, 0, 0]],
                [0.5, [0, 0, 0]],
                [0.5, [0, 0, 0]],
                [1, [0, 0, 0]]
            ]),
            serde_json::json!([
                [0, [0, 0, 0]],
                [0.6, [0, 0, 0]],
                [0.4, [0, 0, 0]],
                [1, [0, 0, 0]]
            ]),
            serde_json::json!([[0, [0, 0, 256]], [1, [0, 0, 0]]]),
        ];
        for ramp in cases {
            let e = refused(&edited(|v| v["ramp"] = ramp.clone()));
            assert_eq!(e.field, "ramp", "{ramp}: {e}");
        }
        let sixteen: serde_json::Value = (0..16).map(|i| stop(i as f64 / 15.0)).collect();
        load(&edited(|v| v["ramp"] = sixteen)).unwrap();
    }

    #[test]
    fn a_font_whose_file_is_missing_is_refused() {
        let e = refused(&edited(|v| v["fonts"][0]["file"] = "Gone.woff2".into()));
        assert_eq!(e.field, "fonts[0].file", "{e}");
    }

    #[test]
    fn a_font_family_that_escapes_is_refused() {
        let e = refused(&edited(|v| v["fonts"][0]["family"] = "F\"; x".into()));
        assert_eq!(e.field, "fonts[0].family", "{e}");
    }

    #[test]
    fn a_bom_prefixed_file_parses() {
        load(&format!("\u{feff}{FIXTURE}")).unwrap();
    }

    #[test]
    fn invalid_json_names_the_file() {
        assert_eq!(refused("{").field, "theme.json");
        assert_eq!(refused("[]").field, "theme.json");
    }

    #[test]
    fn theme_ids() {
        for ok in ["a", "plain", "my-theme_2", &"a".repeat(32)] {
            assert!(is_theme_id(ok), "{ok}");
        }
        for bad in ["", "Bad", "1a", "-a", "a.b", "a b", &"a".repeat(33)] {
            assert!(!is_theme_id(bad), "{bad}");
        }
    }
}
