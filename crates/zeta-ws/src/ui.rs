//! The web app's static files under `/ui/`, served without auth, and `/` redirecting there.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, LOCATION, X_CONTENT_TYPE_OPTIONS,
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

/// The app loads only its own files; the microphone tap is an AudioWorklet module from a
/// `blob:` URL; websockets go back to the node; the map's tiles are images from
/// OpenStreetMap's tile server, the one outside origin. Styles are set through the CSSOM
/// only, but `'unsafe-inline'` is kept for style attributes a future change may add.
const CSP: &str = "default-src 'self'; script-src 'self' blob:; worker-src 'self' blob:; \
connect-src 'self' ws: wss:; img-src 'self' data: https://tile.openstreetmap.org; \
style-src 'self' 'unsafe-inline'; frame-ancestors 'none'";

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
            (CONTENT_SECURITY_POLICY, CSP),
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
