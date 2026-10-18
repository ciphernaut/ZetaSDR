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
use tokio::time::{Instant, timeout, timeout_at};
use tokio_util::task::task_tracker::TaskTrackerToken;
use zeta_core::ops::OpError;
use zeta_core::stream::{Frame, StreamBus};
use zeta_core::types::StreamId;

use crate::http::{Shared, StatsHandle, error_response, other};
use crate::wire::{WsFrame, WsMessage, ws_frame};

fn unknown_stream(bus: &StreamBus, id: &StreamId) -> OpError {
    let mut known: Vec<String> = bus.list().into_iter().map(|i| i.id.0).collect();
    known.sort();
    OpError::Unknown {
        field: "stream".to_string(),
        value: id.0.clone(),
        known,
    }
}

/// Subscribe to `id` and collect frames until `max_frames` arrive, `timeout` passes or the
/// stream ends. Lag notices are skipped.
pub async fn tail(
    bus: &StreamBus,
    id: &StreamId,
    max_frames: u32,
    timeout: Duration,
) -> Result<Vec<WsFrame>, OpError> {
    let mut rx = bus.subscribe(id).ok_or_else(|| unknown_stream(bus, id))?;
    let deadline = Instant::now() + timeout;
    let mut frames = Vec::new();
    while frames.len() < max_frames as usize {
        match timeout_at(deadline, rx.recv()).await {
            Ok(Ok(frame)) => frames.push(ws_frame(&frame)),
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed)) | Err(_) => break,
        }
    }
    Ok(frames)
}

#[derive(Deserialize)]
pub(crate) struct TailQuery {
    max_frames: u32,
    timeout_ms: u32,
}

fn in_range(field: &str, value: u32, max: u32) -> Result<(), OpError> {
    if (1..=max).contains(&value) {
        return Ok(());
    }
    Err(OpError::OutOfRange {
        field: field.to_string(),
        value: value.into(),
        min: 1,
        max: max.into(),
        suggested: None,
    })
}

/// `GET /streams/{id}/tail?max_frames=N&timeout_ms=T`.
pub(crate) async fn tail_route(
    State(st): State<Shared>,
    Path(id): Path<String>,
    query: Result<Query<TailQuery>, QueryRejection>,
) -> Response {
    let Query(q) = match query {
        Ok(q) => q,
        Err(e) => return error_response(e.status(), other(e.body_text())),
    };
    let checked = in_range("max_frames", q.max_frames, st.limits.tail_max_frames)
        .and_then(|()| in_range("timeout_ms", q.timeout_ms, st.limits.tail_max_timeout_ms));
    if let Err(e) = checked {
        return error_response(StatusCode::UNPROCESSABLE_ENTITY, e);
    }
    let wait = Duration::from_millis(q.timeout_ms.into());
    match tail(&st.node.bus(), &StreamId(id), q.max_frames, wait).await {
        Ok(frames) => Json(frames).into_response(),
        Err(e @ OpError::Unknown { .. }) => error_response(StatusCode::NOT_FOUND, e),
        Err(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, e),
    }
}

/// `GET /stream/{id}`: 404 and 503 are answered before upgrading.
pub(crate) async fn ws(
    State(st): State<Shared>,
    Path(id): Path<String>,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
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
        .on_upgrade(move |socket| connection(socket, rx, st, permit, tracked))
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
