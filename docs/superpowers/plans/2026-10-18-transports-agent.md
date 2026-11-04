# ZetaSDR Transports and Agent Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the node reachable and drivable from outside its own process: an authenticated HTTP + websocket server inside `zeta serve`, a `--node` mode that sends every CLI operation to a running node, an MCP server whose tools are the operation schema, and an in-process agent loop that drives the same operations from any OpenAI-compatible model, with every tool description and the agent's system prompt generated from AGENTS.md.

**Architecture:** Four new crates, none of which `zeta-core` depends on. `zeta-client` holds what every caller shares: the HTTP client for a running node (`NodeClient`), `parse_op`, and the generators that turn `Node::schema()` plus AGENTS.md into tool definitions and a system prompt. `zeta-ws` is the axum server embedded in `zeta serve`: `POST /op` is a thin adapter over `Node::apply`, and `WS /stream/{id}` and `GET /streams/{id}/tail` are subscriptions over `StreamBus::subscribe`. `zeta-mcp` is a stdio MCP server binary that talks to a node only through `NodeClient`, so it runs anywhere on the LAN. `zeta-agent` is the only code that speaks the LLM wire: a chat-completions client and a tool loop that executes calls against a node, locally (`Node::apply`) or remotely (`NodeClient`). The `zeta` binary gains one dispatch function, a `serve --listen` path and an `ask` subcommand.

**Tech Stack:** Rust 1.97, existing tokio/serde/schemars/clap stack; `axum` 0.8.9 with the `ws` feature (server; chosen over bare `tokio-tungstenite` because it gives routing, extractors and the websocket upgrade in one crate that is itself built on tokio-tungstenite, where bare tungstenite would mean writing an HTTP router); `reqwest` 0.13.5 (`default-features = false`, features `json`, `rustls`) for both the node client and the LLM client; `rmcp` 3.5 (`default-features = false`, features `server`, `transport-io`) for MCP; `tokio-util` (`CancellationToken`, already in the workspace); `tokio-tungstenite` 0.30.0 as a dev-dependency only (websocket test client); clap's `env` feature. No `openai` or vendor SDK crate anywhere.

**Spec:** `docs/superpowers/specs/2026-10-18-zetasdr-design.md` (sections 3, 4.8, 5.2, 5.3, 5.4, 7, 8 items 4 and 5, 10)

## Parallel work with plan 2 and expected merge conflicts

Plan 2 (`docs/superpowers/plans/2026-10-18-dsp-slice.md`) runs at the same time on its own branch. This plan branches from `main` at `7b732cc` as `transports-agent` and does not touch `zeta-core` at all; it calls only `Node`'s public API, which plan 2 keeps identical when it splits `node.rs` into `node/`. Stream tailing is built inside `zeta-ws` over `StreamBus::subscribe`, not as an `Op`, so plan 2's `Op::StreamTail` does not collide with it.

Expected conflicts when the second branch merges, and how to resolve each:

| File | Plan 2 | Plan 3 | Resolution |
|---|---|---|---|
| `crates/zeta/src/cli.rs` | `Kind` gains four variants; `StreamCmd::Tail`; `--then-tail` on `job start` | globals `node`, `secret`; `Serve(ServeArgs)`; `Ask(AskArgs)`; `AgentsMd { check, skill }`; matching `into_op` arms | keep both; `into_op` returns `None` for `AgentsMd { .. } \| Serve(_) \| Ask(_)` |
| `crates/zeta/src/main.rs` | `--then-tail` handling after `apply` | three lines: `dispatch` call, `serve_args` capture, `transport::serve` call | keep both; `--then-tail` with `--node` is rejected in `dispatch` until a follow-up implements it as two remote calls |
| `crates/zeta/Cargo.toml` | dep `zeta-dsp`, feature `gpu` | deps `zeta-ws`, `zeta-client`, `zeta-agent`; clap `env` | union |
| `crates/zeta/tests/cli.rs` | appends tests | helper gains `.env_remove("ZETA_NODE").env_remove("ZETA_SECRET")` | keep both |
| `AGENTS.md`, `skills/zetasdr/SKILL.md` | regenerated | regenerated | never hand-merge: `zeta agents-md > AGENTS.md && zeta agents-md --skill > skills/zetasdr/SKILL.md` |
| `README.md`, plan-series file, `Cargo.lock` | appended / updated | appended / updated | keep both; `cargo build` regenerates the lock |

After both land: `stream_tail` becomes an MCP and agent tool automatically (no test here hard-codes the number of operations); plan 2's `FrameSummary` should converge on `zeta_ws::wire::WsFrame` (add `source`, `job`, `channel`, move it to `zeta-core`, make `WsMessage::Frame` carry it and delete `WsFrame`), and `GET /streams/{id}/tail` becomes a wrapper over `Op::StreamTail`.

## Global Constraints

- LAN only with a shared secret on every transport (design 3): every HTTP route and websocket upgrade requires `Authorization: Bearer <secret>` (401 otherwise), compared in constant time; listen and peer addresses outside loopback/private/link-local/CGNAT/ULA are refused; `zeta-mcp` and `zeta --node` send the secret on every request. No per-caller rights.
- No privileged model vendor (design 2, 3, 5.2): OpenAI-compatible chat completions with tools is the only LLM wire; the runtime never calls a model except inside `zeta-agent`, enforced by a test that searches every crate's source.
- Every transport is a thin adapter over `Node::apply` and never reimplements an operation; schemas are served verbatim from `Node::schema()` (MCP and agent tool schemas are the `op` variants with the `op` tag removed and local `$ref`s inlined, nothing added).
- Tiny-model friendliness (design 3, 10): strict JSON schemas (`additionalProperties: false`, every field required), one tool per operation, thinking disabled and a token budget by default, errors fed back verbatim so the model sees `field`, range and `suggested`, prose without a tool call is never parsed as a command.
- `zeta-core` is unchanged by this plan. If something proves unavoidable (for example `Node::apply`'s future not being `Send`), the change is additive and is reported before it is made.
- Limits sized for a Pi 5: 1 MiB request bodies, 8 concurrent operations, 16 websocket connections, 30 s per operation, 5 s per websocket send, 64 KiB inbound websocket messages.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass with no hardware, no network beyond loopback and no model; the one live-model test is `#[ignore]`d.
- Build the minimum (ponytail): reuse before stdlib before dependency before new code.

## Review Focus

1. **Wrong secret.** A request with a wrong or missing bearer secret must get 401 and must not reach `Node::apply`: no job is created. Test `wrong_secret_is_401_and_op_not_applied` in Task 3; CLI side `remote_wrong_secret_exits_1` in Task 7.
2. **A websocket client that stops reading.** It must not stall the stream bus, the publisher or other subscribers; the server drops it after the send timeout and counts it. Test `stalled_client_does_not_stall_bus_or_others` in Task 4.
3. **A model that answers in prose.** A command answered with prose and no tool call is the error `no_tool_call`, never parsed and never executed; a question may be answered in prose. Tests `prose_on_command_is_no_tool_call_error` and `prose_on_question_is_answer` in Task 11.
4. **A model that hallucinates a tool name.** The call never reaches the node; the model gets `{"error":"unknown","field":"tool","value":"<name>","known":[...]}` as the tool result and can correct itself. Test `unknown_tool_is_fed_back_with_known_tools` in Task 11; MCP side `hallucinated_tool_is_error_listing_known` in Task 9.
5. **The node restarting under a connected client.** A websocket subscriber gets `{"type":"closed","reason":"node shutting down"}` and close code 1001; its old stream id is 404 on the restarted node; an MCP server reports tool calls as errors while the node is down and recovers without restarting. Tests `shutdown_sends_closed_then_going_away_and_new_node_404s_old_id` in Task 4 and `mcp_survives_node_restart` in Task 9.

---

### Task 1: Crate scaffold and the model-boundary test

**Files:**
- Modify: `Cargo.toml` (workspace already uses `members = ["crates/*"]`; add nothing unless a `[workspace.dependencies]` table is introduced, which this plan does not need)
- Create: `crates/zeta-client/{Cargo.toml,src/lib.rs}`, `crates/zeta-ws/{Cargo.toml,src/lib.rs}`, `crates/zeta-mcp/{Cargo.toml,src/lib.rs,src/main.rs}`, `crates/zeta-agent/{Cargo.toml,src/lib.rs}`
- Test: `crates/zeta-agent/tests/boundary.rs`

**Interfaces:**
- Produces: four workspace members. Dependencies: `zeta-client` → `zeta-core`, `reqwest`, `serde`, `serde_json`, `thiserror`; `zeta-ws` → `zeta-core`, `zeta-client`, `axum` (features `ws`), `tokio`, `tokio-util`, `serde`, `serde_json`, `schemars`; feature `test-server` adds `zeta-source-file/test-fixtures`; `zeta-mcp` → `zeta-core`, `zeta-client`, `rmcp`, `clap` (`derive`, `env`), `tokio` (`io-std`); `zeta-agent` → `zeta-core`, `zeta-client`, `reqwest`, `async-trait`, `thiserror`, `tokio`; feature `test-fake` adds `axum`. All `edition.workspace`, `license.workspace`, `version.workspace`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/zeta-agent/tests/boundary.rs
//! Design 5.2: the runtime never calls a model from anywhere but zeta-agent.
use std::path::{Path, PathBuf};

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) { /* recursive walk, push *.rs */ }

#[test]
fn only_zeta_agent_speaks_the_llm_wire() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    rs_files(&crates, &mut files);
    for f in files.iter().filter(|f| f.components().any(|c| c.as_os_str() == "src")) {
        let text = std::fs::read_to_string(f).unwrap();
        if text.contains("chat/completions") || text.contains("chat_template_kwargs") {
            assert!(f.starts_with(crates.join("zeta-agent")), "{} speaks the LLM wire", f.display());
        }
    }
}

#[test]
fn core_has_no_transport_deps() {
    let toml = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../zeta-core/Cargo.toml")).unwrap();
    for banned in ["axum", "reqwest", "rmcp", "hyper", "tungstenite"] {
        assert!(!toml.contains(banned), "zeta-core must not depend on {banned}");
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p zeta-agent --test boundary` — Expected: error, package `zeta-agent` not found.
- [ ] **Step 3: Create the crates** with the manifests above and empty `lib.rs` files carrying a one-line `//!` doc each; `zeta-mcp/src/main.rs` is `fn main() {}` for now.
- [ ] **Step 4: Run tests** — `cargo test --workspace` — Expected: PASS, the two new tests included.
- [ ] **Step 5: Commit** — `git add -A && git commit -m "chore: zeta-client, zeta-ws, zeta-mcp and zeta-agent crates with model-boundary test"`

---

### Task 2: Secret, constant-time compare and the LAN address policy

**Files:**
- Create: `crates/zeta-ws/src/auth.rs`, `crates/zeta-ws/src/addr.rs`
- Modify: `crates/zeta-ws/src/lib.rs` (`pub mod auth; pub mod addr;`)
- Test: unit tests in both files

**Interfaces:**
- Produces (`auth.rs`):
  - `pub struct Secret(String)` with `pub fn new(s: impl Into<String>) -> Result<Self, String>` (empty → `Err("secret is empty")`), `pub fn matches(&self, presented: &[u8]) -> bool` (via `ct_eq`), `pub fn bearer(&self) -> String` (`"Bearer <s>"`); `impl Debug` prints `Secret(***)`; no `Display`, no `Serialize`.
  - `pub fn ct_eq(a: &[u8], b: &[u8]) -> bool`: `false` on length mismatch (length is not secret); otherwise OR-folds `x ^ y` over every byte pair and compares the fold with 0, so the time does not depend on where the first difference is. Hand-written (stdlib first; no `subtle` dependency).
- Produces (`addr.rs`):
  - `pub fn is_lan(ip: IpAddr) -> bool`: IPv4 loopback `127/8`, private `10/8`, `172.16/12`, `192.168/16`, link-local `169.254/16`, CGNAT `100.64/10` (Tailscale), unspecified `0.0.0.0`; IPv6 loopback, unspecified, ULA `fc00::/7`, link-local `fe80::/10`, and IPv4-mapped addresses judged by their IPv4 part.
  - `pub fn parse_listen(s: &str) -> Result<SocketAddr, String>`: a bare port binds `127.0.0.1:<port>`; otherwise `SocketAddr::from_str`; a non-LAN address → `Err("listen address <a> is not a LAN address; ZetaSDR is LAN only")`.

- [ ] **Step 1: Write the failing tests**

```rust
// auth.rs
#[test] fn ct_eq_cases() {
    assert!(ct_eq(b"abc", b"abc")); assert!(!ct_eq(b"abc", b"abd"));
    assert!(!ct_eq(b"abc", b"abcd")); assert!(ct_eq(b"", b""));
}
#[test] fn empty_secret_rejected() { assert_eq!(Secret::new("").unwrap_err(), "secret is empty"); }
#[test] fn secret_debug_is_redacted() { assert_eq!(format!("{:?}", Secret::new("s3cret").unwrap()), "Secret(***)"); }
#[test] fn secret_matches_only_itself() { let s = Secret::new("s3cret").unwrap(); assert!(s.matches(b"s3cret")); assert!(!s.matches(b"s3creT")); assert_eq!(s.bearer(), "Bearer s3cret"); }
// addr.rs
#[test] fn lan_ranges() {
    for ip in ["127.0.0.1","10.1.2.3","172.16.0.1","172.31.255.255","192.168.1.1","169.254.1.1","100.64.0.1","0.0.0.0","::1","::","fd00::1","fe80::1","::ffff:192.168.1.1"] { assert!(is_lan(ip.parse().unwrap()), "{ip}"); }
    for ip in ["8.8.8.8","172.32.0.1","100.128.0.1","2001:4860::8888","::ffff:8.8.8.8"] { assert!(!is_lan(ip.parse().unwrap()), "{ip}"); }
}
#[test] fn parse_listen_forms() {
    assert_eq!(parse_listen("7979").unwrap(), "127.0.0.1:7979".parse().unwrap());
    assert!(parse_listen("0.0.0.0:7979").is_ok()); assert!(parse_listen("192.168.1.5:80").is_ok());
    assert!(parse_listen("8.8.8.8:7979").unwrap_err().contains("LAN only"));
    assert!(parse_listen("nope").is_err());
}
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-ws` — Expected: compile errors, items missing.
- [ ] **Step 3: Implement** both files per the interfaces.
- [ ] **Step 4: Run tests** — `cargo test -p zeta-ws` — Expected: PASS.
- [ ] **Step 5: Commit** — `git commit -am "feat(ws): shared secret with constant-time compare and LAN address policy"`

---

### Task 3: HTTP API over `Node::apply`

**Files:**
- Modify: `crates/zeta-ws/src/lib.rs`
- Create: `crates/zeta-ws/src/http.rs`, `crates/zeta-ws/src/testing.rs` (behind feature `test-server`), `crates/zeta-client/src/lib.rs` (`parse_op` only, so the server and the agent share one malformed-op error)
- Test: `crates/zeta-ws/tests/http.rs` (dev-deps `reqwest`, `tempfile`, `zeta-ws` with `test-server`)

**Interfaces:**
- Consumes (exact, from `zeta-core`): `Node::apply(&self, op: Op) -> Result<OpResult, OpError>`; `Node::schema(&self) -> serde_json::Value` (keys `op`, `result`, `error`, `stages`); `Node::bus(&self) -> Arc<StreamBus>`; `StreamBus::list(&self) -> Vec<StreamInfo>`; `Node::open(cfg: NodeConfig, registry: StageRegistry, ports: Vec<Arc<dyn Port>>) -> Result<Node, NodeError>`; `NodeConfig { state_dir, ring_seconds, backends, resume }`; `zeta_core::VERSION`.
- Produces (`zeta_client`): `pub fn parse_op(v: serde_json::Value) -> Result<Op, OpError>` returning `OpError::Other { message: format!("invalid operation: {e}") }` with serde's message (which names an unknown or missing field).
- Produces (`zeta_ws`):
  - `pub struct Limits { pub body_bytes: usize, pub max_concurrent_ops: usize, pub max_ws: usize, pub op_timeout: Duration, pub ws_send_timeout: Duration, pub auth_fail_delay: Duration, pub ws_max_message_bytes: usize, pub tail_max_frames: u32, pub tail_max_timeout_ms: u32 }`; `Default` = `1 << 20`, `8`, `16`, `30 s`, `5 s`, `250 ms`, `64 << 10`, `100`, `10_000`.
  - `pub struct ServerConfig { pub listen: SocketAddr, pub secret: Secret, pub agents_md: String, pub limits: Limits }`.
  - `pub async fn bind(node: Arc<Node>, cfg: ServerConfig) -> std::io::Result<Server>` (non-LAN `listen` → `io::ErrorKind::InvalidInput`); `impl Server { pub fn local_addr(&self) -> SocketAddr; pub fn stats(&self) -> StatsHandle; pub async fn run(self, shutdown: CancellationToken) -> std::io::Result<()> }` (serves with `into_make_service_with_connect_info::<SocketAddr>()`, graceful on `shutdown`).
  - `#[derive(Clone)] pub struct StatsHandle` with `pub fn get(&self) -> ServerStats`; `pub struct ServerStats { pub ws_connections: usize, pub ws_dropped_slow: u64 }`.
  - `#[derive(Serialize, Deserialize)] pub struct Health { pub status: String, pub version: String, pub uptime_s: u64, pub ws_connections: usize }`.
  - Routes, all behind the auth middleware (`axum::middleware::from_fn_with_state`): peer not `is_lan` → 403; bearer missing, malformed or wrong → sleep `auth_fail_delay`, then 401 with `WWW-Authenticate: Bearer` and body `{"error":"other","message":"missing or wrong bearer secret"}`.
    - `GET /health` → 200 `Health` (`status: "ok"`).
    - `POST /op` → body read with `axum::body::to_bytes(body, limits.body_bytes)` (over → 413 `OpError::Other "body larger than N bytes"`); `try_acquire` on a `Semaphore(max_concurrent_ops)` (none free → 429 `OpError::Other "busy: N operations in flight"`); `parse_op` (error → 422); `tokio::time::timeout(op_timeout, node.apply(op))` (elapsed → 504 `OpError::Other "operation timed out after N s"`); `Ok` → 200 `OpResult`, `Err` → 422 `OpError`. Every non-2xx body is an `OpError` JSON.
    - `GET /schema` → 200 `node.schema()` verbatim.
    - `GET /streams` → 200 `Vec<StreamInfo>` sorted by id.
    - `GET /agents-md` → 200 `cfg.agents_md`, `content-type: text/markdown; charset=utf-8`.
  - `testing` (feature `test-server`): `pub struct TestOpts { pub secret: String, pub limits: Limits, pub addr: Option<SocketAddr>, pub agents_md: String }` (`Default`: `"s3cret"`, defaults, `None`, `"# test guide\n"`); `pub async fn spawn(dir: &Path) -> TestServer`; `pub async fn spawn_with(dir: &Path, opts: TestOpts) -> TestServer` opening a node on `dir/state` (`resume: true`, `Backend::Cpu`, `PassthroughStage`, one real-time looping `FileSource` `tone` from `zeta_source_file::fixtures::write_tone(dir)`); `pub struct TestServer { pub url: String, pub addr: SocketAddr, pub node: Arc<Node>, pub stats: StatsHandle, .. }` with `pub async fn stop(self)` (cancel, await `run`, drop the node).

- [ ] **Step 1: Write the failing tests**

```rust
// crates/zeta-ws/tests/http.rs
#[test] fn apply_future_is_send() { fn is_send<T: Send>(_: &T) {} async fn f(n: &zeta_core::node::Node) { let fut = n.apply(Op::SourceList {}); is_send(&fut); fut.await.ok(); } let _ = f; }
#[tokio::test] async fn health_with_secret_is_ok() { /* GET /health bearer s3cret -> 200, Health.status == "ok", version == zeta_core::VERSION */ }
#[tokio::test] async fn wrong_secret_is_401_and_op_not_applied() {   // Review Focus 1
    /* POST /op job_start{passthrough, tone, 100_000_000, 48_000} with "Bearer wrong" -> 401, body["error"] == "other";
       same with no Authorization header -> 401; with "Basic s3cret" -> 401; GET /schema with no header -> 401;
       then ts.node.apply(Op::JobList {}) == Ok(OpResult::Jobs(vec![])) */
}
#[tokio::test] async fn op_ok_is_200_with_result() { /* source_list -> 200, body["result"] == "sources", body["value"][0]["id"] == "tone" */ }
#[tokio::test] async fn op_error_is_422_with_suggestion() { /* source_ensure tone 100000/48000 -> 422, error "out_of_range", field "center_hz", suggested 100000000 */ }
#[tokio::test] async fn malformed_op_is_422_naming_field() { /* {"op":"source_ensure","source":"tone","centre_hz":1,"bandwidth_hz":1} -> 422, error "other", message contains "centre_hz" */ }
#[tokio::test] async fn oversized_body_is_413_json() { /* 2 MiB body -> 413, body["error"] == "other" */ }
#[tokio::test] async fn busy_is_429() { /* TestOpts{limits: Limits{max_concurrent_ops: 0, ..}} -> source_list 429, error "other", message starts "busy" */ }
#[tokio::test] async fn schema_is_node_schema_verbatim() { /* GET /schema == ts.node.schema() */ }
#[tokio::test] async fn streams_and_agents_md() { /* node.bus().register(Event, prov) -> GET /streams contains its id; GET /agents-md == "# test guide\n", content-type starts "text/markdown" */ }
#[tokio::test] async fn bind_refuses_non_lan() { /* bind(node, cfg{listen: "8.8.8.8:0"}) -> Err kind InvalidInput */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-ws --features test-server --test http`. If `apply_future_is_send` fails to compile, stop and report: the fix is an additive change in `zeta-core` and needs sign-off.
- [ ] **Step 3: Implement** `parse_op`, `Limits`, `ServerConfig`, `bind`, `Server`, the auth middleware (using Task 2), the five routes and `testing`. Handlers hold `Arc<Node>` in shared state; no handler matches on `Op` variants.
- [ ] **Step 4: Run tests** — `cargo test -p zeta-ws --features test-server` — Expected: PASS.
- [ ] **Step 5: Commit** — `git commit -am "feat(ws): authenticated HTTP API over Node::apply with Pi-sized limits"`

---

### Task 4: Streams over websockets, and tail

**Files:**
- Create: `crates/zeta-ws/src/wire.rs`, `crates/zeta-ws/src/stream.rs`
- Modify: `crates/zeta-ws/src/lib.rs` (routes, shutdown token handed to websocket tasks)
- Test: unit tests in `wire.rs`; `crates/zeta-ws/tests/stream.rs` (dev-dep `tokio-tungstenite`)

**Interfaces:**
- Consumes: `StreamBus::subscribe(&self, id: &StreamId) -> Option<broadcast::Receiver<Frame>>`, `StreamBus::register(&self, kind: StreamKind, provenance: Provenance) -> StreamId`, `StreamBus::publish(&self, frame: Frame)`, `StreamBus::unregister(&self, id: &StreamId)`; `Frame { stream, kind, provenance, payload }`; `Payload::{Iq(IqBlock), Audio { sample_rate_hz, samples }, Video, Text(String), Event(Value), Sink(IqBlock)}`; `Provenance { source, job, channel, span }`; `IqBlock { t0, sample_rate_hz, center_hz, samples }`.
- Produces (`wire.rs`), the shape the UI (plan 6) consumes and plan 2's `FrameSummary` converges on:
  - `#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)] pub struct WsFrame { pub stream: StreamId, pub kind: StreamKind, pub source: SourceId, pub job: Option<JobId>, pub channel: Option<ChannelId>, pub start_ns: Timestamp, pub end_ns: Timestamp, pub payload: serde_json::Value }`.
  - Payload rules (identical to plan 2's `FrameSummary`): Event → the value; Text → the string; Audio → `{"sample_rate_hz", "samples": n, "peak": max |s|}`; Iq and Sink → `{"sample_rate_hz", "center_hz", "samples": n}`; Video → `{}`. Sample data does not cross this wire (binary audio/IQ is plans 5 and 6).
  - `#[serde(tag = "type", rename_all = "snake_case")] pub enum WsMessage { Frame(WsFrame), Lagged { missed: u64 }, Closed { reason: String } }`.
  - `pub fn ws_frame(f: &Frame) -> WsFrame`.
- Produces (`stream.rs`):
  - `GET /stream/{id}` (websocket upgrade, `max_message_size(limits.ws_max_message_bytes)`): unknown id → 404 `OpError::Unknown { field: "stream", value, known }` (known sorted); no connection slot (`Semaphore(max_ws)`) → 503 `OpError::Other "too many stream connections"`. Per connection one task loops on `select!` over: shutdown token → send `Closed { reason: "node shutting down" }`, close 1001; `rx.recv()` → `Ok(frame)` sends `Frame(ws_frame)` under `timeout(ws_send_timeout)` (elapsed or error → drop the connection, `ws_dropped_slow += 1`), `Err(Lagged(n))` sends `Lagged { missed: n }`, `Err(Closed)` sends `Closed { reason: "stream ended" }` and close 1000; inbound `Close`/EOF/error → end. Inbound text and binary are ignored.
  - `pub async fn tail(bus: &StreamBus, id: &StreamId, max_frames: u32, timeout: Duration) -> Result<Vec<WsFrame>, OpError>` (subscribe, collect until `max_frames` or `timeout`, skip `Lagged`, stop on `Closed`).
  - `GET /streams/{id}/tail?max_frames=N&timeout_ms=T` → 200 `Vec<WsFrame>`; `max_frames` outside `1..=tail_max_frames` or `timeout_ms` outside `1..=tail_max_timeout_ms` → 422 `OpError::OutOfRange { field, value, min, max, suggested: None }`; unknown id → 404 as above.

- [ ] **Step 1: Write the failing tests**

```rust
// wire.rs
#[test] fn audio_and_iq_payloads_are_summarised() { /* Audio{48000, [0.5,-0.75]} -> {"sample_rate_hz":48000,"samples":2,"peak":0.75}; Iq 10 samples centre 100 MHz rate 48 kHz -> {"sample_rate_hz":48000,"center_hz":100000000,"samples":10}; Event(json!({"n":1})) -> {"n":1}; provenance.channel Some("c1") -> channel Some(ChannelId("c1")), start_ns/end_ns from span */ }
#[test] fn ws_message_is_type_tagged() { /* Lagged{missed:3} -> {"type":"lagged","missed":3}; Frame(f) has "type":"frame" and "stream" at top level */ }
// tests/stream.rs  (connect with tokio_tungstenite::connect_async on a Request carrying Authorization)
#[tokio::test] async fn ws_pushes_published_frames_as_wsframe() { /* register Event; connect; publish Event({"n":1}); first text -> WsMessage::Frame(f), f.payload == {"n":1}, f.kind == Event */ }
#[tokio::test] async fn ws_without_secret_is_401() { /* connect_async without header -> Err(Http(resp)) with status 401 */ }
#[tokio::test] async fn unknown_stream_is_404_listing_known() { /* /stream/nope upgrade -> 404; GET /streams/nope/tail?max_frames=1&timeout_ms=10 -> 404, error "unknown", field "stream", known contains the registered id */ }
#[tokio::test] async fn stalled_client_does_not_stall_bus_or_others() {   // Review Focus 2
    /* TestOpts limits.ws_send_timeout = 500 ms; register Event; `stalled` connects and never reads; `reader` reads in a task.
       Publish 4000 frames {"n": i, "pad": "x".repeat(16384)}, tokio::task::yield_now() every 100: the loop finishes within 2 s.
       reader sees WsMessage::Frame with payload n == 3999 within 10 s (Lagged notices allowed before it).
       Within 5 s ts.stats.get(): ws_connections == 1 and ws_dropped_slow == 1. */
}
#[tokio::test] async fn stream_end_sends_closed_then_close_1000() { /* connect; bus.unregister(id) -> Closed{reason:"stream ended"} then Close frame with CloseCode::Normal */ }
#[tokio::test] async fn shutdown_sends_closed_then_going_away_and_new_node_404s_old_id() {   // Review Focus 5
    /* connect to a registered stream; ts.stop() -> within 2 s Closed{reason:"node shutting down"} then Close CloseCode::Away;
       spawn_with(same dir, TestOpts{addr: Some(old addr), ..}) -> GET /streams/<old id>/tail?max_frames=1&timeout_ms=10 is 404 */
}
#[tokio::test] async fn tail_collects_until_max_or_timeout() { /* publisher task publishes 3 frames 50 ms after the request starts: max_frames=2&timeout_ms=1000 -> 2 frames; max 5, timeout 200, nothing published -> [] after >= 200 ms; max_frames=0 -> 422 out_of_range field "max_frames" */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-ws --features test-server`.
- [ ] **Step 3: Implement** `wire.rs`, `stream.rs` and the shutdown fan-out: `Server::run` passes a child of `shutdown` to each websocket task and cancels it before axum's graceful shutdown waits for connections.
- [ ] **Step 4: Run tests** — PASS. Run `stalled_client…` ten times (`cargo test -p zeta-ws --features test-server stalled -- --test-threads 1` in a loop) to check it is not flaky.
- [ ] **Step 5: Commit** — `git commit -am "feat(ws): websocket stream push with lag notices, slow-client drop and tail"`

---

### Task 5: `NodeClient`

**Files:**
- Create: `crates/zeta-client/src/remote.rs`
- Modify: `crates/zeta-client/src/lib.rs` (`pub mod remote; pub use remote::{NodeClient, ClientError};`)
- Test: `crates/zeta-client/tests/remote.rs` (dev-deps `zeta-ws` with `test-server`, `tokio`, `tempfile`)

**Interfaces:**
- Produces:
  - `pub struct NodeClient` (`Clone`), `impl NodeClient`:
    - `pub fn new(url: &str, secret: &str) -> Result<Self, ClientError>` (scheme must be `http`; trailing `/` tolerated; request timeout 60 s, longer than the server's 30 s op timeout);
    - `pub fn url(&self) -> &str`;
    - `pub async fn apply(&self, op: &Op) -> Result<Result<OpResult, OpError>, ClientError>`;
    - `pub async fn apply_value(&self, op: serde_json::Value) -> Result<Result<serde_json::Value, serde_json::Value>, ClientError>`;
    - `pub async fn schema(&self) -> Result<serde_json::Value, ClientError>`;
    - `pub async fn streams(&self) -> Result<Vec<StreamInfo>, ClientError>`;
    - `pub async fn tail(&self, stream: &StreamId, max_frames: u32, timeout_ms: u32) -> Result<Result<serde_json::Value, serde_json::Value>, ClientError>`;
    - `pub async fn agents_md(&self) -> Result<String, ClientError>`;
    - `pub async fn health(&self) -> Result<serde_json::Value, ClientError>`.
  - Status mapping: 2xx → `Ok(Ok(body))`; 401 → `Err(Unauthorized)`; any other status whose body is a JSON object with an `error` key → `Ok(Err(body))`; anything else → `Err(Http)`; connect/timeout errors → `Err(Unreachable)`.
  - `#[derive(Debug, thiserror::Error)] pub enum ClientError { #[error("node {url} unreachable: {detail}")] Unreachable { url: String, detail: String }, #[error("node {url} rejected the secret")] Unauthorized { url: String }, #[error("node {url} answered HTTP {status}: {body}")] Http { url: String, status: u16, body: String }, #[error("bad node url {0:?}: expected http://host:port")] BadUrl(String), #[error("decoding node response: {0}")] Decode(String) }`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test] async fn apply_roundtrips_ok_and_error() { /* apply(SourceList) -> Ok(Ok(OpResult::Sources(v))) with v[0].id == "tone"; apply(SourceEnsure tone 100000/48000) -> Ok(Err(OpError::OutOfRange{suggested: Some(100_000_000), ..})) */ }
#[tokio::test] async fn wrong_secret_is_unauthorized() { /* NodeClient::new(url, "wrong").apply(..) -> Err(Unauthorized{url}) */ }
#[tokio::test] async fn stopped_node_is_unreachable_naming_url() { /* ts.stop(); apply -> Err(Unreachable{url, ..}) with url == ts.url */ }
#[tokio::test] async fn schema_streams_agents_md_tail() { /* schema()["op"]["oneOf"] is an array; agents_md() == "# test guide\n"; register + publish 1 Event in a task; tail(id, 1, 1000) -> Ok(Ok(array of 1)); tail(bogus) -> Ok(Err(e)) with e["error"] == "unknown" */ }
#[test] fn parse_op_names_bad_field() { /* parse_op(json!({"op":"job_stop","jobb":"x"})) -> Err(Other{message}) containing "jobb" */ }
#[test] fn new_rejects_non_http_url() { /* new("ftp://x", "s") -> Err(BadUrl(_)) */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-client`.
- [ ] **Step 3: Implement** `remote.rs` with one private `async fn send(&self, req: RequestBuilder) -> Result<(u16, Bytes), ClientError>` that adds the bearer header and maps transport errors.
- [ ] **Step 4: Run tests** — PASS.
- [ ] **Step 5: Commit** — `git commit -am "feat(client): NodeClient for a running node over the authenticated HTTP API"`

---

### Task 6: Tools and system prompt generated from the schema and AGENTS.md

**Files:**
- Create: `crates/zeta-client/src/guide.rs`, `crates/zeta-client/src/tools.rs`
- Modify: `crates/zeta-client/src/lib.rs`
- Test: `crates/zeta-client/tests/tools.rs`

**Interfaces:**
- Consumes: `zeta_core::ops::schema() -> serde_json::Value` (tests); the committed `AGENTS.md`, whose operation sections are `### <op_name>` under `## Operations` (rendered by `crates/zeta/src/agents_md.rs::render(schema: &Value, field_craft: &str) -> String`).
- Produces (`guide.rs`):
  - `pub fn op_sections(md: &str) -> BTreeMap<String, String>`: for every `### <name>` heading between `## Operations` and the next `## ` heading, the text up to the next `### `/`## ` heading, trimmed, with lines starting `CLI: ` removed (tools are called as JSON).
  - `pub fn system_prompt(md: &str) -> String`: the `# ` title line and the paragraphs before the first `## `, then the `## Rules` and `## Field craft` sections verbatim with their headings, joined by one blank line; every other section (Running the node, Operations, Stage kinds) is dropped because the tools carry it.
- Produces (`tools.rs`):
  - `#[derive(Clone, Debug, PartialEq, Serialize)] pub struct ToolDef { pub name: String, pub description: String, pub input_schema: serde_json::Value }`.
  - `pub fn op_tools(schema: &Value, md: &str) -> Vec<ToolDef>`: one per `schema["op"]["oneOf"]` entry, in order. `name` = `properties.op.const`. `input_schema` = the variant with `properties.op` removed, `"op"` removed from `required`, the variant `description` removed, and every `{"$ref": "#/$defs/X"}` replaced by `schema["op"]["$defs"][X]` recursively (sibling keys of the `$ref` kept; depth limit 16, after which the `$ref` stays and `$defs` is attached). Nothing else is added or changed. `description` = `op_sections(md)[name]`, else the variant's description with newlines as spaces, else `name`.
  - `pub fn op_from_call(name: &str, args: Value, tools: &[ToolDef]) -> Result<Value, OpError>`: unknown `name` → `OpError::Unknown { field: "tool", value: name, known: <tool names> }`; `args` not an object → `OpError::Other { message: "arguments must be a JSON object" }`; else `args` with `"op": name` inserted (overwriting any `op` key).

- [ ] **Step 1: Write the failing tests**

```rust
const MD: &str = include_str!("../../../AGENTS.md");
#[test] fn every_op_has_a_section_in_committed_agents_md() { /* for each name in ops::schema()["op"]["oneOf"]: op_sections(MD).contains_key(name) */ }
#[test] fn tool_count_matches_op_variants() { /* op_tools(&schema, MD).len() == schema["op"]["oneOf"].len(); no hard-coded count, so plan 2's stream_tail is covered after merge */ }
#[test] fn tools_are_strict_and_ref_free() { /* every tool: additionalProperties == false; set(required) == set(properties keys); "op" not in properties; serialised schema contains no "$ref" */ }
#[test] fn source_ensure_tool_shape() { /* properties keys == {source, center_hz, bandwidth_hz}; center_hz["type"] == "integer"; description contains "Check a band" and "JSON: `{\"op\":\"source_ensure\"" and not "CLI: " */ }
#[test] fn job_start_kind_is_inlined_enum() { /* properties.kind.enum contains "passthrough" */ }
#[test] fn system_prompt_keeps_rules_and_field_craft_only() { /* contains "# ZetaSDR agent guide", "## Rules", "Call operations, do not explain.", "## Field craft"; contains neither "## Operations" nor "## Running the node" nor "### source_ensure" */ }
#[test] fn op_from_call_cases() { /* ("source_ensure", {"source":"tone","center_hz":1,"bandwidth_hz":1}) -> value with "op":"source_ensure"; ("tune", {}) -> Unknown{field:"tool", value:"tune", known contains "source_ensure"}; ("job_list", json!("x")) -> Other */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-client --test tools`.
- [ ] **Step 3: Implement** `guide.rs` (line-oriented scan, no Markdown parser dependency) and `tools.rs`.
- [ ] **Step 4: Run tests** — PASS.
- [ ] **Step 5: Commit** — `git commit -am "feat(client): tool definitions and system prompt generated from the schema and AGENTS.md"`

---

### Task 7: `zeta serve --listen`, `--node` and `--secret`

**Files:**
- Modify: `crates/zeta/src/cli.rs` (globals, `ServeArgs`, `Serve(ServeArgs)`), `crates/zeta/src/main.rs` (three lines), `crates/zeta/src/agents_md.rs` (add `pub fn rendering() -> String`, used by `run` and by serve), `crates/zeta/Cargo.toml` (clap `env` feature; deps `zeta-ws`, `zeta-client`, `tokio-util`; dev-deps `zeta-client`, `tokio` with `process`), `crates/zeta/tests/cli.rs` (helper `zeta()` gains `.env_remove("ZETA_NODE").env_remove("ZETA_SECRET")`)
- Create: `crates/zeta/src/transport.rs`, `crates/zeta/tests/common/mod.rs`
- Test: `crates/zeta/tests/transports.rs`

**Interfaces:**
- Consumes: `node_setup::open(state_dir: PathBuf, source_files: &[(String, PathBuf)], resume: bool) -> Result<Node, String>`; `serve_lock::acquire`/`owner`; `Command::into_op(self) -> Result<Option<Op>, OpError>`; `zeta_ws::{bind, ServerConfig, Limits, auth::Secret, addr::parse_listen}`; `zeta_client::NodeClient`.
- Produces (`cli.rs`):
  - On `Cli`: `#[arg(long, global = true, env = "ZETA_NODE", value_name = "URL")] pub node: Option<String>` ("Send the operation to a running `zeta serve` at URL instead of opening a local node") and `#[arg(long, global = true, env = "ZETA_SECRET", hide_env_values = true, value_name = "SECRET")] pub secret: Option<String>`.
  - `#[derive(Debug, Clone, Default, Args)] pub struct ServeArgs { #[arg(long, value_name = "[ADDR:]PORT")] pub listen: Option<String> }`; `Command::Serve(ServeArgs)`.
- Produces (`transport.rs`):
  - `pub async fn dispatch(cli: Cli) -> Result<ExitCode, Cli>`: handles, and returns `Ok(code)` for, (a) `--node` set: `serve` with `--node` → exit 1 `"serve does not take --node"`; `--source-file` or `--state-dir` with `--node` → exit 1 `"--source-file and --state-dir belong to the serving node; drop them with --node"`; no secret → exit 1 `"--node requires --secret or ZETA_SECRET"`; else `into_op`, `NodeClient::apply`, print the JSON result (exit 0) or `OpError` (exit 2), `ClientError` → `zeta: <error>` on stderr, exit 1; (b) `serve --listen` whose address fails `parse_listen` or whose secret is absent/empty → exit 1 before any state is touched (`"--listen requires --secret or ZETA_SECRET"`). Everything else returns `Err(cli)` untouched. The serve-lock refusal is skipped on the `--node` path: the serving node applies the op.
  - `pub async fn serve(node: Node, args: &ServeArgs, secret: Option<&str>) -> ExitCode`: without `listen`, today's behaviour (print `zeta: serving; Ctrl-C to stop`, wait for Ctrl-C). With it: `Secret::new`, `bind(Arc::new(node), ServerConfig { listen, secret, agents_md: agents_md::rendering(), limits: Limits::default() })`, print `zeta: listening on <local_addr>` on stderr (warn `zeta: warning: secret shorter than 12 characters` when it is), `run` until Ctrl-C cancels the token.
- `main.rs` changes, and only these: after the `AgentsMd` early return, `let cli = match transport::dispatch(cli).await { Ok(code) => return code, Err(cli) => cli };`; before `into_op`, `let (serve_args, secret) = (match &cli.command { cli::Command::Serve(a) => a.clone(), _ => Default::default() }, cli.secret.clone());`; the body of `let Some(op) = op else { … }` becomes `return transport::serve(node, &serve_args, secret.as_deref()).await;`.
- `tests/common/mod.rs`: `pub struct ServeProc { pub url: String, pub dir: PathBuf, child: std::process::Child }` with `pub fn start(dir: &Path, secret: &str) -> ServeProc` (spawns `zeta --state-dir <dir>/state --source-file tone=<fixture> serve --listen 127.0.0.1:0 --secret <secret>`, reads stderr lines until `zeta: listening on `, 10 s deadline) and `Drop` (kill, wait); `pub fn zeta_remote(url: &str, secret: &str, args: &[&str]) -> assert_cmd::Command` (with `env_remove` of both variables).

- [ ] **Step 1: Write the failing tests**

```rust
// crates/zeta/tests/transports.rs
#[test] fn remote_job_start_then_list_sees_running_job() { /* ServeProc; zeta_remote(job start --kind passthrough --source tone --center-hz 100000000 --bandwidth-hz 48000) exit 0 -> id; zeta_remote(job list) -> value contains {id, state "running"}; zeta_remote(stream list) -> some stream with provenance.job == id (the job runs inside the serving node, which a one-shot could never show) */ }
#[test] fn remote_op_error_exits_2_with_json() { /* source ensure --center-hz 100000 --bandwidth-hz 48000 -> exit 2, stdout suggested 100000000 */ }
#[test] fn remote_wrong_secret_exits_1() { /* secret "wrong" -> exit 1, stderr contains "rejected the secret"; job list via the right secret shows no job */ }   // Review Focus 1 (CLI)
#[test] fn remote_unreachable_exits_1_naming_url() { /* --node http://127.0.0.1:9 -> exit 1, stderr contains "unreachable" and "http://127.0.0.1:9" */ }
#[test] fn node_flag_rejects_local_flags() { /* --node u --secret s --source-file x=y source list -> exit 1; --node u without secret -> exit 1 "requires --secret" */ }
#[test] fn listen_requires_secret_and_lan() { /* serve --listen 127.0.0.1:0 without secret -> exit 1 "--listen requires --secret"; with --secret s and --listen 8.8.8.8:7979 -> exit 1 "LAN only"; neither leaves serve.lock behind */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta --test transports`.
- [ ] **Step 3: Implement** the flags, `transport.rs`, `agents_md::rendering`, the three `main.rs` lines and the test helper. `--secret` on the command line is visible in `ps`; the `--help` text for it says so and recommends `ZETA_SECRET`.
- [ ] **Step 4: Run tests** — `cargo test -p zeta` — Expected: PASS, plan-1 CLI tests unchanged.
- [ ] **Step 5: Commit** — `git commit -am "feat(cli): serve --listen and --node, so one-shot commands drive a running node"`

---

### Task 8: AGENTS.md documents the transports; skill generated from it

**Files:**
- Modify: `crates/zeta/src/agents_md.rs` (`RUNNING` text; `pub fn render_skill(md: &str) -> String`; `run(check: bool, skill: bool) -> ExitCode`), `crates/zeta/src/cli.rs` (`AgentsMd { check, skill }`), `crates/zeta/src/main.rs` only if the `AgentsMd` match arm must name the new field (`AgentsMd { check, skill }` in the existing early return)
- Create: `skills/zetasdr/SKILL.md` (generated)
- Regenerate: `AGENTS.md`
- Test: `crates/zeta/tests/transports.rs`

**Interfaces:**
- `RUNNING` gains, after the `zeta serve` bullet, exactly: "- Against a running node, `zeta --node URL --secret SECRET <group> <verb> ...` (or the `ZETA_NODE` and `ZETA_SECRET` variables): the operation is sent to that node over HTTP and applied there, so jobs started this way keep running; `--state-dir` and `--source-file` belong to the serving node and are rejected. Start that node with `zeta serve --listen PORT` (binds 127.0.0.1) or `--listen ADDR:PORT` (a LAN address) and a secret. `zeta-mcp --node URL` offers the same operations as MCP tools, and `zeta ask \"REQUEST\"` lets a local model call them." The serve-lock paragraph gains: "Use `--node` to change a node that `zeta serve` owns."
- `render_skill(md)` = `"---\nname: zetasdr\ndescription: Use when driving a ZetaSDR node (sources, jobs, IQ captures, classification, knowledge lookup) through the zeta CLI or the zeta-mcp tools.\n---\n\n"` + `md`. `zeta agents-md --skill` prints it; `--skill --check` compares with `skills/zetasdr/SKILL.md` exactly as `--check` compares `AGENTS.md`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn committed_skill_matches_rendering() { /* zeta agents-md --skill --check -> success */ }
#[test] fn skill_has_frontmatter_and_full_guide() { /* `agents-md --skill` stdout starts "---\nname: zetasdr\n" and ends with the full `agents-md` stdout */ }
#[test] fn guide_documents_transports() { /* agents-md contains "--node URL", "ZETA_SECRET", "zeta serve --listen", "zeta-mcp --node URL", "zeta ask" */ }
#[tokio::test] async fn serve_serves_committed_agents_md() { /* ServeProc; NodeClient::agents_md() == read_to_string(AGENTS.md): the guide behind every MCP description and system prompt is the committed one */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta --test transports`.
- [ ] **Step 3: Implement**, then `cargo run -q -p zeta -- agents-md > AGENTS.md` and `mkdir -p skills/zetasdr && cargo run -q -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md`.
- [ ] **Step 4: Run tests** — `cargo test -p zeta` and `cargo test -p zeta-client` (the section test re-reads `AGENTS.md`) — PASS.
- [ ] **Step 5: Commit** — `git commit -am "docs: AGENTS.md covers the transports; Claude Code skill generated from it"`

---

### Task 9: `zeta-mcp`

**Files:**
- Modify: `crates/zeta-mcp/src/lib.rs`, `crates/zeta-mcp/src/main.rs`
- Test: `crates/zeta-mcp/tests/stdio.rs` (dev-deps `zeta-ws` with `test-server`, `tokio` with `process`, `tempfile`)

**Interfaces:**
- Consumes: `NodeClient` (Task 5), `op_tools`, `op_from_call`, `system_prompt` (Task 6).
- Produces (`lib.rs`):
  - `pub struct ZetaMcp { client: NodeClient }` with `pub fn new(client: NodeClient) -> Self`; private `async fn tools(&self) -> Vec<ToolDef>` = `op_tools(node schema, node agents-md)` when the node answers, else `op_tools(&zeta_core::ops::schema(), FALLBACK_MD)` where `const FALLBACK_MD: &str = include_str!("../../../AGENTS.md")` (same build, same vocabulary; keeps the tool list available while the node restarts); private `async fn call(&self, name: &str, args: Value) -> (bool, Value)` returning `(is_error, json)`: `op_from_call` error → `(true, error)`; `apply_value` `Ok(Ok(v))` → `(false, v)`, `Ok(Err(e))` → `(true, e)`, `Err(ClientError)` → `(true, {"error":"other","message": <client error>})`.
  - `impl rmcp::ServerHandler for ZetaMcp`: `get_info` (server name `zeta-mcp`, version `zeta_core::VERSION`, capabilities tools and resources, `instructions` = `system_prompt(md)`); `list_tools` (each `ToolDef` → `rmcp::model::Tool { name, description, input_schema, .. }`); `call_tool` (result as one text content block holding the JSON, `is_error` set; never a JSON-RPC error for an operation failure, so the model sees `suggested`); `list_resources` (`zeta://sources`, `zeta://jobs`, `zeta://streams`, `zeta://schema`, `zeta://agents-md`, plus `zeta://streams/<id>` per current stream when the node answers); `read_resource` (`sources` → `source_list` result JSON, `jobs` → `job_list`, `streams` → `GET /streams`, `schema` → `GET /schema`, `agents-md` → `text/markdown`, `streams/<id>` → `tail(id, 20, 2000)`; unknown URI → resource-not-found error).
  - rmcp 3.5 names used: `ServerHandler`, `CallToolRequestParams`, `CallToolResponse` (`From<CallToolResult>`), `CallToolResult::success`, `ListToolsResult`, `Tool`, `ListResourcesResult`, `ReadResourceRequestParams`, `ReadResourceResponse` (`From<ReadResourceResult>`), `ResourceContents::text`, `ServiceExt::serve`, `transport::stdio()`. Constructor spellings may need adjusting against `cargo doc -p rmcp`; the tests pin the wire, not the API. If rmcp's handler API cannot serve a tool list computed at runtime, replace it with a hand-written newline-delimited JSON-RPC loop in `src/jsonrpc.rs` covering `initialize`, `notifications/initialized`, `ping`, `tools/list`, `tools/call`, `resources/list`, `resources/read`; the tests do not change.
- Produces (`main.rs`): `zeta-mcp --node <URL> --secret <SECRET>` (clap, `env = "ZETA_NODE"` / `"ZETA_SECRET"`, both required); serves stdio until stdin closes; logs only to stderr.

- [ ] **Step 1: Write the failing tests**

```rust
// helper: struct Mcp { child, stdin, lines } spawning env!("CARGO_BIN_EXE_zeta-mcp") with --node ts.url --secret s3cret;
// fn request(&mut self, id, method, params) -> Value reads lines until "id" matches; fn notify(&mut self, method)
// server: spawn_with(dir, TestOpts{agents_md: include_str!("../../../AGENTS.md").into(), ..})
#[tokio::test] async fn initialize_lists_tools_from_schema() { /* initialize {protocolVersion:"2025-06-18", capabilities:{}, clientInfo:{name:"t",version:"0"}} -> serverInfo.name == "zeta-mcp", capabilities.tools present; notify initialized; tools/list names == op names of ts.node.schema() in order; every inputSchema.additionalProperties == false; source_ensure description == op_sections(AGENTS.md)["source_ensure"] */ }
#[tokio::test] async fn tools_call_ok_and_corrective_error() { /* source_list -> isError false, content[0].text parses with result "sources"; source_ensure {tone, 100000, 48000} -> isError true, text has error "out_of_range" and suggested 100000000 */ }
#[tokio::test] async fn hallucinated_tool_is_error_listing_known() { /* tools/call "tune" -> isError true, text {"error":"unknown","field":"tool","value":"tune", known contains "source_ensure"} */ }   // Review Focus 4 (MCP)
#[tokio::test] async fn resources_list_and_read() { /* resources/list contains the five zeta:// URIs; read zeta://agents-md text == committed AGENTS.md; read zeta://jobs text parses with result "jobs" */ }
#[tokio::test] async fn mcp_survives_node_restart() {   // Review Focus 5
    /* source_list ok; ts.stop(); source_list -> isError true, message contains "unreachable"; tools/list still returns every tool (fallback);
       spawn_with(same dir, addr: Some(old addr)); source_list -> isError false; the zeta-mcp process was never restarted */
}
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-mcp`.
- [ ] **Step 3: Implement** `lib.rs` and `main.rs`.
- [ ] **Step 4: Run tests** — PASS.
- [ ] **Step 5: Commit** — `git commit -am "feat(mcp): stdio MCP server exposing every operation as a tool over the node HTTP API"`

---

### Task 10: OpenAI-compatible chat client with readiness

**Files:**
- Create: `crates/zeta-agent/src/llm.rs`, `crates/zeta-agent/src/wire.rs`, `crates/zeta-agent/src/fake.rs` (feature `test-fake`), `crates/zeta-agent/examples/fake_llm.rs` (requires `test-fake`)
- Modify: `crates/zeta-agent/src/lib.rs`
- Test: `crates/zeta-agent/tests/llm.rs`, unit tests in `wire.rs` and `llm.rs`

**Interfaces:**
- Produces (`wire.rs`; tolerant decoding: unknown fields such as `provider_specific_fields`, `timings`, `usage` are ignored):
  - `#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)] pub struct ChatMessage { pub role: String, #[serde(default, skip_serializing_if = "Option::is_none")] pub content: Option<String>, #[serde(default, skip_serializing_if = "Vec::is_empty")] pub tool_calls: Vec<ToolCall>, #[serde(default, skip_serializing_if = "Option::is_none")] pub tool_call_id: Option<String> }` with `fn system(s)`, `fn user(s)`, `fn tool(id, content)` constructors.
  - `pub struct ToolCall { pub id: String, #[serde(rename = "type", default = "function_kind")] pub kind: String, pub function: FunctionCall }`; `pub struct FunctionCall { pub name: String, pub arguments: serde_json::Value }` with `pub fn args(&self) -> Result<Value, String>` (a string is parsed as JSON, as llama-server sends it; an object is taken as is; anything else or unparsable → `Err("arguments are not a JSON object: <detail>")`).
  - Response: `choices[0].message` with `content`, `tool_calls`, `reasoning_content`; `choices[0].finish_reason`.
- Produces (`llm.rs`):
  - `pub struct LlmConfig { pub url: String, pub model: String, pub key: Option<String>, pub max_tokens: u32, pub temperature: f32, pub thinking: bool, pub status_url: Option<String>, pub ready_timeout: Duration, pub request_timeout: Duration, pub backoff_initial: Duration, pub backoff_max: Duration }`; `pub fn new(url: impl Into<String>, model: impl Into<String>) -> Self` with defaults `None`, `1024`, `0.0`, `false`, `None`, `600 s` (model loads from SD took four to six minutes, design 10), `300 s` (probe latencies reached 142 s), `1 s`, `15 s`; `pub fn endpoint(&self) -> String`: the URL itself if it ends in `/chat/completions`; else trimmed of `/`, plus `/v1` unless it already ends in `/v1`, plus `/chat/completions`.
  - `pub struct Completion { pub content: Option<String>, pub tool_calls: Vec<ToolCall>, pub finish_reason: Option<String>, pub reasoning: Option<String> }`.
  - `pub struct LlmClient` with `pub fn new(cfg: LlmConfig) -> Result<Self, AgentError>`, `pub fn request_body(&self, messages: &[ChatMessage], tools: &[ToolDef]) -> serde_json::Value`, `pub async fn wait_ready(&self) -> Result<(), AgentError>`, `pub async fn complete(&self, messages: &[ChatMessage], tools: &[ToolDef]) -> Result<Completion, AgentError>`.
  - Request body: `model`, `messages`, `tools: [{"type":"function","function":{"name","description","parameters": input_schema}}]`, `tool_choice: "auto"`, `max_tokens`, `temperature`, and, unless `thinking`, `chat_template_kwargs: {"enable_thinking": false}`. `Authorization: Bearer <key>` when `key` is set.
  - Readiness (design 10, "a sequence, not a flag"): `wait_ready` polls `status_url` (if set) until a JSON body with `"healthy": true`, tolerating HTTP errors, timeouts, non-JSON and `healthy: false` in between, recording the last as `last`; `complete` retries HTTP 503 (any body; llama-server says `Loading model`) and connection errors with backoff doubling from `backoff_initial` to `backoff_max`, until `ready_timeout` from the first attempt. Either deadline → `AgentError::NotReady { waited_s, last }`. Any other non-2xx → `AgentError::Llm { status, body }`, no retry. `finish_reason == "length"` with no tool calls → `AgentError::TokenBudget { max_tokens }` (the probe's reasoning overrun).
  - `#[derive(Debug, Serialize, thiserror::Error)] #[serde(tag = "error", rename_all = "snake_case")] pub enum AgentError { NoToolCall { content: String }, TokenBudget { max_tokens: u32 }, TurnLimit { turns: u32 }, NotReady { waited_s: u64, last: String }, Llm { status: u16, body: String }, Unreachable { url: String, detail: String }, BadResponse { detail: String }, Node { detail: String } }` with `#[error]` messages naming the remedy (`TokenBudget`: "raise --llm-max-tokens or turn thinking off"; `NoToolCall`: "the model answered in prose instead of calling a tool").
- Produces (`fake.rs`, feature `test-fake`): `pub struct FakeLlm { pub url: String, pub requests: Arc<Mutex<Vec<Value>>> }` with `pub async fn start(chat: Vec<(u16, Value)>, status: Vec<(u16, Value)>) -> FakeLlm` (axum on `127.0.0.1:0`; `POST /v1/chat/completions` records the body and answers the next canned pair, `500 {"error":"fake exhausted"}` after the last; `GET /status` answers the next, repeating the last); `pub fn tool_call_response(calls: &[(&str, Value)]) -> Value` (OpenAI shape: `finish_reason "tool_calls"`, `content ""`, ids `call_<i>`, `arguments` as a JSON string, and a `reasoning_content` string, as llama-server emits for thinking models); `pub fn text_response(content: &str, finish_reason: &str) -> Value`; `pub struct RecordingExecutor` (Task 11). `examples/fake_llm.rs <ADDR>` serves `[tool_call(source_list), text("tone")]` in a loop for the by-hand slice.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn endpoint_forms() { /* "http://h:8080" and "http://h:8080/v1" -> "http://h:8080/v1/chat/completions"; ".../v1/chat/completions" unchanged; "http://h:4000/" -> "http://h:4000/v1/chat/completions" */ }
#[test] fn arguments_as_string_or_object() { /* json!("{\"a\":1}") and json!({"a":1}) -> Ok({"a":1}); json!("not json") -> Err */ }
#[tokio::test] async fn request_disables_thinking_and_sets_budget() { /* recorded body: chat_template_kwargs.enable_thinking == false, max_tokens == 1024, temperature == 0.0, tool_choice == "auto", tools[0].type == "function", tools[i].function.parameters == tools_in[i].input_schema; with thinking: true the body has no chat_template_kwargs */ }
#[tokio::test] async fn reasoning_content_is_tolerated() { /* tool_call_response with reasoning_content -> Completion.tool_calls.len() == 1, reasoning.is_some() */ }
#[tokio::test] async fn loading_503_is_waited_out() { /* chat [(503, {"error":{"code":503,"message":"Loading model"}}) x2, (200, tool_call)], backoff 10 ms -> Ok; requests.len() == 3 */ }
#[tokio::test] async fn readiness_timeout_is_not_ready() { /* always 503, ready_timeout 100 ms, backoff 10 ms -> Err(NotReady{last contains "Loading model"}) */ }
#[tokio::test] async fn status_error_then_healthy_is_ready() { /* status [(500, {"error":"timeout"}), (200, {"phase":"warming","healthy":false}), (200, {"phase":"idle","healthy":true})] -> wait_ready Ok after exactly three polls */ }
#[tokio::test] async fn length_without_tool_call_is_token_budget() { /* text_response("", "length") -> Err(TokenBudget{max_tokens: 1024}) */ }
#[tokio::test] async fn http_400_is_llm_error_not_retried() { /* (400, {"error":"bad"}) -> Err(Llm{status: 400, ..}); requests.len() == 1 */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-agent --features test-fake`.
- [ ] **Step 3: Implement** `wire.rs`, `llm.rs`, `fake.rs`, the example.
- [ ] **Step 4: Run tests** — PASS; `cargo test -p zeta-agent --test boundary` still PASS (the wire strings live in `zeta-agent`).
- [ ] **Step 5: Commit** — `git commit -am "feat(agent): OpenAI-compatible chat client with thinking off, token budget and readiness sequence"`

---

### Task 11: The agent loop

**Files:**
- Create: `crates/zeta-agent/src/agent.rs`, `crates/zeta-agent/src/exec.rs`
- Modify: `crates/zeta-agent/src/lib.rs`, `crates/zeta-agent/src/fake.rs` (`RecordingExecutor`)
- Test: `crates/zeta-agent/tests/agent.rs` (dev-deps `zeta-source-file` with `test-fixtures`, `tempfile`), `crates/zeta-agent/tests/live.rs`

**Interfaces:**
- Produces (`exec.rs`):
  - `#[async_trait] pub trait Executor: Send + Sync { async fn apply(&self, op: serde_json::Value) -> Result<Result<serde_json::Value, serde_json::Value>, AgentError>; }`: `Ok(Ok(result))`, `Ok(Err(op_error))`, and `Err(AgentError::Node)` only when the node itself cannot be reached (the loop aborts; that is not the model's fault).
  - `pub struct LocalExecutor(pub Arc<Node>)`: `parse_op` → `Node::apply` → `serde_json::to_value`.
  - `pub struct RemoteExecutor(pub NodeClient)`: `apply_value`; `ClientError` → `AgentError::Node { detail }`.
  - (`fake.rs`) `pub struct RecordingExecutor { pub calls: Mutex<Vec<Value>>, .. }` with `pub fn new(answers: Vec<(&str, Result<Value, Value>)>) -> Self` keyed by op name; unlisted ops answer `Ok({"result":"ok"})`.
- Produces (`agent.rs`):
  - `pub struct AgentConfig { pub system_prompt: String, pub tools: Vec<ToolDef>, pub max_turns: u32 }` (`max_turns` default 8).
  - `pub struct Agent<E: Executor> { .. }` with `pub fn new(llm: LlmClient, exec: E, cfg: AgentConfig) -> Self` and `pub async fn run(&self, request: &str) -> Result<Transcript, AgentFailure>`.
  - `#[derive(Debug, Serialize)] pub struct Transcript { pub request: String, pub answer: Option<String>, pub calls: Vec<CallRecord>, pub turns: u32 }`; `pub struct CallRecord { pub tool: String, pub arguments: Value, pub ok: bool, pub result: Value }`; `pub struct AgentFailure { #[serde(flatten)] pub error: AgentError, pub transcript: Transcript }`.
  - `pub fn is_question(request: &str) -> bool`: trimmed text ends with `?`, or its first word, lowercased and stripped of punctuation, is one of `what which where when who whom whose why how is are was were do does did can could will would should has have any`.
  - Loop: `wait_ready`; messages `[system(system_prompt), user(request)]`; for each turn up to `max_turns`: `complete`; if there are tool calls, push the assistant message (`content` and `tool_calls` only; `reasoning_content` is never sent back), then for each call in order: `args()` error or `op_from_call` error becomes the result without touching the node, otherwise `exec.apply`; push `tool(id, <result JSON string>)`; record a `CallRecord`. With no tool calls: if the trimmed content is non-empty and (at least one call has been made or `is_question(request)`), it is the answer; otherwise `AgentError::NoToolCall { content }` (design 10: a model that emits no tool calls is unusable; its prose is never parsed). Turns exhausted → `TurnLimit`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn is_question_cases() { /* true: "What radios do I have connected?", "can you hear any weather balloons right now", "  how many jobs"; false: "Tune hackrf0 to 101.5 MHz with 200 kHz bandwidth.", "start decoding ADS-B on hackrf0." */ }
#[tokio::test] async fn tool_call_then_prose_is_answer() { /* chat [tool_call(source_list,{}), text("One source: tone.","stop")]; RecordingExecutor -> answer Some("One source: tone."), calls[0].tool == "source_list", turns == 2;
   second request's messages == [system, user, assistant with tool_calls, tool{tool_call_id "call_0", content == JSON of the executor's answer}] and no message carries "reasoning_content" */ }
#[tokio::test] async fn op_error_is_fed_back_so_model_can_correct() { /* LocalExecutor over a Node with the tone FileSource; chat [tool_call(source_ensure,{tone,100000,48000}), tool_call(source_ensure,{tone,100000000,48000}), text("Tuned.","stop")]
   -> calls[0].ok == false, calls[0].result["suggested"] == 100000000; calls[1].ok == true; the second request's last message content contains "\"suggested\":100000000" */ }
#[tokio::test] async fn unknown_tool_is_fed_back_with_known_tools() {   // Review Focus 4
    /* chat [tool_call("tune", {"source":"tone","center_hz":1,"bandwidth_hz":1}), tool_call(source_list,{}), text("Done.","stop")]
       -> executor.calls.len() == 1 (tune never reached the node); calls[0].result == {"error":"unknown","field":"tool","value":"tune","known":[...]} with known containing "source_ensure"; answer Some */ }
#[tokio::test] async fn prose_on_command_is_no_tool_call_error() {   // Review Focus 3
    /* request "Tune hackrf0 to 101.5 MHz with 200 kHz bandwidth."; chat [text("hackrf0.set_frequency(101.5e6)","stop")]
       -> Err(AgentFailure{error: NoToolCall{content contains "set_frequency"}, transcript.calls empty}); executor.calls empty */ }
#[tokio::test] async fn prose_on_question_is_answer() { /* request "What radios do I have connected?"; chat [text("None that I can see.","stop")] -> Ok, answer Some */ }
#[tokio::test] async fn malformed_arguments_are_fed_back() { /* tool call with arguments "{not json" -> calls[0].ok == false, result["error"] == "other", message contains "arguments"; the next canned tool call still runs */ }
#[tokio::test] async fn turn_limit_stops_loop() { /* max_turns 3; chat 3x tool_call(job_list) -> Err(TurnLimit{turns: 3}); requests.len() == 3 */ }
// tests/live.rs
#[tokio::test]
#[ignore = "needs field-host:8080 serving gemma-4-12b"]
async fn live_llm_tunes_a_source() {
    /* url = env ZETA_LIVE_LLM_URL or "http://field-host:8080", model "gemma-4-12b";
       tools = op_tools(&zeta_core::ops::schema(), AGENTS_MD); system = system_prompt(AGENTS_MD);
       RecordingExecutor answering source_list with one source "hackrf0" (tune 1 MHz..6 GHz, max bandwidth 20 MHz) and source_ensure with a Tuning;
       run("tune hackrf0 to 101.5 MHz with 200 kHz bandwidth") (Ok or Err) ->
       executor.calls contains {"op":"source_ensure","source":"hackrf0","center_hz":101500000,"bandwidth_hz":200000} */
}
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-agent --features test-fake --test agent`.
- [ ] **Step 3: Implement** `exec.rs`, `agent.rs`, `RecordingExecutor`.
- [ ] **Step 4: Run tests** — PASS; `cargo test -p zeta-agent --features test-fake --test live -- --ignored` if `field-host` answers, recording the outcome in the report either way.
- [ ] **Step 5: Commit** — `git commit -am "feat(agent): tool loop with corrective-error feedback, prose-answer detection and turn limit"`

---

### Task 12: `zeta ask`

**Files:**
- Create: `crates/zeta/src/ask.rs`
- Modify: `crates/zeta/src/cli.rs` (`Ask(AskArgs)`; `into_op` returns `None` for it), `crates/zeta/src/transport.rs` (`dispatch` routes `Ask`), `crates/zeta/Cargo.toml` (dep `zeta-agent`; dev-dep `zeta-agent` with `test-fake`)
- Test: `crates/zeta/tests/ask.rs`

**Interfaces:**
- Produces (`cli.rs`): `#[derive(Debug, Clone, Args)] pub struct AskArgs { /// The request; omit it or pass "-" to read it from stdin  pub request: Option<String>, #[arg(long, env = "ZETA_LLM_URL")] pub llm_url: String, #[arg(long, env = "ZETA_LLM_MODEL")] pub llm_model: String, #[arg(long, env = "ZETA_LLM_KEY", hide_env_values = true)] pub llm_key: Option<String>, #[arg(long, default_value_t = 1024)] pub llm_max_tokens: u32, #[arg(long)] pub llm_thinking: bool, #[arg(long)] pub llm_status_url: Option<String>, #[arg(long, default_value_t = 600)] pub llm_ready_timeout_s: u64, #[arg(long, default_value_t = 8)] pub max_turns: u32 }`.
- Produces (`ask.rs`): `pub async fn run(args: AskArgs, remote: Option<NodeClient>, state_dir: PathBuf, source_files: &[(String, PathBuf)]) -> ExitCode`. Remote (`--node`): schema and guide from `client.schema()` / `client.agents_md()`, `RemoteExecutor`. Local: refused with exit 1 `"zeta serve (pid N) owns this state dir; use --node"` while `serve_lock::owner` is `Some`; otherwise `node_setup::open(.., resume: false)`, guide `agents_md::rendering()`, schema `node.schema()`, `LocalExecutor`. Request from the argument, or from stdin when absent or `-` (so `local-voice`'s transcript can be piped in, design 5.4); empty → exit 1 `"empty request"`. `Ok(transcript)` → JSON on stdout, exit 0; `Err(failure)` → JSON on stdout, exit 1.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test(flavor = "multi_thread")] async fn ask_local_lists_sources_via_fake_llm() { /* FakeLlm [tool_call(source_list,{}), text("tone","stop")]; `zeta --state-dir … --source-file tone=… ask "what sources are there?" --llm-url <fake> --llm-model fake` (std::process in spawn_blocking) -> exit 0; stdout calls[0].tool == "source_list", calls[0].result.value[0].id == "tone", answer == "tone" */ }
#[tokio::test(flavor = "multi_thread")] async fn ask_reads_request_from_stdin() { /* no positional, stdin "what sources are there?\n" -> fake.requests[0].messages[1].content == "what sources are there?" */ }
#[tokio::test(flavor = "multi_thread")] async fn ask_prose_on_command_exits_1() { /* FakeLlm [text("ok","stop")]; "tune tone to 100 MHz" -> exit 1, stdout error == "no_tool_call" */ }
#[tokio::test(flavor = "multi_thread")] async fn system_prompt_and_tools_come_from_agents_md() { /* fake.requests[0]: messages[0].content == system_prompt(committed AGENTS.md); tool names == op names of ops::schema() */ }
#[tokio::test(flavor = "multi_thread")] async fn ask_remote_starts_job_on_serving_node() { /* ServeProc; FakeLlm [tool_call(job_start,{kind:"passthrough",source:"tone",center_hz:100000000,bandwidth_hz:48000}), text("Started.","stop")];
   `zeta --node <url> --secret s3cret ask "start a passthrough job on tone at 100 MHz, 48 kHz wide" …` -> exit 0; zeta_remote(job list) shows one job, state "running" */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta --test ask`.
- [ ] **Step 3: Implement** `ask.rs`, the `AskArgs` flags and the `dispatch` route.
- [ ] **Step 4: Run tests** — `cargo test -p zeta` PASS; `zeta agents-md --check` still PASS (no schema change).
- [ ] **Step 5: Commit** — `git commit -am "feat(cli): zeta ask runs a local model against the node's operations"`

---

### Task 13: Whole-workspace verification, README, plan series

**Files:**
- Modify: `README.md` (a "Transports" section: `serve --listen`, `--node`, the bearer secret and `ZETA_SECRET`, the HTTP routes and `WsMessage` shape, `zeta-mcp` registration for Claude Code as `claude mcp add zeta -e ZETA_SECRET=… -- zeta-mcp --node http://127.0.0.1:7979`, installing `skills/zetasdr`, `zeta ask` with `--llm-url`/`--llm-model`, and one line: "`zeta ask` reads its request from stdin, so a `local-voice` transcript can be piped in"), `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (plan 3 delivered; note the live-model test result)

- [ ] **Step 1: Run** `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` — PASS, no warnings. Also `cargo test --workspace --all-features` (pulls in `test-server` and `test-fake`).
- [ ] **Step 2: By-hand slice**, pasting outputs into the report:

```bash
cargo build -q --workspace --all-features
cargo run -q -p zeta-source-file --features test-fixtures --example gen_tone -- scratch/fx
A="--state-dir scratch/state3 --source-file tone=scratch/fx/tone.sigmf-meta"
./target/debug/zeta $A serve --listen 127.0.0.1:7979 --secret s          # terminal 1
B="--node http://127.0.0.1:7979 --secret s"                               # terminal 2
./target/debug/zeta $B job start --kind passthrough --source tone --center-hz 100000000 --bandwidth-hz 48000
./target/debug/zeta $B job list                                           # state "running", in the serving node
./target/debug/zeta $B stream list                                        # the job's stream
curl -s -H 'Authorization: Bearer s' http://127.0.0.1:7979/health
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:7979/health     # 401
websocat -H 'Authorization: Bearer s' ws://127.0.0.1:7979/stream/<id>     # if installed: {"type":"frame",…} lines
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"sh","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | ZETA_SECRET=s ./target/debug/zeta-mcp --node http://127.0.0.1:7979   # one tool per operation
./target/debug/examples/fake_llm 127.0.0.1:8099 &                         # canned: source_list, then "tone"
./target/debug/zeta $B ask "what sources are there?" --llm-url http://127.0.0.1:8099 --llm-model fake
echo "what sources are there?" | ./target/debug/zeta $B ask --llm-url http://127.0.0.1:8099 --llm-model fake
```

If `field-host` answers, also `./target/debug/zeta $B ask "tune tone to 100 MHz with 48 kHz bandwidth" --llm-url http://field-host:8080 --llm-model gemma-4-12b` and `cargo test -p zeta-agent --all-features --test live -- --ignored`.
- [ ] **Step 3: Commit** — `git commit -am "docs: README transports walkthrough; plan series updated"`

---

## Self-review notes

- Spec coverage: design 3 LAN-only and shared secret on every transport (Tasks 2, 3, 4, 7, 9), OpenAI-compatible wire as the only model wire (10, boundary test in 1), tiny-model friendliness (6 strict ref-free tool schemas, 10 thinking off and budget, 11 corrective errors fed back and prose never parsed); 4.8 a socket speaking the operation set and typed streams for external processes (3, 4); 5.2 every transport shipped together: CLI against a running node (7), MCP (9), HTTP/websocket (3, 4), in-process agent (10, 11, 12), each a thin adapter over `Node::apply`; 5.3 AGENTS.md as the single source of MCP descriptions, MCP instructions, the agent system prompt and the Claude Code skill, with drift tests (6, 8, 9, 12); 5.4 voice by stdin (12, README); 7 the UI as a websocket client, with `WsFrame`/`WsMessage` defined here (4); 8 item 4 (the same operation through MCP and through the CLI hits `POST /op` → `Node::apply`; the GUI half is plan 6) and item 5 (CLI on a running node, 7); 10 thinking off, `max_tokens`, readiness as a sequence (status poll tolerating error-then-healthy, 503 as wait), no-tool-call as an error, corrective errors (10, 11).
- Deferred, by design or by the series: CLI help generated from AGENTS.md (5.3 lists it; plan 1 did not do it either; one later task for both); browser websocket authentication (a browser cannot set `Authorization` on a websocket; plan 6 adds a `Sec-WebSocket-Protocol` token); binary audio and IQ over the websocket (plans 5 and 6); an operation log stream for the transcript pane (plan 6); federation authentication (plan 5); per-caller rights (design 3: not built); tool search (14 tools after plan 2 is still small).
- Decisions taken here: `zeta-client` is a fourth crate because the CLI, MCP server and agent all need the client, `parse_op` and the tool generator, and none of them should pull in the server; `--listen` takes `PORT` (binds 127.0.0.1) or `ADDR:PORT` instead of a separate `--bind`; `/health` also needs the secret ("every request"); `/agents-md` is an extra route so `zeta-mcp` on another host serves the node's own guide; MCP tool schemas inline `$ref`s, because the tool must stand alone and small models handle `$ref` badly, so "verbatim" means nothing is added or reworded; `zeta-mcp` falls back to its compiled-in schema and AGENTS.md for `tools/list` while the node is down; the system prompt is AGENTS.md's intro, rules and field craft only, since the tools carry the operation sections and a 4B model on a Pi pays for every prompt token.
- Names used across tasks checked: `Secret`, `ct_eq`, `is_lan`, `parse_listen`, `Limits`, `ServerConfig`, `bind`, `Server::{local_addr, stats, run}`, `StatsHandle`, `ServerStats`, `Health`, `testing::{spawn, spawn_with, TestOpts, TestServer}`, `WsFrame`, `WsMessage`, `ws_frame`, `tail`, `parse_op`, `NodeClient`, `ClientError`, `op_sections`, `system_prompt`, `ToolDef`, `op_tools`, `op_from_call`, `ServeArgs`, `transport::{dispatch, serve}`, `agents_md::{rendering, render_skill}`, `ZetaMcp`, `LlmConfig`, `LlmClient`, `Completion`, `ChatMessage`, `ToolCall`, `FunctionCall::args`, `AgentError`, `FakeLlm`, `RecordingExecutor`, `Executor`, `LocalExecutor`, `RemoteExecutor`, `AgentConfig`, `Agent::run`, `Transcript`, `CallRecord`, `AgentFailure`, `is_question`, `AskArgs`, `ask::run`.
- Review Focus items each have a named test: 1 → Task 3 (and 7), 2 → Task 4, 3 → Task 11, 4 → Task 11 (and 9), 5 → Tasks 4 and 9.
