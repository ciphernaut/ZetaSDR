# View Policy, Elastic Leases and Atomic Replace Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The waterfall page stops holding the radio: its spectrum job yields to any decoder (elastic leases), a client can hand the radio over atomically (`replace`), a restart brings every job back the way it was (widest capture first, graphs re-expanded), every applied operation is on disk with its caller (ops log), and the page shows two layers, the targeted capture over a stitched scan from a free device, governed by a table of named policies set by a typed command.

**Architecture:** In `zeta-core` the lease gains an `elastic` flag (ignored by conflict checks, relaunched on every retune of its source), `job_start`/`source_ensure` gain `replace: [job ids]` applied under the source's gate, `Node::open` sorts a source's records widest first and re-expands built-in graphs for the actual capture rate, and `apply_from` appends each op to `<state_dir>/ops.log`. In the page, the own spectrum job is elastic and visible; a new `view.js` holds the policy registry and the `view` commands; a new `coarse.js` owns automatic scans on the scanner source; `waterfall.js` paints coarse rows under the fine capture and eases the view on retunes; `overlays.js` outlines every rigid holder's band.

**Tech Stack:** Rust 2024 (`zeta-core`, `zeta-source-file`, `zeta-ws`, `zeta`), vanilla JS (`crates/zeta-ui/web`), Playwright scenarios in `crates/zeta-ui/tests/e2e/ui.py` driven by `crates/zeta/tests/ui.rs`.

**Spec:** `docs/superpowers/specs/2026-10-20-view-policy-design.md` (sections 3.1 to 3.4 node, 4.1 to 4.6 page, 5 CLI, 6 constraints, 8 calls made).

## Global Constraints

- `zeta-core` gains no dependency. `JobRecord` keeps format 1; every new field carries `#[serde(default)]` (`elastic: bool`, `capture_rate_hz: Option<u64>`). The ws and plugin wires are unchanged.
- The guide's rule holds for the ops: every field of an op is required and unknown fields are rejected, so `job_start` gains `elastic: bool` and `replace: [JobId]` as required fields, `source_ensure` gains `replace: [JobId]`, and every in-repo caller (UI, CLI, tests, agent fixtures) sends them. Every task that changes an op, kind, flag or note regenerates `AGENTS.md` (`cargo run -p zeta -- agents-md > AGENTS.md`) and `skills/zetasdr/SKILL.md` (`--skill`) and runs `cargo test -p zeta --test agents_md --test transports`.
- Elastic is allowed on `spectrum` only (`JobKind::elastic_capable()`); another kind is `OpError::Other("<kind> cannot be elastic")`. An elastic job never conflicts, never retunes a held port, follows every retune of its source (same job id, new graph and streams, record `tuning` = the capture), and is stopped by a scan on its source. `source_list` lists elastic jobs under `elastic`, never under `holders`.
- `replace` is checked under the source's gate: every named job must hold a rigid lease on that source (`unknown_job`, or `OpError::Other("job <id> does not hold source <src>")`); named jobs are stopped with full `job_stop` semantics before the request; a later conflict with an unnamed holder leaves them stopped and names the remaining holders. The `conflict` options gain the line `retry with replace: [<holders>]`.
- Resume: a source's running records launch widest `bandwidth_hz` first (ties by `created_ns`), elastic after every rigid one, attached after their inputs; a built-in kind whose `capture_rate_hz` differs from the actual rate after the tune is re-expanded from its kind and the parameters in its stored graph; `graph_put` clears `capture_rate_hz`, so an operator's graph is launched as stored; records without the field are treated as matching.
- Ops log: one JSON line per applied op (`ops_payload` plus `"t_ns"`) appended to `<state_dir>/ops.log`, rotated to `ops.log.1` when it exceeds 8 MiB (`8 * 1024 * 1024` bytes, two files at most); a write failure never fails the op and is recorded once in `persist_errors`. `zeta ops tail [--lines N]` (default 50) prints the last N lines; with `--node` it is a usage error (exit 1).
- Page policies and defaults: `follow` (`target` | `free`, default `target`), `zoom_out` (`scan` | `none`, default `scan`), `auto_scan_stop` (`inside` | `keep`, default `inside`), `finished_rows_s` (non-negative integer seconds, default `60`, `0` = never shown), `scanner` (`auto` | a source id, default `auto`). Commands `view set <key> <value>`, `view show`, `view reset`, `view clear`; an unknown key or value is a transcript error naming the allowed values. State in `sessionStorage["zeta.view"]`.
- Easing on a capture change under `follow target`: about 400 ms. Automatic scans are debounced about 300 ms after the last zoom or pan, span = the visible span clamped to the scanner's tune range, replaced (stop old, start new) when the span changes, stopped when the view returns inside the fine capture under `auto_scan_stop inside`; a scan the page did not start is never touched. The page's automatic scan id is kept in `sessionStorage["zeta.autoScan"]`.
- The page reloads itself when the node's `build` changes (already on `main`); `stream tail --timeout-ms` is capped at 10 000 by the node.
- Gates per task: `cargo fmt -p <touched crates> -- --check` (the `--all` form fails in a nested worktree), `cargo clippy --workspace --all-targets --all-features -- -D warnings`, the crate's tests (`--features test-fixtures` where fixture-gated); UI tasks run `cargo test -p zeta --test ui` with no `ui e2e skipped` line within its 300 s deadline. Final gate: `cargo test --workspace --all-features --no-fail-fast` (no `FAILED`, no skip line), `agents-md --check` and `--skill --check`. Temp files under the job's tmp directory, never `/tmp`; Python via `python3 -I` except the Playwright runner.

## Review Focus

1. A restart with a persisted running `scan` and demodulators on the same source: the scan resumes with its time-shared lease and the demodulators fail with the `conflict` reason naming the scan, exactly as they would live; nothing spins. Pinned in Task 1 (`scan_resumes_first_and_names_itself_in_the_others_reason`).
2. `replace` naming the page's elastic job, or an id that holds another source: the request is refused with the exact text, nothing is stopped. Pinned in Task 4 (`replace_refuses_an_elastic_or_foreign_holder`).
3. The port cannot retune for a rigid start while an elastic job holds it (a port error): the rigid start fails with the port's reason and the elastic job keeps running on the old capture. Pinned in Task 3 (`failed_retune_leaves_the_elastic_job_running`).
4. Zooming out to a span the scanner cannot reach at all (entirely outside its tune range): no scan is started, one transcript line says so, and the coarse layer stays empty. Pinned in Task 6 (`scenario_zoom_out_beyond_scanner_range_starts_nothing`).
5. A state directory whose `ops.log` cannot be written (read-only): ops still apply, one `persist_errors` entry names the file. Pinned in Task 2 (`unwritable_ops_log_does_not_fail_ops`).

---

## File map

- Modify `crates/zeta-core/src/job.rs` (`elastic`, `capture_rate_hz`), `src/lease.rs` (elastic flag, rigid-only conflicts, `elastic_holders`, gate-held variants, option line), `src/ops.rs` (`JobStart.elastic`/`.replace`, `SourceEnsure.replace`, `JobKind::elastic_capable`, `JobKind::reexpand`, `SourceInfo.elastic`), `src/node/mod.rs` (resume order, `job_start` capture rate, `job_graph_put` clears it, `replace_holders`, `relaunch_elastic`, `stop_elastic`, ops log hook), `src/node/launch.rs` (re-expansion before launch, elastic relaunch after a retune, scan stops elastic), `src/node/persist.rs` (unchanged API).
- Create `crates/zeta-core/src/node/opslog.rs`; tests `crates/zeta-core/tests/resume.rs`, `tests/elastic.rs`, `tests/replace.rs`.
- Modify `crates/zeta-source-file/src/lib.rs` (`FileOpts.tune_range`), `crates/zeta/src/sources.rs` (`--source-file ID=PATH[,tune_min_hz=N,tune_max_hz=M]`), `crates/zeta/src/cli.rs` (`--elastic`, `--replace`, `Command::Ops`), `src/main.rs`, `src/agents_md.rs`; tests `crates/zeta/tests/cli.rs`, `tests/transports.rs`.
- Modify `crates/zeta-ws/tests/http.rs` (ops log over `/op`).
- Create `crates/zeta-ui/web/view.js`, `crates/zeta-ui/web/coarse.js`; modify `waterfall.js`, `overlays.js`, `agent.js`, `app.js`, `menu.js`, `style.css`, `index.html`; scenarios in `crates/zeta-ui/tests/e2e/ui.py`, harness `crates/zeta/tests/ui.rs`.
- Modify `README.md`, `docs/agents/field-craft.md`, `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (row 11, "Plan 11 status").

---

### Task 1: Resume restores the capture

**Files:**
- Modify: `crates/zeta-core/src/job.rs`, `src/ops.rs` (`expand` callers, `JobKind::reexpand`), `src/node/mod.rs:240-333` (`open`), `src/node/mod.rs:488-517` (`job_start`), `src/node/mod.rs:594-626` (`job_graph_put`), `src/node/launch.rs:35-80` (`launch`)
- Test: `crates/zeta-core/tests/resume.rs` (new; the order and scan cases, with `passthrough` records), `crates/zeta/tests/resume.rs` (new; the re-expansion cases, which need a channelised kind and so the real registry: `PassthroughStage` + `zeta_dsp::register_all`, opened in-process with `zeta_core::node::Node::open` as `crates/zeta/tests/attach.rs` does), `crates/zeta-core/tests/ops.rs` (schema)

**Interfaces:**
- Produces: `JobRecord { ..., #[serde(default)] pub capture_rate_hz: Option<u64> }` (schema nullable like `input`); `Node::job_start(&self, kind: &str, source: SourceId, tuning: Tuning, graph: Graph, capture_rate_hz: Option<Hz>) -> Result<JobId, NodeError>` (the apply branch passes `Some(granted.bandwidth_hz)`; scan and attached pass `None`); `JobKind::reexpand(&self, source: &SourceId, tuning: Tuning, source_rate_hz: Hz, bank_bin_rate_hz: Option<u64>, stored: &Graph) -> Result<Graph, OpError>` (reads `deviation_hz` and `sideband` from the stored graph's node params and calls `expand`); `Node::open` record order `(input.is_some(), Reverse(tuning.bandwidth_hz.0), created_ns)` (Task 3 inserts `elastic` after `input.is_some()`).
- Consumes: `Inner::launch` has the capture after `ensure` as `port.current()` (its `bandwidth_hz` is the source rate, Port contract); `Node::bank_bin_rate(&self, source, granted)` from `ops.rs` apply (move it onto `Inner` so `launch` can call it).

- [ ] **Step 1: Write the failing tests**: the first, fourth and fifth below in `crates/zeta-core/tests/resume.rs` (helpers `source_with`, `cfg`, `registry`, `on_disk` copied from `tests/jobs.rs`); the second and third in `crates/zeta/tests/resume.rs` with the real registry and a `wfm` fixture written at 240 000 Hz (`fixtures::wfm_modulate` as `tests/attach.rs`), the hand-written record claiming `capture_rate_hz: Some(480_000)` with a channel `out_rate_hz` that divides 480 000 but not 240 000:

```rust
#[tokio::test]
async fn resume_launches_the_widest_record_first_whatever_the_file_order() {
    // Two records persisted by hand on one file source: a 48 000 Hz passthrough created
    // first, a 9 600 Hz passthrough created second. Open with resume: both end Running
    // and source_list current.bandwidth_hz == 48_000 (the widest tuned first).
}
#[tokio::test]
async fn resume_reexpands_a_builtin_graph_for_the_actual_rate() {
    // (crates/zeta/tests/resume.rs) A record of kind "wfm_demod" persisted with
    // capture_rate_hz: Some(480_000) and a graph whose channel node has out_rate_hz 480_000;
    // the file port runs at 240_000. After open the job is Running and the on-disk graph's
    // channel node out_rate_hz divides 240_000; capture_rate_hz == Some(240_000).
}
#[tokio::test]
async fn resume_leaves_a_graph_put_graph_alone() {
    // job_graph_put sets capture_rate_hz to None; reopen at another rate: the stored graph is
    // launched unchanged (Failed with the stage's own reason is acceptable; the graph bytes
    // on disk equal what graph_put wrote).
}
#[tokio::test]
async fn scan_resumes_first_and_names_itself_in_the_others_reason() {
    // A persisted running scan and a running passthrough on one source: after open the scan
    // is Running and the passthrough is Failed with a reason containing the scan's id.
}
#[tokio::test]
async fn job_start_records_the_capture_rate() {
    // Op::JobStart passthrough: on_disk(id).capture_rate_hz == Some(<port rate>).
}
```
and in `tests/ops.rs`: the schema's `JobRecord` has `capture_rate_hz` nullable and a record JSON without it parses with `None`.

- [ ] **Step 2: Run to fail**: `cargo test -p zeta-core --test resume --test ops` → compile errors on the missing field and function.
- [ ] **Step 3: Implement**: the field and schema; `job_start`'s new parameter and the apply branch; `job_graph_put` writes `capture_rate_hz: None`; `JobKind::reexpand`; in `Inner::launch`, after `ensure`, when `rec.capture_rate_hz` is `Some(r)` and `r != port.current().bandwidth_hz` and the kind is a built-in `JobKind` (`JobKind::from_name(&rec.kind)` exists or add it), replace `rec.graph` by `reexpand(...)`, set `capture_rate_hz` to the actual rate, persist before `record_running`; the sort in `open`.
- [ ] **Step 4: Run to pass**: `cargo test -p zeta-core` (all) and `cargo test -p zeta --test resume`.
- [ ] **Step 5: Gate and commit**: `git commit -m "Resume restores the capture: widest record first, built-in graphs re-expanded for the actual rate"`.

---

### Task 2: The ops log

**Files:**
- Create: `crates/zeta-core/src/node/opslog.rs`
- Modify: `crates/zeta-core/src/node/mod.rs:456-475` (`apply_from`), `crates/zeta/src/cli.rs` (`Command::Ops`), `src/main.rs`, `src/agents_md.rs` (running-the-node paragraph), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-core/src/node/opslog.rs` (unit), `crates/zeta-ws/tests/http.rs`, `crates/zeta/tests/cli.rs`

**Interfaces:**
- Produces: `pub(super) struct OpsLog { path: PathBuf, max_bytes: u64 }` with `pub(super) fn new(state_dir: &Path) -> Self` (`<state_dir>/ops.log`, `8 * 1024 * 1024`), `pub(super) fn with_max_bytes(path: PathBuf, max_bytes: u64) -> Self`, `pub(super) fn append(&self, line: &serde_json::Value) -> std::io::Result<()>` (one line, `\n`-terminated; rotation before the write when the file's length exceeds `max_bytes`: rename to `<path>.1`, replacing any); `Inner.ops_log: OpsLog`; CLI `zeta ops tail [--lines N]` (default 50) printing the last N lines of `ops.log.1` + `ops.log` in order, exit 0 (an absent file prints nothing); with `--node`: stderr `ops tail reads the state directory: not supported with --node`, exit 1.
- Consumes: `ops_payload(&op, &outcome, elapsed_ms, transport)` (`ops.rs:1008`); `persist_errors: Mutex<Vec<String>>` on `Inner`.

- [ ] **Step 1: Write the failing tests**: in `opslog.rs` `#[cfg(test)]`: `append_writes_one_line_per_call`, `rotates_when_over_max_bytes` (`with_max_bytes(path, 64)`: three 40-byte lines leave `ops.log.1` with the first two and `ops.log` with the third), `unwritable_ops_log_does_not_fail_ops` lives in `crates/zeta-core/tests/ops_stream.rs`: a state dir whose `ops.log` is a directory → `apply_from` still returns `Ok`, `persist_errors()` has one entry containing `ops.log`. In `crates/zeta-ws/tests/http.rs` `ops_log_records_the_callers_transport`: two `/op` calls with `X-Zeta-Client: ui` (a `job_start` passthrough and its `job_stop`) → `<state>/ops.log` has two lines, each `"transport":"ui"`, in order, the second's `op.op == "job_stop"`. In `crates/zeta/tests/cli.rs` `ops_tail_prints_the_last_lines`: after two one-shot ops against a state dir, `zeta --state-dir D ops tail --lines 1` prints exactly the last line; `zeta --node http://127.0.0.1:1 ops tail` exits 1 with the usage text.
- [ ] **Step 2: Run to fail**: `cargo test -p zeta-core opslog`, `-p zeta-core --test ops_stream`, `-p zeta-ws --test http ops_log`, `-p zeta --test cli ops_tail`.
- [ ] **Step 3: Implement** `opslog.rs`, the hook in `apply_from` after the bus publish (`t_ns` = the frame's start), the CLI subcommand (local only), the guide sentence in the "Running the node" section: "Every operation the node applies is appended to `ops.log` in the state directory (rotated at 8 MiB) with its caller's transport; `zeta ops tail --lines N` prints the last N (local only)."; regenerate the guide.
- [ ] **Step 4: Run to pass**: the four test targets plus `cargo test -p zeta --test agents_md --test transports`.
- [ ] **Step 5: Gate and commit**: `git commit -m "Ops log: every applied op on disk with its transport; zeta ops tail"`.

---

### Task 3: Elastic leases

**Files:**
- Modify: `crates/zeta-core/src/lease.rs`, `src/job.rs` (`elastic`), `src/ops.rs` (`JobStart.elastic`, `JobKind::elastic_capable`, `SourceInfo.elastic`, apply branch), `src/node/mod.rs` (`open` order, `job_start`, `job_list`, `source_list`, `relaunch_elastic`, `stop_elastic`), `src/node/launch.rs` (`launch`, `launch_scan`), `crates/zeta/src/cli.rs` (`--elastic`), `src/main.rs`, `src/agents_md.rs`, every in-repo `job_start` caller (`grep -rn '"job_start"' crates sdk docs/agents`), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-core/tests/elastic.rs` (new), `tests/ops.rs`, `crates/zeta/tests/cli.rs`

**Interfaces:**
- Produces: `Op::JobStart { ..., elastic: bool }` (required); `JobKind::elastic_capable(&self) -> bool` (`Spectrum` only); `JobRecord { ..., #[serde(default)] pub elastic: bool }`; `SourceInfo { ..., pub elastic: Vec<JobId> }`; `LeaseManager::ensure(&self, port, holder: JobId, want: Tuning, elastic: bool) -> Result<Tuning, LeaseError>` (rigid request: elastic leases ignored for the conflict check and the port tuned as if unheld when only elastic leases exist; elastic request on a held port: lease band = the current capture, returned, no tune); `LeaseManager::holders(&self, source) -> Vec<(JobId, Tuning)>` (rigid only), `LeaseManager::elastic_holders(&self, source) -> Vec<JobId>`; `Inner::relaunch_elastic(self: &Arc<Self>, source: &SourceId, except: &JobId)` (for each running elastic job on `source` other than `except`: stop its stages, set `tuning` to the capture, re-expand its graph for the capture rate (Task 1's `reexpand`), persist, `launch` again under the same id; called by `launch` after an `ensure` that tuned the port); `Inner::stop_elastic(&self, source)` (called by `launch_scan` before `acquire_shared`; the jobs end `Stopped`); CLI `zeta job start --elastic`.
- Consumes: Task 1's `capture_rate_hz`, `reexpand`, resume order (insert `elastic` so the key is `(input.is_some(), elastic, Reverse(bw), created_ns)`).

- [ ] **Step 1: Write the failing tests** in `crates/zeta-core/tests/elastic.rs` (file source, Passthrough and a real `spectrum` registry is not reachable from core: register `PassthroughStage` and a tiny inline `spectrum`-named stand-in stage that emits one event per IQ frame, so `elastic` on kind `spectrum` passes `elastic_capable`):

```rust
#[tokio::test]
async fn rigid_start_over_an_elastic_holder_tunes_and_relaunches_it() {
    // elastic spectrum at 100.000 MHz / 9 600 Hz, then rigid passthrough at 100.020 MHz /
    // 9 600 Hz (outside): Ok; source_list current == the rigid band; the elastic job is still
    // Running with the SAME id, its record tuning == current, its stream ids differ from
    // before, and source_list.holders == [rigid], .elastic == [elastic].
}
#[tokio::test]
async fn elastic_start_on_a_held_port_joins_the_capture() { /* no tune; tuning == current */ }
#[tokio::test]
async fn elastic_on_a_non_spectrum_kind_is_refused() { /* "passthrough cannot be elastic" */ }
#[tokio::test]
async fn scan_stops_elastic_jobs_on_its_source() { /* elastic -> Stopped within 1 s */ }
#[tokio::test]
async fn failed_retune_leaves_the_elastic_job_running() {
    // a port whose tune fails (the fake port from tests/scan.rs with_fail_tune): the rigid
    // start errs with the port's reason; the elastic job is Running on the old capture.
}
#[tokio::test]
async fn elastic_records_resume_after_rigid_ones() { /* reopen: order by state after open */ }
```
`tests/ops.rs`: schema has `job_start.elastic` required boolean and `source_list` items have `elastic`. `crates/zeta/tests/cli.rs`: `zeta job start --kind spectrum --elastic ...` round trip shows `"elastic": true` in `job list`.

- [ ] **Step 2: Run to fail.** **Step 3: Implement** (lease flag and rules first, then the node relaunch and scan stop, then the op, CLI, callers and the guide note for `job_start`: "`elastic: true` (spectrum only) makes the job follow the source's capture instead of holding it: it never conflicts, is relaunched on every retune and is listed under `elastic` in `source_list`; the web page's own spectrum job is elastic."). **Step 4: Run to pass**: `cargo test -p zeta-core`, `-p zeta --test cli --test agents_md --test transports`, `-p zeta-ws` (callers), `-p zeta-ui` if it has tests.
- [ ] **Step 5: Gate and commit**: `git commit -m "Elastic leases: a spectrum job that follows the capture instead of holding it"`.

---

### Task 4: Atomic replace

**Files:**
- Modify: `crates/zeta-core/src/lease.rs` (`conflict_options` line; gate-held variants `ensure_locked`, `tune_if_unheld_locked`, `pub(crate) async fn gate_guard(&self, source) -> tokio::sync::OwnedMutexGuard<()>`), `src/ops.rs` (`JobStart.replace`, `SourceEnsure.replace`, apply branches), `src/node/mod.rs` (`replace_holders`), `crates/zeta/src/cli.rs` (`--replace ID[,ID]` on `job start` and `source ensure`), `src/main.rs`, `src/agents_md.rs`, all in-repo callers, `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-core/tests/replace.rs` (new), `tests/ops.rs`, `crates/zeta/tests/cli.rs`

**Interfaces:**
- Produces: `Op::JobStart { ..., replace: Vec<JobId> }`, `Op::SourceEnsure { ..., replace: Vec<JobId> }` (required, `[]` for none); `Node::replace_holders(&self, source: &SourceId, replace: &[JobId], guard: &OwnedMutexGuard<()>) -> Result<(), NodeError>` (validates every id holds a rigid lease on `source`: `NodeError::UnknownJob`, or `NodeError::Other(format!("job {id} does not hold source {src}"))`; then `job_stop` each); the apply branches take the gate once, call `replace_holders`, then the `_locked` lease call, so nothing interleaves; `conflict_options` appends `format!("retry with replace: [{}]", holders joined by ", ")` whenever `holders` is non-empty and the port is not time-shared; CLI flags.
- Consumes: Task 3's rigid/elastic distinction (an elastic id "does not hold").

- [ ] **Step 1: Write the failing tests** in `crates/zeta-core/tests/replace.rs`: `replace_stops_the_named_holder_and_starts` (rigid A at band 1; `JobStart` B at band 2 with `replace: [A]` → Ok, A `Stopped`, B `Running`, holders == [B]); `replace_refuses_an_elastic_or_foreign_holder` (an elastic spectrum id → the exact "does not hold" text and it keeps running; an id holding another source → same; an unknown id → `UnknownJob`; nothing stopped in each case); `replace_leaves_named_jobs_stopped_when_another_holder_remains` (A and C rigid in one capture; B with `replace: [A]` at a band outside → `Conflict` naming `[C]`, A `Stopped`); `conflict_options_name_the_replace_retry` (the option line text); `replace_and_a_competing_start_never_both_hold_incompatible_bands` (spawn `JobStart` D at band 3 concurrently with the replace: after both settle, every holder's band fits `source_list.current`). `tests/ops.rs`: schema fields required. `cli.rs`: `zeta job start ... --replace A,B` round trip; `zeta source ensure --replace A`.
- [ ] **Step 2: Run to fail.** **Step 3: Implement.** Guide notes: `job_start` and `source_ensure` gain "`replace`: job ids that hold the source and may be stopped to make room; they are stopped under the source's gate right before the request, so nothing can take the radio in between. A `conflict` lists the holders; decide whether those jobs may stop, then retry with `replace`. The page's elastic job is never a holder." **Step 4: Run to pass**: `cargo test -p zeta-core`, `-p zeta --test cli --test agents_md --test transports`, every caller crate.
- [ ] **Step 5: Gate and commit**: `git commit -m "Atomic replace: job_start and source_ensure stop named holders under the source's gate"`.

---

### Task 5: The page yields, policies, commands, finished rows, conflict offers

**Files:**
- Create: `crates/zeta-ui/web/view.js`
- Modify: `crates/zeta-ui/web/waterfall.js` (own job elastic; `requestView` outside the band while a rigid holder exists: clamp instead of retune, until Task 6), `agent.js` (rows: elastic badge, finished rows with inline reason and no Stop, aging; `Transcript.note(text, isError, actions)`), `app.js` (`runCommand`: `view …`; conflict offers in `applyOp` and the ops feed), `menu.js` (`replace: []`, `elastic: false` on every `job_start`; the page's spectrum job `elastic: true`), `style.css`, `index.html` if a container is needed
- Test: `crates/zeta-ui/tests/e2e/ui.py`

**Interfaces:**
- Produces: `view.js`: `export const POLICIES = [{ key, values | parse, default_, describe }]` (five entries per Global Constraints), `export function mountView() -> { get(key), set(key, value) -> { ok: true } | { ok: false, error: string }, show() -> string /* one line per key: `key value (allowed …)` */, reset(), clear() /* fires onClear */, onChange(cb(key, value)), onClear(cb) }` (sessionStorage `zeta.view`); `app.js`: `state.view`; commands handled before the op/ask paths; `agent.js`: `startJobs(api, el, transcript, { ..., view })` renders rows for jobs that are `running`, or finished within `view.get("finished_rows_s")` seconds of when the page first saw them finished (a `Map` of id → `performance.now()`), finished rows carry `.reason` text (the state's `reason`, or `stopped`) and no Stop/Play/Transcribe buttons, rows of elastic jobs carry the text `elastic` after the kind; `Transcript.note(text, isError = false, actions = [])` where `actions` is `[{ label, run }]` rendered as buttons after the line; `app.js` `conflictActions(err, retry) -> actions` builds, from a `conflict` error (`holders`, with kinds and tunings looked up in the last job list), one action per holder `stop <kind> <id> and retry` whose `run` re-issues the original op with `replace: [id]`.
- Consumes: `job_start.elastic` / `.replace` (Tasks 3, 4), `source_list.elastic`, `conflict` errors' `holders`.

- [ ] **Step 1: Write the failing scenarios** in `ui.py`:
  - `scenario_page_job_is_elastic_and_a_rigid_start_needs_no_replace`: show the tone waterfall; `job_list` shows the page's spectrum job with `elastic: true`; `job_start` (API) a `passthrough` on `tone` at a band outside the page's capture with `replace: []` → `result job`; within 5 s the page's job id is unchanged, its tuning equals `source_list.current`, and `window.zetaTest.wf().jobId` still names it.
  - `scenario_view_set_show_reset`: type `view set follow free` → transcript `follow = free`; `view show` lists five keys with `follow free`; `view set follow sideways` → error naming `target, free`; `view set finished_rows_s 5` ok; `view reset` → `view show` has `follow target`; the value survives a reload (`sessionStorage`).
  - `scenario_finished_rows_age_out`: `view set finished_rows_s 1`; start and stop a passthrough via the API → the row appears with text `stopped` and no `button.stop`, and is gone within 3 s; with `finished_rows_s 0` a stopped job never appears.
  - `scenario_conflict_offers_replace`: rigid passthrough A at band 1 via the API; from the command line `{"op":"job_start",...}` for band 2 → the transcript shows `conflict … held by passthrough <A id> @ <centre MHz>` and a button `stop passthrough <A short id> and retry`; click → A stopped, the new job running, `source_list.current` == band 2.
- [ ] **Step 2: Run to fail**: `ZETA_UI_ONLY=<name> cargo test -p zeta --test ui` for each.
- [ ] **Step 3: Implement** (view.js first, then rows, then conflict actions, then the own-job flag and clamp).
- [ ] **Step 4: Run to pass**: the four scenarios, then the whole suite `cargo test -p zeta --test ui`.
- [ ] **Step 5: Gate and commit**: `git commit -m "UI: elastic own job, view policies and commands, finished rows, conflict offers replace"`.

---

### Task 6: Automatic scans on the scanner source

**Files:**
- Create: `crates/zeta-ui/web/coarse.js`
- Modify: `crates/zeta-source-file/src/lib.rs` (`FileOpts { ..., pub tune_range: Option<(Hz, Hz)> }`: a tune inside the range is accepted, the centre recorded as `current`, samples unchanged; outside: the existing `out_of_range`), `crates/zeta/src/sources.rs` (`--source-file ID=PATH.sigmf-meta[,tune_min_hz=N,tune_max_hz=M]`), `crates/zeta-ui/web/waterfall.js` (`onView` carries `{ lo, hi, outside: bool }`; `requestView` outside the fine capture no longer clamps when a scanner exists: the view is allowed to exceed the capture, `wf.view` may be wider than `wf.band`), `agent.js` (the row of the scan whose id is `sessionStorage["zeta.autoScan"]` carries the text `auto` after the kind), `app.js` (mount), `crates/zeta/tests/ui.rs` (a second file source `scanner=<tone fixture>,tune_min_hz=50000000,tune_max_hz=150000000`)
- Test: `crates/zeta-source-file` unit test, `crates/zeta/tests/cli.rs` (flag parse), `ui.py`

**Interfaces:**
- Produces: `coarse.js`: `export function mountCoarse(wf, api, view, { transcript, sources, jobs }) -> { scanJobId, onRow(cb(msg)), destroy() }` where `sources()`/`jobs()` return the last `source_list`/`job_list` results; it listens to `wf.onView`, and when the view is `outside` the fine capture and `view.get("zoom_out") === "scan"`: chooses the scanner (`view.get("scanner")`: `auto` = the first source that is not `wf.sourceId`, has `holders.length === 0`, and whose tune range contains the view's centre, else the shown source itself when it has no rigid holder; a pinned id is used as given), debounces 300 ms, clamps the span to the scanner's `tune_min_hz..tune_max_hz`, and `job_start { kind: "scan", source, center_hz, bandwidth_hz, deviation_hz: null, sideband: null, elastic: false, replace: [] }`, remembering the id in `sessionStorage["zeta.autoScan"]`; when the span changes by more than 5 % it stops the old scan and starts a new one; when the view returns inside and `auto_scan_stop === "inside"` it stops its scan; it never stops a scan not in `zeta.autoScan`; it subscribes to its scan's event stream (and to a manual scan running on the scanner, read-only) and hands each frame to `onRow` callbacks; when no source can scan it notes `coarse layer: no free source can scan <lo>-<hi> MHz` once per view change.
- Consumes: Task 5's `view`; `wf.onView`; the `scan` job kind.

- [ ] **Step 1: Write the failing tests**: `zeta-source-file` unit `tune_inside_tune_range_is_accepted_outside_is_refused`; `cli.rs` `source_file_flag_parses_a_tune_range`; scenarios: `scenario_zoom_out_starts_and_stops_an_auto_scan` (show `tone`; zoom out past the capture → within 3 s a `scan` job runs on `scanner` with `bandwidth_hz` ≈ the view span, `sessionStorage["zeta.autoScan"]` names it; zoom back inside → it is stopped within 3 s); `scenario_manual_scan_is_left_alone` (API `scan` on `scanner`; zoom out and in → that job stays running); `scenario_scanner_policy_pins_source` (`view set scanner scanner` then zoom out → the scan runs on source `scanner`; then `view set zoom_out none` and zoom out again → no new scan starts); `scenario_own_scan_row_is_marked` (the job row of the page's automatic scan carries the text `auto`; a manual scan's row does not); `scenario_zoom_out_beyond_scanner_range_starts_nothing` (a view entirely above 150 MHz → no scan job, one transcript line containing `no free source can scan`).
- [ ] **Step 2: Run to fail.** **Step 3: Implement.** **Step 4: Run to pass**: `cargo test -p zeta-source-file`, `-p zeta --test cli`, the scenarios, the whole UI suite.
- [ ] **Step 5: Gate and commit**: `git commit -m "UI: automatic scans on the scanner source under the zoom_out and auto_scan_stop policies"`.

---

### Task 7: Two layers, outlines, easing

**Files:**
- Modify: `crates/zeta-ui/web/waterfall.js` (coarse rows and painting; easing), `overlays.js` (outlines), `app.js` (wire `coarse.onRow` → `wf.addCoarseEvent`), `style.css`
- Test: `ui.py`

**Interfaces:**
- Produces: `wf.addCoarseEvent(msg)` (a scan row painted into a second column buffer across the whole view, stitched per step exactly as scan mode does, drawn first; the fine row then painted over its own band; `wf.coarsePainted` counts coarse columns; the axis spans `wf.view` even when wider than `wf.band`); `wf.easeTo(lo, hi, ms = 400)` (requestAnimationFrame interpolation of the view; `wf.easing` true while running); under `follow === "target"`, a change of `source_list.current` for the shown source calls `easeTo(capture)`; under `free` nothing moves; `overlays.outline(lo, hi, text, key)` / `unoutline(key)` drawing a bordered band with a label; `overlays.sync(jobs)` outlines every running rigid job on the shown source (`kind` as text, key = job id) and the capture itself when there is none.
- Consumes: Task 6's rows and view semantics; Task 5's `view`.

- [ ] **Step 1: Write the failing scenarios**: `scenario_coarse_layer_paints_outside_the_capture` (zoom out → `window.zetaTest.wf().coarsePainted > 0` within 5 s while fine `painted` keeps growing); `scenario_rigid_bands_are_outlined` (two rigid passthroughs in the capture → two `.outline` elements with their kinds; stop one → one); `scenario_follow_target_eases_on_a_capture_change` (API rigid start with `replace` moving the capture → `wf().easing` becomes true then false within 2 s and `wf.view` equals the new capture); `scenario_follow_free_holds_the_view` (same with `view set follow free` → `wf.view` unchanged after 2 s).
- [ ] **Step 2: Run to fail.** **Step 3: Implement.** **Step 4: Run to pass**: the scenarios, the whole UI suite.
- [ ] **Step 5: Gate and commit**: `git commit -m "UI: coarse scan layer under the capture, rigid bands outlined, eased follow"`.

---

### Task 8: Docs, plan series, final gate

**Files:**
- Modify: `README.md` (routes table: `job start --elastic/--replace`, `source ensure --replace`, `zeta ops tail`; the web page section: policies and `view` commands, the two layers), `docs/agents/field-craft.md` ("Taking the radio from another job": read the conflict's holders, decide, retry with `replace`; the page's elastic job never blocks), `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (row 11 and "Plan 11 status" with the gate counts), `crates/zeta/src/agents_md.rs` if field-craft is embedded, `AGENTS.md`, `skills/zetasdr/SKILL.md`

- [ ] **Step 1: Write the docs.** **Step 2: Final gate**: `cargo fmt -p <each member> -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features --no-fail-fast` (no `FAILED`, no `ui e2e skipped`), `cargo run -p zeta -- agents-md --check` and `--skill --check`; record the counts in the status. **Step 3: Commit**: `git commit -m "Plan 11: docs, field-craft, status"`.

---

## Live acceptance (controller, after the final review)

On the field host (the `zeta.service` unit): rebuild from a bundle of the branch, restart (open pages reload themselves), then: with the page zoomed to 200 kHz at 1090.5 MHz, `zeta --node … job start --kind adsb_decode --source b210 --center-hz 1090000000 --bandwidth-hz 2400000 --replace ''` succeeds without stopping anything and the page's job follows the 2.4 MHz capture; stop ADS-B, start a broadcast demod, attach a transcriber, restart the service: both resume and transcripts continue; `zeta ops tail --lines 5` on the field host names `ui`/`http`/`agent` as the transports of the last ops. The LimeSDR, once attached as a second `--source-soapy`, is the scanner: zooming out past the B210's capture starts a scan on it and zooming back stops it.
