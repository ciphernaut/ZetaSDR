//! The theme registry: the built-in theme folders under `themes/` in the repo, embedded,
//! then the operator's folder layered over them. A theme is a folder holding
//! `theme.json`, an optional `style.css` and the fonts and images it names.

pub mod theme_json;
pub mod tokens;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use theme_json::{
    ColorScheme, FontFace, Layout, ThemeError, ThemeJson, ThemeState, is_theme_id, parse, validate,
};
pub use tokens::TOKENS;

/// The built-in themes' files, `<id>/<path>` under `themes/` in the repo, one row per file.
pub const BUILTIN: &[(&str, &[u8])] = &[
    (
        "console/Antonio-VF.ttf",
        include_bytes!("../../../../themes/console/Antonio-VF.ttf"),
    ),
    (
        "console/OFL-Antonio.txt",
        include_bytes!("../../../../themes/console/OFL-Antonio.txt"),
    ),
    (
        "console/style.css",
        include_bytes!("../../../../themes/console/style.css"),
    ),
    (
        "console/theme.json",
        include_bytes!("../../../../themes/console/theme.json"),
    ),
    (
        "plain/style.css",
        include_bytes!("../../../../themes/plain/style.css"),
    ),
    (
        "plain/theme.json",
        include_bytes!("../../../../themes/plain/theme.json"),
    ),
];

/// `$XDG_CONFIG_HOME/zetasdr/themes`, else `$HOME/.config/zetasdr/themes`; `None` when
/// neither variable is set (an empty one counts as unset).
pub fn default_operator_dir() -> Option<PathBuf> {
    let var = |k| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    var("XDG_CONFIG_HOME")
        .map(|x| x.join("zetasdr/themes"))
        .or_else(|| var("HOME").map(|h| h.join(".config/zetasdr/themes")))
}

/// The content type a file is served with, by extension; `None` for a file not served.
fn mime(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "json" => "application/json",
        "css" => "text/css; charset=utf-8",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "jpg" | "jpeg" => "image/jpeg",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => return None,
    })
}

/// One file a theme serves.
#[derive(Clone, Debug)]
struct ThemeFile {
    mime: &'static str,
    bytes: Arc<[u8]>,
}

/// One theme as loaded: ready, or broken with the reason.
#[derive(Clone, Debug)]
pub struct LoadedTheme {
    /// The theme's id; a broken folder whose id a ready theme has is listed as
    /// `<id>@<folder path>`, the ready theme staying in force.
    pub id: String,
    /// The parsed `theme.json`; `None` for a broken theme.
    pub theme: Option<ThemeJson>,
    pub state: ThemeState,
    /// `<field>: <error>` for a broken theme, `None` when ready.
    pub reason: Option<String>,
    /// `themes/<id>` for a built-in, the folder's path for an operator theme.
    pub file: String,
    pub builtin: bool,
    /// An operator theme that replaces the built-in with its id.
    pub replaces_builtin: bool,
    /// The files served, by path relative to the folder; empty for a broken theme.
    files: BTreeMap<String, ThemeFile>,
}

/// Every theme the node knows, sorted by id.
#[derive(Clone, Debug, Default)]
pub struct ThemeRegistry {
    themes: Vec<LoadedTheme>,
}

impl ThemeRegistry {
    /// Load the built-in themes, then each folder of `operator_dir` (by name; a missing
    /// or unreadable directory reads as empty). A ready operator theme replaces the theme
    /// with its id; a broken one is listed beside it (see [`LoadedTheme::id`]). Never
    /// fails.
    pub fn load(operator_dir: Option<&Path>) -> ThemeRegistry {
        let mut reg = ThemeRegistry::default();
        let mut builtin: BTreeMap<&str, BTreeMap<String, Vec<u8>>> = BTreeMap::new();
        for (path, bytes) in BUILTIN {
            if let Some((id, rest)) = path.split_once('/') {
                builtin
                    .entry(id)
                    .or_default()
                    .insert(rest.to_string(), bytes.to_vec());
            }
        }
        for (id, files) in builtin {
            reg.add(id, format!("themes/{id}"), true, Ok(files));
        }
        let mut dirs: Vec<PathBuf> = operator_dir
            .and_then(|d| std::fs::read_dir(d).ok())
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            let id = dir
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            reg.add(&id, dir.display().to_string(), false, read_folder(&dir));
        }
        reg.themes.sort_by(|a, b| a.id.cmp(&b.id));
        reg
    }

    /// Parse and validate one folder's files: a ready theme replaces the theme with its id.
    fn add(
        &mut self,
        folder_id: &str,
        file: String,
        builtin: bool,
        files: Result<BTreeMap<String, Vec<u8>>, ThemeError>,
    ) {
        let (theme, files) = match files.and_then(|f| check_folder(folder_id, f)) {
            Ok(loaded) => loaded,
            Err(e) => return self.add_broken(folder_id, file, builtin, e),
        };
        let existing = self.themes.iter().position(|t| t.id == theme.id);
        let replaces_builtin = existing.is_some_and(|i| self.themes[i].builtin);
        let loaded = LoadedTheme {
            id: theme.id.clone(),
            theme: Some(theme),
            state: ThemeState::Ready,
            reason: None,
            file,
            builtin,
            replaces_builtin: !builtin && replaces_builtin,
            files,
        };
        match existing {
            Some(i) => self.themes[i] = loaded,
            None => self.themes.push(loaded),
        }
    }

    /// List a broken folder under its folder name, or under `<id>@<file>` when a theme
    /// already has that id (which stays in force).
    fn add_broken(&mut self, folder_id: &str, file: String, builtin: bool, e: ThemeError) {
        let id = match self.themes.iter().any(|t| t.id == folder_id) {
            true => format!("{folder_id}@{file}"),
            false => folder_id.to_string(),
        };
        self.themes.push(LoadedTheme {
            id,
            theme: None,
            state: ThemeState::Broken,
            reason: Some(e.to_string()),
            file,
            builtin,
            replaces_builtin: false,
            files: BTreeMap::new(),
        });
    }

    /// Every theme, ready and broken, sorted by id.
    pub fn all(&self) -> &[LoadedTheme] {
        &self.themes
    }

    /// The ready theme with id `id`.
    pub fn get(&self, id: &str) -> Option<&LoadedTheme> {
        self.themes
            .iter()
            .find(|t| t.id == id && t.state == ThemeState::Ready)
    }

    /// A ready theme's served file at `path` (relative to its folder): its content type
    /// and bytes. `theme.json` is the parsed theme written out again.
    pub fn file(&self, id: &str, path: &str) -> Option<(&'static str, Arc<[u8]>)> {
        let f = self.get(id)?.files.get(path)?;
        Some((f.mime, f.bytes.clone()))
    }

    /// One stderr line per broken theme: `zeta: theme <id> (<path>): <field>: <error>`.
    pub fn startup_lines(&self) -> Vec<String> {
        self.themes
            .iter()
            .filter(|t| t.state == ThemeState::Broken)
            .map(|t| {
                let id = t.id.split('@').next().unwrap_or_default();
                let reason = t.reason.as_deref().unwrap_or_default();
                format!("zeta: theme {id} ({}): {reason}", t.file)
            })
            .collect()
    }
}

/// Parse and validate a folder's files into its theme and the files it serves.
fn check_folder(
    folder_id: &str,
    mut files: BTreeMap<String, Vec<u8>>,
) -> Result<(ThemeJson, BTreeMap<String, ThemeFile>), ThemeError> {
    let raw = files
        .remove("theme.json")
        .ok_or_else(|| ThemeError::new("theme.json", "missing"))?;
    let text = String::from_utf8(raw).map_err(|e| ThemeError::new("theme.json", e.to_string()))?;
    let theme = parse(&text)?;
    let json = serde_json::to_vec_pretty(&theme)
        .map_err(|e| ThemeError::new("theme.json", e.to_string()))?;
    files.insert("theme.json".into(), json);
    let mut served: BTreeMap<String, ThemeFile> = files
        .into_iter()
        .filter_map(|(path, bytes)| {
            let mime = mime(&path)?;
            let bytes = bytes.into();
            Some((path, ThemeFile { mime, bytes }))
        })
        .collect();
    let names: BTreeSet<String> = served.keys().cloned().collect();
    validate(&theme, folder_id, &names)?;
    // Task 2 wraps style.css; until then it is served as written.
    served.entry("style.css".into()).or_insert(ThemeFile {
        mime: "text/css; charset=utf-8",
        bytes: Arc::from(&b""[..]),
    });
    Ok((theme, served))
}

/// Every file under `dir`, by path relative to it with `/` between segments. Each
/// segment must be `[A-Za-z0-9._ -]+`; a symlink is followed only to a file inside the
/// folder. A refusal names `files`.
fn read_folder(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>, ThemeError> {
    let err = |e: String| ThemeError::new("files", e);
    let root = dir
        .canonicalize()
        .map_err(|e| err(format!("{}: {e}", dir.display())))?;
    let mut out = BTreeMap::new();
    let mut stack = vec![(root.clone(), String::new())];
    while let Some((at, prefix)) = stack.pop() {
        let entries = std::fs::read_dir(&at).map_err(|e| err(format!("{prefix}: {e}")))?;
        for entry in entries {
            let entry = entry.map_err(|e| err(format!("{prefix}: {e}")))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let rel = match prefix.is_empty() {
                true => name.to_string(),
                false => format!("{prefix}/{name}"),
            };
            if !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ' ' | '-'))
            {
                return Err(err(format!(
                    "{rel:?}: a name may hold only A-Z, a-z, 0-9, `.`, `_`, ` ` and `-`"
                )));
            }
            let path = entry.path();
            let kind = entry.file_type().map_err(|e| err(format!("{rel}: {e}")))?;
            if kind.is_dir() {
                stack.push((path, rel));
                continue;
            }
            if kind.is_symlink() {
                let target = path
                    .canonicalize()
                    .map_err(|e| err(format!("{rel}: {e}")))?;
                if !target.starts_with(&root) {
                    return Err(err(format!("{rel}: links outside the theme folder")));
                }
                if !target.is_file() {
                    return Err(err(format!("{rel}: a link must name a file")));
                }
            }
            let bytes = std::fs::read(&path).map_err(|e| err(format!("{rel}: {e}")))?;
            out.insert(rel, bytes);
        }
    }
    Ok(out)
}
