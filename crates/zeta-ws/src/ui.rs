//! The web app's static files under `/ui/`, served without auth, and `/` redirecting there.

use std::sync::LazyLock;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, LOCATION, REFERRER_POLICY,
    X_CONTENT_TYPE_OPTIONS,
};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::http::{Shared, error_response, other};

/// Outside `require_auth`, so also outside its LAN-peer check: the assets hold no secret and
/// the listener binds a LAN address only.
pub(crate) fn router() -> Router<Shared> {
    Router::new()
        .route("/", get(root))
        .route("/ui/", get(index))
        .route("/ui/themes/{id}/{*path}", get(theme))
        .route("/ui/{*path}", get(file))
}

async fn root() -> Response {
    (StatusCode::FOUND, [(LOCATION, "/ui/")]).into_response()
}

async fn index() -> Response {
    serve("")
}

async fn file(Path(path): Path<String>) -> Response {
    serve(&path)
}

/// The map's tile servers (map.js TILE_PROVIDERS; a test checks the two agree): the only
/// outside origins, for images only.
const TILE_HOSTS: [&str; 4] = [
    "https://tile.openstreetmap.org",
    "https://*.basemaps.cartocdn.com",
    "https://*.tile.opentopomap.org",
    "https://server.arcgisonline.com",
];

/// The app loads only its own files; the microphone tap is an AudioWorklet module from a
/// `blob:` URL; websockets go back to the node; the map's tiles are images from the
/// [`TILE_HOSTS`]. Styles are set through the CSSOM only, but `'unsafe-inline'` is kept for
/// style attributes a future change may add.
static CSP: LazyLock<String> = LazyLock::new(|| {
    format!(
        "default-src 'self'; script-src 'self' blob:; worker-src 'self' blob:; \
connect-src 'self' ws: wss:; img-src 'self' data: {}; \
style-src 'self' 'unsafe-inline'; frame-ancestors 'none'",
        TILE_HOSTS.join(" ")
    )
});

/// A tile request carries the page's origin (never its path) as its Referer: the tile hosts'
/// usage policies refuse requests without one. It is the browsers' default too; sent so a
/// browser set otherwise still gets tiles.
const REFERRER_POLICY_VALUE: &str = "strict-origin-when-cross-origin";

/// A ready theme's file. The registry is read per request: `serve` sets it after the router
/// is built. Lookup is exact, so a decoded `..` finds nothing.
async fn theme(State(st): State<Shared>, Path((id, path)): Path<(String, String)>) -> Response {
    match st.node.themes().file(&id, &path) {
        Some((mime, bytes)) => asset_response(mime, bytes.to_vec()),
        None => error_response(StatusCode::NOT_FOUND, other("no such theme file")),
    }
}

fn asset_response(mime: &'static str, bytes: impl Into<axum::body::Body>) -> Response {
    (
        [
            (CONTENT_TYPE, mime),
            (CACHE_CONTROL, "no-cache"),
            (CONTENT_SECURITY_POLICY, CSP.as_str()),
            (REFERRER_POLICY, REFERRER_POLICY_VALUE),
            (X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes.into(),
    )
        .into_response()
}

fn serve(path: &str) -> Response {
    match zeta_ui::asset(path) {
        Some((mime, bytes)) => asset_response(mime, bytes),
        None => error_response(StatusCode::NOT_FOUND, other("no such ui file")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// A JS asset's text.
    fn js(name: &str) -> &'static str {
        let (_, js) = zeta_ui::asset(name).unwrap_or_else(|| panic!("{name}"));
        std::str::from_utf8(js).unwrap()
    }

    /// map.js's TILE_PROVIDERS array, its text.
    fn tile_providers() -> &'static str {
        let js = js("map.js");
        let start = js
            .find("export const TILE_PROVIDERS = [")
            .expect("TILE_PROVIDERS in map.js");
        let end = start + js[start..].find("\n];").expect("TILE_PROVIDERS ends");
        &js[start..end]
    }

    /// The quoted values after each `key` (ending in `"`) in `text`.
    fn quoted_after<'a>(text: &'a str, key: &str) -> Vec<&'a str> {
        text.split(key)
            .skip(1)
            .map(|rest| &rest[..rest.find('"').unwrap()])
            .collect()
    }

    /// The hosts of map.js's TILE_PROVIDERS urls, a `{s}.` subdomain as `*.`.
    fn provider_hosts() -> BTreeSet<String> {
        quoted_after(tile_providers(), "url: \"")
            .into_iter()
            .map(|url| {
                let host = url
                    .strip_prefix("https://")
                    .unwrap_or_else(|| panic!("{url} is not https"));
                let host = &host[..host.find('/').unwrap()];
                format!("https://{}", host.replace("{s}.", "*."))
            })
            .collect()
    }

    #[test]
    fn the_csp_names_exactly_the_tile_providers_hosts() {
        let hosts: BTreeSet<String> = TILE_HOSTS.iter().map(|h| h.to_string()).collect();
        assert_eq!(provider_hosts(), hosts);
        assert_eq!(hosts.len(), TILE_HOSTS.len(), "TILE_HOSTS repeats a host");
        let img = format!("img-src 'self' data: {};", TILE_HOSTS.join(" "));
        assert!(CSP.contains(&img), "{}", &*CSP);
    }

    /// The tile ids agree: map.js TILE_PROVIDERS, view.js's `map_tiles` values (less
    /// `auto`) and zeta-core's `layout.map_tiles` ids, in the same order.
    #[test]
    fn the_tile_ids_agree_across_map_js_view_js_and_the_theme_schema() {
        let providers = quoted_after(tile_providers(), "id: \"");
        let view = js("view.js");
        let policy = &view[view.find("key: \"map_tiles\"").expect("map_tiles policy")..];
        let values = &policy[policy.find("values: [").unwrap()..];
        let values = &values[..values.find(']').unwrap()];
        let mut ids: Vec<&str> = values.split('"').skip(1).step_by(2).collect();
        assert_eq!(ids.first(), Some(&"auto"), "{ids:?}");
        ids.remove(0);
        assert_eq!(providers, ids);
        assert_eq!(providers, zeta_core::themes::MAP_TILES);
    }
}
