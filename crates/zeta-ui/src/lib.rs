//! The ZetaSDR web app's static files, embedded in the binary.

/// Every embedded file: path under `/ui/`, MIME type, bytes.
const ASSETS: &[(&str, &str, &[u8])] = &[(
    "index.html",
    "text/html; charset=utf-8",
    include_bytes!("../web/index.html"),
)];

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
}
