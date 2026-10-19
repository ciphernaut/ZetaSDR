//! Stream routes: the `/stream/{id}` websocket push loop and `/streams/{id}/tail`.

use std::time::Duration;

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::time::timeout;
use tokio_util::task::task_tracker::TaskTrackerToken;
use zeta_core::ops::{Op, OpError};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::StreamId;
use zeta_vrt::{IqMeta, VrtEncoder, stream_id_of};

use crate::http::{Shared, StatsHandle, error_response, other};
use crate::wire::{CREDIT_CAP, CREDIT_MAX_GRANT, WsMessage, envelope, ws_frame};

fn unknown_stream(bus: &StreamBus, id: &StreamId) -> OpError {
    let mut known: Vec<String> = bus.list().into_iter().map(|i| i.id.0).collect();
    known.sort();
    OpError::Unknown {
        field: "stream".to_string(),
        value: id.0.clone(),
        known,
    }
}

#[derive(Deserialize)]
pub(crate) struct TailQuery {
    max_frames: u32,
    timeout_ms: u32,
}

/// `GET /streams/{id}/tail?max_frames=N&timeout_ms=T`: applies `Op::StreamTail`, so limits,
/// errors and the body (the `frames` result, as `POST /op` returns it) are the operation's.
/// An unknown stream is 404, any other operation error 422.
pub(crate) async fn tail_route(
    State(st): State<Shared>,
    Path(id): Path<String>,
    query: Result<Query<TailQuery>, QueryRejection>,
) -> Response {
    let Query(q) = match query {
        Ok(q) => q,
        Err(e) => return error_response(e.status(), other(e.body_text())),
    };
    let op = Op::StreamTail {
        stream: StreamId(id),
        max_frames: q.max_frames,
        timeout_ms: q.timeout_ms,
    };
    match st.node.apply(op).await {
        Ok(result) => Json(result).into_response(),
        Err(e @ OpError::Unknown { .. }) => error_response(StatusCode::NOT_FOUND, e),
        Err(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, e),
    }
}

#[derive(Deserialize)]
pub(crate) struct FormatQuery {
    format: Option<String>,
}

/// `GET /stream/{id}?format=text|binary`: 400, 404 and 503 are answered before upgrading.
/// `text` (or no `format`) pushes JSON text messages; `binary` pushes VRT packets for IQ and
/// typed envelopes for the rest, paced by the client's credits.
pub(crate) async fn ws(
    State(st): State<Shared>,
    Path(id): Path<String>,
    query: Result<Query<FormatQuery>, QueryRejection>,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let binary = match query {
        Ok(Query(q)) => match q.format.as_deref() {
            None | Some("text") => false,
            Some("binary") => true,
            Some(_) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    other("format must be text or binary"),
                );
            }
        },
        Err(e) => return error_response(e.status(), other(e.body_text())),
    };
    let bus = st.node.bus();
    let id = StreamId(id);
    let Some(rx) = bus.subscribe(&id) else {
        return error_response(StatusCode::NOT_FOUND, unknown_stream(&bus, &id));
    };
    let Ok(permit) = st.ws_slots.clone().try_acquire_owned() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            other("too many stream connections"),
        );
    };
    // Taken before upgrading so `Server::run` waits for an upgrade still in progress.
    let tracked = st.ws_tasks.token();
    let upgrade = match upgrade {
        Ok(u) => u,
        Err(e) => return error_response(e.status(), other(e.body_text())),
    };
    upgrade
        .max_message_size(st.limits.ws_max_message_bytes)
        .on_upgrade(move |socket| async move {
            if binary {
                connection_binary(socket, rx, st, id, permit, tracked).await;
            } else {
                connection(socket, rx, st, permit, tracked).await;
            }
        })
}

/// Counts a live connection for as long as it is held.
struct Live(StatsHandle);

impl Live {
    fn new(stats: &StatsHandle) -> Self {
        stats.ws_opened();
        Self(stats.clone())
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.0.ws_closed();
    }
}

/// Send `msg` within `limit`; `false` if it timed out or failed.
async fn send(socket: &mut WebSocket, msg: Message, limit: Duration) -> bool {
    matches!(timeout(limit, socket.send(msg)).await, Ok(Ok(())))
}

async fn send_json(socket: &mut WebSocket, msg: &WsMessage, limit: Duration) -> bool {
    let text = serde_json::to_string(msg).expect("WsMessage serialises");
    send(socket, Message::Text(text.into()), limit).await
}

/// Send `Closed { reason }`, then a close frame with `code`, both within one `limit`.
async fn close(socket: &mut WebSocket, reason: &str, code: u16, limit: Duration) {
    let closed = WsMessage::Closed {
        reason: reason.to_string(),
    };
    let text = serde_json::to_string(&closed).expect("WsMessage serialises");
    let frame = CloseFrame {
        code,
        reason: "".into(),
    };
    let _ = timeout(limit, async {
        socket.send(Message::Text(text.into())).await?;
        socket.send(Message::Close(Some(frame))).await
    })
    .await;
}

/// One task per connection; the permit, the live count and the tracker token are released on
/// every exit.
async fn connection(
    mut socket: WebSocket,
    mut rx: broadcast::Receiver<Frame>,
    st: Shared,
    _permit: OwnedSemaphorePermit,
    _tracked: TaskTrackerToken,
) {
    let _live = Live::new(&st.stats);
    let limit = st.limits.ws_send_timeout;
    loop {
        tokio::select! {
            biased;
            () = st.ws_shutdown.cancelled() => {
                close(&mut socket, "node shutting down", close_code::AWAY, limit).await;
                return;
            }
            inbound = socket.recv() => match inbound {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(_)) => {}
            },
            next = rx.recv() => {
                let msg = match next {
                    Ok(frame) => WsMessage::Frame(ws_frame(&frame)),
                    Err(RecvError::Lagged(missed)) => WsMessage::Lagged { missed },
                    Err(RecvError::Closed) => {
                        close(&mut socket, "stream ended", close_code::NORMAL, limit).await;
                        return;
                    }
                };
                if !send_json(&mut socket, &msg, limit).await {
                    st.stats.ws_dropped_slow();
                    return;
                }
            }
        }
    }
}

/// `credit` after an inbound text message: a valid `{"type":"credit","packets":N}` with N in
/// `1..=CREDIT_MAX_GRANT` adds N, capped at `CREDIT_CAP`; anything else changes nothing.
fn granted(credit: u32, text: &str) -> u32 {
    match serde_json::from_str(text) {
        Ok(WsMessage::Credit { packets }) if (1..=CREDIT_MAX_GRANT).contains(&packets) => {
            credit.saturating_add(packets).min(CREDIT_CAP)
        }
        _ => credit,
    }
}

/// The binary messages for one frame: VRT packets for IQ (a context packet first when one is
/// due), one typed envelope for anything else.
fn binary_messages(encoder: &mut VrtEncoder, f: &Frame) -> Result<Vec<Vec<u8>>, String> {
    match &f.payload {
        Payload::Iq(b) => {
            let meta = IqMeta {
                center_hz: b.center_hz.0,
                sample_rate_hz: b.sample_rate_hz.0,
                bandwidth_hz: b.sample_rate_hz.0,
            };
            Ok(encoder.encode(b.t0.0, meta, &b.samples))
        }
        _ => envelope(f).map(|e| vec![e]),
    }
}

/// The binary-mode loop. Credit starts at 0 and every binary message spends one. A frame
/// that arrives with no credit is skipped and counted, as is one whose later packets found
/// the credit spent, one that cannot be enveloped, and any broadcast lag; the count goes out
/// as `{"type":"lagged","missed":K}` just before the next frame that is sent.
async fn connection_binary(
    mut socket: WebSocket,
    mut rx: broadcast::Receiver<Frame>,
    st: Shared,
    id: StreamId,
    _permit: OwnedSemaphorePermit,
    _tracked: TaskTrackerToken,
) {
    let _live = Live::new(&st.stats);
    let limit = st.limits.ws_send_timeout;
    let mut encoder = VrtEncoder::new(stream_id_of(&id.0));
    let mut credit: u32 = 0;
    let mut missed: u64 = 0;
    loop {
        tokio::select! {
            biased;
            () = st.ws_shutdown.cancelled() => {
                close(&mut socket, "node shutting down", close_code::AWAY, limit).await;
                return;
            }
            inbound = socket.recv() => match inbound {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(Message::Text(t))) => credit = granted(credit, &t),
                Some(Ok(_)) => {}
            },
            next = rx.recv() => {
                let frame = match next {
                    Ok(frame) => frame,
                    Err(RecvError::Lagged(n)) => {
                        missed = missed.saturating_add(n);
                        continue;
                    }
                    Err(RecvError::Closed) => {
                        close(&mut socket, "stream ended", close_code::NORMAL, limit).await;
                        return;
                    }
                };
                if credit == 0 {
                    missed = missed.saturating_add(1);
                    st.stats.ws_credit_skipped();
                    continue;
                }
                let Ok(messages) = binary_messages(&mut encoder, &frame) else {
                    missed = missed.saturating_add(1);
                    continue;
                };
                if missed > 0 {
                    if !send_json(&mut socket, &WsMessage::Lagged { missed }, limit).await {
                        st.stats.ws_dropped_slow();
                        return;
                    }
                    missed = 0;
                }
                let total = messages.len();
                let mut sent = 0;
                for m in messages {
                    if credit == 0 {
                        break;
                    }
                    if !send(&mut socket, Message::Binary(m.into()), limit).await {
                        st.stats.ws_dropped_slow();
                        return;
                    }
                    credit -= 1;
                    sent += 1;
                }
                if sent < total {
                    missed = missed.saturating_add(1);
                    st.stats.ws_credit_skipped();
                }
            }
        }
    }
}
