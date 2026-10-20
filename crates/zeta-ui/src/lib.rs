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
        ] {
            let (m, bytes) = asset(path).unwrap_or_else(|| panic!("{path} embedded"));
            assert_eq!(m, mime, "{path}");
            assert!(!bytes.is_empty(), "{path}");
        }
        let on_disk = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/web"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| !n.starts_with('.'));
        for name in on_disk {
            assert!(paths().any(|p| p == name), "web/{name} is not in ASSETS");
        }
    }
}
