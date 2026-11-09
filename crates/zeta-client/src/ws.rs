//! `NodeClient::subscribe`: a stream's websocket in binary mode, paced by credits.
//!
//! IQ streams arrive as VRT packets and are decoded here; every other kind arrives as typed
//! envelopes. Text messages are `lagged` and `closed`. The subscription grants
//! [`CLIENT_INITIAL_CREDIT`] at open and [`CLIENT_TOPUP`] more every time that many binary
//! messages have been consumed since the last grant.

use futures_util::{SinkExt, StreamExt};
use num_complex::Complex;
use reqwest::Url;
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async_with_config};
use zeta_core::ops::FrameSummary;
use zeta_core::types::{StreamId, StreamKind};
use zeta_vrt::envelope::{MAX_BODY, MAX_HEADER};
use zeta_vrt::{IqMeta, VrtDecoder, parse_envelope};

use crate::remote::{ClientError, NodeClient, REQUEST_TIMEOUT};

/// Credits granted right after the upgrade.
pub const CLIENT_INITIAL_CREDIT: u32 = 256;
/// Credits granted each time this many binary messages have been consumed since the last grant.
pub const CLIENT_TOPUP: u32 = 128;

/// One item from a subscription.
#[derive(Debug, Clone, PartialEq)]
pub enum Received {
    /// One decoded VRT data packet with the context in force.
    Iq {
        t0_ns: u64,
        meta: IqMeta,
        samples: Vec<Complex<f32>>,
        missed_packets: u32,
    },
    /// One non-IQ frame: its summary and the envelope body.
    Frame { header: FrameSummary, body: Vec<u8> },
    /// The node skipped `missed` frames (no credit, or the socket fell behind).
    Lagged { missed: u64 },
}

/// The text messages a node sends in binary mode.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerText {
    Lagged { missed: u64 },
    Closed { reason: String },
}

/// An open binary-mode stream websocket.
pub struct Subscription {
    url: String,
    kind: StreamKind,
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    decoder: VrtDecoder,
    since_grant: u32,
    ended: bool,
    close_reason: Option<String>,
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription")
            .field("url", &self.url)
            .field("kind", &self.kind)
            .field("since_grant", &self.since_grant)
            .field("ended", &self.ended)
            .field("close_reason", &self.close_reason)
            .finish_non_exhaustive()
    }
}

impl NodeClient {
    /// Open `ws://HOST:PORT/stream/{id}?format=binary` with the bearer header and grant the
    /// initial credit. The stream's kind is looked up first; an unknown stream is
    /// `ClientError::Stream` naming the known ids.
    pub async fn subscribe(&self, stream: &StreamId) -> Result<Subscription, ClientError> {
        self.open(stream, "format=binary", false).await
    }

    /// As [`NodeClient::subscribe`] for a video stream, asking for delivery `profile`
    /// (`smooth`, `sharp` or `thumb`): the bodies are PNG. A stream that is not video is
    /// `ClientError::Stream`.
    pub async fn subscribe_video(
        &self,
        stream: &StreamId,
        profile: &str,
    ) -> Result<Subscription, ClientError> {
        self.open(stream, &format!("format=binary&video={profile}"), true)
            .await
    }

    async fn open(
        &self,
        stream: &StreamId,
        query: &str,
        video: bool,
    ) -> Result<Subscription, ClientError> {
        let streams = self.streams().await?;
        let Some(kind) = streams.iter().find(|i| &i.id == stream).map(|i| i.kind) else {
            let mut known: Vec<&str> = streams.iter().map(|i| i.id.0.as_str()).collect();
            known.sort_unstable();
            return Err(ClientError::Stream {
                url: self.url().to_string(),
                detail: format!(
                    "unknown stream {:?}; known: [{}]",
                    stream.0,
                    known.join(", ")
                ),
            });
        };
        if video && kind != StreamKind::Video {
            return Err(ClientError::Stream {
                url: self.url().to_string(),
                detail: format!("stream {} is {}, not video", stream.0, kind_name(kind)),
            });
        }
        let url = self.ws_url(stream, query)?;
        let bad = || ClientError::BadUrl(self.url().to_string());
        let mut req = url.as_str().into_client_request().map_err(|_| bad())?;
        let mut bearer =
            HeaderValue::from_str(&format!("Bearer {}", self.secret())).map_err(|_| {
                ClientError::Stream {
                    url: url.to_string(),
                    detail: "the secret is not a valid header value".to_string(),
                }
            })?;
        bearer.set_sensitive(true);
        req.headers_mut().insert(AUTHORIZATION, bearer);
        let unreachable = |detail: String| ClientError::Unreachable {
            url: self.url().to_string(),
            detail,
        };
        // Large enough for the largest envelope (`u32 | header | u32 | body`); a VRT
        // packet is far smaller.
        let max = MAX_BODY + MAX_HEADER + 8;
        let ws_cfg = WebSocketConfig::default()
            .max_message_size(Some(max))
            .max_frame_size(Some(max));
        let connect = connect_async_with_config(req, Some(ws_cfg), false);
        let ws = match timeout(REQUEST_TIMEOUT, connect).await {
            Err(_) => return Err(unreachable("websocket upgrade timed out".to_string())),
            Ok(Err(WsError::Http(resp))) => {
                let status = resp.status().as_u16();
                if status == 401 {
                    return Err(ClientError::Unauthorized {
                        url: self.url().to_string(),
                    });
                }
                return Err(ClientError::Http {
                    url: self.url().to_string(),
                    status,
                    body: String::from_utf8_lossy(resp.body().as_deref().unwrap_or_default())
                        .into_owned(),
                });
            }
            Ok(Err(e)) => return Err(unreachable(e.to_string())),
            Ok(Ok((ws, _))) => ws,
        };
        let mut sub = Subscription {
            url: url.to_string(),
            kind,
            ws,
            decoder: VrtDecoder::new(),
            since_grant: 0,
            ended: false,
            close_reason: None,
        };
        sub.grant(CLIENT_INITIAL_CREDIT)
            .await
            .map_err(|e| sub.error(format!("granting credit: {e}")))?;
        Ok(sub)
    }

    /// The node's URL with `ws` for `http`, at `/stream/{id}?{query}`.
    fn ws_url(&self, stream: &StreamId, query: &str) -> Result<Url, ClientError> {
        let bad = || ClientError::BadUrl(self.url().to_string());
        let mut url = Url::parse(self.url()).map_err(|_| bad())?;
        url.set_scheme("ws").map_err(|()| bad())?;
        url.path_segments_mut()
            .map_err(|()| bad())?
            .extend(["stream", stream.0.as_str()]);
        url.set_query(Some(query));
        Ok(url)
    }
}

impl Subscription {
    /// The next item. A message that does not decode is `Some(Err(ClientError::Stream))` and
    /// the subscription goes on; `None` once the node sent `closed`, closed the socket, or the
    /// socket ended, and every time after that.
    pub async fn next(&mut self) -> Option<Result<Received, ClientError>> {
        while !self.ended {
            let msg = match self.ws.next().await {
                None => {
                    self.end("connection closed".to_string());
                    break;
                }
                Some(Err(e)) => {
                    self.end(format!("socket error: {e}"));
                    break;
                }
                Some(Ok(m)) => m,
            };
            match msg {
                Message::Binary(bytes) => {
                    if let Err(e) = self.consumed_one().await {
                        // Hand this message over; the next call reports the end.
                        self.end(format!("granting credit: {e}"));
                    }
                    match self.decode(&bytes) {
                        Ok(Some(r)) => return Some(Ok(r)),
                        Ok(None) => {}
                        Err(detail) => return Some(Err(self.error(detail))),
                    }
                }
                Message::Text(text) => match serde_json::from_str::<ServerText>(&text) {
                    Ok(ServerText::Lagged { missed }) => {
                        return Some(Ok(Received::Lagged { missed }));
                    }
                    Ok(ServerText::Closed { reason }) => self.end(reason),
                    Err(e) => {
                        return Some(Err(self.error(format!("unexpected text message: {e}"))));
                    }
                },
                Message::Close(frame) => {
                    let reason = match frame {
                        Some(f) if !f.reason.is_empty() => f.reason.to_string(),
                        Some(f) => format!("closed by the node (code {})", u16::from(f.code)),
                        None => "closed by the node".to_string(),
                    };
                    self.end(reason);
                }
                Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
            }
        }
        None
    }

    /// Why the subscription ended: the node's `closed` reason or close-frame reason, or what
    /// ended the socket. `None` while it is open.
    pub fn close_reason(&self) -> Option<&str> {
        self.close_reason.as_deref()
    }

    /// The kind of the subscribed stream.
    pub fn kind(&self) -> StreamKind {
        self.kind
    }

    fn end(&mut self, reason: String) {
        self.ended = true;
        self.close_reason.get_or_insert(reason);
    }

    fn error(&self, detail: String) -> ClientError {
        ClientError::Stream {
            url: self.url.clone(),
            detail,
        }
    }

    async fn grant(&mut self, packets: u32) -> Result<(), WsError> {
        let text = serde_json::json!({"type": "credit", "packets": packets}).to_string();
        self.ws.send(Message::Text(text.into())).await
    }

    /// Count one binary message; top the credit up once `CLIENT_TOPUP` have been consumed.
    async fn consumed_one(&mut self) -> Result<(), WsError> {
        self.since_grant += 1;
        if self.since_grant >= CLIENT_TOPUP {
            self.since_grant = 0;
            self.grant(CLIENT_TOPUP).await?;
        }
        Ok(())
    }

    /// VRT for IQ (a context packet yields nothing), an envelope for anything else.
    fn decode(&mut self, bytes: &[u8]) -> Result<Option<Received>, String> {
        if self.kind == StreamKind::Iq {
            let decoded = self.decoder.feed(bytes).map_err(|e| format!("vrt: {e}"))?;
            Ok(decoded.map(|d| Received::Iq {
                t0_ns: d.t0_ns,
                meta: d.meta,
                samples: d.samples,
                missed_packets: d.missed_packets,
            }))
        } else {
            let (header, body) = parse_envelope(bytes).map_err(|e| format!("envelope: {e}"))?;
            Ok(Some(Received::Frame { header, body }))
        }
    }
}

fn kind_name(kind: StreamKind) -> &'static str {
    match kind {
        StreamKind::Iq => "iq",
        StreamKind::Audio => "audio",
        StreamKind::Video => "video",
        StreamKind::Text => "text",
        StreamKind::Event => "event",
        StreamKind::Sink => "sink",
    }
}
