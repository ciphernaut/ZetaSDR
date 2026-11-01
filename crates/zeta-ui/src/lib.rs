//! The ZetaSDR web app's static files, embedded in the binary.

/// Every embedded file: path under `/ui/`, MIME type, bytes.
const ASSETS: &[(&str, &str, &[u8])] = &[
    (
        "index.html",
        "text/html; charset=utf-8",
        include_bytes!("../web/index.html"),
    ),
    (
        "style.css",
        "text/css; charset=utf-8",
        include_bytes!("../web/style.css"),
    ),
    (
        "app.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../web/app.js"),
    ),
    (
        "api.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../web/api.js"),
    ),
    (
        "agent.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../web/agent.js"),
    ),
    (
        "waterfall.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../web/waterfall.js"),
    ),
    (
        "overlays.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../web/overlays.js"),
    ),
    (
        "menu.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../web/menu.js"),
    ),
    (
        "audio.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../web/audio.js"),
    ),
    (
        "graph.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../web/graph.js"),
    ),
    (
        "voice.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../web/voice.js"),
    ),
    (
        "view.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../web/view.js"),
    ),
    (
        "coarse.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../web/coarse.js"),
    ),
    (
        "plans.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../web/plans.js"),
    ),
    (
        "mix.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../web/mix.js"),
    ),
    (
        "map.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../web/map.js"),
    ),
    (
        "units.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../web/units.js"),
    ),
    // Leaflet 1.9.4 (BSD-2-Clause), verbatim from its release; map.js loads it on first use.
    (
        "vendor/leaflet/leaflet.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../web/vendor/leaflet/leaflet.js"),
    ),
    (
        "vendor/leaflet/leaflet.css",
        "text/css; charset=utf-8",
        include_bytes!("../web/vendor/leaflet/leaflet.css"),
    ),
    (
        "vendor/leaflet/LICENSE",
        "text/plain; charset=utf-8",
        include_bytes!("../web/vendor/leaflet/LICENSE"),
    ),
];

/// The MIME type and bytes of the file at `path`; `""` is `index.html`.
pub fn asset(path: &str) -> Option<(&'static str, &'static [u8])> {
    let path = if path.is_empty() { "index.html" } else { path };
    ASSETS
        .iter()
        .find(|(p, _, _)| *p == path)
        .map(|(_, mime, bytes)| (*mime, *bytes))
}

/// The path of every embedded file.
pub fn paths() -> impl Iterator<Item = &'static str> {
    ASSETS.iter().map(|(p, _, _)| *p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_is_embedded_and_paths_lists_it() {
        let (mime, bytes) = asset("index.html").expect("index embedded");
        assert_eq!(mime, "text/html; charset=utf-8");
        assert!(std::str::from_utf8(bytes).unwrap().contains("ZetaSDR"));
        assert_eq!(asset(""), Some((mime, bytes)));
        assert_eq!(asset("nope"), None);
        assert!(paths().any(|p| p == "index.html"));
    }

    #[test]
    fn every_web_file_is_embedded_with_its_mime() {
        for (path, mime) in [
            ("style.css", "text/css; charset=utf-8"),
            ("app.js", "text/javascript; charset=utf-8"),
            ("api.js", "text/javascript; charset=utf-8"),
            ("agent.js", "text/javascript; charset=utf-8"),
            ("waterfall.js", "text/javascript; charset=utf-8"),
            ("overlays.js", "text/javascript; charset=utf-8"),
            ("menu.js", "text/javascript; charset=utf-8"),
            ("audio.js", "text/javascript; charset=utf-8"),
            ("graph.js", "text/javascript; charset=utf-8"),
            ("voice.js", "text/javascript; charset=utf-8"),
            ("view.js", "text/javascript; charset=utf-8"),
            ("coarse.js", "text/javascript; charset=utf-8"),
            ("plans.js", "text/javascript; charset=utf-8"),
            ("map.js", "text/javascript; charset=utf-8"),
            ("mix.js", "text/javascript; charset=utf-8"),
            ("units.js", "text/javascript; charset=utf-8"),
            (
                "vendor/leaflet/leaflet.js",
                "text/javascript; charset=utf-8",
            ),
            ("vendor/leaflet/leaflet.css", "text/css; charset=utf-8"),
            ("vendor/leaflet/LICENSE", "text/plain; charset=utf-8"),
        ] {
            let (m, bytes) = asset(path).unwrap_or_else(|| panic!("{path} embedded"));
            assert_eq!(m, mime, "{path}");
            assert!(!bytes.is_empty(), "{path}");
        }
        for name in files_under(std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/web"
        ))) {
            assert!(paths().any(|p| p == name), "web/{name} is not in ASSETS");
        }
    }

    /// Every file under `dir` (dot files left out), as a `/`-separated path relative to it.
    fn files_under(dir: &std::path::Path) -> Vec<String> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().into_string().unwrap();
            if name.starts_with('.') {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                out.extend(
                    files_under(&e.path())
                        .into_iter()
                        .map(|f| format!("{name}/{f}")),
                );
            } else {
                out.push(name);
            }
        }
        out
    }
}
