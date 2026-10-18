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
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use zeta_client::parse_op;
use zeta_core::node::Node;
use zeta_core::ops::OpError;

use crate::addr::is_lan;
use crate::auth::Secret;

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
    pub tail_max_frames: u32,
    pub tail_max_timeout_ms: u32,
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
            tail_max_frames: 100,
            tail_max_timeout_ms: 10_000,
        }
    }
}

pub struct ServerConfig {
    pub listen: SocketAddr,
    pub secret: Secret,
    pub agents_md: String,
    pub limits: Limits,
}

/// A snapshot of the server's counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerStats {
    pub ws_connections: usize,
    pub ws_dropped_slow: u64,
}

#[derive(Default)]
struct Counters {
    ws_connections: AtomicUsize,
    ws_dropped_slow: AtomicU64,
}

/// A cheap handle on the server's live counters.
#[derive(Clone, Default)]
pub struct StatsHandle(Arc<Counters>);

impl StatsHandle {
    pub fn get(&self) -> ServerStats {
        ServerStats {
            ws_connections: self.0.ws_connections.load(Ordering::Relaxed),
            ws_dropped_slow: self.0.ws_dropped_slow.load(Ordering::Relaxed),
        }
    }
}

/// The `GET /health` body.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Health {
    pub status: String,
    pub version: String,
    pub uptime_s: u64,
    pub ws_connections: usize,
}

/// State shared by every route and middleware.
pub(crate) struct AppState {
    pub(crate) node: Arc<Node>,
    pub(crate) secret: Secret,
    pub(crate) limits: Limits,
    pub(crate) op_slots: Semaphore,
    pub(crate) stats: StatsHandle,
    pub(crate) started: Instant,
    pub(crate) agents_md: String,
}

type Shared = Arc<AppState>;

/// Every non-2xx answer: the error as JSON under `status`.
pub(crate) fn error_response(status: StatusCode, err: OpError) -> Response {
    (status, Json(err)).into_response()
}

fn other(message: impl Into<String>) -> OpError {
    OpError::Other {
        message: message.into(),
    }
}

/// Refuse non-LAN peers (403), then require `Authorization: Bearer <secret>` (401 after a delay).
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
    let ok = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.as_bytes().strip_prefix(b"Bearer "))
        .is_some_and(|presented| st.secret.matches(presented));
    if !ok {
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
        uptime_s: st.started.elapsed().as_secs(),
        ws_connections: st.stats.get().ws_connections,
    })
}

async fn op(State(st): State<Shared>, body: Body) -> Response {
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
    match tokio::time::timeout(timeout, st.node.apply(op)).await {
        Ok(Ok(result)) => (StatusCode::OK, Json(result)).into_response(),
        Ok(Err(e)) => error_response(StatusCode::UNPROCESSABLE_ENTITY, e),
        Err(_) => error_response(
            StatusCode::GATEWAY_TIMEOUT,
            other(format!(
                "operation timed out after {} s",
                timeout.as_secs_f64()
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

fn router(state: Shared) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/op", post(op))
        .route("/schema", get(schema))
        .route("/streams", get(streams))
        .route("/agents-md", get(agents_md))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn_with_state(state.clone(), require_auth))
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
        stats: StatsHandle::default(),
        started: Instant::now(),
        agents_md: cfg.agents_md,
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

    /// Serve until `shutdown` is cancelled, then finish in-flight requests.
    pub async fn run(self, shutdown: CancellationToken) -> io::Result<()> {
        let app = router(self.state).into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(self.listener, app)
            .with_graceful_shutdown(async move { shutdown.cancelled().await })
            .await
    }
}
