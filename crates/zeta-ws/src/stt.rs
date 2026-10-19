//! `POST /stt`: hand a WAV body to the `local-voice` daemon and return the transcript.
//!
//! The daemon (`voiced`) speaks newline-delimited text on a Unix socket: `file:<path>` is
//! answered by one line, `ok:<text>` or `error:<message>`.

use std::path::{Path, PathBuf};

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::http::{Shared, error_response, other};

/// Removes the staged WAV file when dropped, on every path out of the handler.
struct StagedFile(PathBuf);

impl Drop for StagedFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn is_wav_type(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .is_some_and(|t| t == "audio/wav" || t == "audio/x-wav")
}

pub(crate) async fn stt(State(st): State<Shared>, headers: HeaderMap, body: Body) -> Response {
    if !is_wav_type(&headers) {
        return error_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            other("Content-Type must be audio/wav or audio/x-wav"),
        );
    }
    let Some(socket) = st.stt_socket.clone() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            other("serve has no speech daemon (--stt-socket)"),
        );
    };
    let limit = st.limits.stt_max_bytes;
    let Ok(bytes) = axum::body::to_bytes(body, limit).await else {
        return error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            other(format!("body larger than {limit} bytes")),
        );
    };
    if !bytes.starts_with(b"RIFF") {
        return error_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            other("body is not a WAV file (no RIFF header)"),
        );
    }
    let dir = st.state_dir.join("stt");
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            other(format!("creating {}: {e}", dir.display())),
        );
    }
    let staged = StagedFile(dir.join(format!("{}.wav", ulid::Ulid::generate())));
    if let Err(e) = tokio::fs::write(&staged.0, &bytes).await {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            other(format!("writing {}: {e}", staged.0.display())),
        );
    }
    let timeout = st.limits.stt_timeout;
    match tokio::time::timeout(timeout, transcribe(&socket, &staged.0)).await {
        Ok(response) => response,
        Err(_) => error_response(
            StatusCode::GATEWAY_TIMEOUT,
            other(format!(
                "the speech daemon did not answer within {} s",
                timeout.as_secs()
            )),
        ),
    }
}

/// Send `file:<wav>` to the daemon at `socket` and turn its one-line answer into a response.
async fn transcribe(socket: &Path, wav: &Path) -> Response {
    let mut conn = match UnixStream::connect(socket).await {
        Ok(c) => c,
        Err(e) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                other(format!("speech daemon at {}: {e}", socket.display())),
            );
        }
    };
    let bad_gateway = |msg: String| error_response(StatusCode::BAD_GATEWAY, other(msg));
    let command = format!("file:{}\n", wav.display());
    if let Err(e) = conn.write_all(command.as_bytes()).await {
        return bad_gateway(format!("speech daemon: sending the command: {e}"));
    }
    let mut line = String::new();
    match BufReader::new(conn).read_line(&mut line).await {
        Ok(0) => return bad_gateway("speech daemon: closed the connection".into()),
        Ok(_) => {}
        Err(e) => return bad_gateway(format!("speech daemon: reading the answer: {e}")),
    }
    let line = line.trim_end_matches(['\n', '\r']);
    if let Some(text) = line.strip_prefix("ok:") {
        (StatusCode::OK, Json(json!({ "text": text }))).into_response()
    } else if let Some(msg) = line.strip_prefix("error:") {
        bad_gateway(format!("speech daemon: {msg}"))
    } else {
        bad_gateway(format!("speech daemon: unexpected answer {line:?}"))
    }
}
