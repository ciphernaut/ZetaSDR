# Source Event Feed Implementation Plan (plan 27)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One `event` stream per source (its feed) carries every event frame of every job on that source, and the page watches the shown source through that one socket instead of four per-job sockets, so 39 CB transcribers all draw ribbons, counters and squelch dots.

**Architecture:** `StreamBus` gains a feed table (source id to feed stream id) and an unfed set; `publish` of a fed `Event` frame whose source has a feed also sends a clone on the feed with `stream` rewritten and provenance intact. `Node::add_port` registers a feed per source; `launch.rs` registers the `fft` node's streams and the scan's row streams unfed. `overlays.js` replaces `MAX_WATCHED`, the per-job sockets and `resolve` with one `api.streamText` on the shown source's feed, rendering each frame by its `job` from the last poll, holding frames for unknown jobs in a bounded pending list, and re-seeding every squelch dot after a `lagged` or a reconnect.

**Tech Stack:** Rust (zeta-core bus and node, zeta-ws axum websocket tests with tokio-tungstenite), vanilla ES modules in `crates/zeta-ui/web`, Playwright scenarios in `crates/zeta-ui/tests/e2e/ui.py` driven by `crates/zeta/tests/ui.rs` (`ZETA_UI_ONLY=a,b`).

**Spec:** `docs/superpowers/specs/2026-11-05-source-event-feed-design.md` (read it first; section numbers below refer to it).

## Global Constraints

- `StreamInfo` gets no new field (spec 3): a feed is recognised by `kind == event`, `provenance.job == null` and `provenance.source != "node"`.
- Never fed: every stream registered for an edge whose `from` node has kind `fft`, every job output stream `node_outputs` makes for an `fft` node, the scan's `out` stream in `launch_scan`, and the per-step `output` stream in `fft_step`. Frames whose `provenance.source` is `node` (`NODE_SOURCE`) reach no feed. A frame published with a feed's own id is dropped and counted in `StreamBus::dropped`.
- The feed clone keeps `kind`, `provenance` (job, channel, span) and `payload` unchanged; only `stream` is the feed's id.
- The page's sockets with the CB plan running: ops, waterfall spectrum, feed = 3; `GET /health` `ws_connections` is the measure in e2e.
- At most 4 `job_context` seeds in flight (`SEED_IN_FLIGHT = 4` in `overlays.js`); the pending list holds at most 200 frames (`PENDING_MAX = 200`), oldest dropped.
- Ops doc text changes regenerate `AGENTS.md` (`cargo run -p zeta -- agents-md > AGENTS.md`) and `skills/zetasdr/SKILL.md` (`cargo run -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md`); `cargo test -p zeta --test agents_md` must pass.
- Gates per task: `cargo fmt -p <each touched crate> -- --check` (per crate; the nested worktree breaks `--all`), `cargo clippy --workspace --all-targets --all-features -- -D warnings`, the task's tests, and the task's scenarios via `ZETA_UI_ONLY=<names> cargo test -p zeta --test ui`. Use a private `CARGO_TARGET_DIR` for the gates so parallel sessions do not clobber each other.
- e2e traps (from the "UI e2e gotchas" note): `page.wait_for_function("() => …")` only, never a bare expression (CSP); the `cb` fixture plays 48 kHz, ch59, ch20 and ch60 only, distinct bandwidths avoid `duplicate`; `channel_plans` answers at `["value"]["plans"]`; the whole suite must stay under its 600 s deadline (about 474 s today).
- Temp files under `$CLAUDE_JOB_DIR/tmp`, never `/tmp`. Commits on the plan's branch, one per task, with the message the task names. No merge and no deploy in this plan.

## Review Focus

1. **A watch's rows.** A `watch` job carries 20 fft rows a second on an `fft → detect` edge stream, not a job output. If only `node_outputs` marks fft streams unfed, the feed is flooded. Pinned in Task 2 (`a_watch_graphs_fft_edge_is_unfed_but_its_signal_events_are_fed`).
2. **A job started between two polls.** Its first frames arrive before the page knows the job; dropped, a decoder's only lines are gone. Pinned in Task 6 (`scenario_frame_before_poll_is_drawn_after_it`).
3. **A lag or reconnect on the feed.** Every squelch dot on the page can be stale at once, for as long as an idle channel stays idle. Pinned in Task 7 (`scenario_feed_lag_reseeds_squelch_dots`).
4. **An older node.** A page updated before the node finds no feed for the source; it must say so once and show outlines, not sit silent. Pinned in Task 5 (`scenario_no_feed_is_noted_once`).
5. **A source added after `open`.** Tests and the node attach ports after `open`; a feed registered only in `open` misses them. Pinned in Task 3 (`a_port_added_after_open_has_a_feed`).

---

### Task 1: The bus fans event frames into a per-source feed

**Files:**
- Modify: `crates/zeta-core/src/stream.rs` (`StreamBus` at line 128; tests module at the end)

**Interfaces:**
- Produces: `StreamBus::register_feed(&self, source: &SourceId) -> StreamId` (registers an `Event` stream with provenance `{source, job: None, channel: None, span: 0..0}`, records it in the feed table, idempotent: a second call for the same source returns the existing id); `StreamBus::feed_of(&self, source: &SourceId) -> Option<StreamId>`; `StreamBus::register_unfed(&self, kind: StreamKind, provenance: Provenance) -> StreamId` (as `register`, and the id is added to the unfed set); `register` unchanged (fed). `publish` fan-in as in the Global Constraints. `unregister` removes an id from the unfed set too.

- [ ] **Step 1: Write the failing tests** in `stream.rs`'s `tests` module, beside `subscriber_receives_published_frame` (the `prov(src)` helper is there; add `job_prov(src, job)` setting `job: Some(JobId(job.into()))`):

```rust
#[tokio::test]
async fn feed_carries_every_event_of_a_source_with_its_job() {
    let bus = StreamBus::new(8);
    let feed = bus.register_feed(&SourceId("a".into()));
    let mut rx = bus.subscribe(&feed).unwrap();
    let s1 = bus.register(StreamKind::Event, job_prov("a", "j1"));
    let s2 = bus.register(StreamKind::Event, job_prov("a", "j2"));
    let mut own = bus.subscribe(&s1).unwrap();
    bus.publish(Frame::new(s1.clone(), job_prov("a", "j1"), Payload::Event(json!({"n": 1}))));
    bus.publish(Frame::new(s2.clone(), job_prov("a", "j2"), Payload::Event(json!({"n": 2}))));
    let f1 = rx.recv().await.unwrap();
    let f2 = rx.recv().await.unwrap();
    assert_eq!((f1.stream.clone(), f1.provenance.job.clone().unwrap().0.as_str()), (feed.clone(), "j1"));
    assert_eq!((f2.stream.clone(), f2.provenance.job.clone().unwrap().0.as_str()), (feed.clone(), "j2"));
    assert_eq!(f1.payload, Payload::Event(json!({"n": 1})));
    // The job's own stream still carries the frame, under its own id.
    assert_eq!(own.recv().await.unwrap().stream, s1);
}

#[tokio::test]
async fn feed_skips_unfed_streams_other_sources_audio_and_node() {
    let bus = StreamBus::new(8);
    let feed = bus.register_feed(&SourceId("a".into()));
    let mut rx = bus.subscribe(&feed).unwrap();
    let unfed = bus.register_unfed(StreamKind::Event, job_prov("a", "fft"));
    let other = bus.register(StreamKind::Event, job_prov("b", "j"));
    let audio = bus.register(StreamKind::Audio, job_prov("a", "j"));
    let node = bus.register(StreamKind::Event, job_prov("node", "mix"));
    let fed = bus.register(StreamKind::Event, job_prov("a", "j"));
    bus.publish(Frame::new(unfed, job_prov("a", "fft"), Payload::Event(json!({"q": [1]}))));
    bus.publish(Frame::new(other, job_prov("b", "j"), Payload::Event(json!({"n": 1}))));
    bus.publish(Frame::new(audio, job_prov("a", "j"), Payload::Audio { sample_rate_hz: Hz(48_000), channels: 1, samples: Arc::from([0.0f32]) }));
    bus.publish(Frame::new(node, job_prov("node", "mix"), Payload::Event(json!({"n": 1}))));
    bus.publish(Frame::new(fed, job_prov("a", "j"), Payload::Event(json!({"n": 2}))));
    let f = rx.recv().await.unwrap();
    assert_eq!(f.payload, Payload::Event(json!({"n": 2})));
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn feed_is_registered_once_and_never_written_directly() {
    let bus = StreamBus::new(8);
    let feed = bus.register_feed(&SourceId("a".into()));
    assert_eq!(bus.register_feed(&SourceId("a".into())), feed);
    assert_eq!(bus.feed_of(&SourceId("a".into())), Some(feed.clone()));
    assert_eq!(bus.feed_of(&SourceId("b".into())), None);
    let mut rx = bus.subscribe(&feed).unwrap();
    bus.publish(Frame::new(feed.clone(), prov("a"), Payload::Event(json!({"n": 1}))));
    assert_eq!(bus.dropped(), 1);
    assert!(rx.try_recv().is_err());
    let info = bus.list().into_iter().find(|i| i.id == feed).unwrap();
    assert_eq!((info.kind, info.provenance.job, info.provenance.source.0.as_str()), (StreamKind::Event, None, "a"));
}
```

`job_prov` needs `JobId` from `crate::types`; `Hz` and `Arc` are already used by `publish_refuses_audio_with_zero_channels`.

- [ ] **Step 2: Run to verify they fail:** `cargo test -p zeta-core feed_` → FAIL (`register_feed` not found).

- [ ] **Step 3: Implement** on `StreamBus`: fields `feeds: RwLock<HashMap<SourceId, StreamId>>` and `unfed: RwLock<HashSet<StreamId>>`; `register_feed`, `feed_of`, `register_unfed` with the signatures above; in `publish`, after the existing kind and payload checks and before `tx.send`, when `frame.kind == StreamKind::Event`: if the frame's stream is itself a feed id, count it in `dropped` and return; else if the stream is not in `unfed`, the source is not `"node"` and `feeds` has the source, send `Frame { stream: feed_id, ..frame.clone() }` on the feed's sender (a send with no subscribers is dropped silently, as today). Take the read locks once per publish. `unregister` also removes from `unfed`. Module doc: one sentence on feeds.

- [ ] **Step 4: Run** `cargo test -p zeta-core stream::` → PASS (the new three and the existing bus tests).

- [ ] **Step 5: Commit:** `git add crates/zeta-core/src/stream.rs && git commit -m "Bus: one event feed per source, fed by publish with provenance intact; fft and scan rows registered unfed"`

---

### Task 2: The node registers fft and scan row streams unfed

**Files:**
- Modify: `crates/zeta-core/src/node/launch.rs:250-254` (edge registration), `:299-303` (`node_outputs`' `fresh` closure), `:820` (`launch_scan`'s `out`), `:1360` (`fft_step`'s `output`)
- Test: `crates/zeta-core/tests/ops_stream.rs` (its `open(dir)` node and `next(rx)` helpers; add the tests there)

**Interfaces:**
- Consumes: Task 1's `register_unfed`, `register_feed`, `feed_of`.
- Produces: nothing new; the rule of the Global Constraints holds for every launched graph.

- [ ] **Step 1: Write the failing tests** in `ops_stream.rs`. They need a feed on the test source: until Task 3 lands, the test registers it itself with `node.bus().register_feed(&SourceId("tone".into()))` right after `open` (Task 3 makes that call a no-op). Use the file's existing way of starting jobs (look at `every_apply_is_logged_with_outcome` for `node.apply(Op::JobStart {..})`; the `tone` fixture source of `cfg(dir)`):

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spectrum_jobs_rows_never_reach_the_feed_but_a_measure_event_does() {
    // Start a `spectrum` job and a `measure` job on `tone`; subscribe to the feed; collect
    // frames for 2 s. Assert: at least one frame whose provenance.job is the measure job's
    // id and whose payload has `label` and `center_hz`; no frame whose payload has a `q`
    // or `db` array; no frame whose provenance.job is the spectrum job's id.
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watch_graphs_fft_edge_is_unfed_but_its_signal_events_are_fed() {
    // Start a `watch` job on the `tone` fixture band (centre and width as the cb_watch or
    // resume tests start one; the tone is a steady carrier the detector reports as `signal`
    // after persist_ms: lower `persist_ms` with graph_put is not needed if the test waits
    // 4 s). Collect feed frames for 5 s. Assert: no frame with a `q` array; at least one
    // frame with `protocol == "signal"` whose provenance.job is the watch's id.
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scans_rows_never_reach_the_feed() {
    // Start a `scan` on `tone` wide enough for two steps (the retune-capable fixture of
    // `resume.rs` or `cli.rs` shows a working scan start; a one-step scan is fine too).
    // Collect feed frames for 2 s. Assert: no frame whose provenance.job is the scan's id.
}
```

Write the bodies from those comments with the file's helpers; the assertions are the contract.

- [ ] **Step 2: Run to verify they fail:** `cargo test -p zeta-core --test ops_stream feed` → FAIL (rows reach the feed).

- [ ] **Step 3: Implement** in `launch.rs`: at the edge registration (line 250) look up the `from` node's kind in `graph.nodes` and call `register_unfed` when it is `"fft"`, else `register`; in the `fresh` closure (line 299) the same by `n.kind == "fft"`; in `launch_scan` (line 820) and `fft_step` (line 1360) register the `Event` stream with `register_unfed`. A one-line helper `fn register_event_stream(bus, from_kind: &str, kind, prov)` is fine if it keeps the four sites to one rule.

- [ ] **Step 4: Run** `cargo test -p zeta-core --test ops_stream` and `cargo test -p zeta-core` → PASS.

- [ ] **Step 5: Commit:** `git add crates/zeta-core/src/node/launch.rs crates/zeta-core/tests/ops_stream.rs && git commit -m "Launch: fft edge and output streams and the scan's row streams are registered unfed"`

---

### Task 3: A feed per source from `add_port`, listed by `stream_list`

**Files:**
- Modify: `crates/zeta-core/src/node/mod.rs:969-990` (`add_port`), `crates/zeta-core/src/ops.rs:355-360` (`StreamList` doc text)
- Modify: `AGENTS.md`, `skills/zetasdr/SKILL.md` (regenerated)
- Test: `crates/zeta-core/tests/ops_stream.rs`, `crates/zeta/tests/attach.rs`

**Interfaces:**
- Consumes: Task 1's `register_feed`.
- Produces: every source attached by `add_port` has a feed for the node's life; `stream_list` lists it.

- [ ] **Step 1: Write the failing tests.** In `ops_stream.rs`, beside `ops_stream_is_listed_with_node_provenance`:

```rust
#[tokio::test]
async fn stream_list_has_one_feed_per_source_and_a_job_adds_none() {
    // open(dir) attaches `tone`. Apply Op::StreamList: exactly one entry with kind Event,
    // provenance.job None and provenance.source "tone"; exactly one with source "node" and
    // job None (ops). Start a measure job on tone, list again: still exactly one such
    // `tone` entry, and its id is unchanged.
}

#[tokio::test]
async fn a_port_added_after_open_has_a_feed() {
    // open(dir), then node.add_port(a second FileSource port with id "second", as
    // crates/zeta-core/tests/bank.rs:172 does). Assert node.bus().feed_of(&"second") is
    // Some and stream_list shows it. Remove the explicit register_feed from Task 2's tests
    // now that add_port does it.
}
```

In `attach.rs`, extend `attach_transcribe_emits_transcript_events`: after the transcript arrives on the job's own streams, assert the same event (`protocol == "transcript"`) is received on `node.bus().feed_of(&record.source)` (subscribe to the feed before attaching) with `provenance.job == Some(attached id)`.

- [ ] **Step 2: Run to verify they fail:** `cargo test -p zeta-core --test ops_stream feed && cargo test -p zeta --test attach attach_transcribe_emits` → FAIL.

- [ ] **Step 3: Implement.** In `add_port`, after the port is inserted: `inner.bus.register_feed(&source);` (the bus is `inner.bus`; see `launch.rs` uses). Replace the `StreamList` doc text (`ops.rs:355`) with:

```
/// List the streams: id, kind and provenance. One is always the node's own `ops` event
/// stream (provenance source `node` and no `job`; a mix's streams have source `node` too,
/// with their `job`): one event per applied operation with its outcome and caller
/// (`transport`: `local`, `http`, `ui` or `agent`), logged after the operation completes.
/// One more per source is its feed (kind `event`, provenance that source and no `job`):
/// every event frame of every job on the source, each with its own `job`, `channel` and
/// span, spectrum rows excepted. `stream_tail` a feed to see what a radio is doing in one
/// call.
```

Regenerate `AGENTS.md` and `skills/zetasdr/SKILL.md` per the Global Constraints.

- [ ] **Step 4: Run** `cargo test -p zeta-core --test ops_stream && cargo test -p zeta --test attach --test agents_md` → PASS.

- [ ] **Step 5: Commit:** `git add crates/zeta-core/src/node/mod.rs crates/zeta-core/src/ops.rs crates/zeta-core/tests/ops_stream.rs crates/zeta/tests/attach.rs AGENTS.md skills/zetasdr/SKILL.md && git commit -m "Node: a feed per source registered in add_port and named by stream_list; the attached transcriber's events reach it"`

---

### Task 4: The feed over the websocket

**Files:**
- Test: `crates/zeta-ws/tests/stream.rs` (helpers `prov`, `publish`, `connect`, `next`, `parse`; `TestServer` from `zeta_ws::testing`)
- Modify: `docs/guide.md:503-512` (route table) and `:719-730` (the websocket example)

**Interfaces:**
- Consumes: Task 1's `register_feed`.
- Produces: nothing new in code; the wire behaviour is pinned.

- [ ] **Step 1: Write the failing test** (it fails only until Task 1 is in the tree; with Tasks 1-3 merged it passes at once, which is the point: the websocket needs no change):

```rust
#[tokio::test]
async fn a_feed_socket_sees_frames_of_two_jobs_with_their_jobs() {
    let ts = spawn().await;
    let feed = ts.node.bus().register_feed(&SourceId("tone".into()));
    let mut ws = connect(&ts, &feed.0, Some(BEARER)).await.unwrap();
    let j1 = ts.node.bus().register(StreamKind::Event, job_prov("tone", "j1"));
    let j2 = ts.node.bus().register(StreamKind::Event, job_prov("tone", "j2"));
    publish_as(&ts, &j1, job_prov("tone", "j1"), json!({"protocol": "transcript", "text": "one"}));
    publish_as(&ts, &j2, job_prov("tone", "j2"), json!({"protocol": "squelch", "open": true}));
    let a = parse(next(&mut ws, Duration::from_secs(2)).await);
    let b = parse(next(&mut ws, Duration::from_secs(2)).await);
    let WsMessage::Frame(a) = a else { panic!("{a:?}") };
    let WsMessage::Frame(b) = b else { panic!("{b:?}") };
    assert_eq!((a.stream, a.job.unwrap().0.as_str(), a.payload["text"].as_str()), (feed.clone(), "j1", Some("one")));
    assert_eq!((b.stream, b.job.unwrap().0.as_str()), (feed, "j2"));
}
```

Add `job_prov(src, job)` and `publish_as(ts, id, prov, v)` helpers beside `prov` and `publish`.

- [ ] **Step 2: Run:** `cargo test -p zeta-ws --test stream a_feed_socket` → PASS (if it fails, the fan-in lost `job` or rewrote more than `stream`: fix Task 1). Lag on a feed is lag on any bus stream: `stalled_client_does_not_stall_bus_or_others` in the same file already pins the `lagged` message past `BUS_CAPACITY`; no second test.

- [ ] **Step 3: Docs.** In `docs/guide.md` route table add a row after `GET /streams`: `` | `GET /stream/{id}` of a source's feed | every event of every job on the source over one socket; the feed is the `event` stream in `GET /streams` with that `provenance.source` and `job: null` | ``. In the websocket example add, after the `ops` line, `feed = next(s["id"] for s in streams if s["provenance"]["source"] == "b210" and s["kind"] == "event" and s["provenance"]["job"] is None)` with a one-line comment `# the b210 feed: swap it for `ops` below to watch the radio instead`.

- [ ] **Step 4: Run** `cargo test -p zeta-ws` → PASS.

- [ ] **Step 5: Commit:** `git add crates/zeta-ws/tests/stream.rs docs/guide.md && git commit -m "Ws: a feed socket carries two jobs' frames with their jobs; the guide names the feed"`

---

### Task 5: The page watches the shown source's feed

**Files:**
- Modify: `crates/zeta-ui/web/overlays.js` (header comment lines 56-128; constructor fields 333-353; `setSource` 375-391; `sync` 393-448; `watchJob`/`seedSquelch`/`unwatch`/`lookupStream`/`subscribe`/`render` 593-700; `activity` 748; `jobRow` 762-774; `destroy` 1701)
- Modify: `crates/zeta-ui/web/subjects.js` (the "not watched (limit 4)" line: it reads `ov.watching`; keep it, the text now names `MAX_POSITIONED`)
- Modify: `crates/zeta-ui/tests/e2e/ui.py:3162-3200` (replace `scenario_watched_jobs_are_capped`), `:3218-3260` (`scenario_ws_busy_is_noted_once`), `:4944-4966` (`scenario_subjects_name_the_jobs_not_watched`)

**Interfaces:**
- Consumes: the feed rule (Global Constraints) via `api.streams()`; `api.streamText(id, onMessage, onClose, {onReconnect, onOpen})` from `api.js:246`.
- Produces: `ov.feed` (`{id, sub}` or `null`), `ov.watching(jobId)` true for every running job on the shown source; `ov.activity(jobId).watched` true for every running job on the shown source; `ov.render(job, payload)` (the `rec` argument goes: the job is passed); `SEED_IN_FLIGHT = 4` exported; the `.events` cell never shows `—`.

- [ ] **Step 1: Write the failing scenarios** in `ui.py`. Replace `scenario_watched_jobs_are_capped` with:

```python
def scenario_every_job_on_the_source_is_watched(page, url, secret):
    # Six measure jobs and four fm_demods on tone: every row counts or is tagged, no row
    # says "not watched", and the page holds ops + waterfall + feed = 3 sockets throughout.
    select_source(page, url, secret, "tone")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=5_000)
    jobs = []
    try:
        for i in range(6):
            jobs.append(start_job(url, secret, "measure", "tone", 100_000_000, 10_000 + 100 * i))
        for i in range(4):
            jobs.append(start_job(url, secret, "fm_demod", "tone", 100_015_000, 12_500 + 100 * i))
        page.evaluate("window.zetaTest.poll()")
        expect(page.locator('#jobs .job[data-state="running"]')).to_have_count(10)
        for j in jobs[:6]:
            cell = page.locator(f'#jobs .job[data-id="{j}"] .events')
            expect(cell).not_to_have_text("—")
            page.wait_for_function(
                "(id) => { const e = document.querySelector(`#jobs .job[data-id=\"${id}\"] .events`);"
                " return !!e && parseInt(e.textContent, 10) > 0; }", arg=j, timeout=5_000)
        for j in jobs[6:]:
            expect(job_row(page, j).locator(".kind .tag.squelch")).to_be_visible(timeout=5_000)
        expect(page.locator('#jobs .job .events[title^="not watched"]')).to_have_count(0)
        counts = stable_ws_connections(url, secret)
        assert max(counts) <= 3, f"ws_connections {counts}"
    finally:
        for j in jobs:
            stop_job(url, secret, j)
```

In `scenario_ws_busy_is_noted_once` change the trigger: with the pool hogged, `select_source(page, url, secret, "retune")` (a source switch opens its feed) replaces the job start; keep the one-note and recovery assertions; the hog's `free` count must subtract the hog page's own feed socket (read `ws_connections` after the hog page has loaded, as the scenario already does). In `scenario_subjects_name_the_jobs_not_watched` start the five `fake_subjects` jobs on a source other than the shown one (`select_source(..., "tone")` then jobs on `retune`, or the reverse), so the cap in play is `MAX_POSITIONED`; assert the text reads `not watched (limit 4)` as before.

Add `scenario_no_feed_is_noted_once(page, url, secret)`: it needs a source without a feed; the e2e node has none after Task 3, so the scenario stubs the lookup: `page.evaluate("window.zetaTest.hideFeeds(true)")` (a test hook added in Step 3 that makes `ov`'s feed lookup return null), then `select_source(page, url, secret, "retune")`; assert one transcript line `no event feed for retune; job events are not shown` (`error=True`), the outlines still draw (start an `fm_demod` and expect its `.outline`), and after `hideFeeds(false)` plus two `zetaTest.poll()` the line is not repeated and a `squelch` tag appears.

- [ ] **Step 2: Run to verify they fail:** `ZETA_UI_ONLY=every_job_on_the_source_is_watched,no_feed_is_noted_once,ws_busy_is_noted_once,subjects_name_the_jobs_not_watched cargo test -p zeta --test ui` → FAIL.

- [ ] **Step 3: Implement** in `overlays.js`:
  - Remove `MAX_WATCHED`, `overCap` for the shown source (keep the set for positioned jobs over `MAX_POSITIONED`), `noEventStream`, `watched`, `lookupStream`, `resolve`, `subscribe(rec, slot)` for the shown source (positioned jobs keep `subscribe`/`resolvePositioned` unchanged) and the `—` branch of `jobRow` (its title now `not watched (limit ${MAX_POSITIONED})`, used only by positioned rows).
  - `setSource(id)`: close `this.feed.sub` if any, set `this.feed = null`, clear `this.pending`, then open the feed: `openFeed()` finds the id in `await this.api.streams()` by the rule (`kind === "event" && provenance.job === null && provenance.source === id`); without one, note once per source `no event feed for <id>; job events are not shown` through `this.notice` (a new option of `mountOverlays`, `app.js` passes `(text) => state.transcript?.note(text, true)` as it does for `wf.onNotice`) and retry on each `sync`; with one, `this.feed = {id, sub: this.api.streamText(id, onMessage, null, {onReconnect, onOpen})}` where `onReconnect` resolves again by the same rule and `onOpen` after the first open calls `this.reseedAll()` (Task 7 fills it; here it is `this.squelches.clear()` plus a seed pass).
  - `onMessage(msg)`: `frame` with `msg.payload.job` found running in `this.lastJobs` and `rowKnown` → `this.render(job, msg.payload.payload)`; else → `this.hold(msg.payload)` (Task 6 fills `hold`; here it drops). `lagged` → `this.reseedAll()`.
  - `sync(jobs, source)`: keep outlines, marks, the squelch-graph-change check and the row counters; `want` is every running non-spectrum job on the source; for each SQUELCH_KINDS job in `want` with no known gate, `seedSquelch(job)` subject to `SEED_IN_FLIGHT` (a `this.seeding` Set of job ids; skip while its size is 4; the next sync picks the rest up); `this.lookup(jobs)` and `syncPositioned` unchanged.
  - `render(job, p)`: the body of today's `render(rec, p)` with `job` passed; positioned frames keep calling it with `rec.positionOnly` semantics via a second argument `{positionOnly}`.
  - `activity(jobId)`: `watched` is `this.lastJobs.some((j) => j.id === jobId && j.source === this.sourceId && isRunning(j))`.
  - `destroy()`: close the feed.
  - Header comment: rewrite the "Then watchJob for the MAX_WATCHED…" paragraph (lines 65-80) to describe the feed, the pending list and the re-seed.
  - `app.js`: `window.zetaTest.hideFeeds(on)` sets a flag `ov.hideFeeds` that makes `openFeed` behave as if no feed were listed; `mountOverlays` gets the `notice` option.
  - `subjects.js`: the "not watched" line's limit text uses `MAX_POSITIONED` (import it from overlays.js).

- [ ] **Step 4: Run** step 2's command plus `ZETA_UI_ONLY=transcript_ribbon_appears,quiet_channel_row_is_tagged_squelch,aprs_ribbon_appears,pocsag_ribbon_appears,watch_entry_starts_a_watch_and_outlines_its_band cargo test -p zeta --test ui` → PASS. Then `cargo test -p zeta-ui` → PASS.

- [ ] **Step 5: Commit:** `git add crates/zeta-ui/web/overlays.js crates/zeta-ui/web/app.js crates/zeta-ui/web/subjects.js crates/zeta-ui/tests/e2e/ui.py && git commit -m "Page: the shown source's feed replaces the 4-job watch; every running job counts, is tagged and ribbons"`

---

### Task 6: Frames before their job is known are held and replayed

**Files:**
- Modify: `crates/zeta-ui/web/overlays.js` (`hold`, `sync`), `crates/zeta-ui/web/app.js` (a debounced poll request)
- Modify: `crates/zeta-ui/tests/e2e/ui.py` (new scenario)

**Interfaces:**
- Consumes: Task 5's `onMessage` calling `this.hold(frame)`; `window.zetaTest.poll()` → `state.jobs.refresh()`.
- Produces: `ov.hold(frame)` (pushes to `this.pending`, capped at `PENDING_MAX = 200` oldest first out, and calls `this.requestPoll()` once per in-flight poll); `mountOverlays` option `requestPoll: () => Promise` (app.js passes `() => state.jobs ? state.jobs.refresh() : Promise.resolve()`); after each `sync`, `replayPending()` renders held frames whose job is now running and row-known, in order, and drops frames of jobs the poll does not list as running.

- [ ] **Step 1: Write the failing scenario:**

```python
def scenario_frame_before_poll_is_drawn_after_it(page, url, secret):
    # A measure job started between polls publishes its first event before the page knows
    # the job: the frame is held and counted once the poll lands, not lost.
    select_source(page, url, secret, "tone")
    page.wait_for_function(BRIGHT_AT_RIGHT_JS, timeout=5_000)
    page.evaluate("window.zetaTest.pausePolling(true)")
    job = None
    try:
        job = start_job(url, secret, "measure", "tone", 100_000_000, 10_000)
        # The first measure event lands within about a second; wait for two.
        page.wait_for_timeout(2_500)
        assert page.evaluate("window.zetaTest.pendingFrames()") >= 1
        page.evaluate("window.zetaTest.pausePolling(false)")
        page.evaluate("window.zetaTest.poll()")
        page.wait_for_function(
            "(id) => { const e = document.querySelector(`#jobs .job[data-id=\"${id}\"] .events`);"
            " return !!e && parseInt(e.textContent, 10) >= 1; }", arg=job, timeout=5_000)
        assert page.evaluate("window.zetaTest.pendingFrames()") == 0
    finally:
        page.evaluate("window.zetaTest.pausePolling(false)")
        if job:
            stop_job(url, secret, job)
```

Add the hooks `window.zetaTest.pausePolling(on)` (the jobs poll timer and `requestPoll` do nothing while on; `zetaTest.poll()` still works) and `window.zetaTest.pendingFrames()` (`state.ov.pending.length`) in `app.js`.

- [ ] **Step 2: Run to verify it fails:** `ZETA_UI_ONLY=frame_before_poll_is_drawn_after_it cargo test -p zeta --test ui` → FAIL (pending is 0, the frame was dropped).

- [ ] **Step 3: Implement** `hold`, `requestPoll` (debounced: one in flight), `replayPending` called at the end of `sync`, and the two hooks.

- [ ] **Step 4: Run** `ZETA_UI_ONLY=frame_before_poll_is_drawn_after_it,every_job_on_the_source_is_watched cargo test -p zeta --test ui` → PASS.

- [ ] **Step 5: Commit:** `git add crates/zeta-ui/web/overlays.js crates/zeta-ui/web/app.js crates/zeta-ui/tests/e2e/ui.py && git commit -m "Page: a feed frame for a job the last poll did not list is held and replayed after the next poll"`

---

### Task 7: A lag or reconnect re-seeds every squelch dot

**Files:**
- Modify: `crates/zeta-ui/web/overlays.js` (`reseedAll`, `seedSquelch` bound), `crates/zeta-ui/web/app.js` (test hook)
- Modify: `crates/zeta-ui/tests/e2e/ui.py` (new scenario; `scenario_job_rows_fold_only_on_evidence`)

**Interfaces:**
- Consumes: Task 5's `onMessage` (`lagged`) and `onOpen` after a reconnect both call `this.reseedAll()`.
- Produces: `ov.reseedAll()`: clears `this.squelches`, clears every row's and tag's squelch dot (`setSquelchTag(row, null)`, `refreshTag`), clears `this.pending`, leaves `this.counters`, and runs the seed pass of `sync` at once (at most `SEED_IN_FLIGHT` requests in flight, the rest on later syncs); `window.zetaTest.feedLag()` injects a `{type: "lagged", missed: 1}` into the feed's `onMessage`.

- [ ] **Step 1: Write the failing scenario:**

```python
def scenario_feed_lag_reseeds_squelch_dots(page, url, secret):
    # A quiet fm_demod (closed) is tagged; the test flips its gate with graph_put
    # (squelch: false, so the next event is `open: true`) while the page's feed is told it
    # lagged: the stale dot is re-seeded from job_context, and the counter is not reset.
    quiet = start_job(url, secret, "fm_demod", "tone", 100_015_000, 12_500)
    try:
        select_source(page, url, secret, "tone")
        page.evaluate("window.zetaTest.poll()")
        tag = job_row(page, quiet).locator(".kind .tag.squelch")
        expect(tag).to_be_visible(timeout=5_000)
        assert "closed" in (tag.get_attribute("title") or "")
        # Pretend the feed lagged, then read what the node holds: still closed, dot back.
        page.evaluate("window.zetaTest.feedLag()")
        expect(tag).to_have_count(0)
        page.evaluate("window.zetaTest.poll()")
        expect(job_row(page, quiet).locator(".kind .tag.squelch")).to_be_visible(timeout=5_000)
        # Same through a reconnect: close the feed socket from the page; onOpen re-seeds.
        page.evaluate("window.zetaTest.feedReconnect()")
        page.evaluate("window.zetaTest.poll()")
        expect(job_row(page, quiet).locator(".kind .tag.squelch")).to_be_visible(timeout=8_000)
    finally:
        stop_job(url, secret, quiet)
```

Add `window.zetaTest.feedReconnect()` (closes the underlying socket so `api.streamText`'s reconnect path runs; `api.js` exposes nothing for that today, so give the `{close}` handle a `drop()` that closes the socket without setting `closed`, and call it).

Adjust `scenario_job_rows_fold_only_on_evidence` (ui.py:2476): every fm_demod on the shown source is now watched, so after `foldIdle(0)` and the squelch seeds the fourteen quiet rows fold **without** a mix (the group appears); then the mix start keeps the group and the talking rows out of it; the "newest mix on another source" branch now asserts the group **stays** (its evidence is the rows' own squelch, not the mix). Update the scenario's comment to say so.

- [ ] **Step 2: Run to verify they fail:** `ZETA_UI_ONLY=feed_lag_reseeds_squelch_dots,job_rows_fold_only_on_evidence cargo test -p zeta --test ui` → FAIL.

- [ ] **Step 3: Implement** `reseedAll`, the two hooks and the `drop()` on `streamText`'s handle.

- [ ] **Step 4: Run** `ZETA_UI_ONLY=feed_lag_reseeds_squelch_dots,job_rows_fold_only_on_evidence,job_rows_fold_idle_plan_channels,quiet_channel_row_is_tagged_squelch cargo test -p zeta --test ui` → PASS.

- [ ] **Step 5: Commit:** `git add crates/zeta-ui/web/overlays.js crates/zeta-ui/web/app.js crates/zeta-ui/web/api.js crates/zeta-ui/tests/e2e/ui.py && git commit -m "Page: a lagged or reconnected feed forgets every squelch dot and re-seeds them from job_context; idle plan channels fold on their own evidence"`

---

### Task 8: Whole suite, docs, TODO and UAT

**Files:**
- Modify: `TODO.md:71-78` and `:435-438`, `UAT.md` (append a Plan 27 section), `docs/agents/field-craft.md` (the "Transcribing a channel" section; `zeta agents-md` renders it into `AGENTS.md`)

- [ ] **Step 1: Run the whole UI suite and the workspace:** `cargo test -p zeta --test ui` (expect under 600 s; note the time) and `cargo test --workspace` → PASS. If the suite time grew past 520 s, trim waits in the two new scenarios before going on.

- [ ] **Step 2: TODO and UAT.** Remove the entries "UI: the page watches at most 4 jobs, so a tag loses its squelch dot" (line 75) and "UI: transcript ribbons stop at the 4-job watch cap on a plan" (line 435). In "Node: stream sockets are a fixed pool" (line 71) strike "and the page releasing the streams it no longer shows" and add one sentence: "The page now holds one feed socket per shown source (plan 27)." Add a new entry "Page: positioned jobs on other sources onto their feeds" (two sentences: `MAX_POSITIONED` and the per-job sockets remain for the map's jobs on other sources; one feed per such source replaces them). Append to `UAT.md`:

```
## Plan 27: one event feed per source

- [ ] 1. With the CB plan and transcribers running on the B210, open the page on it: every demodulator row has a squelch dot within one poll, and `GET /health` `ws_connections` reads 3 for the page (5 with Listen to all).
- [ ] 2. A transmission on any channel draws its ribbon on that channel's line within a second of the line landing in `transcripts.log` (`zeta transcripts tail --lines 5`).
- [ ] 3. A second page opens with no `stream refused`; an agent `stream tail` of the b210 feed answers frames while both pages are open.
- [ ] 4. Idle CB channels fold into their group within the idle age with no mix playing.
- [ ] 5. Pull the laptop's network for ten seconds and plug it back: the dots clear and return, ribbons resume, no row says "not watched".
```

- [ ] **Step 3: Field craft.** Add to the "Transcribing a channel" section one paragraph: "`stream_tail` the source's feed (`stream_list`: the `event` stream with that source and no `job`) shows every transcript, `vad` and `squelch` event of every job on the radio in one call, each frame naming its job." Regenerate `AGENTS.md` and `skills/zetasdr/SKILL.md`; `cargo test -p zeta --test agents_md` → PASS.

- [ ] **Step 4: Gates:** `cargo fmt -p zeta-core -p zeta-ws -p zeta -p zeta-ui -- --check` (one `-p` per crate) and `cargo clippy --workspace --all-targets --all-features -- -D warnings` → clean.

- [ ] **Step 5: Commit:** `git add TODO.md UAT.md AGENTS.md skills/zetasdr/SKILL.md docs/agents/field-craft.md && git commit -m "Plan 27 wrap: TODO entries retired, UAT 27 added, field craft names the feed"`
