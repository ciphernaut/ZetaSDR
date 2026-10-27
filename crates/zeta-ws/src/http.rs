//! The authenticated HTTP API: shared state, auth middleware, routes, `bind` and `Server`.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::{
    AUTHORIZATION, CONNECTION, CONTENT_TYPE, SEC_WEBSOCKET_PROTOCOL, UPGRADE, WWW_AUTHENTICATE,
};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use zeta_agent::LlmConfig;
use zeta_client::parse_op;
use zeta_core::node::Node;
use zeta_core::ops::OpError;
use zeta_speech::SpeechBackend;

use crate::addr::is_lan;
use crate::auth::{Secret, subprotocol_secret};
use crate::{ask, stream, stt, ui};

/// Resource limits sized for a Raspberry Pi.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub body_bytes: usize,
    pub max_concurrent_ops: usize,
    pub max_ws: usize,
    pub op_timeout: Duration,
    pub ws_send_timeout: Duration,
    pub auth_fail_delay: Duration,
    pub ws_max_message_bytes: usize,
    /// Longest `POST /ask` agent run.
    pub ask_timeout: Duration,
    /// Largest `POST /stt` WAV body.
    pub stt_max_bytes: usize,
    /// Longest wait for the speech daemon on `POST /stt`.
    pub stt_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            body_bytes: 1 << 20,
            max_concurrent_ops: 8,
            max_ws: 16,
            op_timeout: Duration::from_secs(30),
            ws_send_timeout: Duration::from_secs(5),
            auth_fail_delay: Duration::from_millis(250),
            ws_max_message_bytes: 64 << 10,
            ask_timeout: Duration::from_secs(300),
            stt_max_bytes: 4 << 20,
            stt_timeout: Duration::from_secs(30),
        }
    }
}

pub struct ServerConfig {
    pub listen: SocketAddr,
    pub secret: Secret,
    pub agents_md: String,
    pub limits: Limits,
    /// The model `POST /ask` runs; `None` answers 503.
    pub llm: Option<LlmConfig>,
    /// The speech backend behind `POST /stt` and `GET /stt/languages`; `None` answers 503.
    pub speech: Option<Arc<dyn SpeechBackend>>,
}

/// A snapshot of the server's counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerStats {
    pub ws_connections: usize,
    pub ws_dropped_slow: u64,
    /// Frames a binary-mode socket skipped because it had no credit.
    pub ws_credit_skipped: u64,
}

#[derive(Default)]
struct Counters {
    ws_connections: AtomicUsize,
    ws_dropped_slow: AtomicU64,
    ws_credit_skipped: AtomicU64,
}

/// A cheap handle on the server's live counters.
#[derive(Clone, Default)]
pub struct StatsHandle(Arc<Counters>);

impl StatsHandle {
    pub fn get(&self) -> ServerStats {
        ServerStats {
            ws_connections: self.0.ws_connections.load(Ordering::Relaxed),
            ws_dropped_slow: self.0.ws_dropped_slow.load(Ordering::Relaxed),
            ws_credit_skipped: self.0.ws_credit_skipped.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn ws_opened(&self) {
        self.0.ws_connections.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn ws_closed(&self) {
        self.0.ws_connections.fetch_sub(1, Ordering::Relaxed);
    }

    pub(crate) fn ws_dropped_slow(&self) {
        self.0.ws_dropped_slow.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn ws_credit_skipped(&self) {
        self.0.ws_credit_skipped.fetch_add(1, Ordering::Relaxed);
    }
}

/// The `GET /health` body.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Health {
    pub status: String,
    pub version: String,
    /// The build this node runs ([`crate::build_id`]); a page reloads when it changes.
    pub build: String,
    pub uptime_s: u64,
    pub ws_connections: usize,
    /// Tasks running for jobs' stages ([`Node::debug_stage_count`]): it falls back
    /// when jobs stop, so one that only grows is a run left behind.
    pub stages_running: usize,
}

/// State shared by every route and middleware.
pub(crate) struct AppState {
    pub(crate) node: Arc<Node>,
    pub(crate) secret: Secret,
    pub(crate) limits: Limits,
    pub(crate) op_slots: Semaphore,
    pub(crate) ws_slots: Arc<Semaphore>,
    /// Cancelled by `Server::run` when shutdown starts; every websocket task watches it.
    pub(crate) ws_shutdown: CancellationToken,
    /// Every websocket connection, from its accepted upgrade request until its task ends.
    pub(crate) ws_tasks: TaskTracker,
    pub(crate) stats: StatsHandle,
    pub(crate) started: Instant,
    pub(crate) agents_md: String,
    pub(crate) llm: Option<LlmConfig>,
    /// Held for the length of one `POST /ask` run.
    pub(crate) ask_lock: tokio::sync::Mutex<()>,
    pub(crate) speech: Option<Arc<dyn SpeechBackend>>,
}

pub(crate) type Shared = Arc<AppState>;

/// Every non-2xx answer: the error as JSON under `status`.
pub(crate) fn error_response(status: StatusCode, err: OpError) -> Response {
    (status, Json(err)).into_response()
}

pub(crate) fn other(message: impl Into<String>) -> OpError {
    OpError::Other {
        message: message.into(),
    }
}

/// Whether the request carries the secret: as `Authorization: Bearer <secret>` on any route,
/// or as the `zeta.bearer.<base64url>` entry of `Sec-WebSocket-Protocol` only on a websocket
/// upgrade (`GET`, `Upgrade: websocket`, `Connection: upgrade`) of the `/stream/` route.
fn authenticated(secret: &Secret, method: &Method, path: &str, headers: &HeaderMap) -> bool {
    let by_header = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.as_bytes().strip_prefix(b"Bearer "))
        .is_some_and(|presented| secret.matches(presented));
    if by_header {
        return true;
    }
    let stream_upgrade = method == Method::GET
        && path.starts_with("/stream/")
        && headers
            .get(UPGRADE)
            .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"))
        && headers
            .get_all(CONNECTION)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .any(|token| token.trim().eq_ignore_ascii_case("upgrade"));
    stream_upgrade
        && headers
            .get_all(SEC_WEBSOCKET_PROTOCOL)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(subprotocol_secret)
            .any(|presented| secret.matches(&presented))
}

/// Refuse non-LAN peers (403), then require the secret (401 after a delay): as
/// `Authorization: Bearer <secret>`, or for a websocket upgrade as the bearer subprotocol.
async fn require_auth(
    State(st): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    if !is_lan(peer.ip()) {
        return error_response(
            StatusCode::FORBIDDEN,
            other(format!("peer {} is not a LAN address", peer.ip())),
        );
    }
    if !authenticated(&st.secret, req.method(), req.uri().path(), req.headers()) {
        tokio::time::sleep(st.limits.auth_fail_delay).await;
        let mut resp = error_response(
            StatusCode::UNAUTHORIZED,
            other("missing or wrong bearer secret"),
        );
        resp.headers_mut()
            .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return resp;
    }
    next.run(req).await
}

async fn health(State(st): State<Shared>) -> Json<Health> {
    Json(Health {
        status: "ok".to_string(),
        version: zeta_core::VERSION.to_string(),
        build: crate::build_id().to_string(),
        uptime_s: st.started.elapsed().as_secs(),
        ws_connections: st.stats.get().ws_connections,
        stages_running: st.node.debug_stage_count(),
    })
}

/// The `ops` record's `transport` for an `/op` call: `"ui"` when the
/// `X-Zeta-Client` header names an allow-listed client, `"http"` otherwise
/// (a caller cannot claim `"agent"` or `"local"`).
fn op_transport(headers: &HeaderMap) -> &'static str {
    match headers.get("x-zeta-client").map(HeaderValue::as_bytes) {
        Some(b"ui") => "ui",
        _ => "http",
    }
}

async fn op(State(st): State<Shared>, headers: HeaderMap, body: Body) -> Response {
    let transport = op_transport(&headers);
    let limit = st.limits.body_bytes;
    let Ok(bytes) = axum::body::to_bytes(body, limit).await else {
        return error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            other(format!("body larger than {limit} bytes")),
        );
    };
    let Ok(_permit) = st.op_slots.try_acquire() else {
        return error_response(
            StatusCode::TOO_MANY_REQUESTS,
            other(format!(
                "busy: {} operations in flight",
                st.limits.max_concurrent_ops
            )),
        );
    };
    let parsed = serde_json::from_slice(&bytes)
        .map_err(|e| other(format!("invalid operation: {e}")))
        .and_then(parse_op);
    let op = match parsed {
        Ok(op) => op,
        Err(e) => return error_response(StatusCode::UNPROCESSABLE_ENTITY, e),
    };
    let timeout = st.limits.op_timeout;
    match tokio::time::timeout(timeout, st.node.apply_from(op, transport)).await {
        Ok(Ok(result)) => (StatusCode::OK, Json(result)).into_response(),
        Ok(Err(e)) => error_response(StatusCode::UNPROCESSABLE_ENTITY, e),
        Err(_) => error_response(
            StatusCode::GATEWAY_TIMEOUT,
            other(format!(
                "operation timed out after {} s; it may have been partly applied, check `job list`",
                timeout.as_secs()
            )),
        ),
    }
}

async fn schema(State(st): State<Shared>) -> Json<serde_json::Value> {
    Json(st.node.schema())
}

async fn streams(State(st): State<Shared>) -> Response {
    let mut list = st.node.bus().list();
    list.sort_by(|a, b| a.id.0.cmp(&b.id.0));
    Json(list).into_response()
}

async fn agents_md(State(st): State<Shared>) -> Response {
    (
        [(CONTENT_TYPE, "text/markdown; charset=utf-8")],
        st.agents_md.clone(),
    )
        .into_response()
}

async fn not_found() -> Response {
    error_response(StatusCode::NOT_FOUND, other("no such route"))
}

async fn method_not_allowed() -> Response {
    error_response(
        StatusCode::METHOD_NOT_ALLOWED,
        other("method not allowed on this route"),
    )
}

/// Every route requires auth except the static UI (`/`, `/ui/`, `/ui/{*path}`).
fn router(state: Shared) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .route("/op", post(op))
        .route("/ask", post(ask::ask))
        .route("/stt", post(stt::stt))
        .route("/stt/languages", get(stt::languages))
        .route("/schema", get(schema))
        .route("/streams", get(streams))
        .route("/agents-md", get(agents_md))
        .route("/stream/{id}", get(stream::ws))
        .route("/streams/{id}/tail", get(stream::tail_route))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn_with_state(state.clone(), require_auth));
    ui::router()
        // Merged routers keep their own 405 fallback, so the unauthenticated one needs it too.
        .method_not_allowed_fallback(method_not_allowed)
        .merge(api)
        .with_state(state)
}

/// A bound, not yet serving, HTTP server.
pub struct Server {
    listener: TcpListener,
    local_addr: SocketAddr,
    state: Shared,
}

/// Bind `cfg.listen`, refusing a non-LAN address with `InvalidInput` before binding.
pub async fn bind(node: Arc<Node>, cfg: ServerConfig) -> io::Result<Server> {
    if !is_lan(cfg.listen.ip()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "listen address {} is not a LAN address; ZetaSDR is LAN only",
                cfg.listen.ip()
            ),
        ));
    }
    let listener = TcpListener::bind(cfg.listen).await?;
    let local_addr = listener.local_addr()?;
    let state = Arc::new(AppState {
        node,
        secret: cfg.secret,
        limits: cfg.limits,
        op_slots: Semaphore::new(cfg.limits.max_concurrent_ops),
        ws_slots: Arc::new(Semaphore::new(cfg.limits.max_ws)),
        ws_shutdown: CancellationToken::new(),
        ws_tasks: TaskTracker::new(),
        stats: StatsHandle::default(),
        started: Instant::now(),
        agents_md: cfg.agents_md,
        llm: cfg.llm,
        ask_lock: tokio::sync::Mutex::new(()),
        speech: cfg.speech,
    });
    Ok(Server {
        listener,
        local_addr,
        state,
    })
}

impl Server {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn stats(&self) -> StatsHandle {
        self.state.stats.clone()
    }

    /// Serve until `shutdown` is cancelled, then finish in-flight requests and wait for every
    /// stream websocket to send `Closed` and a 1001 close frame. A stream task may be part way
    /// through a send when shutdown starts and its close is itself bounded by
    /// `ws_send_timeout`, so the wait is bounded by twice that; a client too slow to take the
    /// close in that time is dropped without it.
    pub async fn run(self, shutdown: CancellationToken) -> io::Result<()> {
        let ws_shutdown = self.state.ws_shutdown.clone();
        let ws_tasks = self.state.ws_tasks.clone();
        let ws_bound = 2 * self.state.limits.ws_send_timeout;
        let app = router(self.state).into_make_service_with_connect_info::<SocketAddr>();
        let served = axum::serve(self.listener, app)
            .with_graceful_shutdown(async move {
                shutdown.cancelled().await;
                // Upgraded websockets are not tracked by graceful shutdown: tell them first.
                ws_shutdown.cancel();
            })
            .await;
        ws_tasks.close();
        // A connection task that outlives the bound sees the cancelled token and ends alone.
        let _ = tokio::time::timeout(ws_bound, ws_tasks.wait()).await;
        served
    }
}
