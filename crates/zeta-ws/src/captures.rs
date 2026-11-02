//! `GET /captures/{file}`: a stored capture's SigMF pair, for a remote caller.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use zeta_core::ops::OpError;

use crate::http::{Shared, error_response};

/// `<id>.sigmf-meta` (`application/json`) or `<id>.sigmf-data`
/// (`application/octet-stream`), read from the paths of the store's own record: the
/// request names an id, never a path. An unknown id or any other name is 404.
pub(crate) async fn file(State(st): State<Shared>, Path(file): Path<String>) -> Response {
    let not_found = || {
        error_response(
            StatusCode::NOT_FOUND,
            OpError::Other {
                message: format!("no capture file {file}"),
            },
        )
    };
    let Some((id, ext)) = file.rsplit_once('.') else {
        return not_found();
    };
    let Some(rec) = st.node.bursts().get(id) else {
        return not_found();
    };
    let (path, ctype) = match ext {
        "sigmf-meta" => (rec.meta_path, "application/json"),
        "sigmf-data" => (rec.data_path, "application/octet-stream"),
        _ => return not_found(),
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => ([(CONTENT_TYPE, ctype)], bytes).into_response(),
        Err(_) => not_found(),
    }
}
