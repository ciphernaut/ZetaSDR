# Agent Bulk Operations Implementation Plan (plan 21)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A voice-console request that fans out over a channel plan ("an FM decoder on every CB channel with a transcribe on each") lands as one `plan_start` operation under one lease decision, the agent cannot stop or replace the operator's jobs without an explicit allowance, a duplicate start is refused, the page asks for 32 turns, and the model's reasoning is kept per turn in the transcript.

**Architecture:** Four independent pieces behind one scenario. In `zeta-agent`, `Agent::call` checks each parsed op against an `Allow` policy before the executor sees it, feeding a `not_allowed` error back to the model; `/ask` and `zeta ask` carry the allowance, and the transcript gains a per-completion `turn_log`. In `zeta-core`, the `Stage` trait gains a `readiness()` hook (ready/reason plus `capacity`) that `transcribe` answers and `schema` lists; `job_start` takes the source's gate always and refuses an identical running rigid job with a new `OpError::Duplicate`; a new `Op::PlanStart` validates, preflights duplicates, the attached stage and the lease for the band bounding the requested channels under the gate, then starts each channel inside the capture and attaches to each, answering `started`/`existing`/`refused`/`warnings`. The page sends 32 turns, gates with `allow stops on|off`, offers the unlock button on a refused call, renders `plan_start` outcomes and types `plan start …`.

**Tech Stack:** Rust 2024 (tokio, serde, schemars, clap, axum), the existing `FakeLlm` (`zeta-agent`, feature `test-fake`), `FileSource` fixtures (`zeta-source-file`, feature `test-fixtures`), the `Radio` port fixture of `crates/zeta-core/tests/replace.rs`, `FakeBackend`/`FakeDaemon` (`zeta_speech::testing`), Playwright (`crates/zeta-ui/tests/e2e/ui.py`), vanilla ES modules in `crates/zeta-ui/web`.

**Spec:** `docs/superpowers/specs/2026-11-01-agent-bulk-ops-design.md` (read it first; section numbers below refer to it).

## Global Constraints

- Every op field is required and unknown fields are rejected (`deny_unknown_fields` on `Op`); a nullable field is `Option<T>` with a `nullable_*` deserializer and `#[schemars(required, schema_with = ...)]` as `job_start`'s `deviation_hz` is, so the schema says `["T","null"]`.
- Exact texts (spec §4.2, §4.3, §4.4): the gate error `{"error":"not_allowed","field":"replace"|"op","op":"<op name>","message":"stopping jobs is not allowed in this run: report the conflict and its holders to the operator; they unlock it with the retry button or by allowing stops for the run"}`; the duplicate error `{"error":"duplicate","job":"<id>","message":"an identical <kind> already runs on <source> at <center_hz>/<bandwidth_hz> Hz as job <id>"}`; `transcribe`'s unready reason `this node has no speech backend (--stt-socket or --stt-node)` (the existing `NO_BACKEND` minus its `transcribe: ` prefix); the load warning `<n> <kind> jobs share the <stage>'s <c> slots: with the squelch on most channels are silent, so the slots are rarely contested; a busy daemon drops utterances with reason busy` for `transcribe` (the builder may keep it as one constant string with `{n}`, `{kind}`, `{c}` filled).
- Page permission words are fixed: `allow stops on` and `allow stops off` (spec §4.7), never parsed from a request.
- The agent's default `max_turns` stays 8 (`max_turns_defaults_to_eight`); the page sends 32; `MAX_TURNS` in `zeta-ws/src/ask.rs` stays 32.
- Every task that changes an op, a result, an error, a stage listing or field craft regenerates `AGENTS.md` (`cargo run -p zeta -- agents-md > AGENTS.md`) and `skills/zetasdr/SKILL.md` (`cargo run -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md`) and runs `cargo test -p zeta --test agents_md`.
- Gates per task: `cargo fmt --all -- --check` (per member if the worktree breaks `--all`: `cargo fmt -p <crate> -- --check`), `cargo clippy --workspace --all-targets --all-features -- -D warnings`, the touched crates' tests. Final gate: `cargo test --workspace --all-features --no-fail-fast` (no `FAILED`; `ui e2e skipped` only for a missing Playwright), `agents-md --check`, `agents-md --skill --check`. Temp files under the job's tmp directory, never `/tmp`; Python via `python3 -I` except Playwright (`ui.rs` runs it without `-I` on purpose).
- Commits on the worktree branch `worktree-agent-bulk-ops-design` (from `main` at 5f43af4), one per task, messages in the repo's style (`Agent: ...`, `Jobs: ...`, `Stages: ...`, `UI: ...`, `Docs: ...`), each ending with the session attribution line the harness gives.

## Review Focus

1. **A gated `job_start` with `replace: []` must pass.** The gate refuses only a non-empty `replace`; an ordinary start under `replace: false` reaches the node. Pinned in Task 1 (`an_empty_replace_passes_the_gate`).
2. **`plan_start` with a conflict starts nothing and stops nothing it was not told to.** A rigid holder elsewhere on the source, `replace: []`: zero jobs, the holder running, the source untuned. Pinned in Task 5 (`a_holder_outside_the_band_is_a_conflict_and_nothing_starts`).
3. **A second `plan_start` is idempotent.** 80 `existing`, nothing started, nothing stopped, and an `attach` on the second run attaches only where nothing is attached. Pinned in Task 5 (`a_second_plan_start_adds_only_what_is_missing`).
4. **The duplicate check must not refuse the page's own spectrum job.** An elastic start twice on one band is never `duplicate`. Pinned in Task 4 (`an_elastic_start_is_never_a_duplicate`).
5. **A `transcribe` attach on a node without a backend is refused at op time, and the node with a backend still accepts it.** Pinned in Task 3 (`job_attach_is_refused_at_op_time_without_a_backend` in `crates/zeta/tests/attach.rs`) and Task 6 (`plan_start_with_transcribe_is_refused_without_a_backend`).

---

## File map

- Modify `crates/zeta-agent/src/agent.rs` (`Allow`, `AgentConfig.allow`, `TurnEntry`, `Transcript.turn_log`, `Transcript.allow`, the gate in `Agent::call`, `build_agent` taking `Allow`), `src/lib.rs` (re-exports), `src/fake.rs` (`FakeLlm::start_by_request`, `tool_call_response_with` carrying `content`/`reasoning_content`), tests `crates/zeta-agent/tests/agent.rs`.
- Modify `crates/zeta-ws/src/ask.rs` (`AskRequest.allow`), tests `crates/zeta-ws/tests/ask.rs`.
- Modify `crates/zeta/src/cli.rs` (`AskArgs.allow_stop`, `AskArgs.allow_replace`, `Command::Plan(PlanCmd)`, `parse_channels`), `src/ask.rs` (pass `Allow`), tests `crates/zeta/tests/ask.rs`, `crates/zeta/tests/cli.rs`.
- Modify `crates/zeta-core/src/stage.rs` (`Readiness`, `Stage::readiness`, `StageRegistry::readiness`, `describe` adds `readiness`), `crates/zeta-speech/src/transcribe.rs` (`readiness` for `TranscribeStage`, `NO_BACKEND_REASON`), `crates/zeta-speech/src/socket.rs` (nothing: `STT_CONCURRENCY` is read), `crates/zeta-core/src/ops.rs` (`job_attach` readiness check), tests `crates/zeta-core/tests/attach.rs`, `crates/zeta/tests/attach.rs`, `crates/zeta/tests/cli.rs` (schema).
- Modify `crates/zeta-core/src/ops.rs` (`OpError::Duplicate`, `Op::PlanStart`, `OpResult::PlanStart` with `PlanStarted`, `PlanExisting`, `PlanRefused`, `plan_start`, `JobKind::start_params`, the duplicate check in `job_start`), `crates/zeta-core/src/node/mod.rs` (`Node::duplicate_of`, `Node::gate_guard`), `crates/zeta-core/src/lease.rs` (`LeaseManager::tune_if_unheld_locked` is reused; nothing new unless the builder needs a `check_band` helper), tests `crates/zeta-core/tests/replace.rs` (duplicate), new `crates/zeta-core/tests/plan_start.rs`.
- Modify `crates/zeta/src/agents_md.rs` (`RULES` two lines, `note("plan_start")`, `note("job_stop")`, `hint("plan")`, `hint("channels")`, `example_for("plan_start", ...)`, `cli_form` for `plan_start`), `docs/agents/field-craft.md` ("Starting a whole plan", "Taking the radio from another job"), `README.md`, `AGENTS.md`, `skills/zetasdr/SKILL.md`, `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (row 21), `TODO.md` (the section becomes a pointer to this plan).
- Modify `crates/zeta-ui/web/api.js` (`ask(text, maxTurns = 32, allow)`), `app.js` (`allow stops on|off`, `plan start …`, the `not_allowed` note and button, `REPLACEABLE` gains `plan_start`, the collapsed turn log), `agent.js` (`resultText` for `plan_start`, `errorText` for `not_allowed` and `duplicate`, `Transcript.assistant(text, error, turnLog)`), `menu.js`/`index.html` (`Start plan…` on the selection menu), tests `crates/zeta-ui/tests/e2e/ui.py`, `crates/zeta/tests/ui.rs` (a third server with `--llm-url` at a `FakeLlm` started by request), `crates/zeta-agent/src/bin/zeta-fake-llm.rs` (new, behind feature `test-fake`: serves `FakeLlm::start_by_request` from a JSON file for the harness).

---

### Task 1: The stop/replace gate in the agent (Sonnet)

**Files:**
- Modify: `crates/zeta-agent/src/agent.rs:11-25` (`AgentConfig`), `:163-188` (`Agent::call`), `:82-93` (`build_agent`)
- Modify: `crates/zeta-agent/src/lib.rs` (re-export `Allow`)
- Modify: `crates/zeta-ws/src/ask.rs:84-91`, `crates/zeta/src/ask.rs:93-109` (callers of `build_agent`; pass `Allow::default()` here, Task 2 wires the real values)
- Test: `crates/zeta-agent/tests/agent.rs`

**Interfaces:**
- Produces, in `zeta_agent::agent`:
  - `#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)] #[serde(deny_unknown_fields, default)] pub struct Allow { pub stop_jobs: bool, pub replace: bool }` (both false by default; `serde(default)` so `{}` and a partial object parse).
  - `AgentConfig.allow: Allow` (public field, `Allow::default()` from `AgentConfig::new`).
  - `pub fn build_agent<E: Executor>(llm: LlmConfig, exec: E, schema: &Value, agents_md: &str, max_turns: u32, allow: Allow) -> Result<Agent<E>, AgentError>` (one new trailing parameter).
  - `pub const GATED_REPLACE_OPS: &[&str] = &["job_start", "job_retune", "source_ensure", "source_pin", "plan_start"];` (public so the page's docs and Task 5 can refer to one list).
  - `pub fn gate(op: &Value, allow: Allow) -> Option<Value>`: `Some(error JSON)` when `op["op"] == "job_stop"` and `!allow.stop_jobs` (`field: "op"`), or when `op["op"]` is in `GATED_REPLACE_OPS`, `op["replace"]` is a non-empty array and `!allow.replace` (`field: "replace"`); `None` otherwise. The error JSON is the Global Constraints text with `"op"` set to the op name.
- `Agent::call` calls `gate` on the `Ok(op)` branch before `self.exec.apply(op)`; a `Some(e)` becomes `result = Err(e)` (so `ok: false`, the record kept, the executor untouched).

- [ ] **Step 1: Write the failing tests** in `crates/zeta-agent/tests/agent.rs` (use `RecordingExecutor`, `fake(vec![tool_call_response(...), text_response("done", "stop")])`):

```rust
#[tokio::test]
async fn job_stop_is_refused_until_stops_are_allowed() {
    // Script: one job_stop call, then prose. allow = default.
    // assert: exec.calls is empty; transcript.calls[0].ok == false;
    // transcript.calls[0].result["error"] == "not_allowed"; ["field"] == "op"; ["op"] == "job_stop";
    // the second request body sent to the fake carries a tool message whose content contains "not_allowed".
}

#[tokio::test]
async fn job_stop_is_applied_when_stops_are_allowed() {
    // same script, cfg.allow = Allow { stop_jobs: true, replace: false }: exec.calls.len() == 1, calls[0].ok == true
}

#[tokio::test]
async fn a_non_empty_replace_is_refused_until_replace_is_allowed() {
    // job_start with replace: ["01X"], allow default: not_allowed, field "replace", op "job_start"; exec untouched.
    // with Allow { replace: true, .. }: applied.
}

#[tokio::test]
async fn an_empty_replace_passes_the_gate() {
    // job_start with replace: [], allow default: exec.calls.len() == 1
}

#[test]
fn gate_covers_every_replace_taking_op() {
    // for each name in GATED_REPLACE_OPS: gate(json!({"op": name, "replace": ["a"]}), Allow::default()).is_some()
    // gate(json!({"op": "source_unpin", "source": "s"}), Allow::default()).is_none()
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p zeta-agent --features test-fake --test agent gate` and the four async tests by name.
Expected: compile errors on `Allow`, `gate`, `cfg.allow`.

- [ ] **Step 3: Implement** `Allow`, `GATED_REPLACE_OPS`, `gate`, `AgentConfig.allow`, the check in `Agent::call`, the new `build_agent` parameter; update the two callers with `Allow::default()`; re-export `Allow` and `gate` from `lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p zeta-agent --features test-fake` and `cargo test -p zeta-ws --test ask` and `cargo test -p zeta --test ask`.
Expected: all PASS (existing tests unchanged in behaviour).

- [ ] **Step 5: Gates and commit**

```bash
cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/zeta-agent crates/zeta-ws/src/ask.rs crates/zeta/src/ask.rs
git commit -m "Agent: job_stop and a non-empty replace are refused by the executor path until the run allows them (Allow, not_allowed)"
```

---

### Task 2: The allowance over `/ask` and `zeta ask`, and the turn log (Sonnet)

**Files:**
- Modify: `crates/zeta-agent/src/agent.rs:27-45` (`Transcript`), `:120-160` (`Agent::turns`)
- Modify: `crates/zeta-agent/src/fake.rs:111-136` (`tool_call_response_with`)
- Modify: `crates/zeta-ws/src/ask.rs:19-24, 84-91`
- Modify: `crates/zeta/src/cli.rs:362-372` (`AskArgs`), `crates/zeta/src/ask.rs:20-109`
- Test: `crates/zeta-agent/tests/agent.rs`, `crates/zeta-ws/tests/ask.rs`, `crates/zeta/tests/ask.rs`

**Interfaces:**
- Consumes: `Allow`, `build_agent(.., allow)` from Task 1.
- Produces:
  - `#[derive(Clone, Debug, Serialize)] pub struct TurnEntry { pub turn: u32, pub reasoning: Option<String>, pub content: Option<String>, pub calls: usize }` in `zeta_agent::agent`, re-exported.
  - `Transcript.turn_log: Vec<TurnEntry>` (one entry per completion, pushed right after `self.llm.complete` returns, `content` as the model sent it, `None` when empty after trim) and `Transcript.allow: Allow` (what the run ran under). `Transcript.turns` stays.
  - `AskRequest { request: String, max_turns: Option<u64>, allow: Option<Allow> }` (`deny_unknown_fields` already on the struct; `Allow`'s own `deny_unknown_fields` rejects a stray key inside it with 422 as any parse error does).
  - `AskArgs { .., #[arg(long)] pub allow_stop: bool, #[arg(long)] pub allow_replace: bool }` mapped to `Allow { stop_jobs: allow_stop, replace: allow_replace }` in `zeta::ask::run` and passed to `drive` and `build_agent` for both the local and the `--node` paths.
  - `zeta_agent::fake::tool_call_response_with(calls: &[(&str, Value)], content: &str, reasoning: Option<&str>) -> Value` (the existing `tool_call_response` becomes `tool_call_response_with(calls, "", Some("The user wants this; call the tool."))`).

- [ ] **Step 1: Write the failing tests**

In `crates/zeta-agent/tests/agent.rs`:

```rust
#[tokio::test]
async fn turn_log_keeps_reasoning_and_content_per_completion() {
    // Script: tool_call_response_with(&[("source_list", json!({}))], "Looking at the sources.", Some("plan: list first")),
    //         text_response("two sources", "stop").
    // assert: t.turn_log.len() == 2; [0] == {turn: 1, reasoning: Some("plan: list first"), content: Some("Looking at the sources."), calls: 1};
    //         [1] == {turn: 2, reasoning: None, content: Some("two sources"), calls: 0}.
    // assert: no request body sent to the fake contains the key "reasoning_content" (requests(&fake) serialised to string).
}
```

In `crates/zeta-ws/tests/ask.rs`:

```rust
#[tokio::test]
async fn ask_allow_defaults_to_false_and_rejects_an_unknown_key() {
    // POST {"request": "what sources?"}: 200; body["allow"] == {"stop_jobs": false, "replace": false}; body["turn_log"] is an array of 2.
    // POST {"request": "...", "allow": {"stop_jobs": true}}: 200; body["allow"]["stop_jobs"] == true, ["replace"] == false.
    // POST {"request": "...", "allow": {"stops": true}}: 422.
}

#[tokio::test]
async fn ask_refuses_job_stop_without_the_allowance() {
    // fake scripted: tool_call job_stop {"job": "01X"}, then text "refused". assert body["calls"][0]["result"]["error"] == "not_allowed";
    // the ops stream received no job_stop record (subscribe before; after the ask, a 500 ms recv times out or only sees non-job_stop ops).
}
```

In `crates/zeta/tests/ask.rs`:

```rust
#[tokio::test]
async fn ask_allow_stop_flag_reaches_the_agent() {
    // local_args + ["--allow-stop"], fake: job_stop on a made-up id, then prose. Exit 0; json_out["calls"][0]["result"]["error"] == "unknown" (the node refused the id, so the gate let it through).
    // Without the flag: json_out["calls"][0]["result"]["error"] == "not_allowed".
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p zeta-agent --features test-fake --test agent turn_log`, `cargo test -p zeta-ws --test ask allow`, `cargo test -p zeta --test ask allow_stop`.
Expected: compile errors (`turn_log`, `tool_call_response_with`, `allow`), then 422/assert failures.

- [ ] **Step 3: Implement** `TurnEntry`, `Transcript.turn_log`/`allow`, the push in `Agent::turns`, `AskRequest.allow` → `build_agent(.., req.allow.unwrap_or_default())`, `AskArgs` flags → `Allow`, `tool_call_response_with`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p zeta-agent --features test-fake && cargo test -p zeta-ws --test ask && cargo test -p zeta --test ask`.
Expected: PASS.

- [ ] **Step 5: Gates, docs touch, commit**

`README.md` (§Transports, the `POST /ask` paragraph around line 624): add `allow` and `turn_log` in one sentence each; `zeta ask --allow-stop/--allow-replace` beside `--max-turns`.

```bash
cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/zeta-agent crates/zeta-ws crates/zeta/src crates/zeta/tests/ask.rs README.md
git commit -m "Agent: /ask and zeta ask carry the stop/replace allowance; the transcript keeps a per-turn log of reasoning and content"
```

---

### Task 3: `Stage::readiness` (Sonnet)

**Files:**
- Modify: `crates/zeta-core/src/stage.rs:121-137` (trait), `:177-209` (registry)
- Modify: `crates/zeta-speech/src/transcribe.rs:22-24` and its `impl Stage`
- Modify: `crates/zeta-core/src/ops.rs:2557-2585` (`job_attach`)
- Test: `crates/zeta-core/tests/attach.rs`, `crates/zeta/tests/attach.rs`, `crates/zeta/tests/cli.rs` (schema), `crates/zeta/tests/agents_md.rs`

**Interfaces:**
- Produces, in `zeta_core::stage`:
  - `#[derive(Clone, Debug, PartialEq, Serialize)] pub struct Readiness { pub ready: bool, pub reason: Option<String>, pub capacity: Option<u32> }` with `Readiness::always() -> Self { ready: true, reason: None, capacity: None }` and `Readiness::unready(reason: impl Into<String>) -> Self`.
  - `fn readiness(&self) -> Readiness { Readiness::always() }` on `trait Stage` (default).
  - `StageRegistry::readiness(&self, kind: &str) -> Option<Readiness>` (the first implementation registered for `kind`; `None` for an unknown kind).
  - `describe()` adds `"readiness": {ready, reason, capacity}` to each stage object.
- In `zeta_speech::transcribe`: `pub const NO_BACKEND_REASON: &str = "this node has no speech backend (--stt-socket or --stt-node)"` (and `NO_BACKEND` becomes `"transcribe: "` + that, same text as today); `TranscribeStage::readiness` answers `Readiness::unready(NO_BACKEND_REASON)` without a backend, else `Readiness { ready: true, reason: None, capacity: Some(STT_CONCURRENCY as u32) }`.
- `job_attach` (ops.rs): after the kind and language checks and before `job_audio_stream`, `node.stage_readiness(kind.name())` (a new `pub fn stage_readiness(&self, kind: &str) -> Option<Readiness>` on `Node` delegating to the registry); `Some(r) if !r.ready` → `Err(other(format!("{}: {}", kind.name(), reason)))`, the reason the stage's.

- [ ] **Step 1: Write the failing tests**

In `crates/zeta-core/tests/attach.rs`, beside the stand-in stages:

```rust
#[tokio::test]
async fn an_unready_attached_stage_is_refused_at_job_attach() {
    // a MapStage-like stand-in for kind "transcribe" whose readiness() is Readiness::unready("no backend here")
    // job_attach(kind transcribe, running demod input) → Err(OpError::Other { message }) with message == "transcribe: no backend here"; job_list unchanged.
}

#[test]
fn registry_describe_lists_readiness() {
    // registry with PassthroughStage: describe()[i]["readiness"] == {"ready": true, "reason": null, "capacity": null}
}
```

In `crates/zeta/tests/attach.rs` (real `TranscribeStage`):

```rust
#[tokio::test]
async fn job_attach_is_refused_at_op_time_without_a_backend() {
    // registry with TranscribeStage::new(None, ..): job_attach → Err(Other) whose message ends with NO_BACKEND_REASON; no new job record.
}

#[tokio::test]
async fn schema_lists_transcribe_readiness() {
    // node.schema()["stages"] entry with kind "transcribe": readiness.ready false without a backend; with Some(FakeBackend): ready true, capacity 4.
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p zeta-core --test attach readiness`, `cargo test -p zeta --test attach readiness` (and the two named tests).
Expected: compile errors on `Readiness`/`readiness`.

- [ ] **Step 3: Implement** the trait hook, the registry helpers, `describe`, `TranscribeStage::readiness`, `Node::stage_readiness`, the `job_attach` check.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p zeta-core --test attach && cargo test -p zeta --test attach --test speech_chain && cargo test -p zeta-speech`.
Expected: PASS. The existing `speech_chain` and `attach` tests keep passing (they run with a backend).

- [ ] **Step 5: Guide, gates, commit**

In `crates/zeta/src/agents_md.rs` the Stage kinds rendering (lines ~181-213) adds a line `- readiness: ready` or `- readiness: not ready (<reason>)` plus `, capacity <c>` when present, after `outputs`. The agents_md `rendering_lists_every_op_variant`/`guide_covers_...` tests keep passing; regenerate `AGENTS.md` and `SKILL.md`.

```bash
cargo run -p zeta -- agents-md > AGENTS.md && cargo run -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md
cargo test -p zeta --test agents_md
cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/zeta-core crates/zeta-speech crates/zeta AGENTS.md skills/zetasdr/SKILL.md
git commit -m "Stages: a readiness hook (ready, reason, capacity) answered by transcribe, listed by schema, checked by job_attach"
```

---

### Task 4: The duplicate refusal in `job_start` (Sonnet)

**Files:**
- Modify: `crates/zeta-core/src/ops.rs:1793-1831` (`OpError::Duplicate`), `:1835-1843` (`name`), `Display`, `:445-560` (`JobKind::start_params`), `:2293-2420` (`job_start`)
- Modify: `crates/zeta-core/src/node/mod.rs:1160-1175` (`gate_guard` always), new `Node::duplicate_of`
- Test: `crates/zeta-core/tests/replace.rs` (the `Radio` fixture), `crates/zeta/tests/cli.rs` (schema lists the error)

**Interfaces:**
- Produces:
  - `OpError::Duplicate { job: JobId, message: String }`, `name()` → `"duplicate"`, `Display` → the message.
  - `JobKind::start_params(&self, graph: &Graph) -> (Option<u64>, Option<Sideband>)`: the `deviation_hz` of the graph's `nbfm_demod`/`wfm_demod` node and the `sideband` of its `ssb_demod` node (the lookup `reexpand_carrying` already does with `param(key)`), `(None, None)` when the graph has no such node.
  - `Node::duplicate_of(&self, source: &SourceId, kind: &str, tuning: Tuning, deviation_hz: Option<u64>, sideband: Option<Sideband>) -> Option<JobId>`: the first running (`JobState::Running`), rigid (`!elastic`), non-attached (`input.is_none()`) record on `source` with the same `kind` string and `tuning`, whose graph's `start_params` equal the given pair after both sides are normalised to the kind's default (`fm_demod` 5000, `wfm_demod` 75000, `ssb_demod` `Usb`; other kinds compare `(None, None)`), via `expandable_kind(kind)`.
  - `Node::gate_guard(&self, source: &SourceId) -> OwnedMutexGuard<()>` (public-in-crate wrapper over `leases.gate_guard`).
- `job_start` (non-scan branch): replace `gate_for_replace` with `Some(node.gate_guard(&source).await)` always; after `replace_under` and before `tune_if_unheld`, when `!elastic`, `if let Some(id) = node.duplicate_of(..)` → drop the guard and return `Err(OpError::Duplicate { job: id, message })`. The scan branch keeps its own gating (a scan has no duplicate rule here). Because the guard is now always `Some`, `tune_if_unheld` runs its `_locked` form and `after_gate` handles the elastic relaunch as it does today for a replace request; the `idle_after_replace`/`relaunch_elastic` tail stays as it is.

- [ ] **Step 1: Write the failing tests** in `crates/zeta-core/tests/replace.rs` (fixture `Radio::new("radio0")`, `open`, `start(kind, band, elastic, replace)` as `pin.rs` has them; add the helpers to `replace.rs` if absent):

```rust
#[tokio::test]
async fn an_identical_rigid_start_is_a_duplicate_naming_the_first() {
    // start passthrough 100 MHz/48 kHz → id a; start again → Err(OpError::Duplicate { job, message }) with job == a,
    // message == format!("an identical passthrough already runs on radio0 at 100000000/48000 Hz as job {a}"); job_list has one running job.
}

#[tokio::test]
async fn a_different_deviation_is_not_a_duplicate() {
    // registry with the real zeta_dsp stages (zeta_dsp::register_all) so fm_demod expands: start fm_demod 100 MHz/12.5 kHz deviation null (=5000) → a;
    // start fm_demod same band deviation 2500 → Ok (second id); start fm_demod same band deviation 5000 → Duplicate of a.
}

#[tokio::test]
async fn an_elastic_start_is_never_a_duplicate() {
    // start spectrum elastic twice on one band → two ids.
}

#[tokio::test]
async fn a_stopped_identical_job_is_not_a_duplicate() {
    // start, job_stop, start again → Ok with a new id.
}
```

And in `crates/zeta/tests/cli.rs::schema_prints_json_with_op_result_error`, assert the error schema's `oneOf` contains a variant with `properties.error.const == "duplicate"`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p zeta-core --test replace duplicate` and `cargo test -p zeta --test cli schema`.
Expected: the first compile-fails on `OpError::Duplicate`; `schema` fails on the missing variant.

- [ ] **Step 3: Implement** `OpError::Duplicate`, `JobKind::start_params`, `Node::duplicate_of`, `Node::gate_guard`, the always-gated `job_start` with the check.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p zeta-core` (whole crate: `replace`, `pin`, `elastic`, `retune`, `scan`, `resume` all exercise `job_start`'s gate) and `cargo test -p zeta --test cli`.
Expected: PASS, no timing regressions (the gate is per source and brief).

- [ ] **Step 5: Guide, gates, commit**

`agents_md.rs` `note("job_start")`: append one sentence: "An identical rigid start (same source, kind, band, `deviation_hz` and `sideband`) on a running job is refused as `duplicate` naming that job; stop it or use it." Regenerate `AGENTS.md`/`SKILL.md`.

```bash
cargo run -p zeta -- agents-md > AGENTS.md && cargo run -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md && cargo test -p zeta --test agents_md
cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/zeta-core crates/zeta/src/agents_md.rs crates/zeta/tests/cli.rs AGENTS.md skills/zetasdr/SKILL.md
git commit -m "Jobs: job_start takes the source's gate always and refuses an identical running rigid start as duplicate naming the job"
```

---

### Task 5: `plan_start`, the operation (Opus)

**Files:**
- Modify: `crates/zeta-core/src/ops.rs` (`Op::PlanStart` after `JobRetune`, `OpResult::PlanStart`, `name`/`variant_name`, `plan_start` fn beside `job_attach`)
- Modify: `crates/zeta-core/src/plans.rs` (`pub fn plan(id: &str) -> Option<&'static ChannelPlan>`, `pub fn bounding(plan, numbers) -> (Tuning)`)
- Test: new `crates/zeta-core/tests/plan_start.rs` (copy the `Radio` fixture and helpers from `replace.rs`; a `Radio` tunes to exactly the band asked, 20 MHz max)

**Interfaces:**
- Consumes: `OpError::Duplicate`, `Node::duplicate_of`, `Node::gate_guard`, `JobKind::start_params` (Task 4); `Node::stage_readiness` (Task 3).
- Produces:
  - `Op::PlanStart { plan: String, channels: Option<Vec<u32>>, kind: JobKind, source: SourceId, deviation_hz: Option<u64>, sideband: Option<Sideband>, attach: Option<JobKind>, language: Option<String>, replace: Vec<JobId> }`, each `Option` with a `nullable_*` deserializer and a `schema_with` giving `["...", "null"]` (add `nullable_u32_list`/`nullable_kind` helpers beside `nullable_u64`). Doc comment: the spec's §4.4 behaviour in the guide's voice (fields, the one lease decision, `started`/`existing`/`refused`/`warnings`, idempotence).
  - `OpResult::PlanStart { plan: String, source: SourceId, capture: Tuning, started: Vec<PlanStarted>, existing: Vec<PlanExisting>, refused: Vec<PlanRefused>, warnings: Vec<String> }` with `PlanStarted { channel: u32, center_hz: Hz, job: JobId, attached: Option<JobId>, attach_error: Option<OpError> }`, `PlanExisting { channel: u32, center_hz: Hz, job: JobId, attached: Option<JobId> }`, `PlanRefused { channel: u32, center_hz: Hz, error: OpError }`; `variant_name` → `"plan_start"`; `Op::name` → `"plan_start"`; not read-only.
  - `plans::plan(id)` and `plans::bounding(plan: &ChannelPlan, numbers: &[u32]) -> Tuning` (centre at the midpoint of lowest-edge..highest-edge, width that span, each channel `center ± bandwidth_hz/2`).
  - `async fn plan_start(node: &Node, ..fields..) -> Result<OpResult, OpError>` in ops.rs, applying spec §4.4 steps 1-7 in order under one `node.gate_guard(&source)` held for the whole op:
    1. Validate: `plans::plan` else `Unknown { field: "plan", known: PLANS ids }`; `channels` numbers each in the plan else `Unknown { field: "channels", value: n.to_string(), known: <the plan's numbers as strings> }`; a repeat → `Other("channels: <n> is listed twice")`; `kind.attachable()` or `matches!(kind, Spectrum | Scan | Watch | Identify)` → `Other("<kind> cannot be started per channel")`; `deviation_hz`/`sideband` checks as `job_start` does (reuse its code by extracting a `fn check_start_params(kind, deviation_hz, sideband) -> Result<Option<Hz>, OpError>`); `attach` must be `attachable()` else `Other("<kind> is not attachable")`; `language` non-null with `attach` null → `Other("language needs attach")`; `kind.mode_row(&node.modes())?`.
    2. For each channel, `node.duplicate_of(&source, kind.name(), Tuning{center, plan.bandwidth_hz}, deviation, sideband)` → `existing` (with `attached`: the running record whose `input` is that job and whose `kind` is the attach kind, if any).
    3. With `attach`, `node.stage_readiness(attach.name())`: unready → `Other(format!("{}: {}; start without attach", attach.name(), reason))`.
    4. `node.replace_holders(&source, &replace, &guard)` (nothing when empty).
    5. `bounding` of the channels to start (not the existing ones; if none remain, skip 5-7 and answer with `capture` = the port's current band or the bounding band); `checked_port(node, &source, &bounding)?` (the `out_of_range` on width); `node.tune_if_unheld(port, bounding, Some(&guard))` → on `LeaseError::Conflict` return `node_error` of it, on `LeaseError::Port` the `SourcePort` error as `job_start` maps it (a recording without a tune range refuses a band outside its own here, so that is the op's error, not a per-channel refusal); either way nothing is started, the stopped `replace` jobs stay stopped as `job_start` leaves them, then `idle_after_replace` after the guard drops.
    6. For each channel to start, in channel-number order: `kind.expand(&source, tuning, granted.bandwidth_hz, bin_rate, deviation, sideband, &modes)` then `node.job_start_gated(kind.name(), source, tuning, graph, Some(granted.bandwidth_hz), false, Some(&guard))`; collect `AfterGate`s (merge: any `Relaunch` wins over `Nothing`; the builder may run `after_gate` once after the guard drops with the merged value, as `job_start` does with `tuned`); an `Err` → `refused` with `node_error(node, e)`.
    7. With `attach`, for each started job and each `existing` without `attached`: the `job_attach` path's tail (`job_audio_stream`, `expand_attached`, `node.job_attach`) → `attached` or `attach_error`.
    Warnings: with `attach`, let `c = readiness.capacity`; count running jobs of the attach kind after step 7; if `Some(c)` and count > c, push the Global Constraints text.
  - The `apply` match arm `Op::PlanStart { .. } => plan_start(node, ...).await`.

- [ ] **Step 1: Write the failing tests** in `crates/zeta-core/tests/plan_start.rs` (registry: `PassthroughStage` plus `zeta_dsp::register_all` so `fm_demod` expands; `kind: fm_demod` throughout; `source "radio0"`):

```rust
#[tokio::test]
async fn plan_start_starts_every_channel_under_one_tune() {
    // all channels, attach null: Ok(PlanStart) with started.len() == 80 in channel order 1..=80 by `channel`,
    // existing and refused empty; source_list holders.len() == 80; the Radio's tune count == 1 (add an AtomicUsize to the fixture's tune());
    // capture covers 476_418_750..=477_418_750 (ch1's lower edge to ch80's upper edge, ch80 being 477_412_500):
    // center_hz == 476_918_750, bandwidth_hz == 1_000_000 (the Radio grants exactly what is asked; this is the pin the guide recommends).
}

#[tokio::test]
async fn a_holder_outside_the_band_is_a_conflict_and_nothing_starts() {
    // start passthrough at 144.8 MHz/12.5 kHz (rigid holder) → h; plan_start all → Err(Conflict { holders == [h], .. });
    // job_list running == 1 (h); Radio current == 144.8 MHz band. With replace: [h] → Ok, 80 started, h stopped.
}

#[tokio::test]
async fn a_scan_on_the_source_is_a_conflict() { /* start scan on 400-406 MHz; plan_start → Conflict naming it */ }

#[tokio::test]
async fn inside_a_pin_nothing_retunes_and_a_pin_elsewhere_conflicts() {
    // source_pin 476_918_750/1_024_000 → plan_start all: Ok, tune count stays 1 (the pin's). Unpin; pin at 144.8 MHz/1 MHz → plan_start → Conflict whose options contain "unpin source radio0 (source_unpin)".
}

#[tokio::test]
async fn channels_are_validated_before_anything_happens() {
    // channels [1, 2, 41] → 3 started; [1, 1] → Other("channels: 1 is listed twice"); [81] → Unknown { field: "channels" }; plan "nope" → Unknown { field: "plan", known: ["uhf_cb_au"] };
    // kind spectrum → Other; kind transcribe → Other; attach fm_demod → Other("fm_demod is not attachable"); language Some("de") with attach None → Other.
    // after each refusal job_list is unchanged.
}

#[tokio::test]
async fn a_second_plan_start_adds_only_what_is_missing() {
    // plan_start all → 80 started. plan_start all again → started empty, existing.len() == 80 with the same ids, nothing stopped (job_list running == 80).
}

#[tokio::test]
async fn a_bounding_band_wider_than_the_source_is_out_of_range() {
    // a Radio with max_bandwidth_hz 500_000: plan_start all → Err(OutOfRange { field: "bandwidth_hz", value: 1_000_000, max: 500_000, .. }); nothing started.
}

#[tokio::test]
async fn a_restart_resumes_the_plan_jobs_as_ordinary_jobs() {
    // plan_start channels [1,2,3]; drop the node; open with resume: true → 3 running fm_demod records, holders 3 (pattern of tests/resume.rs).
}
```

Attach is covered in Task 6 (it needs a stand-in attachable stage).

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p zeta-core --test plan_start`.
Expected: compile errors on `Op::PlanStart`.

- [ ] **Step 3: Implement** the op, the result, the plan helpers and `plan_start` as specified above.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p zeta-core --test plan_start --test replace --test pin --test resume`.
Expected: PASS.

- [ ] **Step 5: Gates and commit** (the guide text lands in Task 7; `agents-md --check` will fail until then, so skip the regeneration gate here and run `cargo test -p zeta --test agents_md -- --skip committed_agents_md_matches_rendering`):

```bash
cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/zeta-core
git commit -m "Jobs: plan_start starts a kind on every channel of a plan under one lease decision, answering started/existing/refused/warnings"
```

---

### Task 6: `plan_start` attach, readiness preflight and the load warning (Sonnet)

**Files:**
- Modify: `crates/zeta-core/src/ops.rs` (`plan_start` steps 3 and 7, warnings; only if Task 5 left them stubbed)
- Test: `crates/zeta-core/tests/plan_start.rs` (stand-in attachable stage), `crates/zeta/tests/attach.rs` (real `transcribe` with `FakeBackend`)

**Interfaces:**
- Consumes: `plan_start` (Task 5), `Readiness`/`stage_readiness` (Task 3), `JobKind::expand_attached` and `Node::job_attach` as `job_attach` uses them.
- Produces: nothing new; the behaviour pinned below.

- [ ] **Step 1: Write the failing tests**

In `crates/zeta-core/tests/plan_start.rs`, a stand-in `Stage` for kind `"transcribe"` (audio in, event out, as `attach.rs`'s `MapStage`) with a constructor taking its `Readiness`:

```rust
#[tokio::test]
async fn an_unready_attach_refuses_the_whole_op_before_any_start() {
    // stand-in transcribe readiness unready("no backend here"); plan_start all, attach transcribe → Err(Other) message == "transcribe: no backend here; start without attach"; job_list empty.
}

#[tokio::test]
async fn attach_follows_every_started_channel_and_warns_over_capacity() {
    // stand-in ready with capacity Some(4): plan_start channels [1..=8], attach transcribe → 8 started each with attached Some; 16 running jobs;
    // every attached record's input == its demod id; warnings == [the Global Constraints text with n=8, kind=transcribe, c=4]. channels [1,2] alone: warnings empty.
}

#[tokio::test]
async fn a_second_run_with_attach_attaches_the_existing_demods() {
    // plan_start [1,2,3] attach null; plan_start [1,2,3] attach transcribe → existing 3 each with attached Some, started empty, 6 running jobs;
    // a third run → existing 3 with the same attached ids, running still 6.
}
```

In `crates/zeta/tests/attach.rs` (real stages, `speech` fixture is one channel wide; use a `Radio`-like file source with a tune range or the `tone` fixture with `tune_range` over 470-480 MHz and `channels: [1, 2]`):

```rust
#[tokio::test]
async fn plan_start_with_transcribe_is_refused_without_a_backend() {
    // TranscribeStage::new(None, ..): Err(Other) whose message contains NO_BACKEND_REASON and "start without attach".
}

#[tokio::test]
async fn plan_start_with_transcribe_over_a_fake_backend_attaches_real_transcribers() {
    // TranscribeStage::new(Some(FakeBackend::new(TEXT)), ..), channels [1, 2]: 2 started with attached; job_list has 2 fm_demod + 2 transcribe running; warnings empty.
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p zeta-core --test plan_start attach` and `cargo test -p zeta --test attach plan_start`.
Expected: FAIL (behaviour missing or the stubs).

- [ ] **Step 3: Implement** or complete steps 3 and 7 and the warning.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p zeta-core --test plan_start && cargo test -p zeta --test attach`.
Expected: PASS.

- [ ] **Step 5: Gates and commit**

```bash
cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/zeta-core crates/zeta/tests/attach.rs
git commit -m "Jobs: plan_start preflights the attached stage, attaches to every channel it starts or finds, and warns over the stage's capacity"
```

---

### Task 7: CLI `zeta plan start`, the guide and field craft (Sonnet)

**Files:**
- Modify: `crates/zeta/src/cli.rs:192-200` (`Command::Plan(PlanCmd)`; `Plans` stays), `:766-890` (`into_op`, `is_mutating`), `parse_channels`
- Modify: `crates/zeta/src/agents_md.rs:13-20` (`RULES`), `:50-100` (`note`), `:101-121` (`example_for`, `hint`), `:372-420` (`cli_form`)
- Modify: `docs/agents/field-craft.md:130-146`
- Modify: `README.md` (§CLI walkthrough one example; §Transports), `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (row 21), `TODO.md:292-301`
- Test: `crates/zeta/tests/cli.rs`, `crates/zeta/tests/agents_md.rs`

**Interfaces:**
- Produces:
  - `Command::Plan(PlanCmd)` with `PlanCmd::Start { #[arg(long, value_name = "ID")] plan: String, #[arg(long, value_name = "N[,N|N-M]", value_parser = parse_channels)] channels: Option<Channels>, #[arg(long, value_parser = parse_job_kind)] kind: JobKind, #[arg(long)] source: String, #[arg(long)] deviation_hz: Option<u64>, #[arg(long, value_enum)] sideband: Option<SidebandArg>, #[arg(long, value_enum)] attach: Option<AttachKind>, #[arg(long)] language: Option<String>, #[command(flatten)] replace: ReplaceArg }`; `pub fn parse_channels(s: &str) -> Result<Channels, String>` accepting `1,2,5-8` into a sorted, deduplicated `Vec<u32>` (an empty item or a reversed range is an error); mutating (so refused while `serve` owns the dir, like `job start`).
  - `cli_form("plan_start", ..)` → `zeta plan start --plan uhf_cb_au --kind fm_demod --source <id> [--channels 1,2,5-8] [--attach transcribe] [--language de] [--replace ID,ID]`.
  - `example_for("plan_start", "plan")` → `"uhf_cb_au"`, `("plan_start", "attach")` → `"transcribe"`; `hint("plan")` → "an id from `channel_plans`", `hint("channels")` → "channel numbers of the plan (integers), `null` for all".
  - `RULES` gains the two lines of spec §4.2; `note("plan_start")` (spec §4.4 in the guide's voice, including: check `source_list` first; `replace` as on `job_start`; repeat `warnings` to the operator; run it again to complete a partial set); `note("job_stop")` = "Stopping a job is the operator's decision: never `job_stop` a job the request did not name. Through `ask` the run must allow stops, else the call is refused as `not_allowed`: report that, do not retry."
  - Field craft: new section "Starting a whole plan" after "Channel plans" (the steps of spec §4.9: `source_list` holders other than `elastic`; `plan_start`; read the four lists; a `conflict` is reported and stops; what `not_allowed` means and what to tell the operator: "say `allow stops on`, or press the retry button, then ask again"); "Taking the radio from another job" gets the sentence "Stopping or replacing a job is the operator's explicit decision; hand-made jobs survive any request that did not name them."; the `cb_watch.py` paragraph gets "For every channel at once, `plan_start`."

- [ ] **Step 1: Write the failing tests**

In `crates/zeta/tests/cli.rs`:

```rust
#[test]
fn plan_start_cli_maps_to_the_op_and_parses_channel_ranges() {
    // zeta --state-dir D --source-file cb=<tone relabelled at 476.9 MHz, tune_range 470-480 MHz> plan start --plan uhf_cb_au --kind fm_demod --source cb --channels 1,2,5-8
    // exit 0; stdout JSON value.started has 6 entries with channels [1,2,5,6,7,8]; last_logged_op(dir)["op"]["op"] == "plan_start" and ["channels"] == [1,2,5,6,7,8].
    // --channels 8-5 → exit 1 (usage) with "channels" in stderr.
}
```

In `crates/zeta/tests/agents_md.rs`:

```rust
#[test]
fn plan_start_cli_line_is_the_real_command() {
    // rendering contains "CLI: `zeta plan start --plan uhf_cb_au --kind fm_demod --source <id>" and the Rules contain "never `job_stop`" and "use `plan_start`".
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p zeta --test cli plan_start && cargo test -p zeta --test agents_md plan_start`.
Expected: FAIL (`plan` is not a subcommand; text missing).

- [ ] **Step 3: Implement** the CLI, the renderer additions, the field craft, the README lines, the plan-series row (`| 21 | Agent bulk operations | ... Spec ..., plan ... | 10, 18 |`), and replace the TODO section's body with "Scheduled as plan 21 (`docs/superpowers/plans/2026-11-01-agent-bulk-ops.md`); the four parts and the duplicate refusal are its tasks."

- [ ] **Step 4: Regenerate and run**

```bash
cargo run -p zeta -- agents-md > AGENTS.md && cargo run -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md
cargo test -p zeta --test cli --test agents_md --test transports
```
Expected: PASS, including `committed_agents_md_matches_rendering`.

- [ ] **Step 5: Gates and commit**

```bash
cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/zeta docs README.md AGENTS.md skills TODO.md
git commit -m "Docs: zeta plan start, the plan_start guide and rules (never stop unasked, a plan goes through plan_start), field craft for starting a whole plan"
```

---

### Task 8: The page (Sonnet)

**Files:**
- Modify: `crates/zeta-ui/web/api.js:22, 149-151`
- Modify: `crates/zeta-ui/web/app.js:351` (`REPLACEABLE`), `:501-538` (`askAgent`, `runCommand`), new `runPlanCommand`, `allowStops` state
- Modify: `crates/zeta-ui/web/agent.js:130-178` (`resultText`, `errorText`), `:244-247` (`assistant`)
- Modify: `crates/zeta-ui/web/menu.js:46-60, 440-460` and `index.html` (a `Start plan…` entry on the selection context menu, `data-action="plan"`)
- Test: `crates/zeta-ui/tests/e2e/ui.py` (the typed `plan start` and the rendering; the `/ask` scenarios come in Task 9)

**Interfaces:**
- Consumes: `/ask` `allow` and `turn_log` (Task 2); `plan_start` result and the `duplicate`/`not_allowed` errors (Tasks 1, 4, 5).
- Produces:
  - `api.ask(text, maxTurns = 32, allow = { stop_jobs: false, replace: false })` → `POST /ask {request, max_turns, allow}`.
  - `state.allowStops: boolean` (false at load, not persisted); `runCommand`: `/^allow\s+stops\s+(on|off)$/` sets it and notes `allow stops: on` / `allow stops: off`; `askAgent` passes `{stop_jobs: state.allowStops, replace: state.allowStops}`.
  - After an `/ask` answer: the assistant line shows ` [may stop jobs]` appended when either flag of `r.allow` is true; `Transcript.assistant(text, error = false, turnLog = [])` appends, after the line, one `<details class="turn">` per entry with `reasoning` or `content`, `<summary>turn N (K calls)</summary>` and the two texts as `<pre>`; a refused call (`r.calls.some(c => c.result && c.result.error === "not_allowed")`) adds `t.note("the agent was not allowed to stop or replace jobs", false, [{label: "allow stops and ask again", run: () => { state.allowStops = true; askAgent(text); }}])`.
  - `runPlanCommand(st, words)` for `plan start <plan> <kind> [<attach>] [ch N,N-M]` on the shown source (`shownSource()` as `source` commands do): builds `{op: "plan_start", plan, channels, kind, source, deviation_hz: null, sideband: null, attach, language: null, replace: []}` and applies it through `applyOp` (so a `conflict` gets the retry buttons; `REPLACEABLE` gains `"plan_start"`); the reply line is `resultText`'s.
  - `resultText` case `"plan_start"`: `ok (<started.length> started, <existing.length> existing, <refused.length> refused)`; the page's reply handler additionally notes each `refused` entry as `ch<channel>: <errorText(error)>` and each warning as a note.
  - `errorText`: `duplicate` → `duplicate of job <job.slice(-6)> <message>`; `not_allowed` → `not allowed: <message>`.
  - Context menu (a drag on the waterfall, `#ctx`): a `Start plan…` item, visible when `view`'s plan policy resolves a plan for the shown source (the overlays' `plan` policy, `overlays.js`), that puts `plan start <planId> fm_demod transcribe` in `#cmd` and focuses it.

- [ ] **Step 1: Write the failing e2e scenarios** in `ui.py` (against the main server's `cb` source, the `tone` relabelled at 476.9 MHz; it has no tune range, so only channels inside its 48 kHz capture start: ch20 is 476.900 MHz, ch59 476.8875, ch60 476.9125, all inside ±24 kHz; use `ch 20,59`):

```python
def scenario_plan_start_typed_renders_counts_and_refusals(page, url, secret):
    # select cb tab; cmd "plan start uhf_cb_au fm_demod ch 20,59"; transcript contains "plan_start" and "ok (2 started, 0 existing, 0 refused)";
    # jobs list shows 2 running fm_demod rows; cmd again → "ok (0 started, 2 existing, 0 refused)";
    # cmd "plan start uhf_cb_au fm_demod ch 1" → an error line (".line.error") containing "plan_start" (ch1 is outside the recording's band, so the tune is refused and the whole op errors; nothing partial);
    # stop the two jobs via their rows.

def scenario_allow_stops_toggle_is_noted(page, url, secret):
    # cmd "allow stops on" → note "allow stops: on"; "allow stops off" → "allow stops: off".

def scenario_duplicate_start_renders_the_job(page, url, secret):
    # start measure on tone twice from the command line (JSON op): the second line contains "duplicate of job".
```

- [ ] **Step 2: Run them to verify they fail**

Run: `ZETA_UI_ONLY=plan_start_typed_renders_counts_and_refusals,allow_stops_toggle_is_noted,duplicate_start_renders_the_job cargo test -p zeta --test ui`.
Expected: FAIL on the missing transcript text.

- [ ] **Step 3: Implement** the page changes above.

- [ ] **Step 4: Run the scenarios, then the whole suite**

Run: the `ZETA_UI_ONLY` line, then `cargo test -p zeta --test ui`.
Expected: PASS; the full run under `DEADLINE` (raise it by the measured addition and say so in the constant's comment).

- [ ] **Step 5: Gates and commit**

```bash
cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/zeta-ui crates/zeta/tests/ui.rs
git commit -m "UI: 32 turns, allow stops on|off, the not-allowed unlock, plan start typed and on the menu, plan_start and duplicate rendering, the collapsed turn log"
```

---

### Task 9: A scripted model behind the e2e harness and the `/ask` page scenarios (Opus)

**Files:**
- Modify: `crates/zeta-agent/src/fake.rs` (`FakeLlm::start_by_request`, `serve_by_request`)
- Create: `crates/zeta-agent/src/bin/zeta-fake-llm.rs` (feature `test-fake`; `Cargo.toml` `[[bin]] required-features = ["test-fake"]`, and a dev-dependency of the crate on itself with the feature so `cargo test --workspace --all-features` builds it, as plan 20 did for `zeta-fake-worker`)
- Modify: `crates/zeta/tests/ui.rs:385-450` (a third server), `crates/zeta/Cargo.toml` (dev-dep `zeta-agent` with `test-fake` for `CARGO_BIN_EXE_zeta-fake-llm`)
- Modify: `crates/zeta-ui/tests/e2e/ui.py`
- Test: `crates/zeta-agent/tests/agent.rs` (the by-request fake), `ui.py`

**Interfaces:**
- Produces:
  - `FakeLlm::start_by_request(scripts: Vec<(String, Vec<(u16, Value)>)>, status: Vec<(u16, Value)>) -> FakeLlm`: for each chat request, the script whose key (a substring) the last `user` message's content contains is used, its queue consumed in order per conversation (keyed by that user text), `500 {"error":"fake: no script for <text>"}` when none matches.
  - `pub async fn serve_by_request(addr: SocketAddr, scripts: ...)` and the `zeta-fake-llm` binary: `zeta-fake-llm --listen ADDR:PORT --scripts FILE.json` where the file is `[{"match": "substring", "replies": [<chat response JSON>, ...]}]`; prints `listening http://ADDR:PORT` on stdout when bound (the harness reads that line, as it reads `ServeProc`'s URL).
  - `ui.rs`: a third `ServeProc` (`start_full(.., &["--source-file", &cb_llm], &["--llm-url", &fake_url, "--llm-model", "fake"], ..)`) on its own state dir with the `cb` fixture, passed as `ZETA_UI_URL_LLM`; the fake's scripts file written by `ui.rs` into its temp dir with three scripts: `"stop the measure"` → `[tool_call(job_stop {"job": "01NOPE"}), text("I was not allowed to stop it.", "stop")]`; `"every CB channel"` → `[tool_call(source_list {}), tool_call(plan_start {"plan":"uhf_cb_au","channels":[20,59],"kind":"fm_demod","source":"cb","deviation_hz":null,"sideband":null,"attach":null,"language":null,"replace":[]}), text("Started 2 channels.", "stop")]`; `"think"` → `[tool_call_response_with(&[("source_list", {})], "Let me look.", Some("I should list the sources first.")), text("one source", "stop")]`.
- Scenarios in `ui.py` against `ZETA_UI_URL_LLM`:

```python
def scenario_ask_sends_32_turns_and_the_allowance(page, url, secret):
    # capture the /ask request body via page.on("request"); ask "think": body max_turns == 32 and allow == {stop_jobs: false, replace: false};
    # the assistant line "one source" appears with a <details class="turn"> whose summary is "turn 1 (1 call)" and whose text contains "I should list the sources first."

def scenario_not_allowed_shows_the_unlock_and_reruns_with_the_allowance(page, url, secret):
    # ask "stop the measure": assistant line present; a note "the agent was not allowed to stop or replace jobs" with a button "allow stops and ask again";
    # click it: the next /ask body has allow.stop_jobs true; the assistant line carries "[may stop jobs]".

def scenario_ask_fans_out_through_plan_start(page, url, secret):
    # ask "an FM decoder on every CB channel": the transcript's [agent] lines include "plan_start" with "ok (2 started"; 2 fm_demod rows running; stop them.
```

The two existing 503 scenarios stay on the main server unchanged.

- [ ] **Step 1: Write the failing tests**: the three scenarios above, and in `crates/zeta-agent/tests/agent.rs`:

```rust
#[tokio::test]
async fn a_by_request_fake_answers_the_script_the_request_names() {
    // two scripts "alpha"/"beta"; an Agent run with request "please alpha" gets alpha's answer, "beta now" gets beta's; "gamma" fails the run with AgentError::Unreachable or Http (the 500).
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p zeta-agent --features test-fake --test agent by_request`; `ZETA_UI_ONLY=ask_sends_32_turns_and_the_allowance,not_allowed_shows_the_unlock_and_reruns_with_the_allowance,ask_fans_out_through_plan_start cargo test -p zeta --test ui`.
Expected: compile error; `ZETA_UI_URL_LLM` unset assertion.

- [ ] **Step 3: Implement** the by-request fake, the binary, the harness server and scripts, the scenarios.

- [ ] **Step 4: Run them, then the whole suite**

Run: the two lines above, then `cargo test -p zeta --test ui` and `cargo test --workspace --all-features --no-fail-fast`.
Expected: PASS; no `FAILED`.

- [ ] **Step 5: Gates and commit**

```bash
cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo run -p zeta -- agents-md --check && cargo run -p zeta -- agents-md --skill --check
git add crates/zeta-agent crates/zeta/Cargo.toml crates/zeta/tests/ui.rs crates/zeta-ui/tests/e2e/ui.py Cargo.lock
git commit -m "Tests: a request-keyed fake model behind the e2e harness; the page's ask scenarios (32 turns, the allowance, the unlock, the fan-out, the turn log)"
```

---

## Acceptance on the field host (after merge; the operator's)

Spec §6, items 1-4, on the `sdr` node with the B210 pinned at 476 918 750 / 1 000 000 Hz, through the page's voice console. Record the outcome under a "Plan 21 status" section of the plan-series document.
