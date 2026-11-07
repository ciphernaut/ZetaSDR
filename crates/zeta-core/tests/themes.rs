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
    assert_eq!(
        ids,
        [
            "console",
            "cyberpunk",
            "plain",
            "psytrance",
            "terminal",
            "vibecoder"
        ]
    );
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
    let builtins = ThemeRegistry::load(None);
    let mut want: Vec<&str> = builtins.all().iter().map(|t| t.id.as_str()).collect();
    want.push("mytheme");
    want.sort_unstable();
    assert_eq!(ids, want);
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
    assert_eq!(reg.all().len(), ThemeRegistry::load(None).all().len());
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
fn editor_leftovers_with_odd_names_are_ignored() {
    let root = tempfile::tempdir().unwrap();
    theme_dir(
        root.path(),
        "leftovers",
        valid("leftovers"),
        &[
            ("style.css", b"a{}".as_slice()),
            ("style.css~", b"a{}".as_slice()),
            ("#x#", b"x".as_slice()),
        ],
    );
    let reg = ThemeRegistry::load(Some(root.path()));
    let t = reg.all().iter().find(|t| t.id == "leftovers").unwrap();
    assert_eq!(t.state, ThemeState::Ready, "{:?}", t.reason);
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
        source_positions: Default::default(),
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
    for id in [
        "console",
        "cyberpunk",
        "plain",
        "psytrance",
        "terminal",
        "vibecoder",
    ] {
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

/// The `--name:` declarations of the core stylesheet's `:root` block.
fn root_custom_properties() -> Vec<String> {
    let css = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../zeta-ui/web/style.css"
    ))
    .expect("read the core stylesheet");
    let start = css.find(":root {").expect(":root block");
    let block = &css[start..start + css[start..].find("\n}").expect(":root end")];
    block
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix("--"))
        .filter_map(|l| l.split_once(':'))
        .map(|(name, _)| format!("--{name}"))
        .collect()
}

#[test]
fn tokens_match_the_core_stylesheet() {
    let declared = root_custom_properties();
    assert!(declared.len() > 40, "{declared:?}");
    for name in &declared {
        assert!(
            name == "--ribbon-tilt" || zeta_core::themes::TOKENS.contains(&name.as_str()),
            "{name} is declared in :root but is not in TOKENS"
        );
    }
    for token in zeta_core::themes::TOKENS {
        assert!(
            declared.iter().any(|d| d == token),
            "{token} is in TOKENS but :root does not declare it"
        );
    }
}

#[test]
fn docs_list_every_token() {
    let docs =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/themes.md"))
            .expect("read docs/themes.md");
    for token in zeta_core::themes::TOKENS {
        assert!(
            docs.contains(&format!("`{token}`")),
            "docs/themes.md omits {token}"
        );
    }
}

/// The built-in themes held to WCAG AA by `builtin_themes_meet_aa`.
const AA_THEMES: &[&str] = &[
    "console",
    "plain",
    "cyberpunk",
    "terminal",
    "psytrance",
    "vibecoder",
];

/// WCAG 2 relative luminance of a `#rrggbb` colour.
fn luminance(hex: &str) -> f64 {
    let h = hex.strip_prefix('#').filter(|h| h.len() == 6);
    let h = h.unwrap_or_else(|| panic!("{hex} is not #rrggbb"));
    let channel = |i: usize| {
        let c = f64::from(u8::from_str_radix(&h[i..i + 2], 16).unwrap()) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(0) + 0.7152 * channel(2) + 0.0722 * channel(4)
}

/// The WCAG contrast ratio of two `#rrggbb` colours.
fn contrast(a: &str, b: &str) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[test]
fn builtin_themes_meet_aa() {
    let mut failures = Vec::new();
    for id in AA_THEMES {
        let path = format!("{id}/theme.json");
        let bytes = BUILTIN
            .iter()
            .find(|(k, _)| *k == path)
            .unwrap_or_else(|| panic!("{path} is not in BUILTIN"))
            .1;
        let json: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        let token = |name: &str| {
            json["tokens"][name]
                .as_str()
                .unwrap_or_else(|| panic!("{id}: {name} is not set"))
                .to_string()
        };
        let (bg, bar, text, dim) = (
            token("--bg"),
            token("--bar"),
            token("--text"),
            token("--dim"),
        );
        for (what, fg, ground, floor) in [
            ("--text on --bg", &text, &bg, 4.5),
            ("--text on --bar", &text, &bar, 4.5),
            ("--dim on --bg", &dim, &bg, 3.0),
        ] {
            let ratio = contrast(fg, ground);
            if ratio < floor {
                failures.push(format!("{id}: {what} is {ratio:.2}, under {floor}"));
            }
        }
        if json["color_scheme"] == "light" {
            light_theme_checks(id, &token, &mut failures);
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A light theme keeps a dark waterfall, so it is held to more: `--wf-bg` stays dark; the
/// labels and ribbons over the waterfall (`--text` on `--overlay-bg`, the overlay composited
/// over `--wf-bg`, the darkest ground it sits on) and the axis labels over the light page
/// (`--dim`, the core `.wf-axis` colour, on `--bg`) read as body text, 4.5:1.
fn light_theme_checks(id: &str, token: &dyn Fn(&str) -> String, failures: &mut Vec<String>) {
    let (bg, text, dim, wf_bg) = (
        token("--bg"),
        token("--text"),
        token("--dim"),
        token("--wf-bg"),
    );
    if luminance(&wf_bg) >= 0.05 {
        failures.push(format!("{id}: --wf-bg {wf_bg} is not a dark ground"));
    }
    let overlay = over(&token("--overlay-bg"), &wf_bg);
    for (what, fg, ground) in [
        ("--text on --overlay-bg (over --wf-bg)", &text, &overlay),
        ("axis labels (--dim) on --bg", &dim, &bg),
    ] {
        let ratio = contrast(fg, ground);
        if ratio < 4.5 {
            failures.push(format!("{id}: {what} is {ratio:.2}, under 4.5"));
        }
    }
}

/// `colour` (`#rrggbb` or `rgba(r, g, b, a)`) composited over the `#rrggbb` `ground`, as
/// `#rrggbb`.
fn over(colour: &str, ground: &str) -> String {
    let rgb = |hex: &str| {
        let h = hex.strip_prefix('#').filter(|h| h.len() == 6);
        let h = h.unwrap_or_else(|| panic!("{hex} is not #rrggbb"));
        [0, 2, 4].map(|i| f64::from(u8::from_str_radix(&h[i..i + 2], 16).unwrap()))
    };
    let (top, alpha) = if let Some(args) = colour
        .strip_prefix("rgba(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let v: Vec<f64> = args
            .split(',')
            .map(|p| p.trim().parse().unwrap_or_else(|_| panic!("{colour}")))
            .collect();
        assert_eq!(v.len(), 4, "{colour} is not rgba(r, g, b, a)");
        ([v[0], v[1], v[2]], v[3])
    } else {
        (rgb(colour), 1.0)
    };
    let base = rgb(ground);
    let c = [0, 1, 2].map(|i| (top[i] * alpha + base[i] * (1.0 - alpha)).round() as u8);
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// The colour of a theme's ramp at `t`, interpolated linearly between its stops.
fn ramp_at(ramp: &[serde_json::Value], t: f64) -> [f64; 3] {
    let stop = |v: &serde_json::Value| {
        let rgb = &v[1];
        (
            v[0].as_f64().unwrap(),
            [0, 1, 2].map(|i| rgb[i].as_f64().unwrap()),
        )
    };
    for pair in ramp.windows(2) {
        let ((t0, c0), (t1, c1)) = (stop(&pair[0]), stop(&pair[1]));
        if t <= t1 {
            let f = (t - t0) / (t1 - t0);
            return [0, 1, 2].map(|i| c0[i] + (c1[i] - c0[i]) * f);
        }
    }
    stop(ramp.last().unwrap()).1
}

/// Psytrance's rainbow ramp keeps weak signals readable: its first 20 % stays under 25 %
/// relative luminance, and its low end is distinct from the waterfall's ground.
#[test]
fn psytrance_ramp_low_end_stays_dark() {
    let bytes = BUILTIN
        .iter()
        .find(|(k, _)| *k == "psytrance/theme.json")
        .expect("psytrance/theme.json is not in BUILTIN")
        .1;
    let json: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    let ramp = json["ramp"].as_array().unwrap();
    let hex = |c: [f64; 3]| {
        format!(
            "#{:02x}{:02x}{:02x}",
            c[0].round() as u8,
            c[1].round() as u8,
            c[2].round() as u8
        )
    };
    for step in 0..=20 {
        let t = f64::from(step) / 100.0;
        let lum = luminance(&hex(ramp_at(ramp, t)));
        assert!(
            lum < 0.25,
            "psytrance ramp at {t}: luminance {lum:.3}, not under 0.25"
        );
    }
    let floor = json["tokens"]["--wf-bg"].as_str().unwrap();
    let low = hex(ramp_at(ramp, 0.1));
    assert!(
        contrast(&low, floor) >= 1.5,
        "psytrance ramp at 0.1 ({low}) is not distinct from --wf-bg {floor}"
    );
}
