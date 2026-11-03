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

#[test]
fn style_css_is_served_wrapped() {
    let root = tempfile::tempdir().unwrap();
    theme_dir(
        root.path(),
        "mytheme",
        valid("mytheme"),
        &[
            ("style.css", b".a { background: url(bg.png); }\n"),
            ("bg.png", b"png"),
        ],
    );
    let reg = ThemeRegistry::load(Some(root.path()));
    let (mime, bytes) = reg.file("mytheme", "style.css").expect("style.css served");
    assert_eq!(mime, "text/css; charset=utf-8");
    let css = std::str::from_utf8(&bytes).unwrap();
    let open = css.find("[data-theme=\"mytheme\"] {").expect(css);
    assert!(css.find("--accent: #ff00ff;").unwrap() > open, "{css}");
    assert!(
        css.find(".a { background: url(bg.png); }").unwrap() > open,
        "{css}"
    );
}

#[test]
fn a_theme_without_style_css_serves_the_wrapper() {
    let root = tempfile::tempdir().unwrap();
    theme_dir(root.path(), "mytheme", valid("mytheme"), &[]);
    let reg = ThemeRegistry::load(Some(root.path()));
    let (_, bytes) = reg.file("mytheme", "style.css").expect("style.css served");
    let css = std::str::from_utf8(&bytes).unwrap();
    assert!(css.contains("[data-theme=\"mytheme\"] {"), "{css}");
    assert!(css.contains("color-scheme: dark;"), "{css}");
    let (_, console) = reg.file("console", "style.css").unwrap();
    let console = std::str::from_utf8(&console).unwrap();
    assert!(console.contains("font-family: \"Antonio\""), "{console}");
}

#[test]
fn a_bad_style_css_breaks_the_theme() {
    let root = tempfile::tempdir().unwrap();
    theme_dir(
        root.path(),
        "mytheme",
        valid("mytheme"),
        &[("style.css", b"@import \"x.css\";")],
    );
    let big = vec![b' '; 1_048_577];
    theme_dir(root.path(), "big", valid("big"), &[("style.css", &big)]);
    let reg = ThemeRegistry::load(Some(root.path()));
    assert!(reg.get("mytheme").is_none());
    let t = reg.all().iter().find(|t| t.id == "mytheme").unwrap();
    assert_eq!(t.state, ThemeState::Broken);
    assert_eq!(
        t.reason.as_deref(),
        Some("style.css: @import is not allowed")
    );
    let t = reg.all().iter().find(|t| t.id == "big").unwrap();
    assert_eq!(
        t.reason.as_deref(),
        Some("style.css: 1048577 bytes, more than the 1 MiB limit")
    );
}

/// `themes_list` answers the built-ins, the operator's theme and a broken one with its
/// reason, from the registry `set_themes` gave the node.
#[tokio::test]
async fn themes_list_answers_every_theme() {
    use std::sync::Arc;
    use zeta_core::node::{Node, NodeConfig};
    use zeta_core::ops::{Op, OpResult};
    use zeta_core::stage::{Backend, StageRegistry};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("themes");
    theme_dir(&root, "mine", valid("mine"), &[]);
    let bad = root.join("broken");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("theme.json"), r#"{"id":"broken","bogus":1}"#).unwrap();
    let cfg = NodeConfig {
        burst_policy: Default::default(),
        state_dir: dir.path().join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: 64 << 20,
        scan_dwell_ms: 20,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory: None,
        modes_dir: None,
    };
    let node = Node::open(cfg, StageRegistry::new(), vec![]).await.unwrap();
    node.set_themes(Arc::new(ThemeRegistry::load(Some(&root))));
    let OpResult::Themes { themes } = node.apply_from(Op::ThemesList {}, "local").await.unwrap()
    else {
        panic!("not themes");
    };
    let row = |id: &str| {
        themes
            .iter()
            .find(|t| t.id == id)
            .unwrap_or_else(|| panic!("{id}"))
    };
    for id in ["console", "plain"] {
        let t = row(id);
        assert_eq!(t.state, ThemeState::Ready);
        assert!(t.builtin && !t.replaces_builtin && t.reason.is_none());
        assert!(t.color_scheme.is_some());
    }
    let mine = row("mine");
    assert_eq!(mine.name, "My theme");
    assert_eq!(mine.state, ThemeState::Ready);
    assert!(!mine.builtin);
    let broken = row("broken");
    assert_eq!(broken.state, ThemeState::Broken);
    assert_eq!(broken.name, "broken");
    assert_eq!(broken.description, "");
    assert!(broken.color_scheme.is_none());
    assert!(broken.reason.as_deref().unwrap().contains("bogus"));
    assert!(themes.windows(2).all(|w| w[0].id <= w[1].id));
}
