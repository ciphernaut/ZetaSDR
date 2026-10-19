//! The web app's static files under `/ui/`, served without auth, and `/` redirecting there.

use axum::Router;
use axum::extract::Path;
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, LOCATION};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::http::{Shared, error_response, other};

pub(crate) fn router() -> Router<Shared> {
    Router::new()
        .route("/", get(root))
        .route("/ui/", get(index))
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

fn serve(path: &str) -> Response {
    match zeta_ui::asset(path) {
        Some((mime, bytes)) => {
            ([(CONTENT_TYPE, mime), (CACHE_CONTROL, "no-cache")], bytes).into_response()
        }
        None => error_response(
            StatusCode::NOT_FOUND,
            other(format!("no such ui file: {path}")),
        ),
    }
}
