//! `GET /captures/{file}`: a stored capture's SigMF pair or a packet capture, for a remote caller.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
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
    if ext == "pcapng" {
        return packet_capture(&st, id, not_found()).await;
    }
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

/// `<id>.pcapng`: a packet capture from the packet store (`application/vnd.tcpdump.pcap`),
/// cut to its last whole block, so a reader racing the writer never gets half a block.
/// The cut is a linear scan of block headers, cheap beside the read. A capture of a
/// restricted row is answered as an unknown id (404, not 403), so the route says nothing
/// of it until access control lands.
async fn packet_capture(st: &Shared, id: &str, not_found: Response) -> Response {
    let Some(rec) = st
        .node
        .packets()
        .get(id)
        .filter(|r| r.access != Some(zeta_core::modes::Access::Restricted))
    else {
        return not_found;
    };
    let Ok(mut bytes) = tokio::fs::read(&rec.path).await else {
        return not_found;
    };
    bytes.truncate(zeta_core::pcapng::whole_blocks(&bytes));
    (
        [
            (CONTENT_TYPE, "application/vnd.tcpdump.pcap".to_string()),
            (
                CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}.pcapng\"", rec.id),
            ),
        ],
        bytes,
    )
        .into_response()
}
