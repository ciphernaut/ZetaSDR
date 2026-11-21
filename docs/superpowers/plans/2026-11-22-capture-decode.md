# Replaying a Stored Capture into a Mode's Handler (Plan 44) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** From a stored burst capture, run any mode that fits it through any of its handlers, and show what it decodes with the recording's own times, marked so that none of it passes for live traffic.

**Architecture:** A new op, `capture_decode`, builds the mode row's graph as for a radio of the capture's sample rate and puts the existing `capture_in` stage in front of its `channel`. The job is leaseless (record source `node`), its kind is the mode's id, and it carries `replay: <capture id>` on the record, in the stage context and on every event. Its streams have provenance source `replay`, for which the node keeps one event feed; the page holds that feed open and files its subjects in a group of their own per capture. Explore's capture rows get a Send to menu from a new `capture` surface of the menu entry table.

**Tech Stack:** Rust 1.97 (zeta-core, zeta-dsp, zeta-ext, zeta), the plain-JS page (`crates/zeta-ui/web`), Python Playwright scenarios (`crates/zeta-ui/tests/e2e/ui.py`) started by `crates/zeta/tests/ui.rs`.

**Spec:** `docs/superpowers/specs/2026-11-22-capture-decode-design.md` (the calls, numbered 1 to 21; cited below as "call N"), on `docs/superpowers/specs/2026-11-21-alternative-handlers-design.md` section "Send to: a stored IQ capture". Read both.

## Global Constraints

- One operation set: the op is in `crates/zeta-core/src/ops.rs` and applied by `Node::apply`; transports only translate.
- `AGENTS.md` and `skills/zetasdr/SKILL.md` are generated: prose goes in `crates/zeta/src/agents_md.rs` or `docs/agents/field-craft.md`, then regenerate; never hand-edit.
- A job that is not a replay behaves exactly as before: every existing test passes unchanged except where a task says otherwise (`tests/ops.rs`'s variant count goes from 55 to 56).
- The mark is `replay`, its value the capture id the request named: `JobRecord.replay: Option<String>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`, `StageContext.replay: Option<String>`, and the event member `"replay"`. A record that is not a replay is written without the field.
- A replay's record has `source: "node"` (`NODE_SOURCE`) and `kind` the mode's id; its streams' provenance source is `"replay"` (a new `REPLAY_SOURCE` beside `NODE_SOURCE`). No source may be attached under either name.
- A replay holds no lease, makes no tune, stops no job, resumes never.
- A replay's events reach the `replay` feed and its own streams only: no `events.log` line, no signature superseded, no session, no station tally, no upload.
- Exact strings: `a replay does not resume: run it again`; `a replay uploads nothing`; `a replay's graph is its mode's`; note `replayed <n> capture(s) from <source>: <events> event(s)`.
- Exact values: replay drain bound 8000 ms; packet drain 1 s (the events' `DRAIN`); `lead_ms` 300 with `packets: true`, else 0.
- Public repository: roles not host names, no observed traffic, no real aircraft addresses; fixtures use synthesised signals and lines. `scripts/public-check.sh` and `scripts/check-doc-links.sh` clean before the branch lands.
- Gates per `CLAUDE.md`: `cargo fmt -p <crate> -- --check` per crate (nested worktree), clippy `-D warnings`, a private `CARGO_TARGET_DIR`. Per task run only its tests and neighbours; crate suites once per batch; the workspace and UI suites once at the end. `zeta-core` test runs take `--features test-fixtures`.
- Tests that read state right after an operation poll for the end state (`common::until`, ui.py `wait_until`), never read once.
- Commit messages: `Replay: <what changed>`.

## Review Focus

- A capture of the set expires or is deleted between the request and the launch, or while the replay runs: the op answers `unknown` on `capture`, or the job fails with `capture <id> not found`; nothing is left `running`. (Task 2)
- A program handler that exits late or never (it ignores end of input): the job still ends, `stopped`, within the 8 s bound, with the events and packets it gave before then counted and written. (Task 3)
- A node stopped mid-replay and started again: the record is `stopped` with the no-resume note, and no stage of it runs. (Task 3)
- A live job and a replay of the same mode on the same band at once: the live job's group, its Send to menu, the merged view, the map and `events.log` show nothing of the replay. (Tasks 2, 7)
- A capture too narrow, off-range or of an undividable rate for the chosen mode, asked for through the op directly (the page greys it): refused before anything starts, with the expansion's own message. (Task 2)

---

### Task 1: The mark in the stage context and on events, and `capture_in`'s lead

**Files:**
- Modify: `crates/zeta-core/src/stage.rs` (`StageContext`), `crates/zeta-core/src/modes/summary.rs` (`envelope`), `crates/zeta-dsp/src/util.rs` (`event`), `crates/zeta-dsp/src/capture_in.rs` (`CaptureInParams`, the `segment` event), `crates/zeta-ext/src/external.rs` (`Publisher::publish`), every `StageContext { .. }` literal the compiler names
- Test: unit tests in `summary.rs`, `capture_in.rs`, `external.rs`

**Interfaces:**
- Produces:
  - `StageContext.replay: Option<String>` (default `None` wherever a context is built).
  - `envelope(row, handler, start_ns, fields, replay: Option<&str>)`: one more parameter; with `Some(id)` the envelope has `"replay": id`. `zeta_dsp::util::event` passes `ctx.replay.as_deref()`, as does `Publisher::publish`.
  - `CaptureInParams.lead_ms: u64`, `#[serde(default)]`: a cancellable wait before the first capture's first block (a cancel during it is `Err(Cancelled)`). The `segment` event gains `"replay"` when `ctx.replay` is set.

- [ ] **Step 1: Write the failing tests.**

```rust
// summary.rs
#[test] fn an_envelope_of_a_replay_carries_the_capture()      // envelope(.., Some("01CAP"))["replay"] == "01CAP"
#[test] fn an_envelope_of_a_live_job_has_no_replay_member()   // envelope(.., None).get("replay").is_none()
// capture_in.rs (beside its existing tests, same store helpers)
#[tokio::test] async fn a_lead_delays_the_first_block()       // lead_ms 200: no IQ frame within 150 ms, one within 600 ms; frame times unchanged (recorded)
#[tokio::test] async fn a_cancel_during_the_lead_ends_cancelled()
#[tokio::test] async fn a_replay_context_marks_the_segment_event()   // ctx.replay Some("01CAP"): segment event ["replay"] == "01CAP"; None: absent
// external.rs (beside Publisher's tests)
#[test] fn a_program_event_of_a_replay_carries_the_capture()
```

- [ ] **Step 2: Run them to see them fail.** `cargo test -p zeta-core modes::summary` and `cargo test -p zeta-dsp capture_in` — FAIL (no field `replay`, no `lead_ms`).
- [ ] **Step 3: Add the field, the parameter and `lead_ms`;** fix every literal and call the compiler names with `replay: None` / `None`.
- [ ] **Step 4: Run.** Step 2's commands and `cargo test -p zeta-ext --lib` — PASS.
- [ ] **Step 5: Commit.** `Replay: the stage context and the event envelope carry the capture, and capture_in can wait before its first block`

### Task 2: `capture_decode` starts the mode's graph on the capture

**Files:**
- Modify: `crates/zeta-core/src/ops.rs` (`Op::CaptureDecode`, `Op::name`, the apply arm, schema look), `crates/zeta-core/src/job.rs` (`JobRecord.replay`, `require_format`), `crates/zeta-core/src/node/mod.rs` (`Node::capture_decode`, a shared `capture_set`, `REPLAY_SOURCE`, the feed registered at open, the reserved-name check in the attach path, `duplicate` for replays), `crates/zeta-core/src/node/launch.rs` (`ctx.replay`, the base provenance), `crates/zeta-core/src/stream.rs` only if the `replay` feed needs it, every `JobRecord { .. }` literal the compiler names, `crates/zeta-core/tests/ops.rs` (55 → 56)
- Create: `crates/zeta-core/tests/capture_decode.rs`
- Test: also `job.rs` unit test

**Interfaces:**
- Consumes: Task 1's `StageContext.replay`, `lead_ms`.
- Produces:
  - `Op::CaptureDecode { capture: String, kind: String, handler: Option<String>, packets: bool }`; `handler` nullable and schema-required as `bits_start`'s nullable fields are; `packets` `#[serde(default)]`. Name `capture_decode`; not read-only. Result `OpResult::Job`.
  - `Node::capture_decode(&self, capture: &str, kind: &str, handler: Option<&str>, packets: bool) -> Result<JobId, NodeError>` (the result type `bits_start` has). In this task `packets: true` only adds `needs: ["packet"]` to the handler resolution; Task 5 attaches.
  - `fn capture_set(store: &BurstStore, id: &str) -> Result<Vec<CaptureRecord>, NodeError>`: the named capture's set sorted by `(start_ns, id)`; `bits_start` is changed to use it.
  - `pub const REPLAY_SOURCE: &str = "replay"`; `inner.bus.register_feed(&SourceId(REPLAY_SOURCE))` when the node opens, with no `tee_events`.
  - `JobRecord.replay: Option<String>`.
  - Test helpers in `tests/capture_decode.rs`: `write_aprs(node: &Node, n: usize) -> Vec<String>` (a set of `n` captures of `zeta_dsp::synth::aprs_packet_iq`, each one packet, at a rate and width `aprs_decode`'s row accepts, one `set`, start times 2 s apart from a fixed past instant), `decode(node, capture, kind, handler, packets) -> Result<OpResult, OpError>`, `replay_feed(node) -> Receiver` (subscribed to `bus.feed_of(REPLAY_SOURCE)`).

How `capture_decode` builds the job (calls 2 to 6, 12, 13):

1. `capture_set`; the first record gives centre, width and `sample_rate_hz`.
2. `kind` must be a mode row, else `OpError::Other("<kind> has no handlers")`. Resolve the handler with `start_handler(handler, needs, modes, policy)`; its refusals pass through.
3. Expand the row with `expand_voiced` for tuning (the capture's centre, its width), source rate the capture's, no bank; its errors pass through (`check_band`'s minimum and range, `OutOfRange`).
4. Prepend a node `capture_in` with params `{ "captures": [ids], "loop": false, "realtime": true, "lead_ms": 0 }` and an IQ edge into the graph's `channel` root, as `bits_graph` joins them; `graph::validate`.
5. Duplicate: a running record with the same `replay`, `kind` and resolved handler is `OpError::Duplicate { job, message: "capture <id> is already replaying as <kind> via <handler>: job <job>" }`.
6. The record: `kind` the mode id, `source: NODE_SOURCE`, tuning as in 3, `handler` and `handler_named` as `job_start` writes them, `operator_graph: true`, `sinks: []`, `group: None`, `replay: Some(<the id the request named>)`, context `{"replay": {"capture", "captures", "source", "start_ns", "end_ns"}}` (the set's ids, the first capture's source, the first start and the last end). Launch it as `bits_start` does.

In `launch.rs`: `ctx.replay = rec.replay.clone()`, and the job's base provenance (where it is built, near line 999) has `source: REPLAY_SOURCE` when `rec.replay` is set.

- [ ] **Step 1: Write the failing tests.**

```rust
// tests/capture_decode.rs — node built as tests/bits_start.rs builds its own (radio "rx", register_all)
#[tokio::test] async fn a_capture_replays_into_its_mode_and_the_events_carry_the_recording()
//   ids = write_aprs(&node, 1); feed = replay_feed(&node); Job(id) = decode(ids[0], "aprs_decode", None, false)
//   an event arrives on the feed with ["replay"] == ids[0], ["handler"] set, a "summary",
//   ["t_ns"] within the capture's start_ns..=end_ns, frame.provenance.source == "replay", provenance.job == Some(id)
//   record(id): kind "aprs_decode", source "node", replay Some(ids[0]), operator_graph true; until state Stopped
#[tokio::test] async fn the_whole_set_is_replayed_whichever_capture_is_named()
//   ids = write_aprs(&node, 3); decode(ids[1], ..); three envelope events, t_ns ascending, one inside each capture's span;
//   record.context["replay"]["captures"] == ids (by start time), ["capture"] == ids[1]
#[tokio::test] async fn a_replay_holds_no_lease_and_tunes_nothing()
//   radio.tunes() and the lease list are the same before, during and after
#[tokio::test] async fn a_replay_stays_out_of_the_radios_feed_and_the_event_log()
//   subscribed to feed_of("rx") and with events.log read after the job ends: nothing from the job in either
#[tokio::test] async fn an_unknown_capture_is_refused_with_the_known_ones()        // Err(Unknown { field: "capture", .. })
#[tokio::test] async fn a_kind_that_is_no_mode_row_is_refused()                    // "bits has no handlers"
#[tokio::test] async fn a_mode_the_recording_cannot_hold_is_refused_before_anything_starts()
//   decode(ids[0], "adsb_decode", ..) is Err (the expansion's band error); job_list is empty
#[tokio::test] async fn a_named_handler_runs_the_replay()
//   with common::handlers (MODE, ALT, External stand-in) and a capture MODE's row accepts:
//   decode(cap, MODE, Some(ALT), false): record.handler == Some(ALT), handler_named; stop the job
#[tokio::test] async fn a_second_replay_of_the_same_capture_mode_and_handler_is_a_duplicate()
//   while the first runs (a 3-capture set): Err(Duplicate { job: first, .. }); with another handler: Ok
#[tokio::test] async fn a_source_may_not_be_named_replay()                         // attach of a port with id "replay" is refused as "node" is
// job.rs
#[test] fn a_record_without_replay_loads_and_writes_no_replay_member()
```

- [ ] **Step 2: Run them to see them fail.** `cargo test -p zeta-core --features test-fixtures --test capture_decode` — FAIL (no `Op::CaptureDecode`).
- [ ] **Step 3: Add the op, the record field, `capture_set`, `REPLAY_SOURCE` and the feed;** fix the literals; change `bits_start` to `capture_set`.
- [ ] **Step 4: Implement `Node::capture_decode`, the apply arm and the two `launch.rs` changes.** The op's doc comment states what a replay is, the four fields, each refusal's wording, that it never resumes, and that its events are on the `replay` feed with the recording's times.
- [ ] **Step 5: Run.** Step 2's command, then `cargo test -p zeta-core --features test-fixtures --test bits_start --test graph_start --test ops --test jobs` and `cargo test -p zeta-core job::` — PASS.
- [ ] **Step 6: Commit.** `Replay: capture_decode runs a mode's graph on a stored capture, marked and on its own feed`

### Task 3: A replay ends with what it decoded, in its own time, and never resumes

**Files:**
- Modify: `crates/zeta-core/src/node/launch.rs` (the drain bound, the event count, the end note), `crates/zeta-core/src/node/recover.rs` (`resumable`), `crates/zeta-core/src/node/mod.rs` (the stop at open beside `stop_packet_captures`, the history prune key), `crates/zeta-ext/src/external.rs` (`PacketReader`'s drain)
- Test: `crates/zeta-core/tests/capture_decode.rs`, `crates/zeta-core/tests/resume.rs`, `crates/zeta-ext/tests` (the test file that drives `fake_lines.py` with `--pcap-port`)

**Interfaces:**
- Consumes: Task 2's record and helpers.
- Produces:
  - `const REPLAY_DRAIN_TIMEOUT_MS: u64 = 8000` beside `DRAIN_TIMEOUT_MS`, used when `rec.replay` is set.
  - At the end of a replay that did not fail: `context.replay.events = <count>` and `context.note = "replayed <n> capture(s) from <source>: <events> event(s)"`, written through `context_merge` before the record is finished. Counted: frames on the job's event outputs whose payload has both `"replay"` and `"summary"`.
  - `pub(crate) const REPLAY_NO_RESUME_NOTE: &str = "a replay does not resume: run it again"`; `resumable` is false for a record with `replay`; at open every `Running` record with `replay` is finished `Stopped` with that note.
  - The history prune key includes `replay`.
  - `PacketReader`: when the run's input is done, it keeps reading until the program closes the socket or `DRAIN` (1 s) passes, and only then is dropped; the abort stays for a cancelled run.

- [ ] **Step 1: Write the failing tests.**

```rust
// tests/capture_decode.rs
#[tokio::test] async fn a_finished_replay_notes_what_it_decoded()
//   3-capture set: until Stopped; context["replay"]["events"] == 3;
//   context["note"] == "replayed 3 capture(s) from rx: 3 event(s)"
#[tokio::test] async fn a_replay_that_decodes_nothing_says_so()
//   a capture of noise: note ends ": 0 event(s)", state Stopped
#[tokio::test] async fn a_handler_that_never_exits_is_given_eight_seconds_and_the_job_still_stops()
//   common::handlers::External (ends only on cancel): Stopped between 2.5 s and 10 s after capture_in's end
#[tokio::test] async fn two_replays_of_one_capture_keep_one_finished_record_per_handler()
//   run twice with None, once with Some(ALT): job_list has two finished records for the capture
// tests/resume.rs
#[tokio::test] async fn a_replay_found_running_is_stopped_with_a_note_and_not_resumed()
//   persist a Running record with replay Some(..) (as the packets test there does), open the node:
//   state Stopped, context["note"] == "a replay does not resume: run it again", no stream of the job on the bus
// zeta-ext
#[tokio::test] async fn packets_written_as_the_input_ends_are_still_read()
//   fake_lines.py with --pcap-port and --pcap-interval-ms so its last packets leave after stdin's EOF:
//   every packet of the file is published
```

- [ ] **Step 2: Run them to see them fail.** `cargo test -p zeta-core --features test-fixtures --test capture_decode --test resume -- replay` and the zeta-ext test by name — FAIL.
- [ ] **Step 3: Implement** the drain bound, the count and note, `resumable` and the stop at open, the prune key, the packet drain.
- [ ] **Step 4: Run.** Step 2's commands, then `cargo test -p zeta-core --features test-fixtures --test bits_start --test attach --test resume` and `cargo test -p zeta-ext` — PASS. Remove the "Packets still in a program's socket" line from `TODO.md`'s plan 42 follow-ups.
- [ ] **Step 5: Commit.** `Replay: a replay ends with a note of what it decoded, waits for its decoder, and never resumes`

### Task 4: A replay proves nothing live

**Files:**
- Modify: `crates/zeta-core/src/node/propose.rs` (`on_frame`'s supersede, `keeps_session`), `crates/zeta-core/src/node/launch.rs` (`tee_rules` not started for a replay), `crates/zeta-ext/src/sink_sondehub.rs` (`run`), `crates/zeta-core/src/node/mod.rs` or `ops.rs` (the `graph_put` refusal)
- Test: `crates/zeta-core/tests/capture_decode.rs`, `sink_sondehub.rs` unit test

**Interfaces:**
- Consumes: `JobRecord.replay`, `StageContext.replay`.
- Produces: no new names. `graph_put` on a job with `replay` is `OpError::Other("a replay's graph is its mode's")`. `SondehubStage::run` with `ctx.replay` set fails at once with `a replay uploads nothing`, before any request.

- [ ] **Step 1: Write the failing tests.**

```rust
// tests/capture_decode.rs
#[tokio::test] async fn a_replay_supersedes_no_signature()
//   a learned signature on the capture's band (as tests/proposals.rs makes one), then a replay that decodes:
//   signature_list shows it unchanged after the job stops
#[tokio::test] async fn a_replay_keeps_no_session_and_tallies_no_station()
//   the sessions and stations the node holds are the same before and after
#[tokio::test] async fn graph_put_of_a_replay_is_refused()
// sink_sondehub.rs
#[tokio::test] async fn the_sink_refuses_a_replay()            // run() Err whose message is "a replay uploads nothing"; the fake server saw no request
```

- [ ] **Step 2: Run them to see them fail.** `cargo test -p zeta-core --features test-fixtures --test capture_decode -- supersedes session graph_put` and `cargo test -p zeta-ext sink_sondehub` — FAIL.
- [ ] **Step 3: Implement the four gates,** each one `if rec.replay.is_some()` / `ctx.replay.is_some()` at the place the Files list names.
- [ ] **Step 4: Run.** Step 2's commands and `cargo test -p zeta-core --features test-fixtures --test proposals --test origin_ops` — PASS.
- [ ] **Step 5: Commit.** `Replay: a replay supersedes, tallies and uploads nothing, and its graph cannot be replaced`

### Task 5: `packets: true` captures from the first sample

**Files:**
- Modify: `crates/zeta-core/src/node/mod.rs` (`Node::capture_decode`)
- Test: `crates/zeta-core/tests/capture_decode.rs`

**Interfaces:**
- Consumes: `lead_ms`, the node's own `job_attach` of `packets` (the function the `JobAttach` arm calls), `capture_decode` from Task 2.
- Produces: with `packets: true`, `capture_in`'s `lead_ms` is 300 and the node attaches `packets` to the new job before returning. A failed attachment stops the replay, waits for it to end, and is the op's error. With every handler of the mode giving no packets the refusal is `start_handler`'s `outputs no packet` wording, and nothing starts.

- [ ] **Step 1: Write the failing tests.**

```rust
#[tokio::test] async fn a_replay_with_packets_captures_every_frame()
//   write_aprs(&node, 3) (aprs_decode's own decoder gives packets): decode(.., None, true);
//   until the replay is Stopped and the packet capture closed: packets_list shows one capture of the job with 3 packets,
//   each packet's time inside a capture's span
#[tokio::test] async fn packets_on_a_mode_that_gives_none_is_refused_and_nothing_starts()
#[tokio::test] async fn a_replay_without_packets_has_no_lead()   // the first IQ frame within 200 ms of the answer
```

- [ ] **Step 2: Run them to see them fail.** `cargo test -p zeta-core --features test-fixtures --test capture_decode -- packets lead` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Step 2's command and `cargo test -p zeta-core --features test-fixtures --test attach` — PASS.
- [ ] **Step 5: Commit.** `Replay: capture_decode with packets captures from the recording's first sample`

### Task 6: The CLI, the agent's prose, the changelog

**Files:**
- Modify: `crates/zeta/src/cli.rs` (`CaptureCmd::Decode`, `mutates`, `into_op`), `crates/zeta/src/main.rs` (`starts_job`), `crates/zeta/src/agents_md.rs` (the op's CLI form, hints), `crates/zeta-agent/src/agent.rs` (`OwnJobs::record`), `docs/agents/field-craft.md`, `CHANGELOG.md`
- Regenerate: `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta/tests/cli.rs`, `cli.rs` unit tests

**Interfaces:**
- Consumes: `Op::CaptureDecode`.
- Produces: `zeta capture decode --capture <id> --kind <mode> [--handler <h>] [--packets]`, `--kind` parsed as a plain string (a mode id), printing the job as `bits start` does; it works with `--node`. The agent's own-jobs record takes the op's job.

Prose (call 15), in `field-craft.md` under the handlers section: what a replay is for, that it proves nothing live (no signature, session, station or upload), that its events are on the `replay` feed and carry `replay` and the recording's `t_ns`, that it never resumes, and that `packets: true` is the way to get a capture file from one. `CHANGELOG.md`: the op, the CLI form, the `replay` feed and reserved name, the packet drain fix.

- [ ] **Step 1: Write the failing tests.**

```rust
// cli.rs unit
#[test] fn capture_decode_parses_to_its_op()   // `capture decode --capture C --kind aprs_decode` -> CaptureDecode { capture: "C", kind: "aprs_decode", handler: None, packets: false }
#[test] fn capture_decode_takes_a_handler_and_packets()
#[test] fn capture_decode_mutates()
// tests/cli.rs
#[test] fn capture_decode_of_an_unknown_capture_names_the_field()   // a one-shot against an empty state dir: exit non-zero, stderr contains "capture"
```

- [ ] **Step 2: Run them to see them fail.** `cargo test -p zeta capture_decode` — FAIL.
- [ ] **Step 3: Implement, write the prose, regenerate** (`cargo run -q -p zeta -- agents-md > AGENTS.md`, `... --skill > skills/zetasdr/SKILL.md`).
- [ ] **Step 4: Run.** Step 2's command, `cargo test -p zeta-client --test tools`, `cargo test -p zeta-agent`, both `agents-md --check` commands — PASS.
- [ ] **Step 5: Commit.** `Replay: the CLI's capture decode, and the agent's notes`

### Task 7: The page hears the replay feed and groups a capture's subjects

**Files:**
- Modify: `crates/zeta-ui/web/overlays.js` (the `replay` feed; `syncPositioned`, `rigidBand`), `crates/zeta-ui/web/app.js` (the event wiring near line 1382), `crates/zeta-ui/web/subjects.js` (`groupId`, `sync`, the section title and its close, times), `crates/zeta-ui/web/map.js` (`event`), `crates/zeta-ui/web/actions.js` (`groupHandlers`), `crates/zeta-ui/web/agent.js` (the job row), `crates/zeta-ui/web/style.css`
- Test: `crates/zeta-ui/tests/e2e/ui.py`, `crates/zeta/tests/ui.rs` only if a fixture is needed

**Interfaces:**
- Consumes: the `replay` feed (a `/streams` entry of kind `event`, `provenance.job === null`, `provenance.source === "replay"`), events' `replay` and `t_ns`, records' `replay` and `context.note`.
- Produces:
  - `overlays`: `openReplayFeed()` at mount and after a reconnect, using `findFeed("replay")`; its messages call the same `onEvent(job, p)` listeners and draw nothing. The job is looked up by `provenance.job` in the page's job list, fetched once more when unknown.
  - `subjects.groupId(job)`: a job whose record has `replay` is in group `replay:<capture id>`, before any other rule.
  - `store.closeGroup(id)`; replay groups are exempt from `sync`'s "no job runs" drop and from the dim and drop clock; their `first_seen`/`last_seen` are `p.t_ns` in ms.
  - A replay group's section: `section.subject-replay[data-capture="<id>"]`, title `replay of <HH:MM:SS> <centre> MHz` from the capture record (the capture id when the record is gone), a `button.close` that calls `closeGroup`.
  - The job row: source cell `replay` for a record with `replay`; a finished replay shows `context.note`.
- A replay job is skipped by `map.event`, `groupHandlers`, the cross-job merged view (`parts()`), `rigidBand` and `syncPositioned`.

- [ ] **Step 1: Write the failing scenarios** in ui.py, in the shard the Send to scenarios run in. A helper `replay_capture(url, secret, source, kind, handler=None, packets=False)` makes a set with `two_captures_of` and applies `capture_decode`.

```python
def a_replay_lists_its_subjects_as_a_group_of_its_own(page, url, secret):
    # a set captured from the `aprs` file source, replayed as aprs_decode:
    # section.subject-replay[data-capture=<id>] appears with a row for the fixture's callsign;
    # its title starts "replay of"; no ribbon is drawn (the ribbon count is unchanged); the map has no new track
def a_replay_group_outlives_its_job_and_closes_on_its_button(page, url, secret):
    # wait until the job is stopped: the section and its row are still there after two polls; click button.close: gone
def two_handlers_replaying_one_capture_merge_and_mark(page, url, secret):
    # fake_pair on a capture of at least 3 s (the fake handlers print after 384000 bytes), replayed with
    # handler fake_lines and then alt: one section, one row per subject, .mark-1/.mark-2 present, the legend names both
def a_finished_replay_row_shows_its_note(page, url, secret):
    # the job row's source cell reads "replay"; once stopped its state cell text starts "replayed "
def a_replay_is_no_sibling_of_a_live_job(page, url, secret):
    # a live aprs_decode job on the capture's band and a replay of it: the live job's menu Send to lists the same
    # handlers as before the replay, and the live Subjects section has no row from the replay
```

- [ ] **Step 2: Run them to see them fail.** `ZETA_UI_ONLY=a_replay_lists_its_subjects_as_a_group_of_its_own,a_replay_group_outlives_its_job_and_closes_on_its_button,two_handlers_replaying_one_capture_merge_and_mark,a_finished_replay_row_shows_its_note,a_replay_is_no_sibling_of_a_live_job cargo test -p zeta --test ui -- --nocapture` — FAIL (check the run did not print `ui e2e skipped`).
- [ ] **Step 3: Implement** the feed, the group, the exclusions and the row.
- [ ] **Step 4: Run.** Step 2's command, plus `ZETA_UI_ONLY` with the Send to and Subjects scenarios that neighbour the change (`send_to_starts_a_sibling` and the group scenarios after it) — PASS.
- [ ] **Step 5: Commit.** `Replay: the page hears the replay feed and lists a capture's subjects as their own group`

### Task 8: Explore's Send to

**Files:**
- Modify: `crates/zeta-ui/web/actions.js` (`GROUPS.capture`, the `capture` surface's entries), `crates/zeta-ui/web/menu.js` (a popup opened at an element for a `capture` context; `host.decodeCapture`; `modeRows` shared), `crates/zeta-ui/web/explore.js` (the button, its wiring), `crates/zeta-ui/web/app.js` (passing the menu to Explore), `crates/zeta-ui/web/style.css`
- Test: `crates/zeta-ui/tests/e2e/ui.py`

**Interfaces:**
- Consumes: `capture_list` records (`id`, `center_hz`, `bandwidth_hz`, `sample_rate_hz`), `modes_list` rows through the menu's `modeRows()`, `rowFits`/`rowMeets`, `decoderHandlers`' shape, Task 7's group.
- Produces:
  - `button[data-act="sendto"]` labelled `Send to ›` in each `tr.capture`, opening `menu.openCapture(anchorEl, capture)`.
  - Surface `capture` in the entry table, one generated entry per ready mode row, `data-action="replay"`, `data-mode="<mode id>"`:
    - a row that fits and has fewer than two ready handlers and none giving packets: plain, runs `host.decodeCapture(capture, mode, null, false)`;
    - otherwise a submenu: `<pick> (policy)`, each handler (`data-handler`; not ready is greyed with `title` its reason), and for each ready handler whose outputs include `packet` a `<name> with packets` entry (`data-packets="1"`);
    - rows that do not fit: under one `elsewhere ›` entry (`data-action="replay_elsewhere"`), each greyed with `title` `needs <min width>, the recording is <width>` or `works <range>`.
  - `host.decodeCapture(capture, mode, handler, packets)`: applies `capture_decode`, notes `replaying <mode> from the capture` on success and the op's message on a refusal, and selects the Subjects pane when the mode has a subject.

- [ ] **Step 1: Write the failing scenarios.**

```python
def explore_send_to_lists_the_modes_that_fit(page, url, secret):
    # a capture from the `aprs` source: Send to › opens a popup; [data-mode="aprs_decode"] is first level and enabled;
    # [data-action="replay_elsewhere"] opens a submenu where [data-mode="adsb_decode"] is disabled with a title starting "needs "
def explore_send_to_replays_through_the_chosen_handler(page, url, secret):
    # aprs_decode's submenu lists "(policy)" and `alt`; choosing alt applies capture_decode with
    # body["capture"] == id, body["kind"] == "aprs_decode", body["handler"] == "alt", body["packets"] is False
def explore_send_to_with_packets_lists_a_packet_capture(page, url, secret):
    # the "with packets" entry: body["packets"] is True; Explore's packet captures gain a row for the replay's job
def explore_send_to_shows_a_refusal(page, url, secret):
    # the capture deleted by an op after the popup opened: choosing a mode notes the op's message; no job starts
```

Add `menu_entry_table_is_whole`'s expectations for the new surface in the same step.

- [ ] **Step 2: Run them to see them fail.** `ZETA_UI_ONLY=<the four>,menu_entry_table_is_whole cargo test -p zeta --test ui -- --nocapture` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Step 2's command, plus the Explore scenarios (`capture_bursts_from_the_box_menu_and_explore_lists_it`, `explore_what_is_this_and_delete`, `typing_survives_the_explore_poll`, `work_the_bits_shows_a_stream_and_a_sync_hit`) — PASS.
- [ ] **Step 5: Commit.** `Replay: Explore's Send to runs a capture through a mode's handler`

### Task 9: The acceptance checks, the recipe, the open work

**Files:**
- Modify: `UAT.md` (plan 44), `RECIPES.md`, `TODO.md`, `docs/superpowers/specs/2026-11-21-alternative-handlers-design.md` only to point delivery item 3 at this plan

- [ ] **Step 1: `UAT.md`, plan 44,** unticked, against the field host's radio:
  - 44.1 a burst capture of APRS replayed from Explore: the subject appears under `replay of …` with the recording's time;
  - 44.2 a capture of aircraft replayed through dump1090 and through OnAir: one group, marked;
  - 44.3 the same with packets: the capture opens in Wireshark with the recording's times;
  - 44.4 a replay running beside the live decoder of the same band: the live map and `events.log` show nothing of it;
  - 44.5 the node restarted mid-replay: the row reads the no-resume note;
  - 44.6 a mode that does not fit is greyed with its reason, and refused by the CLI with the same reason.
- [ ] **Step 2: `RECIPES.md`:** "Check a decoder against a recording": capture, `zeta capture decode`, read the note, compare two handlers.
- [ ] **Step 3: `TODO.md`:** plan 44's line becomes what was built and what is left (the spec's "Not in this plan" list, one line each).
- [ ] **Step 4: Gates,** once, on the head: the list in `CLAUDE.md`, with a private `CARGO_TARGET_DIR`.
- [ ] **Step 5: Commit.** `Replay: the acceptance checks, a recipe, and what is left`
