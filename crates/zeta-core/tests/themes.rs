//! The theme registry: the built-in folders under `themes/`, the operator's folder
//! layered over them, broken folders and the files a ready theme serves.

use std::path::{Path, PathBuf};

use serde_json::json;
use zeta_core::themes::{BUILTIN, ThemeRegistry, ThemeState};

/// A valid `theme.json` for `id`.
fn valid(id: &str) -> serde_json::Value {
    json!({
        "id": id,
        "name": "My theme",
        "description": "A test theme.",
        "color_scheme": "dark",
        "layout": { "rail": false, "ribbon_tilt_deg": 0 },
        "tokens": { "--accent": "#ff00ff" },
        "ramp": [[0, [0, 0, 0]], [1, [255, 0, 255]]]
    })
}

/// Write `root/<id>/theme.json` from `json` and each of `files` beside it; the folder.
fn theme_dir(root: &Path, id: &str, json: serde_json::Value, files: &[(&str, &[u8])]) -> PathBuf {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("theme.json"), json.to_string()).unwrap();
    for (name, bytes) in files {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    dir
}

#[test]
fn builtins_load_ready() {
    let reg = ThemeRegistry::load(None);
    let ids: Vec<&str> = reg.all().iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["console", "plain"]);
    for t in reg.all() {
        assert_eq!(t.state, ThemeState::Ready, "{}: {:?}", t.id, t.reason);
        assert!(t.builtin, "{}", t.id);
        assert!(!t.replaces_builtin, "{}", t.id);
        assert_eq!(t.file, format!("themes/{}", t.id));
    }
    assert!(reg.startup_lines().is_empty());
}

#[test]
fn an_operator_theme_is_added() {
    let root = tempfile::tempdir().unwrap();
    let dir = theme_dir(root.path(), "mytheme", valid("mytheme"), &[]);
    let reg = ThemeRegistry::load(Some(root.path()));
    let t = reg.get("mytheme").expect("mytheme loads");
    assert_eq!(t.state, ThemeState::Ready, "{:?}", t.reason);
    assert!(!t.builtin);
    assert!(!t.replaces_builtin);
    assert_eq!(t.file, dir.display().to_string());
    let ids: Vec<&str> = reg.all().iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["console", "mytheme", "plain"]);
}

#[test]
fn an_operator_theme_replaces_a_builtin() {
    let root = tempfile::tempdir().unwrap();
    let mut json = valid("plain");
    json["name"] = "Mine".into();
    theme_dir(root.path(), "plain", json, &[]);
    let reg = ThemeRegistry::load(Some(root.path()));
    let t = reg.get("plain").unwrap();
    assert_eq!(t.theme.as_ref().unwrap().name, "Mine");
    assert!(t.replaces_builtin);
    assert!(!t.builtin);
    assert_eq!(reg.all().iter().filter(|t| t.id == "plain").count(), 1);
}

#[test]
fn a_broken_override_leaves_the_builtin_in_force() {
    let root = tempfile::tempdir().unwrap();
    let mut json = valid("console");
    json["tokens"]["--nope"] = "1px".into();
    let dir = theme_dir(root.path(), "console", json, &[]);
    let reg = ThemeRegistry::load(Some(root.path()));
    let t = reg.get("console").unwrap();
    assert!(t.builtin);
    assert_eq!(t.state, ThemeState::Ready);
    let broken_id = format!("console@{}", dir.display());
    let broken = reg
        .all()
        .iter()
        .find(|t| t.id == broken_id)
        .expect("the broken override is listed");
    assert_eq!(broken.state, ThemeState::Broken);
    assert!(
        broken
            .reason
            .as_deref()
            .unwrap()
            .starts_with("tokens.--nope: "),
        "{:?}",
        broken.reason
    );
    let lines = reg.startup_lines();
    assert!(lines[0].starts_with("zeta: theme console ("), "{lines:?}");
    assert!(lines[0].contains("tokens.--nope"), "{lines:?}");
}

#[test]
fn a_missing_operator_dir_is_not_an_error() {
    let root = tempfile::tempdir().unwrap();
    let reg = ThemeRegistry::load(Some(&root.path().join("absent")));
    assert_eq!(reg.all().len(), 2);
    assert!(reg.startup_lines().is_empty());
}

#[test]
fn a_symlink_out_of_the_folder_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("secret.png");
    std::fs::write(&target, b"png").unwrap();
    let dir = theme_dir(root.path(), "mytheme", valid("mytheme"), &[]);
    std::os::unix::fs::symlink(&target, dir.join("x.png")).unwrap();
    let reg = ThemeRegistry::load(Some(root.path()));
    assert!(reg.get("mytheme").is_none());
    let t = reg.all().iter().find(|t| t.id == "mytheme").unwrap();
    assert_eq!(t.state, ThemeState::Broken);
    assert!(
        t.reason.as_deref().unwrap().starts_with("files: "),
        "{:?}",
        t.reason
    );
}

#[test]
fn a_symlink_inside_the_folder_is_served() {
    let root = tempfile::tempdir().unwrap();
    let dir = theme_dir(
        root.path(),
        "mytheme",
        valid("mytheme"),
        &[("a.png", b"png")],
    );
    std::os::unix::fs::symlink(dir.join("a.png"), dir.join("b.png")).unwrap();
    let reg = ThemeRegistry::load(Some(root.path()));
    let (mime, bytes) = reg.file("mytheme", "b.png").expect("b.png served");
    assert_eq!(mime, "image/png");
    assert_eq!(&*bytes, b"png");
}

#[test]
fn file_serves_ready_themes_only() {
    let root = tempfile::tempdir().unwrap();
    let mut json = valid("broken");
    json["color_scheme"] = "dim".into();
    theme_dir(root.path(), "broken", json, &[("a.png", b"png")]);
    theme_dir(
        root.path(),
        "mytheme",
        valid("mytheme"),
        &[("img/bg.png", b"png"), ("notes.exe", b"x")],
    );
    let reg = ThemeRegistry::load(Some(root.path()));
    let (mime, bytes) = reg.file("console", "theme.json").unwrap();
    assert_eq!(mime, "application/json");
    let served: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(served["id"], "console");
    assert_eq!(
        reg.file("console", "style.css").unwrap().0,
        "text/css; charset=utf-8"
    );
    assert_eq!(reg.file("console", "Antonio-VF.ttf").unwrap().0, "font/ttf");
    assert_eq!(reg.file("mytheme", "img/bg.png").unwrap().0, "image/png");
    assert!(reg.file("mytheme", "notes.exe").is_none());
    assert!(reg.file("broken", "theme.json").is_none());
    assert!(reg.file("broken", "a.png").is_none());
    assert!(reg.file("console", "nope.css").is_none());
    assert!(reg.file("console", "../plain/theme.json").is_none());
    assert!(reg.file("nope", "theme.json").is_none());
}

#[test]
fn a_folder_without_theme_json_is_broken() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("empty")).unwrap();
    let reg = ThemeRegistry::load(Some(root.path()));
    let t = reg.all().iter().find(|t| t.id == "empty").unwrap();
    assert_eq!(t.state, ThemeState::Broken);
    assert!(
        t.reason.as_deref().unwrap().starts_with("theme.json: "),
        "{:?}",
        t.reason
    );
}

#[test]
fn every_file_under_themes_is_embedded() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../themes");
    let mut found = Vec::new();
    walk(&root, &root, &mut found);
    assert!(!found.is_empty());
    let keys: Vec<&str> = BUILTIN.iter().map(|(k, _)| *k).collect();
    for path in &found {
        assert!(keys.contains(&path.as_str()), "{path} is not in BUILTIN");
    }
    assert_eq!(
        found.len(),
        keys.len(),
        "BUILTIN names a file not under themes/"
    );
}

/// Every file under `dir`, as a path relative to `root`.
fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let path = e.unwrap().path();
        if path.is_dir() {
            walk(root, &path, out);
        } else {
            let rel = path.strip_prefix(root).unwrap();
            out.push(rel.to_string_lossy().into_owned());
        }
    }
}
