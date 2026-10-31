# Box, Fit and Refine Implementation Plan (plan 18)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A demodulator started from a box on the waterfall lands on the signal's measured centre and width; a running job is moved and resized by dragging its outline (or from a small menu of width presets), retuned under the same id with its attached `transcribe` and its ribbons kept; every rigid job on the capture carries a tag linked to its row in the job list; and the node gains `job_retune`, with the CLI, MCP and `ask` forms every operation has.

**Architecture:** In `zeta-core`, `Op::JobRetune` validates like `job_start`, then `Node::job_retune` (a new `node/retune.rs`) runs under the source's gate: `LeaseManager::retune_locked` moves the job's rigid lease (inside the capture untuned, outside it a tune when nobody else holds, else a `conflict` with the old lease put back), the graph is expanded again for the new band at the capture's rate carrying only `squelch`/`squelch_db` (the plan-channel guard is derived again), and `Inner::relaunch_in_place` restarts the run under the same id with the `relaunch` flag that keeps the lease and the record `Running`. Attached jobs are held (`hold_attached`) before the input's run is stopped and relaunched after it (`relaunch_attached`), by the retune and by `graph_put` alike. In the page, `menu.js` measures a box before starting a demodulator (`fitBand`, `fitTuning`), `overlays.js` makes retunable outlines draggable with a ghost and a readout and draws a tag per rigid job, `menu.js` gets a second entry table for the outline menu, and `agent.js` rows gain a band cell and hover linking.

**Tech Stack:** Rust 2024 (tokio, serde, schemars, clap), the existing `FileSource` fixtures (`zeta-source-file`, `test-fixtures`) and the `Radio` fake port of `crates/zeta-core/tests/replace.rs`, `assert_cmd` CLI tests; plain browser JavaScript (no build step) in `crates/zeta-ui/web`; Playwright e2e in `crates/zeta-ui/tests/e2e/ui.py` run by `crates/zeta/tests/ui.rs`.

**Spec:** `docs/superpowers/specs/2026-10-31-box-and-refine-design.md` (read it first; section numbers below refer to it). §4's last paragraph (the measure DC mask) is plan 15's work; this plan only relies on it at live acceptance.

## Global Constraints

- Branch `plan-18`, worked in a git worktree created from `main` 8053f43 (the executor creates it with the using-git-worktrees skill; note the worktree Bash hook refuses compound commands, `$(...)`, the word "source" and `git -C` to the shared checkout; this plan touches no `zeta-source-*` crate). One commit per task.
- Builds and gates run on the field host under the `claude` account in `/Projects/ZetaSDR` (125 GB RAM, 12 cores): plain `cargo` with its default job count; another plan (plan 15) builds there concurrently, so expect contention, not failure. Shared `CARGO_TARGET_DIR` across worktrees still serves stale artifacts: `touch crates/*/src/lib.rs` when a build looks stale. The live acceptance (spec §10) runs on the desktop HackRF from a release build made there, after merge.
- The op: `{"op":"job_retune","job","center_hz","bandwidth_hz","replace":[]}`, CLI `zeta job retune --job ID --center-hz N --bandwidth-hz N [--replace ID,ID]`; refusals and texts exactly as spec §3 (retunable kinds; `scan`/`watch`/`identify`/`spectrum`/`transcribe`/operator-graph jobs refused naming the kind; `job <id> is <state>`; `unknown_job`); carried params `squelch`, `squelch_db`, `deviation_hz`, `sideband`; the guard (`guard_hz`, `guard_db`, `channel_hz`, `wide_hz`) re-derived from the plan at the new band; attached jobs relaunched under their ids after the input, also on `graph_put`.
- Page constants from the spec: fit = measure up to 3 s, one event; noise/unknown under 6 dB SNR → start at the box as drawn with the transcript line `no signal found in <lo>-<hi> MHz; started <kind> at the box as drawn`; else centre = measured (SSB: the dial from the evidence), width = occupied × 1.25 floored at 5000 (fm_demod, am_demod), 3000 (ssb_demod), 150000 (wfm_demod), capped at the box; transcript `fitted <kind> to <centre> MHz, <width> kHz (<label>)`. Drag: handles `.outline-handle[data-edge=lo|hi]` 8 px `ns-resize`, body `grab`, ghost `.outline-ghost`, readout `.outline-readout` with "release to retune", min width 1000 Hz, a drag under 3 px is a click; menu entries "Fit to signal", "2.5 kHz", "5 kHz", "6.25 kHz", "12.5 kHz", "25 kHz", "Stop"; tags `.job-tag[data-job]` with kind, short id (match the job list's `id.slice(-6)`), squelch dot, channel, 40-char snippet; hover class `hot` both ways; narrow footprint: no drag, long-press menu.
- Temp files under `$CLAUDE_JOB_DIR/tmp`, never /tmp. Python via `python3 -I`.
- The UI e2e suite runs near its 300 s deadline: the plan must contain a task that measures the suite's time and either raises the deadline in crates/zeta/tests/ui.rs by the measured cost or splits the suite into two test targets; say which in the task. (Task 9 measures and raises.)
- Refusal texts the spec leaves to the builder, fixed here and pinned by Task 1's tests: `<kind> is not retunable` (scan, watch, identify, spectrum rigid or elastic); `<kind> is attached to job <input>: retune that job`; `job <id> runs an operator graph (graph_put): put a new graph or start a new job`; `<kind> is not a kind the node expands: start a new job` (a `Node::job_start` caller's own kind, a mode row no longer loaded); `job <id> is <status>` with `status` the record state's serialised tag (`stopped`, `failed`); `job <id> is not running in this node (one-shot: use --node against zeta serve)` for a `running` record this process did not launch; `job <id> cannot replace itself`. All are `OpError::Other`. An unknown id is `unknown` (`field: "job"`); a band the source cannot take is `out_of_range` naming the field, before anything moves.
- Number formats on the page, one module `crates/zeta-ui/web/units.js`: `mhzText(hz) = (hz / 1e6).toFixed(4)`, `khzText(hz) = (hz / 1e3).toFixed(1)`, `bandText(c, bw) = "<mhzText(c)> MHz, <khzText(bw)> kHz"`; the readout reads `<bandText> · release to retune`; the outline-menu fit failure reads `no signal found in the band; left as it is`.
- No new third-party dependency, Rust or JavaScript.
- Every task that changes an op, a result, a flag or field craft regenerates `AGENTS.md` (`cargo run -p zeta -- agents-md > AGENTS.md`) and `skills/zetasdr/SKILL.md` (`cargo run -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md`) and runs `cargo test -p zeta --test agents_md --test transports` and `cargo test -p zeta-client`.
- Gates per task: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, the touched crates' tests; page tasks also run the new and the touched e2e scenarios alone (`ZETA_UI_ONLY=<names,comma-separated> cargo test -p zeta --test ui -- --nocapture`). The e2e needs `python3` with `playwright.sync_api` and a Chromium under `~/.cache/ms-playwright` on the build host (`crates/zeta/tests/ui.rs:121-148`); a `ui e2e skipped` line names the missing piece, not a passing gate. Final gate (Task 10): `cargo test --workspace --all-features --no-fail-fast` (no `FAILED`, no `ui e2e skipped`), `agents-md --check`, `agents-md --skill --check`.
- Commit messages in the repo's style (`Jobs: ...`, `Lease: ...`, `CLI: ...`, `UI: ...`, `Tests: ...`, `Docs: ...`).

## Review Focus

1. **A band the source cannot take leaves the job untouched.** `job_retune` to a centre outside the tune range, or a width over `max_bandwidth_hz`, is `out_of_range` naming the field; the job keeps running on its old band and the radio is not tuned. Pinned in Task 1 (`an_out_of_range_band_is_refused_before_anything_moves`).
2. **A `replace` naming the job itself is refused and nothing is stopped.** Pinned in Task 1 (`refusals_name_the_kind_the_state_and_the_id`).
3. **An attached job the operator stopped stays stopped through its input's retune or `graph_put`.** Only running attached jobs are held and relaunched. Pinned in Task 3 (`an_attached_job_the_operator_stopped_stays_stopped_through_a_retune`).
4. **A `job_list` poll mid-drag never snaps the outline back; a `pointercancel` mid-drag does, and sends nothing.** Pinned in Task 6 (`scenario_drag_body_moves_the_centre_and_a_short_drag_is_a_click`, `scenario_drag_handle_narrows_the_band_and_clamps_at_1_khz`).
5. **A watch's, a scan's, a pin's and the capture's outlines carry no handles and no drag,** so a drag across them still selects a box as today. Pinned in Task 6 (the assertion added to `scenario_watch_entry_starts_a_watch_and_outlines_its_band`) and Task 8 (the pin keeps its `.outline-label`, no tag).

---

## File map

- Modify `crates/zeta-core/src/ops.rs` (`Op::JobRetune` after `JobAttach` at :153-161; `Op::name` :1406; `JobKind::retunable` beside `attachable` :505; `reexpand` :585-625 → `reexpand_carrying`; `RETUNE_CARRIED_PARAMS` beside `CARRIED_PARAMS` :1051; the `apply` arm and `fn job_retune` beside `job_attach` :2307; the `JobAttach` doc :153-161), `crates/zeta-core/src/lease.rs` (`retune_locked` beside `pin_locked` :344), `crates/zeta-core/src/node/mod.rs` (`mod retune;`, `JobEntry.relaunch` doc :160-162, `swap_graph` :341-358), create `crates/zeta-core/src/node/retune.rs`, `crates/zeta-core/src/stream_in.rs` (doc :34-44).
- Tests: create `crates/zeta-core/tests/retune.rs`; modify `tests/ops.rs` (:481 count; new guard tests beside :2629-2668), `tests/ops_stream.rs` (:395-446 the variant list), `tests/attach.rs` (:384-399 and new tests), `crates/zeta-core/src/lease.rs` tests.
- Modify `crates/zeta/src/cli.rs` (`JobCmd::Retune` after `Attach` :519-533, `mutates` :712-723, `into_op` after :809), `crates/zeta/src/agents_md.rs` (`RUNNING`'s serve-lock sentence; `note("job_retune")`, `note("job_attach")`), `docs/agents/field-craft.md`, `README.md`, `AGENTS.md`, `skills/zetasdr/SKILL.md`; tests `crates/zeta/tests/cli.rs`, `tests/transports.rs`, `tests/agents_md.rs` (:24 count), `crates/zeta-client/tests/tools.rs`.
- Modify `crates/zeta-ui/web/menu.js` (`choose` :311-350, `startJob` :355-369), `overlays.js` (`rigidBand` :132-145, constructor :193-235, `sync` :254, `syncOutlines` :391-413, `outline`/`unoutline`/`placeOutline` :726-766, `layout` :768-778), `agent.js`, `app.js`, `style.css`; create `crates/zeta-ui/web/units.js`; `crates/zeta-ui/tests/e2e/ui.py`; `crates/zeta/tests/ui.rs` (the `quiet` fixture, `DEADLINE` :167).
- Modify `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (row 18 :42, a "Plan 18 status" section above "Plan 20 status" :47), `TODO.md` (:69-79).

---

### Task 1: `Op::JobRetune`, the lease move and the same-id relaunch (Opus, higher effort)

**Files:**
- Modify: `crates/zeta-core/src/ops.rs:153-161` (add the variant after `JobAttach`), `:499-508` (`retunable`), `:1406-1433` (`Op::name`), `:2283-2288` (the `apply` arm beside `JobAttach`), `:2307` (`fn job_retune` beside `fn job_attach`)
- Modify: `crates/zeta-core/src/lease.rs:344-374` (`retune_locked` beside `pin_locked`), tests at `:706-`
- Create: `crates/zeta-core/src/node/retune.rs`
- Modify: `crates/zeta-core/src/node/mod.rs` (`mod retune;` beside `mod recover;`; `JobEntry.relaunch` doc `:160-162`)
- Test: `crates/zeta-core/tests/retune.rs` (new), `crates/zeta-core/tests/ops.rs:481` (22 → 23), `crates/zeta-core/tests/ops_stream.rs:395-446` (add `Op::JobRetune` to the list), `crates/zeta/tests/agents_md.rs:24` (22 → 23)

**Interfaces:**
- `Op::JobRetune { job: JobId, center_hz: Hz, bandwidth_hz: Hz, replace: Vec<JobId> }`, wire name `job_retune`, not `read_only`, result `OpResult::Job(id)` (the same id). Its doc comment, verbatim (it is the guide's section text):

  > Retune a running job on the radio to a new band under the same id: under the source's gate its lease is moved to `center_hz`/`bandwidth_hz` (inside the current capture it is granted without a tune, whether or not another rigid job's band overlaps it; outside it the source is tuned when no other rigid job or pin holds it, else it is a `conflict` naming the holders and the job is left as it was), its graph is expanded again for the new band at the capture's rate and the job relaunched, `tuning` updated. Applies to a `running` job the node expanded and can re-expand: the four demodulators, the decoders, `measure`, `passthrough` and the mode rows. Refused naming the kind for `scan`, `watch`, `identify`, `spectrum` (elastic or rigid) and `transcribe` (attached: retune its input), and for a job whose graph an operator `graph_put` replaced (it launches as stored: put a new graph or start a new job). A job that is not running is `job <id> is <state>`; an unknown id is `unknown`. The result is the job's id, as `job_start` answers.

- `pub fn JobKind::retunable(&self) -> bool`: `true` for `Passthrough`, `FmDemod`, `WfmDemod`, `AmDemod`, `SsbDemod`, `AprsDecode`, `PocsagDecode`, `RdsDecode`, `AcarsDecode`, `AdsbDecode`, `Measure`, `Mode(_)`; `false` for `Spectrum`, `Scan`, `Identify`, `Watch`, `Transcribe`.
- `async fn job_retune(node: &Node, job: JobId, center_hz: Hz, bandwidth_hz: Hz, replace: Vec<JobId>) -> Result<OpResult, OpError>` in `ops.rs`, checked in this order: the record (`unknown_job`); `rec.input` (`<kind> is attached to job <input>: retune that job`); `rec.operator_graph` (`job <id> runs an operator graph (graph_put): put a new graph or start a new job`); `node.inner.expandable_kind(&rec.kind)` `None` (`<kind> is not a kind the node expands: start a new job`); `!kind.retunable()` (`<kind> is not retunable`); `rec.state != Running` (`job <id> is <status>`, the state's serialised `status` tag); `replace.contains(&job)` (`job <id> cannot replace itself`); `checked_port(node, &rec.source, &want)` (`out_of_range`); `kind.mode_row(&node.modes())?`; then `node.job_retune(&job, &kind, want, &replace).await`, on `Err(e)` `idle_after_replace(node, port, &replace, &e)` then `node_error(node, e)`. `expandable_kind` is `pub(crate)` on `Inner`: expose it as `pub(crate) fn Node::expandable_kind(&self, kind: &str) -> Option<JobKind>`.
- `pub(crate) async fn LeaseManager::retune_locked(&self, port: &dyn Port, holder: &JobId, want: Tuning, guard: &OwnedMutexGuard<()>) -> Result<(Tuning, bool), LeaseError>`: sets the holder's own leases aside (as `pin_locked` does the pin's); when no time-shared lease holds the source and `port.current()` is `Some(cur)` with `want` fitting `cur`, pushes `Lease::new(holder, want, false)` and returns `(cur, false)` whatever else holds the source (this covers the lone holder, whom `ensure_as` would otherwise tune); else `ensure_as(port, holder, want, false, false, guard)`; on `Err` the set-aside leases are put back. The `bool` is whether the port was tuned.
- `pub async fn Node::job_retune(&self, id: &JobId, kind: &JobKind, want: Tuning, replace: &[JobId]) -> Result<(), NodeError>` in `node/retune.rs`, the sequence (an algorithm the tests do not determine, so it is fixed here):
  1. Under the jobs lock: the entry (`UnknownJob`), `entry.cancel.is_some() && state == Running` else `NodeError::Other("job <id> is not running in this node (one-shot: use --node against zeta serve)")`; clone the record. The port (`UnknownSource`).
  2. `let guard = self.inner.leases.gate_guard(&rec.source).await;` then, when `replace` is not empty, `self.replace_holders(&rec.source, replace, &guard).await?`.
  3. `retune_locked(port, id, want, &guard)`; `LeaseError::Port(p)` → `NodeError::SourcePort(source, p)`, else `NodeError::Lease(e)`. On `Err` return it (the old lease is back, nothing else changed).
  4. `let graph = kind.reexpand(&rec.source, want, capture.bandwidth_hz, self.inner.bank_bin_rate(&rec.source, capture), &rec.graph, &self.inner.modes)` (Task 2 switches this call to `reexpand_carrying`). On `Err(e)`: `retune_locked(port, id, rec.tuning, &guard)` best effort to put the old band back, return `NodeError::Expand(e)`.
  5. `let new = JobRecord { graph, tuning: want, capture_rate_hz: Some(capture.bandwidth_hz.0), operator_graph: false, ..rec }`; `let (r, after) = self.inner.relaunch_in_place(new, Some(&guard)).await;`
  6. `drop(guard)`; `self.inner.after_gate(after).await`; when the lease tuned the port, `let _ = self.inner.relaunch_elastic(&rec.source, Some(id)).await;` (the job's own launch found its lease inside the new capture and tuned nothing, so nobody else relaunches the elastic jobs); return `r`.
- `pub(super) async fn Inner::relaunch_in_place(self: &Arc<Self>, rec: JobRecord, gate: Option<&OwnedMutexGuard<()>>) -> (Result<(), NodeError>, AfterGate)`: under the jobs lock the entry for `rec.id` must hold a live run (`cancel.is_some()`, state `Running`; else `(Err(NodeError::Stopped(id)), Nothing)`); set `e.relaunch = true`, take and cancel `cancel`, take `done` and read `run`; await `done` outside the lock (the supervisor sees `relaunch`, keeps the record `Running` and the lease, unregisters the streams, idles nothing); `launch_gated(rec, gate)`; on `Err(e)`: `e.relaunch = false` on the entry, `self.leases.release(&id)`, `self.finish(&id, run, JobState::Failed { reason: e.to_string() })`. Mirrors `relaunch_elastic_now` (`launch.rs:434-493`) but for one rigid job under a held gate and without the idle (the caller idles after the gate). Update `JobEntry.relaunch`'s doc (`mod.rs:160-162`) to name both callers.

- [ ] **Step 1: Write the failing tests.** In `crates/zeta-core/src/lease.rs` tests (the `FakePort` of `:640-706` counts `tunes`):

```rust
#[tokio::test]
async fn retune_locked_moves_a_lease_inside_the_capture_without_a_tune() {
    // j1 at t(100_000_000, 1_000_000); retune_locked(j1, t(100_200_000, 10_000)) → Ok((t(100_000_000, 1_000_000), false));
    // holders == [(j1, t(100_200_000, 10_000))]; p.tunes == 1.
}
#[tokio::test]
async fn retune_locked_tunes_outside_when_only_elastic_leases_remain_and_restores_on_a_conflict() {
    // j1 rigid at t(100_000_000, 1_000_000), e1 elastic; retune_locked(j1, t(433_000_000, 12_500)) → Ok((t(433_000_000, 12_500), true)), p.current() == that, p.tunes == 2;
    // then j2 rigid at t(433_001_000, 1_000) (inside, granted untuned); retune_locked(j2, t(900_000_000, 1_000)) → Err(Conflict{holders == [j1], current == t(433_000_000, 12_500), ..});
    // holders == [(j1, t(433_000_000, 12_500)), (j2, t(433_001_000, 1_000))] (j2's lease put back) and p.tunes still 2.
}
```

  In `crates/zeta-core/tests/retune.rs` (fixtures: `Radio` copied from `tests/replace.rs:28-95` with a `tunes: AtomicUsize` counter incremented in `tune` and `fn tunes(&self) -> usize`; `registry()` = `PassthroughStage` + `zeta_dsp::register_all`; `cfg` as `replace.rs:152-163`; helpers `start(node, kind, t) -> JobId` via `Op::JobStart` with `deviation_hz: None, sideband: None, elastic: false, replace: []`, `retune(node, id, c, bw, replace: &[&JobId]) -> Result<OpResult, OpError>`, `record(node, id) -> JobRecord`, `source(node) -> SourceInfo`, `node_params(rec, stage_kind) -> serde_json::Value` (the params of the first node of that kind), `other(msg) -> OpError`):

```rust
#[tokio::test]
async fn a_retune_inside_the_capture_keeps_the_id_and_does_not_tune() {
    // source_pin radio0 at 476_918_750 / 1_024_000; start FmDemod at 476_900_000 / 12_500 → id;
    // retune(id, 476_905_000, 10_000, &[]) == Ok(OpResult::Job(id)); record: tuning == {476_905_000, 10_000}, state Running,
    // capture_rate_hz == Some(1_024_000), operator_graph false; node_params(rec, "channel")["center_hz"] == 476_905_000 and ["bandwidth_hz"] == 10_000;
    // source(node).current == {476_918_750, 1_024_000}; holders == [id]; radio.tunes() == 1 (the pin's).
}
#[tokio::test]
async fn a_lone_job_retuned_inside_its_capture_does_not_tune_the_port() {
    // start Passthrough at 100_000_000 / 1_000_000 (Radio tunes to exactly that: the capture);
    // retune(id, 100_200_000, 10_000) → Ok; current still {100_000_000, 1_000_000}; radio.tunes() == 1.
}
#[tokio::test]
async fn a_retune_outside_the_capture_tunes_when_nobody_else_holds() {
    // start Spectrum elastic at 100 MHz/1 MHz (el), Passthrough at 100_100_000 / 10_000 (id);
    // retune(id, 433_000_000, 12_500) → Ok; current == {433_000_000, 12_500}; record(id).tuning == that;
    // within 2 s record(el).tuning == {433_000_000, 12_500} and el Running (the elastic job followed).
}
#[tokio::test]
async fn a_retune_outside_the_capture_with_another_holder_is_a_conflict_and_leaves_the_job_as_it_was() {
    // A Passthrough 100_000_000 / 1_000_000; B Passthrough 100_100_000 / 10_000;
    // retune(B, 433_000_000, 12_500) == Err(OpError::Conflict{ source: radio0, holders: [A], current: {100_000_000, 1_000_000}, options })
    //   with options.last() == "retry with replace: [<A>]";
    // record(B): Running, tuning {100_100_000, 10_000}, graph.id unchanged; holders == [A, B]; radio.tunes() == 1.
}
#[tokio::test]
async fn replace_names_the_holder_and_the_retune_succeeds() {
    // as above with replace [A]: Ok; record(A).state == Stopped; record(B).tuning == {433_000_000, 12_500}; holders == [B]; current == that band.
}
#[tokio::test]
async fn a_retune_onto_a_band_another_rigid_job_holds_inside_the_capture_is_granted() {
    // pin 476_918_750 / 1_024_000; A FmDemod 476_900_000 / 12_500; B FmDemod 476_925_000 / 12_500;
    // retune(B, 476_900_000, 12_500) → Ok; both Running; holders == [A, B]; current unchanged.
}
#[tokio::test]
async fn graph_put_squelch_db_is_carried_through_a_retune() {
    // FmDemod at 100_000_000 / 12_500 on an unheld radio; graph_get, set nbfm_demod squelch_db = -70, graph_put (only a carried param: the record stays the node's);
    // retune(id, 100_002_000, 12_500) → node_params(rec, "nbfm_demod")["squelch_db"] == -70 and ["squelch"] == true; operator_graph false; capture_rate_hz.is_some().
}
#[tokio::test]
async fn refusals_name_the_kind_the_state_and_the_id() {
    // scan 100 MHz / 40 MHz → other("scan is not retunable"); watch 403_025_000 / 5_950_000 → "watch is not retunable";
    // identify 100 MHz / 12_500 → "identify is not retunable"; spectrum elastic and spectrum rigid → "spectrum is not retunable";
    // an attached transcribe (registry also holds StreamInStage and attach.rs's stand-ins for denoise/vad/transcribe; attach to an FmDemod) → "transcribe is attached to job <fm>: retune that job";
    // graph_put of the FmDemod's graph with a passthrough node ({"kind": "audio"}) appended on an audio edge from nbfm_demod (a topology change: an operator graph, as crates/zeta/tests/resume.rs:226-282 does) → "job <id> runs an operator graph (graph_put): put a new graph or start a new job";
    // a stopped FmDemod → "job <id> is stopped"; JobId::new() → Err(OpError::Unknown{field == "job", ..});
    // replace [id] on id itself → "job <id> cannot replace itself" and the job still Running;
    // Node::job_start("monitor", .., passthrough graph) → "monitor is not a kind the node expands: start a new job".
}
#[tokio::test]
async fn an_out_of_range_band_is_refused_before_anything_moves() {
    // FmDemod at 100 MHz / 12_500; retune(id, 7_000_000_000, 12_500) == Err(OutOfRange{field == "center_hz", ..});
    // retune(id, 100_000_000, 30_000_000) == Err(OutOfRange{field == "bandwidth_hz", max == 20_000_000, ..});
    // record unchanged, Running; radio.tunes() == 1.
}
#[tokio::test]
async fn a_retune_to_the_same_band_restarts_the_job_and_keeps_its_tuning() {
    // FmDemod; let n = debug_stage_count() after 200 ms; retune to its own band → Ok, Running, tuning same; after 200 ms debug_stage_count() == n.
}
```

  In `tests/ops.rs:481` the variant count becomes 23; in `tests/ops_stream.rs:395-446` add `Op::JobRetune { job: JobId("j".into()), center_hz: Hz(1), bandwidth_hz: Hz(1), replace: vec![] }`; in `crates/zeta/tests/agents_md.rs:24` the count becomes 23.

- [ ] **Step 2: Run to verify they fail**: `cargo test -p zeta-core --test retune --test ops --test ops_stream` → compile errors (no `JobRetune`, no `retune_locked`); `cargo test -p zeta-core --lib lease` → the two new tests fail to compile.
- [ ] **Step 3: Implement** the variant and its doc, `Op::name`, `retunable`, `fn job_retune` and the `apply` arm in `ops.rs`; `retune_locked` in `lease.rs`; `node/retune.rs` with `Node::job_retune` and `Inner::relaunch_in_place`; `Node::expandable_kind`; the `JobEntry.relaunch` doc.
- [ ] **Step 4: Regenerate** `AGENTS.md` and the skill (the new section renders from the doc comment; its CLI line reads `zeta job retune --job <id> --center-hz 144800000 --bandwidth-hz 12500` before Task 4 adds the subcommand, which is the same text). Run `cargo test -p zeta-core` (all), `cargo test -p zeta --test agents_md --test transports`, `cargo test -p zeta-client`, clippy, fmt → green.
- [ ] **Step 5: Commit**: `git commit -m "Jobs: job_retune moves a running job's lease under the gate and relaunches it under its id (inside the capture untuned, outside it a tune or a conflict with the old lease put back); Lease: retune_locked"`.

---

### Task 2: Carried params versus the re-derived guard (Sonnet)

**Files:**
- Modify: `crates/zeta-core/src/ops.rs:578-625` (`reexpand` → `reexpand_carrying`), `:1049-1058` (`RETUNE_CARRIED_PARAMS`), `crates/zeta-core/src/node/retune.rs` (step 4 of `job_retune` calls `reexpand_carrying(.., &RETUNE_CARRIED_PARAMS)`)
- Test: `crates/zeta-core/tests/ops.rs` (beside `reexpand_carries_the_guard_params` `:2629-2668`; `cb_graph` `:2546`, `modes()` `:2669`), `crates/zeta-core/tests/retune.rs`

**Interfaces:**
- `pub fn JobKind::reexpand_carrying(&self, source: &SourceId, tuning: Tuning, source_rate_hz: Hz, bank_bin_rate_hz: Option<u64>, stored: &Graph, modes: &ModeRegistry, carried: &[&str]) -> Result<Graph, OpError>`: today's `reexpand` body with `carried` in place of `CARRIED_PARAMS`; `reexpand` becomes `self.reexpand_carrying(.., &CARRIED_PARAMS)` (its callers `fit_capture`, `follow_capture`, `only_carried_params_differ` unchanged). `deviation_hz` and `sideband` are read back from `stored` as today, whatever `carried` says.
- `pub const RETUNE_CARRIED_PARAMS: [&str; 2] = ["squelch_db", "squelch"]` with a doc: what a retune carries; the guard (`guard_hz`, `guard_db`, `channel_hz`, `wide_hz`) is derived again from the plan at the new band (spec §3 step 3).

- [ ] **Step 1: Write the failing tests.** In `tests/ops.rs` (using `cb_graph` `:2546-2563` and `modes()` `:2669`):

```rust
#[test]
fn reexpand_carrying_squelch_only_rederives_the_guard_at_the_new_band() {
    // stored = cb_graph(476_900_000) with nbfm squelch_db = -70 and guard_hz = null (an operator put);
    // FmDemod.reexpand_carrying(&src, t(476_887_500, 12_500), Hz(1_024_000), Some(256_000), &stored, &modes(), &RETUNE_CARRIED_PARAMS):
    // channel params == {"center_hz": 476_887_500, "bandwidth_hz": 12_500, "out_rate_hz": 51_200, "wide_hz": 37_500};
    // nbfm params == {"deviation_hz": 5_000, "audio_rate_hz": 48_000, "squelch_db": -70, "squelch": true, "guard_hz": 12_500, "guard_db": 12, "channel_hz": 12_500}.
}
#[test]
fn reexpand_carrying_squelch_only_drops_the_guard_off_plan() {
    // same stored, tuning t(476_905_000, 12_500): no wide_hz on the channel node, no guard_hz/guard_db/channel_hz on the demod; squelch_db still -70.
}
#[test]
fn a_width_preset_on_a_plan_channel_changes_channel_hz_and_wide_hz() {
    // stored = cb_graph(476_900_000); tuning t(476_900_000, 25_000) → channel {"bandwidth_hz": 25_000, "wide_hz": 50_000, ..}; nbfm channel_hz == 25_000, guard_hz == 12_500.
}
```

  In `tests/retune.rs` (pin `476_918_750 / 1_024_000` first, as Task 1's tests do):

```rust
#[tokio::test]
async fn a_retune_onto_a_neighbouring_channel_rederives_the_guard() {
    // FmDemod ch20 (476_900_000 / 12_500); graph_put with guard_hz = null and squelch_db = -65 (carried params only);
    // retune to 476_887_500 / 12_500 → nbfm guard_hz == 12_500, guard_db == 12, channel_hz == 12_500, squelch_db == -65; channel center_hz == 476_887_500, wide_hz == 37_500.
}
#[tokio::test]
async fn a_width_preset_retune_changes_the_channel_geometry() {
    // FmDemod ch20 / 12_500 → retune 476_900_000 / 25_000 → channel bandwidth_hz == 25_000, wide_hz == 50_000; nbfm channel_hz == 25_000.
}
#[tokio::test]
async fn a_retune_off_plan_drops_the_guard() {
    // FmDemod ch20 → retune 476_905_000 / 12_500 → no guard_hz/guard_db/channel_hz, no wide_hz.
}
```

- [ ] **Step 2: Run to verify they fail**: `cargo test -p zeta-core --test ops reexpand_carrying --test ops a_width_preset --test retune` → compile error (no `reexpand_carrying`); after a stub that forwards to `reexpand`, the three retune tests fail (the guard is carried, not derived).
- [ ] **Step 3: Implement** `reexpand_carrying`, `RETUNE_CARRIED_PARAMS`, the call in `node/retune.rs`.
- [ ] **Step 4: Run** `cargo test -p zeta-core --test ops --test retune --test resume`, `cargo test -p zeta --test resume` (the `graph_put`-carried tests must stay green), clippy, fmt → green. No guide text changes (Task 4 writes the note).
- [ ] **Step 5: Commit**: `git commit -m "Jobs: a retune carries squelch and squelch_db and derives the plan-channel guard again at the new band (reexpand_carrying)"`.

---

### Task 3: Attached jobs survive their input's retune and `graph_put` (Opus)

**Files:**
- Modify: `crates/zeta-core/src/node/retune.rs` (`hold_attached`, `relaunch_attached`; `Node::job_retune` uses them), `crates/zeta-core/src/node/mod.rs:341-358` (`swap_graph`), `crates/zeta-core/src/stream_in.rs:34-44` (doc), `crates/zeta-core/src/ops.rs:153-161` (the `JobAttach` doc), `crates/zeta/src/agents_md.rs` (`note("job_attach")`)
- Test: `crates/zeta-core/tests/attach.rs:384-399` and new tests, `crates/zeta-core/tests/retune.rs`

**Interfaces:**
- `pub(super) fn Inner::hold_attached(&self, input: &JobId) -> Vec<JobId>`: under the jobs lock, every entry whose `record.input == Some(input)`, state `Running` and `cancel.is_some()` gets `relaunch = true`; their ids. Called before the input's run is stopped, so their supervisors (ending when the input's streams unregister) keep the records `Running`.
- `pub(super) async fn Inner::relaunch_attached(self: &Arc<Self>, held: Vec<JobId>)`: for each id, under the lock take `cancel` (cancel it if live) and `done`, read `run`; await `done`; re-read the record: not `Running` → `relaunch = false`, skip (the operator stopped it meanwhile); else `self.launch(rec).await` (an attached launch takes no gate: `launch_inner` repoints `stream_in` at the input's current audio stream); on `Err(e)` → `relaunch = false`, `self.finish(&id, run, JobState::Failed { reason: e.to_string() })`. `cancel` must be taken before the launch or `record_running` refuses it as `already running a launch`.
- `Node::job_retune`: `let held = self.inner.hold_attached(id);` right before step 5's `relaunch_in_place`; `self.inner.relaunch_attached(held).await` after step 6's `drop(guard)` and `after_gate`, before returning. On a failed relaunch of the input the attached relaunch still runs (each fails `input job <id> not running`, as the spec's "failed with the reason").
- `Inner::swap_graph(id, graph, capture_rate_hz, running)`: when `running`, `let held = self.hold_attached(id)` before `store_graph`, and `self.relaunch_attached(held).await` after `launch_task(rec).await??` (also when the launch failed).
- Docs: `stream_in.rs:34-44` says an input retuned by `job_retune` or restarted by `graph_put` keeps its attached jobs (relaunched by the node on the new stream) and that the stage's own clean end is what makes that possible; the `JobAttach` doc replaces "It ends `stopped` when the input job's stream closes for any reason, a restart by `graph_put` included (attach again to follow the new graph)." with "It follows its input through a `job_retune` or a `graph_put` restart: it is relaunched under its own id on the input's new stream once the input runs again (`failed` with the reason when it cannot be). It ends `stopped` when the input job stops or fails for its own reason."; `note("job_attach")`'s "it holds no lease and ends when the input job stops" becomes "it holds no lease, follows its input through a `job_retune` or a `graph_put` (relaunched under its own id) and ends when the input job stops".
- `recover.rs` keeps its record-driven attached relaunch (`resume_records`, `recover.rs:194-271`): spec §9's "one helper, three callers" is two callers here (the retune and `swap_graph`) plus recovery's own, which relaunches `Failed` records, not live ones.

- [ ] **Step 1: Write the failing tests.** In `tests/attach.rs`, rewrite `attach_follows_graph_put_of_the_input` (`:384-399`; its "Ruling R4" comment goes) and add two tests (a `registry_with_dsp()` = `registry()` plus `zeta_dsp::register_all`, so `Op::JobStart { kind: FmDemod, source: file0(), center_hz: 100_000_000, bandwidth_hz: 12_500, .. }` runs the real demodulator on the tone, whose squelch opens, feeding the stand-in chain):

```rust
#[tokio::test]
async fn attach_follows_graph_put_of_the_input() {
    // input = start_audio; id = attach_ok; job_graph_put(&input, node_graph("audio_src", {})) → Ok;
    // record(input).state == Running; within 2 s record(id).state == Running (same id, never Stopped);
    // event(&node, &id, "transcript", 2000).is_some(); job_list records with input == Some(input): exactly [id].
}
#[tokio::test]
async fn an_attached_transcribe_survives_its_inputs_retune_under_its_id() {
    // registry_with_dsp; FmDemod input on file0 at 100_000_000 / 12_500; att = attach_ok(input); event transcript within 3 s;
    // apply JobRetune { job: input, center_hz: 100_002_000, bandwidth_hz: 10_000, replace: [] } → Ok(Job(input));
    // record(input).tuning == {100_002_000, 10_000}; within 2 s record(att).state == Running, record(att).input == Some(input);
    // event(&node, &att, "transcript", 3000).is_some(); exactly one record has input == Some(input).
}
#[tokio::test]
async fn an_attached_job_the_operator_stopped_stays_stopped_through_a_retune() {
    // as above; job_stop(att) first (Stopped); retune the input → Ok; record(att).state == Stopped after 500 ms; exactly one record has input == Some(input).
}
```

  In `tests/retune.rs` (the shape of `ops_stream.rs:92-127`, its `next(rx)` helper copied):

```rust
#[tokio::test]
async fn the_ops_stream_logs_job_retune_with_its_outcome() {
    // rx = node.bus().subscribe(&node.ops_stream()); start FmDemod (one frame: skip it); retune ok → frame: p["op"]["op"] == "job_retune", p["op"]["job"] == id, p["outcome"] == "ok", p["result"]["result"] == "job", p["result"]["value"] == id, p["transport"] == "local";
    // retune of JobId::new() → p["outcome"] == "error", p["error"]["error"] == "unknown".
}
```

- [ ] **Step 2: Run to verify they fail**: `cargo test -p zeta-core --test attach --test retune` → the three attach tests fail (the attached job ends `Stopped`), the ops test passes already (keep it).
- [ ] **Step 3: Implement** `hold_attached`, `relaunch_attached`, the two call sites, the docs and the note.
- [ ] **Step 4: Regenerate** `AGENTS.md` and the skill; run `cargo test -p zeta-core` (all; `recover.rs` and `attach.rs` must stay green), `cargo test -p zeta --test attach --test speech_chain --test agents_md --test transports`, `cargo test -p zeta-client`, clippy, fmt → green.
- [ ] **Step 5: Commit**: `git commit -m "Jobs: an attached job follows its input through job_retune and graph_put, relaunched under its own id on the new stream (hold_attached, relaunch_attached); the job_attach guide says so"`.

---

### Task 4: `zeta job retune`, the MCP tool, the guide's Operations text (Sonnet)

**Files:**
- Modify: `crates/zeta/src/cli.rs:519-533` (`JobCmd::Retune` after `Attach`), `:712-723` (`mutates`), `:801-809` (`into_op` arm after `Attach`)
- Modify: `crates/zeta/src/agents_md.rs` (`RUNNING`: the serve-lock sentence's list gains `job retune` after `job attach`; `note("job_retune")`), `README.md:45` and `:52` (the same lists), `README.md:69-78` (the CLI walkthrough gains one `job retune` line)
- Test: `crates/zeta/tests/cli.rs` (`:422-468` pattern, `:531-590`), `crates/zeta/tests/transports.rs`, `crates/zeta-client/tests/tools.rs` (beside `source_ensure_tool_shape` `:59`)

**Interfaces:**
- `JobCmd::Retune { #[arg(long, value_name = "ID")] job: String, #[arg(long, value_name = "HZ")] center_hz: u64, #[arg(long, value_name = "HZ")] bandwidth_hz: u64, #[command(flatten)] replace: ReplaceArg }` with the doc "Move a running job to another band under the same id (its attached jobs follow)"; `mutates()` includes it; `into_op` → `Op::JobRetune { job: JobId(job), center_hz: Hz(center_hz), bandwidth_hz: Hz(bandwidth_hz), replace: replace.into_ids() }`. `cli_form` in `agents_md.rs` already renders `zeta job retune --job <id> --center-hz 144800000 --bandwidth-hz 12500`.
- `note("job_retune")`, verbatim:

  > `replace`: as on `job_start`, the holders that may be stopped to make room, stopped under the gate right before the request; a `conflict` offers `retry with replace: [<ids>]` and the page shows the same retry buttons. A demodulator keeps its `squelch`, `squelch_db`, `deviation_hz` and `sideband` through a retune; its adjacent-channel guard (`guard_hz`, `guard_db`, `channel_hz`, the channel node's `wide_hz`) is derived again from the channel plan at the new band, never carried, so a width preset changes what the filter passes and a move to a neighbouring channel takes that channel's geometry; a guard turned off with `graph_put` (`guard_hz: null`) comes back on a retune onto a plan channel. Every attached job (`job_attach`) reading the retuned job is relaunched under its own id once the input runs again (one that cannot relaunch is `failed` with the reason; the retune still succeeds). Centre the band on the signal's measured centre (`measure`, `classify`), never on a raster: see Fitting a demod to a signal under Field craft. A one-shot `zeta job retune` cannot reach a job `zeta serve` runs (`job <id> is not running in this node`): use `--node`. On the CLI, `--replace ID[,ID]`.

- [ ] **Step 1: Write the failing tests.** In `crates/zeta/tests/cli.rs`:

```rust
#[test]
fn job_retune_one_shot_is_refused_for_a_job_not_running_here() {
    // job start passthrough on tone (one-shot: persisted running, not run) → id; `job retune --job <id> --center-hz 100010000 --bandwidth-hz 10000` → exit 2,
    // json error == "other", message contains "is not running in this node"; `job list` still shows the record's tuning 100000000/48000.
}
#[test]
fn job_retune_replace_flag_round_trips_to_the_op() {
    // as job_start_replace_flag_round_trips_to_the_op (:429-468): `job retune --job 01AAAAAAAAAAAAAAAAAAAAAAAA --center-hz 1 --bandwidth-hz 1 --replace 01BBBBBBBBBBBBBBBBBBBBBBBB,01CCCCCCCCCCCCCCCCCCCCCCCC`
    // → exit 2 (unknown job); last_logged_op(dir)["op"] == "job_retune" and ["replace"] == ["01BBBB…", "01CCCC…"]; `--replace ''` logs [].
}
```

  In `mutating_one_shot_refused_while_serve_lock_held` (`:531-590`) add `let retune = ["job", "retune", "--job", "j", "--center-hz", "100000000", "--bandwidth-hz", "48000"];` to the refused list. In `crates/zeta/tests/transports.rs`:

```rust
#[test]
fn remote_job_retune_moves_a_running_job() {
    // ServeProc::start; remote job start passthrough on tone 100000000/48000 → id; remote `job retune --job <id> --center-hz 100010000 --bandwidth-hz 10000` → success, value == id;
    // remote job list: the record Running with tuning {100010000, 10000}; the id is the only passthrough.
}
```

  In `crates/zeta-client/tests/tools.rs`:

```rust
#[test]
fn job_retune_tool_shape() {
    // let t = op_tools(&schema(), MD).into_iter().find(|t| t.name == "job_retune").unwrap();
    // required (as a BTreeSet) == {"bandwidth_hz", "center_hz", "job", "replace"}; input_schema["properties"]["replace"]["type"] == "array";
    // t.description contains "under the same id" (the section text from AGENTS.md).
}
```

- [ ] **Step 2: Run to verify they fail**: `cargo test -p zeta --test cli job_retune --test cli mutating_one_shot --test transports remote_job_retune` → clap refuses `retune`; `cargo test -p zeta-client --test tools job_retune_tool_shape` → passes only once the committed `AGENTS.md` carries the note (it fails on the "under the same id" check until Step 4).
- [ ] **Step 3: Implement** the subcommand, `mutates`, `into_op`, the note, the `RUNNING` list, the README lists and walkthrough line (`./target/debug/zeta $A job retune --job <id> --center-hz 100010000 --bandwidth-hz 10000   # same id; attached jobs follow`).
- [ ] **Step 4: Regenerate** `AGENTS.md` and the skill; `cargo test -p zeta --test cli --test transports --test agents_md`, `cargo test -p zeta-client`, `cargo test -p zeta-mcp` (the tool list comes from the schema), clippy, fmt → green.
- [ ] **Step 5: Commit**: `git commit -m "CLI: zeta job retune (--replace as job start); the guide's job_retune note (replace, carried params, the re-derived guard, attached jobs); README lists"`.

---

### Task 5: The page fits a demodulator to the signal before starting it (Opus)

**Files:**
- Create: `crates/zeta-ui/web/units.js`
- Modify: `crates/zeta-ui/web/menu.js` (header docs `:1-99`; constants after `IDENTIFY_MIN_BANDWIDTH_HZ` `:108`; `choose` `:311-350`; `startJob` `:355-369`)
- Modify: `crates/zeta/tests/ui.rs:150-164` (`tone_at`, the fixture pattern), `:300-360` (the `quiet` fixture and `--source-file quiet=…`), `crates/zeta-ui/tests/e2e/ui.py` (docstring `:1-16`; `fresh_spectrum_jobs` `:222-226` gains `"quiet"`; the three `source_list → ok (8 sources)` assertions at `:118`, `:200`, `:519` become `9 sources`; new scenarios after `scenario_decode_starts_job_and_logs_op` `:606`)

**Interfaces:**
- `units.js` exports `mhzText(hz)`, `khzText(hz)`, `bandText(centerHz, bandwidthHz)` as the Global Constraints fix them.
- `menu.js` exports: `FIT_KINDS = new Set(["fm_demod", "wfm_demod", "am_demod", "ssb_demod"])`, `FIT_MIN_HZ = { fm_demod: 5000, am_demod: 5000, ssb_demod: 3000, wfm_demod: 150000 }`, `FIT_MIN_HZ_DEFAULT = 5000` (a kind without an entry: the outline menu's Fit on a decoder), `FIT_FACTOR = 1.25`, `FIT_TIMEOUT_MS = 3000`, `FIT_NOISE_SNR_DB = 6`.
- `export async function fitBand(api, source, centerHz, bandwidthHz) -> Promise<{centerHz, bandwidthHz, label, snrDb, dialHz} | null>`: `api.op(job_start measure)` on the band (`deviation_hz: null, sideband: null, elastic: false, replace: []`; an `OpError` propagates); `api.streams()` → the measure job's `event` stream; `api.op({op: "stream_tail", stream, max_frames: 3, timeout_ms: FIT_TIMEOUT_MS})`; `api.op(job_stop)` in a `finally`. The frame used is the first whose payload has `partial === false`, else the last; `null` when no frame came, when `label.name === "noise"`, or when `label.name === "unknown"` and `snr_db < FIT_NOISE_SNR_DB`. `dialHz` is the integer of `/dial ≈ (\d+) Hz/` in `label.evidence`, else `null`.
- `export function fitTuning(job, fit, box) -> {centerHz, bandwidthHz}` (`job` a `JOBS` entry or `{kind, sideband}`; `box` the `jobTuning` result, of which only `box.bandwidthHz`, the cap, is read): centre `fit.dialHz` when `job.sideband` and the dial is known, else for SSB `fit.centerHz ∓ SSB_DIAL_OFFSET_HZ` (usb −, lsb +), else `fit.centerHz`; width `Math.min(box.bandwidthHz, Math.max(FIT_MIN_HZ[job.kind] ?? FIT_MIN_HZ_DEFAULT, Math.round(fit.bandwidthHz * FIT_FACTOR)))`; both rounded to integers.
- `choose(action)` for an action whose `jobOf(action).kind` is in `FIT_KINDS` (Decode and Transcribe entries alike): the menu stays open with `#ctx-footer` reading `measuring…` and the box drawn; `fitBand(this.ov.api, s.sourceId, s.centerHz, s.bandwidthHz)`; on a thrown error `this.t.note(`${kind}: ${e.message}`, true)`, close, nothing starts; then close; `null` → `this.t.note(`no signal found in ${mhzText(lo)}-${mhzText(hi)} MHz; started ${kind} at the box as drawn`)` (lo/hi the box edges) and `startJob(action, s)` as today; else `startJob(action, s, fitTuning(job, fit, jobTuning(action, s)))` and `this.t.note(`fitted ${kind} to ${bandText(t.centerHz, t.bandwidthHz)} (${fit.label})`)`. `startJob(action, s, tuning = jobTuning(action, s))` gains the override. The Transcribe twins attach after as today. Every other entry closes first as today.
- `ui.rs`: `fn quiet_fixture(dir: &Path) -> PathBuf`: 48 000 samples of `Complex::new(u(), u())`, `u()` uniform in `-0.01..0.01` from the LCG `awgn_real` uses (`zeta-dsp/src/fixtures.rs:65-71`, constants `6364136223846793005`/`1442695040888963407`, seed 7), written by `zeta_dsp::fixtures::write_fixture(dir, "quiet", &iq, 48_000, 20_000_000)`; attached as `--source-file quiet=<meta>`.

- [ ] **Step 1: Write the failing fixture test** in `ui.rs` (a plain `#[test]`, no browser: it pins what the e2e relies on):

```rust
#[test]
fn quiet_fixture_measures_as_noise() {
    // meta = quiet_fixture(dir); run env!("CARGO_BIN_EXE_zeta") one-shot: --state-dir <dir>/state --source-file quiet=<meta>
    //   job start --kind measure --source quiet --center-hz 20000000 --bandwidth-hz 9600 --then-tail 1,3000 → exit 0;
    // stdout is [job_result, frames_result, {"stopped": true}]; frames_result.value.frames[0].payload.label.name == "noise".
}
```

  Run `cargo test -p zeta --test ui quiet_fixture_measures_as_noise` → compile error (no `quiet_fixture`). Implement the fixture; run again → PASS. If the label is not `noise`, halve the amplitude and try again, recording the amplitude used in the fixture's doc comment.
- [ ] **Step 2: Write the failing scenarios** in `ui.py` (`is_op(name)` helper: `lambda r: r.method == "POST" and r.url.endswith("/op") and json.loads(r.post_data or "{}").get("op") == name`):

```python
def scenario_fit_first_lands_on_the_tone(page, url, secret):
    # open_menu_on_tone: the box is ~99.9952-100.0048 MHz (9.6 kHz); the tone is at 100.001 MHz.
    menu = open_menu_on_tone(page, url, secret)
    page.click('#ctx .ctx-item[data-action="fm_demod"]')
    expect(page.locator("#ctx-footer")).to_have_text("measuring…")
    expect(menu).to_be_hidden(timeout=10_000)
    expect(page.locator("#transcript .line.note", has_text=re.compile(
        r"^fitted fm_demod to 100\.00\d\d MHz, 5\.0 kHz \(carrier\)$"))).to_have_count(1)
    jobs = [running fm_demod jobs on tone]; assert len(jobs) == 1
    t = jobs[0]["tuning"]; assert abs(t["center_hz"] - 100_001_000) <= 100; assert t["bandwidth_hz"] == 5000
    assert no running measure job on tone remains
    finally: stop them

def scenario_fit_first_without_a_signal_starts_at_the_box(page, url, secret):
    select_source(page, url, secret, "quiet")
    page.wait_for_function("() => window.zetaTest.wf().painted > 60", timeout=15_000)
    menu = drag_menu(page, 0.40, 0.60)          # ~19.9952-20.0048 MHz
    footer = page.locator("#ctx-footer").text_content(); c, bw = ints of center_hz=, bandwidth_hz=
    page.click('#ctx .ctx-item[data-action="fm_demod"]')
    expect(menu).to_be_hidden(timeout=10_000)
    expect(page.locator("#transcript .line.note", has_text=re.compile(
        r"^no signal found in 19\.99\d\d-20\.00\d\d MHz; started fm_demod at the box as drawn$"))).to_have_count(1)
    jobs = [running fm_demod on quiet]; assert len(jobs) == 1 and jobs[0]["tuning"] == {"center_hz": c, "bandwidth_hz": bw}
```

  Update the three `8 sources` assertions to `9 sources`, `fresh_spectrum_jobs` and the docstring.

- [ ] **Step 3: Run to verify they fail**: `ZETA_UI_ONLY=fit_first_lands_on_the_tone,fit_first_without_a_signal_starts_at_the_box cargo test -p zeta --test ui -- --nocapture` → FAIL (no `measuring…`, no `fitted` line, no `no signal found` line).
- [ ] **Step 4: Implement** `units.js`, the constants, `fitBand`, `fitTuning`, the `choose`/`startJob` changes, and the menu.js header docs (the fit entries say they measure first and what the transcript reads).
- [ ] **Step 5: Run** the two scenarios plus `command_line_applies_op,double_connect_starts_one_shell,narrow_answer_is_ribbon,decode_starts_job_and_logs_op,ssb_entry_tunes_the_dial,transcript_ribbon_appears,wfm_decode_plays_audio` with `ZETA_UI_ONLY` → all PASS (the SSB and transcribe scenarios now fit first: their `job_start` lines carry the fitted band, so loosen their regexes to the fitted values where they pin `center_hz=`), clippy/fmt untouched (JS), `cargo test -p zeta --test ui` once whole.
- [ ] **Step 6: Commit**: `git commit -m "UI: a demodulator started from a box is fitted first (measure, then the measured centre and 1.25x the occupied width, floored per kind, capped at the box); the quiet e2e fixture"`.

---

### Task 6: Draggable outlines with a ghost, a readout, snap-back and the replace retry (Opus, higher effort)

**Files:**
- Modify: `crates/zeta-ui/web/overlays.js` (header docs `:1-110`; `rigidBand` `:132-145`; constructor `:193-235`; `syncOutlines` `:391-413`; `outline`/`unoutline`/`placeOutline` `:726-766`; `layout` `:768-778`), `crates/zeta-ui/web/app.js:349` (`REPLACEABLE`), `:600-606` (`mountOverlays` opts), `crates/zeta-ui/web/style.css:163-171`
- Modify: `crates/zeta-ui/tests/e2e/ui.py` (new scenarios after `scenario_rigid_bands_are_outlined` `:1534`; one assertion in `scenario_watch_entry_starts_a_watch_and_outlines_its_band` `:2006-2028`)

**Interfaces:**
- `mountOverlays(el, wf, api, { jobsEl, plan, applyOp, footprint })`: `applyOp(op) -> Promise<result | null>` is app.js's (its conflict path shows the retry buttons; `null` means failed and already reported), `footprint() -> "wide" | "narrow"` reads `document.documentElement.dataset.footprint`. `app.js:349` `REPLACEABLE` gains `"job_retune"`.
- `export const NOT_RETUNABLE = ["spectrum", "scan", "identify", "watch", "transcribe"]`, `HANDLE_PX = 8`, `DRAG_CLICK_PX = 3`, `MIN_WIDTH_HZ = 1000`.
- `ov.retunable(job) -> bool`: `isRunning(job) && rigidBand(job) && !job.operator_graph && !NOT_RETUNABLE.includes(job.kind)`.
- `ov.outline(lo, hi, text, key, job = null)`: with a retunable `job` in the wide footprint the element gets class `retunable` and two children `.outline-handle[data-edge="hi"]` (top) and `.outline-handle[data-edge="lo"]` (bottom), each `HANDLE_PX` tall, centred on the edge; the body takes `pointerdown` (button 0) as a move, a handle as a resize of that edge. Pointer capture on the pressed element; `pointermove` maps `ev.clientY - layer.getBoundingClientRect().top` through `wf.hzForY` to the new `lo`/`hi` (move: both shift by the Hz delta; an edge: that edge follows, clamped so `hi - lo >= MIN_WIDTH_HZ`); the outline is re-placed, a `.outline-ghost` (dashed, the old band) is added on the first move past `DRAG_CLICK_PX` with a `.outline-readout` just under the outline reading `${bandText(c, w)} · release to retune`; `pointerup` under `DRAG_CLICK_PX` of travel is a click (nothing; the ghost never appeared); else `await applyOp({ op: "job_retune", job: job.id, center_hz: Math.round((lo + hi) / 2), bandwidth_hz: Math.round(hi - lo), replace: [] })`: `null` → snap back to the ghost's band; a result → keep the dragged band until the next `job_list`; ghost and readout removed either way. `pointercancel` snaps back and sends nothing. `this.drag = { key, job, mode: "move" | "lo" | "hi", y0, lo0, hi0, lo, hi, moved }`; while it is set, `syncOutlines` and `layout` leave that key's outline where the pointer has it. In the narrow footprint no handles are made and the body stays pointer-transparent (Task 7 puts the menu on the tag).
- CSS: `.outline.retunable { pointer-events: auto; cursor: grab; }`, `.outline.retunable:active { cursor: grabbing; }`, `.outline-handle { position: absolute; left: 0; right: 0; height: 8px; cursor: ns-resize; pointer-events: auto; }` (`[data-edge="hi"] { top: -4px }`, `[data-edge="lo"] { bottom: -4px }`), `.outline-ghost` as `.outline` with a dimmer dashed border and no label, `.outline-readout` as `.outline-label` placed under the outline's bottom edge.

- [ ] **Step 1: Write the failing scenarios** in `ui.py` (`retune` source, WIDE; `hzpp = 48_000 / canvas.bounding_box()["height"]`; `cur = source_info(...)["current"]["center_hz"]`; `outline_box(page, job)` returns the outline's bounding box):

```python
def scenario_drag_handle_narrows_the_band_and_clamps_at_1_khz(page, url, secret):
    select_source(... "retune"); wait BRIGHT_AT_RIGHT_JS; job = start_job(passthrough, retune, cur, 10_000)
    ol = page.locator(f'#waterfall .outline[data-key="{job}"]'); page.evaluate("window.zetaTest.poll()")
    expect(ol.locator(".outline-handle")).to_have_count(2)
    hb = ol.locator('.outline-handle[data-edge="hi"]').bounding_box(); x, y = centre of hb
    with page.expect_request(is_op("job_retune")) as req:
        page.mouse.move(x, y); page.mouse.down(); page.mouse.move(x, y + 20, steps=5)
        expect(page.locator("#waterfall .outline-ghost")).to_have_count(1)
        expect(page.locator("#waterfall .outline-readout")).to_contain_text("release to retune")
        top = ol.bounding_box()["y"]; page.evaluate("window.zetaTest.poll()"); page.wait_for_timeout(300)
        assert abs(ol.bounding_box()["y"] - top) < 1, "a poll mid-drag moved the outline"      # Review Focus 4
        page.mouse.up()
    body = json.loads(req.value.post_data); assert body["job"] == job and body["replace"] == []
    assert abs(body["bandwidth_hz"] - (10_000 - 20 * hzpp)) <= 1.5 * hzpp
    assert abs(body["center_hz"] - (cur - 10 * hzpp)) <= 1.5 * hzpp
    expect(ghost).to_have_count(0); expect(readout).to_have_count(0)
    wait_until(lambda: job_record(url, secret, job)["tuning"] == {"center_hz": body["center_hz"], "bandwidth_hz": body["bandwidth_hz"]}, 5.0, "the record follows")
    expect(page.locator("#transcript .line.op", has_text=re.compile(rf"^job_retune job={job} .*→ job {job}"))).to_have_count(1)
    # Clamp: drag the hi handle down past the lo edge.
    hb = ...hi handle box; with page.expect_request(is_op("job_retune")) as req2:
        page.mouse.move(x, y2); page.mouse.down(); page.mouse.move(x, y2 + ol_height + 40, steps=5)
        expect(page.locator("#waterfall .outline-readout")).to_contain_text("1.0 kHz")
        page.mouse.up()
    assert json.loads(req2.value.post_data)["bandwidth_hz"] == 1000

def scenario_drag_body_moves_the_centre_and_a_short_drag_is_a_click(page, url, secret):
    job = start_job(passthrough, retune, cur, 10_000); ob = outline_box; x, y = ob centre (clear of the handles)
    with page.expect_request(is_op("job_retune")) as req: mouse down at (x, y), move to (x, y + 30, steps=5), up
    body: bandwidth_hz == 10_000; abs(center_hz - (cur - 30 * hzpp)) <= 1.5 * hzpp
    # A 2 px drag sends nothing and leaves the outline where it is.
    seen = []; page.on("request", lambda r: seen.append(r) if is_op("job_retune")(r) else None)
    top = outline top; mouse down at centre, move 2 px, up; page.wait_for_timeout(500); assert seen == []; assert abs(top - outline top) < 1
    # A pointercancel mid-drag snaps back and sends nothing.
    mouse down, move 25 px (ghost shows); ol.dispatch_event("pointercancel"); page.wait_for_timeout(300)
    assert seen == []; expect(ghost).to_have_count(0); assert abs(outline top - top) < 1

def scenario_drag_outside_the_capture_conflicts_snaps_back_and_the_retry_moves_it(page, url, secret):
    a = start_job(passthrough, retune, cur, 10_000); b = start_job(passthrough, retune, cur + 8_000, 4_000)
    ob = outline_box(b); top = ob["y"]; h = canvas height
    mouse down at b's centre, move up by h + 20 px (pointer capture keeps the drag), up
    line = page.locator("#transcript .line.error", has_text=re.compile(rf"conflict.*held by passthrough {a} @ 250\.000")); expect(line).to_have_count(1)
    wait_until(lambda: abs(outline_box(b)["y"] - top) < 1, 3.0, "snap back"); assert job_record(b)["tuning"]["center_hz"] == cur + 8_000
    button = line.locator("button", has_text=f"stop passthrough {a[-6:]} and retry"); expect(button).to_have_count(1)
    with page.expect_request(is_op("job_retune")) as req: button.click()
    body = json.loads(req.value.post_data); assert body["replace"] == [a]
    wait_until(lambda: job_record(a)["state"]["status"] == "stopped" and job_record(b)["tuning"]["center_hz"] == body["center_hz"], 5.0, "A stopped, B moved")
    assert source_info(url, secret, "retune")["current"]["center_hz"] == body["center_hz"]
    expect(page.locator("#transcript .line.op", has_text=re.compile(rf"^job_retune job={b} .*replace=… → job {b}"))).to_have_count(1)
```

  In `scenario_watch_entry_starts_a_watch_and_outlines_its_band` (`:2006-2028`) add `expect(el.locator(".outline-handle")).to_have_count(0)` and `assert "retunable" not in (el.get_attribute("class") or "")`.

- [ ] **Step 2: Run to verify they fail**: `ZETA_UI_ONLY=drag_handle_narrows_the_band_and_clamps_at_1_khz,drag_body_moves_the_centre_and_a_short_drag_is_a_click,drag_outside_the_capture_conflicts_snaps_back_and_the_retry_moves_it cargo test -p zeta --test ui -- --nocapture` → FAIL (no handles).
- [ ] **Step 3: Implement** the drag in `overlays.js`, the CSS, the `mountOverlays` options and `REPLACEABLE` in `app.js`, the header docs (the outline paragraph describes the handles, ghost, readout, snap-back and the narrow rule).
- [ ] **Step 4: Run** the three scenarios plus `rigid_bands_are_outlined,watch_entry_starts_a_watch_and_outlines_its_band,channel_plan_labels,overlays_follow_a_zoom,conflict_offers_replace,what_is_this_classifies` → PASS (a selection drag that starts on a retunable outline now moves the job: `drag_menu` starts at 40 px from the left edge over the whole band and no scenario boxes across a retunable outline; if one does, start its drag outside the outline).
- [ ] **Step 5: Commit**: `git commit -m "UI: a retunable job's outline is dragged by its edges (resize) or body (move) and retuned under its id on release, with a ghost, a readout, snap-back on failure and the conflict's replace retry"`.

---

### Task 7: The outline menu: Fit to signal, width presets, Stop; the narrow long-press (Sonnet)

**Files:**
- Modify: `crates/zeta-ui/web/menu.js` (`OUTLINE_ENTRIES`; `#ctx-outline` built in the constructor; `openOutline`, `closeOutline`; the key and outside-press handlers cover both menus), `crates/zeta-ui/web/overlays.js` (`contextmenu` on retunable outlines and tags; the narrow long-press; `onOutlineMenu` option), `crates/zeta-ui/web/app.js:600-603`, `:620-626` (`onOutlineMenu` wiring), `crates/zeta-ui/web/style.css:325-360` (`#ctx, #ctx-outline` share the rules)
- Modify: `crates/zeta-ui/tests/e2e/ui.py`

**Interfaces:**
- `OUTLINE_ENTRIES = [["fit", "Fit to signal"], ["w2500", "2.5 kHz"], ["w5000", "5 kHz"], ["w6250", "6.25 kHz"], ["w12500", "12.5 kHz"], ["w25000", "25 kHz"], ["stop", "Stop"]]`, rendered as `.ctx-item[data-action]` in `#ctx-outline` (role `menu`), footer `#ctx-outline-footer` reading `job=<id> center_hz=<c> bandwidth_hz=<bw>`.
- `menu.openOutline(job, x, y)` (a `job_list` record): shows `#ctx-outline` at the pointer (kept inside the viewport as `open` does), focuses the first entry; `menu.closeOutline()`. Escape and a press outside close it; Up/Down/Enter as `#ctx`.
- Actions: `fit` → `fitBand(api, job.source, job.tuning.center_hz, job.tuning.bandwidth_hz)`; `null` → `this.t.note("no signal found in the band; left as it is")`; else `applyOp({ op: "job_retune", job: job.id, center_hz, bandwidth_hz, replace: [] })` with `fitTuning(JOBS entry for the kind (or `{kind: job.kind, sideband: null}`), fit, { bandwidthHz: job.tuning.bandwidth_hz })` and the note `fitted <kind> to <bandText> (<label>)`; `wNNNN` → `applyOp({ op: "job_retune", job: job.id, center_hz: job.tuning.center_hz, bandwidth_hz: NNNN, replace: [] })`; `stop` → `applyOp({ op: "job_stop", job: job.id })`.
- `overlays.js`: option `onOutlineMenu(job, x, y)`; wide footprint: `contextmenu` on a retunable outline (or, from Task 8, its tag) → `preventDefault`, `onOutlineMenu(job, ev.clientX, ev.clientY)`; narrow footprint: a `pointerdown` on a retunable outline's label (its tag from Task 8) starts a `LONG_PRESS_MS = 500` timer, cancelled by `pointerup`/`pointercancel` or a move over `LONG_PRESS_MOVE_PX = 10`; on expiry `onOutlineMenu` at the press point. `app.js` passes `onOutlineMenu: (job, x, y) => state.menu?.openOutline(job, x, y)`.

- [ ] **Step 1: Write the failing scenarios** in `ui.py`:

```python
def scenario_outline_menu_presets_fit_and_stop(page, url, secret):
    select_source(... "retune"); wait BRIGHT; cur; job = start_job("fm_demod", "retune", cur, 20_000, deviation_hz=5000)
    ol = outline of job (poll first); ol.click(button="right")
    menu = page.locator("#ctx-outline"); expect(menu).to_be_visible()
    assert menu.locator(".ctx-item").evaluate_all("els => els.map(e => [e.dataset.action, e.textContent])") == [
        ["fit", "Fit to signal"], ["w2500", "2.5 kHz"], ["w5000", "5 kHz"], ["w6250", "6.25 kHz"], ["w12500", "12.5 kHz"], ["w25000", "25 kHz"], ["stop", "Stop"]]
    expect(page.locator("#ctx-outline-footer")).to_contain_text(f"job={job} center_hz={cur} bandwidth_hz=20000")
    with page.expect_request(is_op("job_retune")) as req: page.click('#ctx-outline .ctx-item[data-action="w12500"]')
    assert json.loads(req.value.post_data) == {"op": "job_retune", "job": job, "center_hz": cur, "bandwidth_hz": 12500, "replace": []}
    wait_until(lambda: job_record(job)["tuning"]["bandwidth_hz"] == 12500, 5.0, "preset applied")
    # Fit: the tone sits at cur + 1000 Hz.
    ol.click(button="right"); with page.expect_request(is_op("job_retune")) as req: page.click('#ctx-outline .ctx-item[data-action="fit"]')
    body = json.loads(req.value.post_data); assert abs(body["center_hz"] - (cur + 1000)) <= 100 and body["bandwidth_hz"] == 5000
    expect(page.locator("#transcript .line.note", has_text=re.compile(r"^fitted fm_demod to 250\.00\d\d MHz, 5\.0 kHz \(carrier\)$"))).to_have_count(1)
    # Stop.
    wait_until(lambda: job_record(job)["tuning"]["bandwidth_hz"] == 5000, 5.0, "fit applied"); page.evaluate("window.zetaTest.poll()")
    ol.click(button="right"); page.click('#ctx-outline .ctx-item[data-action="stop"]')
    wait_until(lambda: job_record(job)["state"]["status"] == "stopped", 5.0, "stopped from the menu")

def scenario_narrow_long_press_opens_the_outline_menu(page, url, secret):
    fresh_spectrum_jobs(url, secret); page.set_viewport_size(NARROW); login(page, url, secret); page.click('.src-tab[data-id="retune"]')
    wait BRIGHT; job = start_job(passthrough, retune, cur, 10_000); poll
    ol = outline of job; expect(ol.locator(".outline-handle")).to_have_count(0)
    lb = ol.locator(".outline-label").bounding_box(); cx, cy = its centre      # Task 8 replaces the label with the tag and moves this press onto it
    page.mouse.move(cx, cy); page.mouse.down(); page.wait_for_timeout(700); page.mouse.up()
    expect(page.locator("#ctx-outline")).to_be_visible()
    page.keyboard.press("Escape"); expect(page.locator("#ctx-outline")).to_be_hidden()
```

  In this task the narrow long-press target is a retunable outline's `.outline-label` (`pointer-events: auto` under `[data-footprint="narrow"]`); Task 8 moves it to the tag.

- [ ] **Step 2: Run to verify they fail**: `ZETA_UI_ONLY=outline_menu_presets_fit_and_stop,narrow_long_press_opens_the_outline_menu cargo test -p zeta --test ui -- --nocapture` → FAIL (no `#ctx-outline`).
- [ ] **Step 3: Implement** the second menu, the context/long-press hooks, the wiring and the CSS; menu.js header docs gain the outline menu's entry table.
- [ ] **Step 4: Run** the two scenarios plus `what_is_this_classifies,drag_handle_narrows_the_band_and_clamps_at_1_khz,narrow_hides_pane` → PASS.
- [ ] **Step 5: Commit**: `git commit -m "UI: an outline's menu (right-click, or a long press in the narrow footprint) offers Fit to signal, the 2.5/5/6.25/12.5/25 kHz widths about the centre, and Stop"`.

---

### Task 8: Job tags on the waterfall, linked both ways to the job rows (Sonnet)

**Files:**
- Modify: `crates/zeta-ui/web/overlays.js` (tags: `tag(job)`, `untag`, `refreshTag`, `layoutTags`, the snippet from transcript events, `setHot`, hover and click), `crates/zeta-ui/web/agent.js:345-443` (`jobRow`: `dataset.job`, a `.band` cell, hover → `onHot`), `:475-590` (`startJobs` option `onHot`), `crates/zeta-ui/web/app.js:620-636` (`onHot` wiring), `crates/zeta-ui/web/style.css` (`.job-tag`, its `.dot`, `.hot`, `.job .band`)
- Modify: `crates/zeta-ui/tests/e2e/ui.py` (`scenario_rigid_bands_are_outlined` `:1534-1560`, `scenario_channel_plan_labels` `:2426-2460`, `scenario_watch_entry_starts_a_watch_and_outlines_its_band` `:2006-2028`, `scenario_transcript_ribbon_appears` `:2198-2255`, `scenario_narrow_long_press_opens_the_outline_menu`; a new scenario)

**Interfaces:**
- A tag per running rigid job on the shown source (`rigidBand`, the watch included), a child of its outline element so `to_contain_text` on the outline still sees the kind: `<div class="job-tag" data-job="<id>"><span class="kind"/> <span class="short"/> <span class="dot open|closed"/> <span class="channel"/> <span class="snippet"/></div>`; `short` is `job.id.slice(-6)`; the dot is present only for a `SQUELCH_KINDS` job with a known squelch state (`this.squelches`, seeded and live as today; `refreshTag` runs wherever `setSquelchTag` is called in overlays.js), `open` while `open === true`, `closed` while `false`; `channel` is `channelLabel(c, this.plan()).text` or empty; `snippet` is the last `transcript` event text of an attached job whose `input` is this job, `slice(0, SNIPPET_CHARS = 40)` (set in `render` when `p.protocol === "transcript"` and `job.input`). The job outlines no longer get an `.outline-label`; the pin, the capture and the ghost keep theirs. Tags are `position: absolute` at the outline's vertical centre, `right: 6px`, `pointer-events: auto`.
- `layoutTags()`: after placing, sort the tags by their top; a tag whose top is under the previous tag's bottom + 2 px is pushed down to it (at most `MAX_LANES` pushes); run from `layout()` and after any tag text change.
- Linking: `ov.setHot(jobId, on)` toggles `hot` on the job's outline and tag; a tag's or outline's `pointerenter`/`pointerleave` toggles `hot` on `jobRowEl(job)` and records `this.hotJob` so `sync()` re-applies it after agent.js re-renders the rows; a click on a tag calls `row.scrollIntoView({ block: "nearest" })`.
- `agent.js`: `jobRow` sets `row.dataset.job = job.id` beside `dataset.id`, appends a `["band", bandText(job.tuning.center_hz, job.tuning.bandwidth_hz)]` cell after `id` when the tuning is usable, and on `pointerenter`/`pointerleave` calls `onHot(job.id, true/false)` when given; `startJobs(.., { onHot = null })`; `app.js` passes `onHot: (id, on) => state.ov?.setHot(id, on)`.
- CSS: `.job-tag { padding: 0 4px; font-size: 11px; border-radius: 2px; color: var(--accent); background: rgba(11, 14, 20, 0.78); white-space: nowrap; max-width: 60%; overflow: hidden; text-overflow: ellipsis; }`, `.job-tag .dot { display: inline-block; width: 7px; height: 7px; border-radius: 50%; }` `.open { background: var(--ok) }` `.closed { background: var(--dim) }`, `.job-tag .short, .job-tag .snippet { color: var(--dim) }`, `.job.hot { outline: 1px solid var(--accent) }`, `.outline.hot { border-color: var(--ok) }`, `.job-tag.hot { color: var(--ok) }`, `.job .band { color: var(--dim); font-size: 11px }`.

- [ ] **Step 1: Write the failing scenario and update the touched ones** in `ui.py`:

```python
def scenario_tags_show_kind_id_and_link_rows(page, url, secret):
    select_source(... "retune"); wait BRIGHT; cur
    a = start_job("passthrough", "retune", cur, 10_000); b = start_job("fm_demod", "retune", cur + 15_000, 12_500, deviation_hz=5000)   # b is off the tone: squelch closed
    poll; ta = page.locator(f'#waterfall .job-tag[data-job="{a}"]'); tb = ... b
    expect(ta.locator(".kind")).to_have_text("passthrough"); expect(ta.locator(".short")).to_have_text(a[-6:]); expect(ta.locator(".dot")).to_have_count(0)
    expect(tb.locator(".dot.closed")).to_have_count(1, timeout=5_000)
    expect(job_row(page, a).locator(".band")).to_have_text(f"{cur / 1e6:.4f} MHz, 10.0 kHz")
    ta.hover(); expect(page.locator(f'#jobs .job[data-id="{a}"]')).to_have_class(re.compile(r"\bhot\b"))
    page.evaluate("window.zetaTest.poll()"); page.wait_for_timeout(300); expect(row a).to_have_class(re.compile(r"\bhot\b"))   # survives a re-render
    page.mouse.move(5, 5); expect(row a).not_to_have_class(re.compile(r"\bhot\b"))
    job_row(page, b).hover(); expect(tb).to_have_class(re.compile(r"\bhot\b")); expect(outline b).to_have_class(re.compile(r"\bhot\b"))
    finally: stop a, b
```

  `scenario_rigid_bands_are_outlined`: the kind is read from the tag (`.job-tag[data-job=...] .kind`); `scenario_channel_plan_labels`: `label` becomes `page.locator(f'#waterfall .job-tag[data-job="{job}"] .channel')` with `to_have_text("ch20")`, `to_have_text("")` under `plan none`, `"ch20"` after `view reset`; `scenario_watch_entry...`: `expect(el.locator(".job-tag .kind")).to_have_text("watch")`; `scenario_transcript_ribbon_appears`: after the ribbon, `expect(page.locator(f'#waterfall .job-tag[data-job="{demod}"] .snippet')).to_contain_text(SPOKEN[:20])`; `scenario_narrow_long_press_opens_the_outline_menu`: press the tag (`.job-tag[data-job=...]`) instead of the label; `scenario_source_pin_unpin_and_gain_commands` (`:1135-1190`): while pinned, `expect(page.locator('#waterfall .outline[data-key="pin"] .outline-label')).to_have_text("pin")`, `.job-tag` and `.outline-handle` under it have count 0 (Review Focus 5).

- [ ] **Step 2: Run to verify they fail**: `ZETA_UI_ONLY=tags_show_kind_id_and_link_rows,rigid_bands_are_outlined,channel_plan_labels cargo test -p zeta --test ui -- --nocapture` → FAIL (no `.job-tag`).
- [ ] **Step 3: Implement** the tags, stacking, linking, the row cell and hover, the CSS; overlays.js and agent.js header docs describe the tag, the `hot` class and the band cell.
- [ ] **Step 4: Run** the five touched scenarios plus `watch_entry_starts_a_watch_and_outlines_its_band,transcript_ribbon_appears,narrow_long_press_opens_the_outline_menu,quiet_channel_row_is_tagged_squelch,source_pin_unpin_and_gain_commands` → PASS.
- [ ] **Step 5: Commit**: `git commit -m "UI: a tag per rigid job on the waterfall (kind, short id, squelch dot, channel, transcript snippet), stacked, linked by hover with its job row; rows show the band"`.

---

### Task 9: The e2e time budget (Sonnet)

**Files:**
- Modify: `crates/zeta/tests/ui.rs:166-167` (`DEADLINE`)

Decision: **raise the deadline**, not split. One suite keeps one fixture node, one browser and the per-scenario `PASS <name> (<s> s)` accounting the budget is read from; a split would duplicate the nine-source fixture setup for a saving the harness does not need on the field host. The 60 s margin covers plan 15 building concurrently. Fallback, only if the measured total exceeds 420 s: split `ui.py`'s scenarios into two lists run by two `#[test]`s sharing a `fn run_ui(only: &str)` helper, each with its own `ServeProc` and the 300 s deadline, and record that here instead.

- [ ] **Step 1: Measure** the whole suite twice: `cargo test -p zeta --test ui -- --nocapture 2>&1 | tee $CLAUDE_JOB_DIR/tmp/ui-run-1.txt` (then `-2.txt`); the total is the sum of the `PASS <name> (<s> s)` lines (`grep -o '([0-9.]* s)' $CLAUDE_JOB_DIR/tmp/ui-run-1.txt | tr -d '(s) ' | paste -sd+ | bc`). Note the plan-18 scenarios' own cost (the eight added scenarios' lines).
- [ ] **Step 2: Raise** `DEADLINE` to the larger total plus 60 s, rounded up to the next multiple of 30 s, with its doc comment recording the measured totals and the date (`/// Wall-clock limit on the whole ui.py run: measured <T1> s and <T2> s on 2026-11-xx (plan 18 added <n> s), so <D> s.`).
- [ ] **Step 3: Run** `cargo test -p zeta --test ui` once more → PASS inside the new deadline.
- [ ] **Step 4: Commit**: `git commit -m "Tests: the ui e2e deadline is <D> s (measured <T> s with plan 18's scenarios, <n> s of them)"`.

---

### Task 10: Field craft, README, the plan series, TODO, the final gate (Sonnet)

**Files:**
- Modify: `docs/agents/field-craft.md` (a new section "Fitting a demod to a signal" after "Channel plans" `:140-145`, before "Reading a source's health"), `README.md:509-637` (the UI section), `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md:42` (row 18) and a "Plan 18 status" section above "Plan 20 status" `:47`, `TODO.md:69-79`, `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `cargo test -p zeta --test agents_md --test transports`, `cargo test -p zeta-client`

- [ ] **Step 1: Write the field craft**, verbatim:

```markdown
### Fitting a demod to a signal

A box drawn by hand over a 12.5 kHz channel is about 100 kHz wide: 10 dB of extra noise in the audio and a centre a little off the signal. Fit the band to the signal, never to a raster (channels do not sit on a grid; a plan's channel numbers are names, not truth):

1. Measure the box: `job start --kind measure --source S --center-hz C --bandwidth-hz B --then-tail 1,3000`. Read `label.name`, `center_hz` (the occupied band's centre, absolute RF) and `bandwidth_hz` (the occupied width) from the event; an `ssb` label's `evidence` carries the dial frequency (`dial ≈ N Hz`).
2. A `noise` label (or `unknown` under 6 dB SNR) means nothing to fit: start at the box as drawn and say so.
3. Otherwise start the demodulator on the measured centre (the dial for `ssb_demod`) with the occupied width times 1.25, floored at 5000 Hz for `fm_demod` and `am_demod`, 3000 Hz for `ssb_demod`, 150000 Hz for `wfm_demod`, and never wider than the box.
4. A job already running is moved the same way with `job retune --job ID --center-hz C --bandwidth-hz B`: same id, its attached `transcribe` relaunched with it, its squelch settings kept, its channel-plan guard derived again at the new band. Inside the current capture nothing retunes the radio; outside it the radio moves when no other rigid job or pin holds it, else the `conflict` names the holders and `--replace` is the answer when they may stop.

The receiver's DC spike sits at the capture centre. On a local device `measure` ignores the bins near it (plan 15); until that lands, a box that contains the capture centre may fit onto the spike when it outshines the signal, so pin the capture between channels (`source pin`) and box away from its centre.

The web page does this on its own: a demodulator started from a box is fitted first (the transcript says `fitted <kind> to <centre> MHz, <width> kHz (<label>)`, or `no signal found in <lo>-<hi> MHz; started <kind> at the box as drawn`), a running job's outline is dragged by its edges or its body and retuned on release, and its tag's menu offers Fit to signal and the 2.5, 5, 6.25, 12.5 and 25 kHz widths about the current centre.
```

- [ ] **Step 2: Write the README UI prose**: in the menu table (`:577-590`) the "Decode › NBFM audio" row (`:583`) reads "`job_start` `fm_demod` (deviation 5000 Hz) on the box's measured centre and 1.25× its occupied width, floored at 5 kHz; a box with only noise starts as drawn"; a new paragraph after the table: a running demodulator's outline has a handle at each edge (resize) and a body (move); the dragged band is shown with a ghost and a readout and sent as `job_retune` on release, keeping the id, the attached `transcribe` and the ribbons; a conflict snaps it back with the usual retry buttons; right-click (long press on a phone) opens Fit to signal, the width presets and Stop; every rigid job carries a tag (kind, short id, squelch dot, channel, last transcript snippet) linked by hover with its row, and rows show the band.
- [ ] **Step 3: Plan series and TODO**: row 18 (`:42`) gains "**Merged** to `main` pending the session's fast-forward, 2026-11-xx; live acceptance on the desktop HackRF is pending (see "Plan 18 status")". Add "## Plan 18 status" above "## Plan 20 status": what was built (one line per task 1-9, naming the op, the lease move, the carried params and the re-derived guard, the attached relaunch, the CLI/MCP, the fit, the drag, the menu, the tags, the e2e deadline), "Outcomes to record (live)" (the fitted centre's error against the channel centre and the fitted width; whether the DC spike ever wins a fit before plan 15 lands; the drag's feel at 12.5 kHz on a 1 MHz capture), and the acceptance checklist of spec §10 as six unchecked `- [ ]` items, verbatim. In `TODO.md:69-79` replace the "UI: box a signal, demod it, then refine it on the graph" section's body with one line: "Done by plan 18 (`docs/superpowers/specs/2026-10-31-box-and-refine-design.md`): fit first, `job_retune`, draggable outlines, the width presets, tags linked to the rows. Left to plan 15's follow-on: a lone job's capture centre off its channel (the DC spike)."; keep the section heading so cross-references resolve.
- [ ] **Step 4: Regenerate** `AGENTS.md` and the skill; `cargo run -p zeta -- agents-md --check`, `--skill --check`, `cargo test -p zeta --test agents_md --test transports`, `cargo test -p zeta-client` → green.
- [ ] **Step 5: Final gate**: `cargo test --workspace --all-features --no-fail-fast` → no `FAILED`, no `ui e2e skipped`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `cargo fmt --all -- --check`.
- [ ] **Step 6: Commit**: `git commit -m "Docs: plan 18 field craft (fitting a demod to a signal), README UI (fit first, drag to refine, the outline menu, tags), plan series status, TODO"`.

---

## After the plan (not tasks: the session does these)

1. Whole-branch review, then merge `plan-18` into `main` (fast-forward).
2. Live acceptance on the desktop HackRF (spec §10) from a release build made on the field host (`cargo build -p zeta --release`, copied over), UHF CB pinned at 476 918 750 / 1 000 000 Hz, once plan 15's measure DC mask is merged too; record the outcomes in "Plan 18 status".
3. Update the memory file for the HackRF field setup (the page fits and retunes; `zeta job retune` exists).
