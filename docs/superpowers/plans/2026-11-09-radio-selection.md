# Plan 33: Radio selection policy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A request to the agent that names no radio runs on the radio the operator is looking at (the shown tab, or the kept one while the Map is shown), under a selection policy the operator sets (`focus`, `focus-strict`, `free-first`, `ask`, plus a preference list); the default radio's tab is marked, a run on another radio is noted, and the selection record is the seam later access levels plug into.

**Architecture:** A new `zeta-agent/src/radio.rs` owns the mechanism: the policy, the context, `resolve` (precedence: a named radio, then the policy, then no context) over one `source_list` read before the first completion, the `## This request` paragraph appended to the one system message, and `Selection::admit`, the gate beside `gate(op, allow)` that refuses another radio under `focus-strict` with the agent-only error word `policy` and records `off_default` otherwise. `AskRequest` gains an optional `context`; `zeta ask` gains `--radio`, `--radio-policy`, `--radio-prefer`. The page sends its focus and policy with each ask, keeps two new view policies, names the box menu's radio in its text, notes a run off the default, and marks the default tab with an accent dot.

**Tech Stack:** Rust (serde, tokio, axum, clap), vanilla JS page, the fake OpenAI-compatible model (`zeta-agent` feature `test-fake`), the Playwright e2e suite in `crates/zeta-ui/tests/e2e/ui.py`.

**Spec:** `docs/superpowers/specs/2026-11-09-radio-selection-design.md` (all of it; the calls are section 10, the operator's questions section 11, the builder check section 9).

## Global Constraints

- The plan executes on `main` after the digital-modes sprint branch (plans 19.3, 29, 30, 32) merged into it; plans 28.1 and 28.2 are already there. Re-check every named line against the tree before editing; the names below are from both.
- No op changes: `source` stays required on every op; `Op`, `OpResult` and the schema are untouched, so `agents-md --check` changes only through the guide's prose (Task 6).
- The policy words are exactly `focus`, `focus-strict`, `free-first`, `ask`; the default is `focus`. Serde and `FromStr`/`Display` use these words (`kebab-case`).
- Precedence, first that applies: (1) a source id in the request as a whole word, case-insensitive (one named radio becomes the default with reason `named`; several keep the policy's default; a named radio is never refused); (2) the policy over the context; (3) no context: `focus` with no focus, which behaves as `free-first` in id order.
- `RADIO_OPS = ["job_start", "plan_start", "source_ensure", "source_pin", "source_set_gain"]`, read through each op's `source` field. Nothing else is gated by the radio policy.
- The policy refusal is `{"error": "policy", "field": "source", "policy": "focus-strict", "default": "<id>", "message": "radio policy focus-strict: <src> is not the default radio <id>; the operator names another radio in the request"}`. Never `not_allowed`: the page's unlock offer keys on that word.
- Per-request context goes at the end of the one system message under `## This request`; the user message stays exactly the request (the e2e's fake model matches on it). No second system message.
- `radio_policy` and `radio_prefer` never join `AGENT_VIEW_KEYS` (`view.js`) or `VIEW_KEYS` (`crates/zeta-agent/src/page.rs`), and have no theme default.
- The agent never stops or replaces a job to free a radio; the fallback is a free radio only.
- New theme hooks are documented in `docs/themes.md` and listed in the e2e's `THEME_HOOKS`; CSP is unchanged; `wait_for_function` takes a function body (`() => ...`).
- The e2e suite runs about 474 s under the 690 s deadline (`crates/zeta/tests/ui.rs DEADLINE`): this plan adds one scenario (under 20 s) and extends two; each has explicit timeouts and restores the view in `finally`. One scenario: `ZETA_UI_ONLY=<name> cargo test -p zeta --test ui -- --nocapture`.
- `AGENTS.md` and `skills/zetasdr/SKILL.md` are generated: regenerate (`cargo run -q -p zeta -- agents-md > AGENTS.md`, `cargo run -q -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md`) and commit them with any prose change in `agents_md.rs` or `docs/agents/field-craft.md`.
- Gates (CLAUDE.md): `cargo fmt -p <crate> -- --check` per crate (a nested worktree breaks `cargo fmt --all`), `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`, `cargo test -p zeta --test ui -- --nocapture` (no `ui e2e skipped` line), `cargo run -q -p zeta -- agents-md --check`, `cargo run -q -p zeta -- agents-md --skill --check`, `bash scripts/public-check.sh`, `bash scripts/check-doc-links.sh`. Use a private `CARGO_TARGET_DIR`. Poll for asynchronous end states, never read once.
- The repository is public: source ids in tests and docs are `tone`, `cb`, `b210`, `hackrf`; roles, never host names. One commit per task, message in the repo's style (`Area: what changed`).

## Models per task

Opus where the task defines the interface later tasks consume or changes the agent loop or the page's ask path; Sonnet where the brief is complete and the tests pin it.

| Task | Model | Why |
|---|---|---|
| 1 `radio.rs`: policy, context, resolve, admit, the paragraph | Opus | the record and gate every later task and the future ACL use |
| 2 the agent loop: the pre-read, the prompt section, the gate, `Transcript.selection` | Opus | changes `Agent::run`/`call` and `build_agent` for every transport |
| 3 the transports: `AskRequest.context`, `zeta ask` flags | Sonnet | two thin translations onto Task 1's types |
| 4 the page: the context, the two policies, the off-default note, the menu names its radio | Opus | the page's ask path, the unlock flow, two e2e scenarios |
| 5 the default radio's mark | Sonnet | one class, one span, two tokens, the hooks |
| 6 the guide | Sonnet | prose and regeneration |
| 7 TODO, UAT, the plan series, the gate | Sonnet | |

Ledger: `.superpowers/sdd/plan33/progress.md` in the worktree, one line per task with the implementer's and reviewer's verdicts; rulings made on the operator's behalf go into the final report.

## Review Focus

Inputs the spec implies but no obvious test exercises; each gets a test in the task that owns the code:

1. A page whose stored `zeta.source` names a radio no longer attached (restarted without it) must not send a dead default: the resolver drops it with the note `focus <id> is not a source` and the page's note says so. (Task 1: `an_unlisted_focus_is_dropped_with_a_note`; Task 4: the note line, asserted in `scenario_radio_default_mark_and_policy` by setting `sessionStorage["zeta.source"]` to `gone` before a reload.)
2. A request that mentions a source id as part of another word (`b2100`, `cbx`) or inside a frequency must not count as naming it; a capitalised one (`CB`) must. (Task 1: `naming_is_whole_word_and_case_insensitive`.)
3. A `focus-strict` refusal must not trigger the page's "allow and retry" offer. (Task 4: in the new scenario, `expect(page.locator("#transcript button", has_text="allow")).to_have_count(0)` after the refusal.)
4. The pre-read `source_list` is one more agent-tagged ops record at the start of every run: existing tests counting ops-stream records or the page's op lines after an ask must be updated to expect it, never loosened to ignore counts. (Task 2: `ask_runs_the_agent_and_its_calls_appear_on_the_ops_stream` in `crates/zeta-ws/tests/ask.rs` asserts the first agent record is `source_list`; the ask scenarios in `ui.py` are rerun in Task 4.)
5. `ask` and `focus-strict` end with prose; a run whose model asks without any call would fail `NoToolCall`. The guide makes `source_free` the first call, and the loop's rule is unchanged. (Task 2: `ask_policy_answers_after_source_free`; and a scripted prose-only reply to a command under `ask` still fails `NoToolCall`, `ask_policy_does_not_relax_the_prose_rule`.)

---

### Task 1: `radio.rs`: the policy, the context, `resolve`, `admit`, the paragraph

**Files:**
- Create: `crates/zeta-agent/src/radio.rs`
- Modify: `crates/zeta-agent/src/lib.rs` (`pub mod radio;`, re-export `RadioPolicy, RadioContext, FocusFrom, Caller, Selection, Reason, OffDefault, resolve`)
- Test: `crates/zeta-agent/src/radio.rs` tests module

**Interfaces:**
- Consumes: `zeta_core::types::SourceId`, `zeta_core::ops::SourceInfo` (its `id` and `fault`), `serde_json::Value`.
- Produces:
  - `#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all = "kebab-case")] pub enum RadioPolicy { #[default] Focus, FocusStrict, FreeFirst, Ask }` with `FromStr` (error `unknown radio policy <w> (allowed: focus, focus-strict, free-first, ask)`) and `Display` in the same words; `pub const RADIO_POLICIES: &[&str]`.
  - `#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)] #[serde(rename_all = "snake_case")] pub enum FocusFrom { Shown, Kept, Flag, #[default] None }`.
  - `#[derive(Clone, Debug, Default, PartialEq, Eq)] pub struct RadioContext { pub focus: Option<SourceId>, pub focus_from: FocusFrom, pub policy: RadioPolicy, pub prefer: Vec<SourceId> }` (the default is precedence step 3).
  - `#[derive(Clone, Debug, PartialEq, Eq, Serialize)] pub struct Caller { pub transport: String }` with `Caller::page()`, `Caller::http()`, `Caller::cli()` (`"page"`, `"http"`, `"cli"`).
  - `#[serde(rename_all = "snake_case")] pub enum Reason { Named, Shown, Kept, Flag, None }`.
  - `#[derive(Clone, Debug, PartialEq, Eq, Serialize)] pub struct OffDefault { pub tool: String, pub source: SourceId }`.
  - `#[derive(Clone, Debug, PartialEq, Eq, Serialize)] pub struct Selection { pub radio: Option<SourceId>, pub reason: Reason, pub note: Option<String>, pub policy: RadioPolicy, pub caller: Caller, pub candidates: Vec<SourceId>, pub named: Vec<SourceId>, pub off_default: Vec<OffDefault> }` (spec section 5, field for field).
  - `pub fn named_in(request: &str, ids: &[SourceId]) -> Vec<SourceId>`: ids appearing as whole words, case-insensitive, in `ids` order. A word boundary is any char that is not ASCII alphanumeric, `-` or `_` (source ids may hold `-` and `_`).
  - `pub fn resolve(request: &str, ctx: &RadioContext, sources: &[SourceInfo], caller: Caller) -> Selection`.
  - `impl Selection { pub fn admit(&mut self, tool: &str, args: &Value) -> Option<Value>; pub fn prompt_section(&self) -> String }`: `admit` returns the refusal JSON (Global Constraints) or `None`, and pushes `OffDefault` for an admitted radio op whose `source` is not `radio` (nothing when `radio` is `None`).
  - `pub const RADIO_OPS: &[&str]` (Global Constraints).

- [ ] **Step 1: Write the failing tests**

```rust
fn src(id: &str, fault: Option<&str>) -> SourceInfo { /* minimal SourceInfo with the id and fault, file-source capabilities */ }
#[test] fn policy_words_round_trip() { /* every RADIO_POLICIES word parses and displays back; serde json "focus-strict" <-> FocusStrict; "nearest" errors naming the four words */ }
#[test] fn naming_is_whole_word_and_case_insensitive() { /* ids [b210, cb, hackrf]: "start APRS on B210" -> [b210]; "on b2100" -> []; "cbx and cb-2" -> []; "use CB, then hackrf" -> [cb, hackrf]; "144.8 b210:" -> [b210] */ }
#[test] fn a_named_radio_wins_under_every_policy() { /* for each policy with focus cb: request naming hackrf -> radio Some(hackrf), reason Named */ }
#[test] fn two_named_radios_keep_the_policy_default() { /* focus cb, policy focus, request naming b210 and hackrf -> radio Some(cb), reason Shown, named [b210, hackrf] */ }
#[test] fn focus_reads_shown_or_kept() { /* focus_from Shown -> Reason::Shown; Kept -> Reason::Kept; Flag -> Reason::Flag; radio Some(cb) each */ }
#[test] fn an_unlisted_focus_is_dropped_with_a_note() { /* focus "gone" -> radio None, reason None, note Some("focus gone is not a source") */ }
#[test] fn a_faulted_focus_stays_the_default_with_a_note() { /* cb faulted "usb gone" -> radio Some(cb), note Some("focus cb is faulted: usb gone") */ }
#[test] fn free_first_and_ask_have_no_default() { /* radio None, reason None, under both, focus set */ }
#[test] fn candidates_follow_prefer_then_id_order() { /* sources [tone, cb, hackrf, b210], prefer [hackrf, gone, cb] -> [hackrf, cb, b210, tone] */ }
#[test] fn no_context_is_focus_without_a_focus() { /* RadioContext::default() -> policy Focus, radio None, reason None, note None */ }
#[test] fn strict_refuses_another_radio_on_every_radio_op() { /* FocusStrict, radio cb: each RADIO_OPS tool with {"source": "tone"} -> Some(json) equal to the constraint's object with src tone, default cb; with {"source": "cb"} -> None */ }
#[test] fn strict_admits_a_named_radio() { /* request names tone -> admit job_start tone is None */ }
#[test] fn reads_and_releases_are_never_checked() { /* FocusStrict: source_list, source_free, source_unpin, classify, job_stop with a "source" or selection on tone -> None, off_default empty */ }
#[test] fn focus_admits_and_records_off_default() { /* Focus, radio cb: job_start tone -> None; off_default == [OffDefault{tool: "job_start", source: tone}]; job_start cb adds nothing */ }
#[test] fn no_default_records_nothing() { /* FreeFirst: job_start tone -> None, off_default empty */ }
#[test] fn the_paragraph_names_the_default_and_the_rule() { /* prompt_section() starts "## This request"; Focus/Kept contains "Default radio: cb (policy focus: the radio last shown on the page; the Map is shown)" and "use the first free radio `source_free` gives"; FocusStrict contains "do not use another radio" and "ask which radio to use"; FreeFirst contains "No default radio (policy free-first)"; Ask contains "call `source_free` for the band, then ask which"; Named contains "the request names hackrf"; no context contains "No default radio (no radio in focus)"; every variant lists "Radios in preference order:" with the candidates and none contains "replace" except as "never `replace`" */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-agent radio::` — Expected: FAIL to compile (`radio` module missing).
- [ ] **Step 3: Implement** the types and functions in `radio.rs`. The paragraphs are `const` texts per policy and reason with `{radio}` and `{candidates}` filled by `prompt_section` (spec 4.4's example is the `focus`/`kept` text verbatim); the note is appended as its own line when present.
- [ ] **Step 4: Run** — `cargo test -p zeta-agent radio:: && cargo clippy -p zeta-agent --all-targets --all-features -- -D warnings` — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-agent/src/radio.rs crates/zeta-agent/src/lib.rs
git commit -m "Agent: radio selection (radio.rs): the policy, the context, resolve with a named radio first, the focus-strict gate and the request's paragraph"
```

---

### Task 2: The agent loop: the pre-read, the prompt section, the gate, `Transcript.selection`

**Files:**
- Modify: `crates/zeta-agent/src/agent.rs` (`AgentConfig.radio: RadioContext`, `AgentConfig.caller: Caller`; `build_agent` takes `radio: RadioContext, caller: Caller` after `page`; `Agent::run`; `Agent::turns`; `Agent::call`; `Transcript.selection: Option<Selection>`), `crates/zeta-ws/src/ask.rs` and `crates/zeta/src/ask.rs` (pass `RadioContext::default()` and `Caller::page()`/`Caller::http()` by `page_tools`, `Caller::cli()`; Task 3 fills them)
- Test: `crates/zeta-agent/tests/agent.rs`, `crates/zeta-ws/tests/ask.rs`

**Interfaces:**
- Consumes: Task 1's `RadioContext`, `Caller`, `Selection`, `resolve`, `Selection::admit`, `Selection::prompt_section`; `Executor::apply`; the existing `gate(op, allow)`.
- Produces: `build_agent(llm, exec, schema, agents_md, max_turns, allow, page, radio: RadioContext, caller: Caller)`; `Transcript.selection: Option<Selection>` (`Some` once the pre-read succeeded; serialized as `selection`); `AgentConfig::new` defaults `radio` to `RadioContext::default()` and `caller` to `Caller::cli()`.

- [ ] **Step 1: Write the failing tests**

```rust
// tests/agent.rs: a two_source_node(dir) helper beside tone_node: file sources `tone` and `cb`
// (both the tone recording, tune_range Some((100_000_000, 200_000_000)))
#[tokio::test] async fn the_system_message_ends_with_this_request() { /* cfg.radio = focus cb Shown; scripted text answer to a question; requests(&fake)[0] messages: one system message whose content ends with a "## This request" section naming "Default radio: cb"; the user message == the request exactly */ }
#[tokio::test] async fn one_source_list_is_read_before_the_first_completion() { /* a recording executor wrapper: its first applied op is {"op":"source_list"} and it is applied before the fake saw a request; transcript.calls does not contain it */ }
#[tokio::test] async fn strict_refuses_job_start_on_another_radio() { /* FocusStrict, focus cb; script: job_start measure on tone, then text; the second chat request's tool message content has "error":"policy" and "default":"cb"; job_list is empty; transcript.calls[0].ok == false */ }
#[tokio::test] async fn focus_starts_it_and_records_off_default() { /* Focus, focus cb; same script: the job runs on tone; transcript.selection.off_default == [{tool: job_start, source: tone}]; stop the job at the end */ }
#[tokio::test] async fn ask_policy_answers_after_source_free() { /* Ask; script: source_free, then text "Which radio: tone or cb?"; request "start a measure on 150 MHz" -> Ok, answer set */ }
#[tokio::test] async fn ask_policy_does_not_relax_the_prose_rule() { /* Ask; script: text only; request "start a measure on 150 MHz" -> Err NoToolCall */ }
#[tokio::test] async fn no_free_radio_starts_nothing() { /* Focus, focus cb, a measure on tone holding it; script: source_free (empty free for a band only tone's range holds) then text "Nothing free: tone is held by measure <id>."; Ok with the answer; job_list unchanged; no call carried a non-empty replace */ }
#[tokio::test] async fn a_run_without_context_states_no_radio_in_focus() { /* AgentConfig::new default radio; system message contains "No default radio (no radio in focus)" */ }
// crates/zeta-ws/tests/ask.rs
// ask_runs_the_agent_and_its_calls_appear_on_the_ops_stream: the first record tagged "agent" is source_list (the pre-read), then the scripted calls as before
#[tokio::test] async fn ask_answer_carries_the_selection() { /* POST /ask without context: 200, body["selection"]["policy"] == "focus", ["caller"]["transport"] == "http", ["radio"] null */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-agent --features test-fake --test agent && cargo test -p zeta-ws --test ask` — Expected: FAIL (no `radio` on `AgentConfig`, no `selection`).
- [ ] **Step 3: Implement.** In `Agent::run`, after `wait_ready` and before the first completion: apply `{"op":"source_list"}` through the executor (a node error aborts the run as any `AgentError::Node` does; an op error leaves `selection` `None` and the step-3 paragraph); parse `value` as `Vec<SourceInfo>`; `resolve(request, &cfg.radio, &sources, cfg.caller.clone())`; the system message is `cfg.system_prompt` + `"\n"` + `selection.prompt_section()`. In `Agent::call`, after `gate(op, allow)` admits, call `selection.admit(tool, &arguments)` and, on `Some(err)`, record the call `ok: false` with `err` as its result, never applying it. Keep `selection` in the transcript at the end of the run, success or failure. Update the two callers of `build_agent`.
- [ ] **Step 4: Run** — `cargo test -p zeta-agent --features test-fake && cargo test -p zeta-ws && cargo test -p zeta --test ask` — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-agent crates/zeta-ws crates/zeta/src/ask.rs
git commit -m "Agent: every run reads the radios once, states its default radio in the system message, holds focus-strict to it and reports the selection in the transcript"
```

---

### Task 3: The transports: `AskRequest.context` and the `zeta ask` flags

**Files:**
- Modify: `crates/zeta-ws/src/ask.rs` (`AskContext`, `AskRequest.context`), `crates/zeta/src/cli.rs` (`AskArgs.radio`, `radio_policy`, `radio_prefer`), `crates/zeta/src/ask.rs` (build `RadioContext` from the flags)
- Test: `crates/zeta-ws/tests/ask.rs`, `crates/zeta/tests/cli.rs`

**Interfaces:**
- Consumes: Task 1's `RadioPolicy` (`FromStr`), `RadioContext`, `FocusFrom`; Task 2's `build_agent`.
- Produces: `#[derive(Deserialize, Default)] #[serde(deny_unknown_fields)] struct AskContext { focus: Option<String>, main: Option<String>, radio_policy: Option<RadioPolicy>, #[serde(default)] radio_prefer: Vec<String> }`; `main` must be `rf` or `map` when given (else 422 `invalid ask request: context.main must be rf or map`); `focus_from` is `Kept` for `map`, else `Shown`, `None` without a focus. `AskArgs`: `--radio ID` (`env = "ZETA_RADIO"`), `--radio-policy P` (`env = "ZETA_RADIO_POLICY"`, `value_parser` over `RadioPolicy::from_str`, default `focus`), `--radio-prefer IDS` (`env = "ZETA_RADIO_PREFER"`, comma-separated, `value_delimiter = ','`); `--radio` gives `FocusFrom::Flag`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/zeta-ws/tests/ask.rs
#[tokio::test] async fn ask_with_context_resolves_the_focus() { /* {"request": "what sources?", "context": {"focus": "tone", "main": "map", "radio_policy": "focus", "radio_prefer": []}}: 200, selection.radio "tone", reason "kept", caller "page" when page_tools true */ }
#[tokio::test] async fn ask_context_refuses_an_unknown_field() { /* context {"focus": "tone", "colour": 1}: 422 whose message names `colour` */ }
#[tokio::test] async fn ask_context_refuses_an_unknown_policy() { /* radio_policy "nearest": 422 whose message contains "nearest" */ }
#[tokio::test] async fn ask_context_refuses_a_bad_main() { /* main "spectrum": 422 "context.main must be rf or map" */ }
// crates/zeta/tests/cli.rs
#[test] fn ask_radio_policy_refuses_an_unknown_word() { /* `zeta ask --llm-url http://127.0.0.1:9 --llm-model m --radio-policy nearest hi` exits 2, stderr names focus-strict */ }
#[test] fn ask_radio_flags_parse_and_read_the_environment() { /* Cli::try_parse_from with --radio b210 --radio-policy focus-strict --radio-prefer b210,hackrf; and with ZETA_RADIO_POLICY=ask set on the parse, radio_policy == Ask */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-ws --test ask && cargo test -p zeta --test cli ask_radio` — Expected: FAIL.
- [ ] **Step 3: Implement** `AskContext`, its mapping to `RadioContext`, the caller by `page_tools`, the three flags and their mapping; `zeta ask --help` lists them.
- [ ] **Step 4: Run** — `cargo test -p zeta-ws && cargo test -p zeta --test cli && cargo run -q -p zeta -- agents-md --check` (the flag lines change only if the guide lists `zeta ask` flags; regenerate if it fails) — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-ws crates/zeta AGENTS.md skills
git commit -m "Ask: /ask takes the page's context (focus, main, radio_policy, radio_prefer) and zeta ask takes --radio, --radio-policy and --radio-prefer"
```

---

### Task 4: The page: the context, the two policies, the off-default note, the menu names its radio

**Files:**
- Modify: `crates/zeta-ui/web/api.js` (`ask(text, maxTurns, allow, context)` and the header comment), `crates/zeta-ui/web/view.js` (`POLICIES` entries `radio_policy`, `radio_prefer`; the header list), `crates/zeta-ui/web/app.js` (`askContext()`, `askAgent` sends it, the notes after the answer), `crates/zeta-ui/web/menu.js` (the `ask` action's text and the header table), `crates/zeta/tests/ui.rs` (`fake_llm_scripts` gains "radio fallback probe"), `crates/zeta-ui/tests/e2e/ui.py`
- Test: `crates/zeta-ui/tests/e2e/ui.py`

**Interfaces:**
- Consumes: Task 3's body shape `context: {focus, main, radio_policy, radio_prefer}`; Task 2's `selection` on the answer (`radio`, `policy`, `note`, `off_default`).
- Produces: `askContext() -> {focus: state.selected ?? null, main: view.get("main"), radio_policy: view.get("radio_policy"), radio_prefer: [ids]}`; `view.get("radio_prefer")` returns the stored text (`none` or `a,b`), split by `askContext`; `defaultRadio() -> id | null` in `app.js` (`state.selected` under `focus`/`focus-strict`, else `null`) for Task 5.
- The menu text: `` `Look at ${s.centerHz} Hz, ${s.bandwidthHz} Hz wide, on ${s.sourceId}, from ${s.startNs} to ${s.endNs}: what is it?` ``.
- The notes (one transcript note line each, after the answer line): `ran on <src> (<tool>), not the default radio <radio> (<policy>)` per distinct off-default source (tools joined by `, `); `<note>; no default radio` when `selection.note` is set and `selection.radio` is null.
- `radio_prefer`'s parse: `none`, or comma-separated `SOURCE_ID` matches with no repeats; describe `none, a comma-separated list of source ids`.

- [ ] **Step 1: Write the failing assertions**
  - `fake_llm_scripts`: `{"match": "radio fallback probe", "replies": [tool_call_response(&[("job_start", <measure on tone, center 100_000_000, bandwidth 12_500, nulls, replace []>)]), text_response("Started it on tone.", "stop")]}`; the ui.py comment above `llm_url` lists it.
  - `scenario_ask_sends_32_turns_and_the_allowance`: `assert asks[0]["context"] == {"focus": "cb", "main": "rf", "radio_policy": "focus", "radio_prefer": []}`.
  - new `scenario_radio_default_mark_and_policy` (on `llm_url()`, after `select_source(..., "cb")`, view reset and every job it started stopped in `finally`):
    - click the Map tab, run `radio fallback probe`: the ask's `context` has `main` `map`, `focus` `cb`; a note line `ran on tone (job_start), not the default radio cb (focus)` shows.
    - `view set radio_policy focus-strict`, run it again: the turn shows `→ job_start` with `policy` in its result; `expect(page.locator("#transcript button", has_text="allow")).to_have_count(0)`; polling `job_list` shows one measure on `tone` (the first run's), not two.
    - `view set radio_prefer cb,cb` answers the not-allowed error; `view set radio_prefer tone,cb` then an ask posts `radio_prefer: ["tone", "cb"]`.
    - `sessionStorage["zeta.source"] = "gone"` is not reachable through the tabs: instead post `/ask` from the test (`http_json`) with `context.focus` `gone` and assert `selection.note == "focus gone is not a source"` (the page's note formatting is covered by a unit-style `page.evaluate` of the note helper, exported on `window.zetaTest.selectionNotes(selection)`).
  - The existing menu "Send to agent" coverage (find it under `# ---- context menu (Task 9)`; else add to the new scenario through the box menu on `cb`): the user line contains `on cb,`.
- [ ] **Step 2: Run to verify they fail** — `ZETA_UI_ONLY=ask_sends_32_turns_and_the_allowance,radio_default_mark_and_policy cargo test -p zeta --test ui -- --nocapture` — Expected: FAIL.
- [ ] **Step 3: Implement** the policies, `askContext`, the `api.ask` parameter, the notes (`selectionNotes(selection) -> [text]`, also on `window.zetaTest`), the menu text. Insert the new scenario in definition order before `fm_demod_does_not_poll_streams` (shard 2's half); leave `SHARD_STARTS` unchanged unless the timings in the run say a shard passed 345 s.
- [ ] **Step 4: Run** — the two scenarios, then every `ask_*` scenario and `transcript_hides_polls` (`ZETA_UI_ONLY=...`), then the full suite once: `cargo test -p zeta --test ui -- --nocapture` — Expected: PASS, no `ui e2e skipped`.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-ui crates/zeta/tests/ui.rs
git commit -m "UI: each ask carries the shown radio and the radio policy (view set radio_policy, radio_prefer); a run on another radio is noted; Send to agent names its radio"
```

---

### Task 5: The default radio's mark

**Files:**
- Modify: `crates/zeta-ui/web/app.js` (`paintDefault()` called from `select`, `syncMainTabs`, `renderTabs` and the view's `onChange` for `radio_policy`; the header comment), `crates/zeta-ui/web/style.css` (`.src-tab .default-mark`, tokens), `docs/themes.md` (Regions: `.src-tab.default`, `.src-tab .default-mark`; tokens `--default-mark`, `--default-mark-size`), `crates/zeta-ui/tests/e2e/ui.py` (`THEME_HOOKS`; assertions in `scenario_radio_default_mark_and_policy`)
- Test: `crates/zeta-ui/tests/e2e/ui.py`

**Interfaces:**
- Consumes: Task 4's `defaultRadio()`.
- Produces: class `default` on the default radio's `.src-tab`; a `span.default-mark` (`aria-hidden="true"`) first in every source tab, shown only under `.src-tab.default`; the default tab's `title` gains `default radio for the agent (policy <p>)` (appended after any pin title, separated by `; `); tokens `--default-mark: var(--accent)`, `--default-mark-size: 6px`.

- [ ] **Step 1: Write the failing assertions** in `scenario_radio_default_mark_and_policy`, before the asks:
  - `expect(page.locator(".src-tab.default")).to_have_count(1)`; its `data-id` is `cb`; its title contains `default radio for the agent (policy focus)`; `.src-tab.default .default-mark` is visible.
  - after the Map tab: the `cb` tab has classes `kept` and `default`.
  - after `view set radio_policy free-first`: `.src-tab.default` count 0; after `ask`: 0; after `focus-strict`: 1, title says `(policy focus-strict)`.
  - after selecting `tone`'s tab: the mark moves to `tone`.
  - `THEME_HOOKS` gains `".src-tab.default"` and `".src-tab .default-mark"`.
- [ ] **Step 2: Run to verify they fail** — `ZETA_UI_ONLY=radio_default_mark_and_policy,theme_hooks cargo test -p zeta --test ui -- --nocapture` — Expected: FAIL.
- [ ] **Step 3: Implement** `paintDefault`, the span in `renderTabs`, the CSS and tokens, the themes doc rows.
- [ ] **Step 4: Run** — the two scenarios, `theme_shots` and `new_themes` (the tabs changed by one span; update the screenshots only if the change is the dot), then the full suite — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-ui docs/themes.md
git commit -m "UI: the default radio's tab carries an accent dot and says so on hover, on the RF view and the Map alike (.src-tab.default, --default-mark)"
```

---

### Task 6: The guide

**Files:**
- Modify: `crates/zeta/src/agents_md.rs` (`RULES`: the radios rule after the ids rule, spec 4.7 verbatim; the `source_free` note gains "Its `free` list is in id order, which is no preference: the request's default radio and preference order (`## This request`) come first."), `docs/agents/field-craft.md` (the page paragraph: the dot, `view set radio_policy focus|focus-strict|free-first|ask`, `radio_prefer`, the box menu names its radio, the page's note), `docs/guide.md` (the view-policy table rows `radio_policy` and `radio_prefer`; the `zeta ask` flags `--radio`, `--radio-policy`, `--radio-prefer` with their environment variables), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta/tests/agents_md.rs` (existing)

- [ ] **Step 1: Write the prose.**
- [ ] **Step 2: Regenerate and check** — `cargo run -q -p zeta -- agents-md > AGENTS.md && cargo run -q -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md && cargo run -q -p zeta -- agents-md --check && cargo run -q -p zeta -- agents-md --skill --check && cargo test -p zeta --test agents_md && cargo test -p zeta-agent --features test-fake && bash scripts/check-doc-links.sh` — Expected: clean (the agent tests read the system prompt from the regenerated guide).
- [ ] **Step 3: Commit**

```bash
git add crates/zeta/src/agents_md.rs docs AGENTS.md skills
git commit -m "Guide: radios: a named radio, else the request's default, else the first free; say when another radio was used and why; the page's radio policy and mark"
```

---

### Task 7: TODO, UAT, the plan series, the gate

**Files:**
- Modify: `TODO.md`, `UAT.md`, `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md`, `AGENTS.md` and `skills/zetasdr/SKILL.md` (regenerated as the final check)

- [ ] **Step 1: TODO.md** — Under "Access: ACLs on restricted rows" add the radio seam: `Selection.candidates` filtered per caller in `resolve`, `Selection::admit` the agent's check and the same check in `Node::apply_from` for direct ops, transmit ops joining `RADIO_OPS` as transmit and requiring the caller's TX level (and, likely, a named radio: `focus` never implies a transmit radio), `Capabilities.tx` the radio's flag, the selection appended to `ops.log` with the caller. Add a gist "Radio selection: later": `radio_prefer` by driver or role (needs a driver in `SourceInfo`), a node-stored per-caller policy, and the answers to the spec's operator questions if they change anything.
- [ ] **Step 2: UAT.md** — "Plan 33: radio selection policy" with spec 6.5's seven checks as checkboxes.
- [ ] **Step 3: Plan series** — row 33 ("Radio selection policy: the agent's default radio follows the page's focus under a user-set policy (`focus`, `focus-strict`, `free-first`, `ask`, `radio_prefer`); the default tab's mark; the selection record as the access seam; spec `docs/superpowers/specs/2026-11-09-radio-selection-design.md`, plan `docs/superpowers/plans/2026-11-09-radio-selection.md`") and a "Plan 33 status" section listing what shipped, what the gate proved, the calls (spec section 10) and what waits for the field host.
- [ ] **Step 4: Regenerate and gate** — regenerate both generated files, then every gate in Global Constraints. Expected: all green, the generated files unchanged by the regeneration.
- [ ] **Step 5: Commit**

```bash
git add TODO.md UAT.md docs AGENTS.md skills
git commit -m "Docs: plan 33 (radio selection policy) in TODO, UAT and the plan series"
```

---

## Calls made in this plan (the builder's, on the spec's silence)

1. The pre-read is an ordinary `source_list` through the executor, so it works against a remote node and shows as one agent-tagged ops record; it is not a `CallRecord` (the model did not make it), so turn and call counts are unchanged.
2. A word boundary for naming is any character other than ASCII alphanumerics, `-` and `_`.
3. `Caller` is `page` when `page_tools` is true, `http` for any other `/ask`, `cli` for `zeta ask`.
4. The page's note for a dropped focus is tested through `window.zetaTest.selectionNotes` rather than a stale `sessionStorage` reload, because `renderTabs` replaces a stale stored id with the first source before any ask could carry it; the node's resolver is the real guard and is tested on the wire.
5. The two view keys use the view's existing per-tab storage (spec call 5); if the operator answers spec question 2 with "persist", Task 4 gains a local-storage path for exactly these two keys before it starts.
6. `radio_prefer`'s view value is stored as the text the operator typed (`none` or `a,b`) and split when sent, like `home`.

## Self-review notes

- Spec coverage: 4.1 → Task 1 (policies, precedence, prefer); 4.2 → no task (nothing to build; the TODO in Task 7); 4.3 → Task 4 (page), Task 3 (CLI); 4.4 → Tasks 1-3; 4.5 → Tasks 1, 2; 4.6 → Tasks 3, 4; 4.7 → Task 6; 4.8 → Tasks 2, 4; 4.9 → Task 5; section 5 → Task 1's `Selection`, Task 7's TODO; section 6 → each task's tests; 6.5 → Task 7's UAT; section 7 → Tasks 5, 6, 7.
- Names across tasks: `RadioPolicy`/`RADIO_POLICIES`/`FocusFrom`/`RadioContext`/`Caller`/`Reason`/`OffDefault`/`Selection`/`named_in`/`resolve`/`admit`/`prompt_section`/`RADIO_OPS` (1 → 2, 3); `build_agent(.., page, radio, caller)` and `Transcript.selection` (2 → 3, 4); `AskContext` (3 → 4); `askContext`/`defaultRadio`/`selectionNotes` (4 → 5).
