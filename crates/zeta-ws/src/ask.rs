//! `POST /ask`: run the agent against the serving node, one run at a time.

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use zeta_agent::{
    Allow, Caller, FocusFrom, LocalExecutor, PageTools, RadioContext, RadioPolicy, build_agent,
};
use zeta_core::ops::OpError;
use zeta_core::types::SourceId;

use crate::http::{Shared, error_response, other};

/// Turns when the request names none.
const DEFAULT_TURNS: u64 = 8;
const MIN_TURNS: u64 = 1;
const MAX_TURNS: u64 = 32;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AskRequest {
    request: String,
    max_turns: Option<u64>,
    /// What the run may stop; nothing when absent.
    allow: Option<Allow>,
    /// Offer the page-only tools (`view_set`): the web page sets it, `zeta ask` never does.
    #[serde(default)]
    page_tools: bool,
    /// The radio the operator has in front of them; absent for a caller with no page.
    context: Option<AskContext>,
}

/// What the page tells the agent about the operator's view.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct AskContext {
    /// The radio shown.
    focus: Option<String>,
    /// The page's main view, `rf` or `map`: a focus kept while the map is shown is `kept`.
    main: Option<String>,
    /// One of `focus`, `focus-fallback`, `free-first`, `ask`; checked by `radio_context`.
    radio_policy: Option<String>,
    #[serde(default)]
    radio_prefer: Vec<String>,
}

fn invalid(msg: String) -> Response {
    error_response(
        StatusCode::UNPROCESSABLE_ENTITY,
        other(format!("invalid ask request: {msg}")),
    )
}

fn source_id(field: &str, id: &str) -> Result<SourceId, String> {
    if id.trim().is_empty() {
        return Err(format!("{field} is empty"));
    }
    Ok(SourceId(id.to_string()))
}

/// The agent's radio context from the request's; no context is today's `free-first`.
fn radio_context(ctx: Option<AskContext>) -> Result<RadioContext, String> {
    let Some(ctx) = ctx else {
        return Ok(RadioContext::default());
    };
    let focus_from = match ctx.main.as_deref() {
        None | Some("rf") => FocusFrom::Shown,
        Some("map") => FocusFrom::Kept,
        Some(_) => return Err("context.main must be rf or map".into()),
    };
    let policy = ctx
        .radio_policy
        .as_deref()
        .map(|w| {
            w.parse::<RadioPolicy>()
                .map_err(|e| format!("context.radio_policy: {e}"))
        })
        .transpose()?;
    let focus = ctx
        .focus
        .as_deref()
        .map(|f| source_id("context.focus", f))
        .transpose()?;
    let prefer = ctx
        .radio_prefer
        .iter()
        .map(|p| source_id("context.radio_prefer", p))
        .collect::<Result<Vec<_>, _>>()?;
    let focus_from = if focus.is_some() {
        focus_from
    } else {
        FocusFrom::None
    };
    Ok(RadioContext::from_parts(focus, focus_from, policy, prefer))
}

pub(crate) async fn ask(State(st): State<Shared>, body: Body) -> Response {
    let limit = st.limits.body_bytes;
    let Ok(bytes) = axum::body::to_bytes(body, limit).await else {
        return error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            other(format!("body larger than {limit} bytes")),
        );
    };
    let Some(llm) = st.llm.clone() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            other("serve started without --llm-url"),
        );
    };
    let req: AskRequest = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(e) => {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                other(format!("invalid ask request: {e}")),
            );
        }
    };
    let request = req.request.trim();
    if request.is_empty() {
        return error_response(StatusCode::UNPROCESSABLE_ENTITY, other("empty request"));
    }
    let turns = req.max_turns.unwrap_or(DEFAULT_TURNS);
    let Some(max_turns) = (MIN_TURNS..=MAX_TURNS)
        .contains(&turns)
        .then(|| u32::try_from(turns).ok())
        .flatten()
    else {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            OpError::OutOfRange {
                field: "max_turns".into(),
                value: turns,
                min: MIN_TURNS,
                max: MAX_TURNS,
                suggested: Some(turns.clamp(MIN_TURNS, MAX_TURNS)),
                why: None,
            },
        );
    };
    let radio = match radio_context(req.context) {
        Ok(r) => r,
        Err(e) => return invalid(e),
    };
    let Ok(_running) = st.ask_lock.try_lock() else {
        return error_response(StatusCode::CONFLICT, other("an agent run is in progress"));
    };
    let schema = st.node.schema();
    let caller = if req.page_tools {
        Caller::page()
    } else {
        Caller::http()
    };
    let page = req.page_tools.then(|| PageTools {
        themes: st
            .node
            .themes()
            .all()
            .iter()
            .filter(|t| t.state == zeta_core::themes::ThemeState::Ready)
            .map(|t| t.id.clone())
            .collect(),
    });
    let agent = match build_agent(
        llm,
        LocalExecutor::tagged(st.node.clone(), "agent"),
        &schema,
        &st.agents_md,
        max_turns,
        req.allow.unwrap_or_default(),
        page,
        radio,
        caller,
    ) {
        Ok(a) => a,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, other(e.to_string())),
    };
    let timeout = st.limits.ask_timeout;
    match tokio::time::timeout(timeout, agent.run(request)).await {
        Ok(Ok(transcript)) => (StatusCode::OK, Json(transcript)).into_response(),
        Ok(Err(failure)) => (StatusCode::BAD_GATEWAY, Json(failure)).into_response(),
        Err(_) => error_response(
            StatusCode::GATEWAY_TIMEOUT,
            other(format!(
                "the agent run timed out after {} s; operations it applied stay applied, check `job list`",
                timeout.as_secs()
            )),
        ),
    }
}
