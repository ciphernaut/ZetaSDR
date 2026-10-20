//! `POST /stt?lang=`: hand a WAV body to the node's speech backend and return the transcript;
//! `GET /stt/languages`: the backend and the languages it accepts.

use std::collections::HashMap;

use axum::Json;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use zeta_client::STT_HOP_HEADER;
use zeta_speech::{DEFAULT_LANGUAGE, SpeechError};

use crate::http::{Shared, error_response, other};

const NO_BACKEND: &str = "serve has no speech backend (--stt-socket or --stt-node)";

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

/// Whether another node already forwarded this request.
fn is_hop(headers: &HeaderMap) -> bool {
    headers
        .get(STT_HOP_HEADER)
        .is_some_and(|v| v.as_bytes() == b"1")
}

pub(crate) async fn stt(
    State(st): State<Shared>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if !is_wav_type(&headers) {
        return error_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            other("Content-Type must be audio/wav or audio/x-wav"),
        );
    }
    let Some(speech) = st.speech.clone() else {
        return error_response(StatusCode::SERVICE_UNAVAILABLE, other(NO_BACKEND));
    };
    if speech.forwards() && is_hop(&headers) {
        return error_response(StatusCode::LOOP_DETECTED, other("stt forwarding loop"));
    }
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
    let language = query
        .get("lang")
        .map(String::as_str)
        .filter(|l| !l.is_empty())
        .unwrap_or(DEFAULT_LANGUAGE);
    // The backend bounds its own wait; this bounds a backend that does not.
    let timeout = st.limits.stt_timeout;
    let result = tokio::time::timeout(timeout, speech.transcribe(&bytes, language))
        .await
        .unwrap_or(Err(SpeechError::Timeout(timeout)));
    match result {
        Ok(t) => (StatusCode::OK, Json(json!({ "text": t.text }))).into_response(),
        Err(e) => speech_error(e),
    }
}

/// A backend's failure as the answer: 422 for a language it lacks, 503 when it cannot be
/// reached, 502 when it reports an error, 504 when it does not answer in time.
fn speech_error(e: SpeechError) -> Response {
    let status = match &e {
        SpeechError::Unsupported {
            language,
            supported,
        } => {
            let body =
                json!({"error": "unsupported", "language": language, "supported": supported});
            return (StatusCode::UNPROCESSABLE_ENTITY, Json(body)).into_response();
        }
        SpeechError::Unreachable(_) => StatusCode::SERVICE_UNAVAILABLE,
        SpeechError::Daemon(_) => StatusCode::BAD_GATEWAY,
        SpeechError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
    };
    error_response(status, other(e.to_string()))
}

pub(crate) async fn languages(State(st): State<Shared>) -> Response {
    match &st.speech {
        Some(speech) => Json(json!({
            "backend": speech.name(),
            "languages": speech.languages(),
        }))
        .into_response(),
        None => error_response(StatusCode::SERVICE_UNAVAILABLE, other(NO_BACKEND)),
    }
}
