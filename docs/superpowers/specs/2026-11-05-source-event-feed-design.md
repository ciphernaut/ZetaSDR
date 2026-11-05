# Plan 27: one event feed per source

Design spec, 2026-11-05. Decided with the operator after the CB transcribers' ribbons went missing
on the page; the operator chose this option over raising the watch cap, a `transcripts` stream and
merging the transcriber into its demodulator. A reviewer read it as the builder before hand-over;
its findings are folded in (the fft rule at both registration sites, the scan's two row streams,
`add_port`, the lag re-seed, the ws-busy scenario's trigger).
Builds on the stream bus (`crates/zeta-core/src/stream.rs`), the stream websocket
(`crates/zeta-ws/src/stream.rs`), the job launch (`crates/zeta-core/src/node/launch.rs`) and the
page's overlays (`crates/zeta-ui/web/overlays.js`).

## 1. Intent and why

The page draws a job's ribbons, its event counter and its squelch dot from the job's own event
streams, one websocket each, and watches at most `MAX_WATCHED` (4) jobs on the shown source,
newest first. A `plan_start` on UHF CB attaches 39 transcribers in channel order, so the four
watched are ch37-40 and every utterance on ch9, ch16 and ch20 reaches the node, the mix and
`transcripts.log` but never the waterfall. The operator's words: "not seeing transcript ribbons
hit the rf view". The rows over the cap say "not watched (limit 4)", which nobody reads with 78
rows.

The cap exists because each watch is a socket and the node's pool is 64 (`Limits::max_ws`) for
every client together: 39 transcribers at two sockets each (their `vad` and `transcript` streams)
would not fit, and a second page or an agent `stream tail` would be refused. Raising the cap moves
the failure; a `transcripts` stream fixes ribbons but leaves squelch dots and counters capped;
merging the transcriber into its demodulator needs fan-out and keeps one socket per stage.

The fix is one stream per source that carries every event of every job on it. The page opens one
socket for the shown source and sees everything; the cap, the per-job sockets and the resolve-by-
provenance dance go away. It is the same shape as the `ops` stream (one node-level stream the page
reads with one socket) applied per radio, so an agent gets it for free: `stream_tail` the feed of
`b210` is "what is happening on that radio".

## 2. Goals, non-goals, anti-goals

### Goals

- A page shows ribbons, counters and squelch dots for every running job on the shown source,
  however many there are: 39 CB transcribers, 80, more.
- A page costs a fixed number of sockets whatever the job count: ops, the waterfall's spectrum,
  the feed, and the mix's two when one is playing. Two pages and an agent fit the pool with room.
- The jobs fold (agent.js `foldPlan`) has squelch and event evidence for every job on the shown
  source, not for four of them, so idle CB channels fold at 40 channels without a mix playing.
- An agent or a script can `stream_tail` one stream and read every event of every job on a
  source, with each frame still naming its job.
- Nothing a job publishes changes: its own streams stay as they are, `--then-tail`, `job_attach`,
  the mix and the transcript tee keep reading them.

### Non-goals

- Positioned jobs on the other sources (`MAX_POSITIONED`, the map's and the Subjects pane's) keep
  their per-job watches in this plan. Moving them onto those sources' feeds is the obvious
  follow-up and is noted in TODO, not built here.
- Per-client fairness of the socket pool, `--max-streams`, and the page releasing streams it no
  longer shows beyond what this plan removes (TODO "Node: stream sockets are a fixed pool" stays
  open for those parts).
- Audio and IQ: the feed carries events only.
- Fan-out in the graph engine, and anything about merging the transcriber into its demodulator
  (TODO "Architecture: fan-out, and the transcriber as part of its demodulator's graph").
- Replay. A feed starts at subscription like every stream; what a job published before the page
  subscribed is not sent (the squelch gate is seeded from `job_context` as today).

### Anti-goals

- A feed that carries spectrum rows. The waterfall's `fft` rows (20 a second, 1024 bins, three
  elastic spectrum jobs on the field host) and a scan's rows would swamp it; they have their own
  streams and the page reads them there (text mode, `waterfall.js`).
- A frame on the feed that lost its job. Every feed frame carries its original provenance, so the
  reader knows which job, channel and span it belongs to.
- Lag that lies. A slow reader is told `lagged` with a count, as on every stream, never silently
  fed a subset.

## 3. Constraints

- The bus (`StreamBus`) is a map of `broadcast` channels of `BUS_CAPACITY` frames, each with a
  `StreamInfo {id, kind, provenance}`; `publish` drops a frame whose kind differs from its
  stream's. `StreamInfo` is `deny_unknown_fields` and serialised to every `--node` client, so a new
  field breaks an older `zeta` client with `decoding node response: unknown field`
  (the field host's `z.sh` deploy helper has the note). This plan adds no field to it.
- A job's output streams and its inner edge streams are both registered on the bus by
  `node_outputs` in `launch.rs`; a `scan` registers its row stream itself (`launch_scan`); the
  `ops` stream is registered by `Node::open` with provenance source `node` (`NODE_SOURCE`). The
  mix's streams have source `node` too, with their job.
- An attached job's provenance source is its input's source (`job_attach` copies `source` and
  `tuning`), so a transcriber's `vad` and `transcript` frames already name the radio.
- The wire frame (`FrameSummary`) carries `stream`, `kind`, `source`, `job`, `channel`,
  `start_ns`, `end_ns`, `payload`. The page dispatches on the payload's shape (`protocol`,
  `summary`, `label`, `q`), never on which stream a frame came on.
- `GET /stream/{id}` takes one semaphore permit per socket from `max_ws` (64). The page's
  `api.streamText` reconnects with `onReconnect` re-resolving the id by provenance from
  `GET /streams`, and notes a 503 (`stream refused`) once.
- The page's 2 s poll (`ov.sync(jobs, source)`) is the only place it learns a job's kind, tuning,
  input and colour; a frame can arrive for a job the last poll did not list.
- `agent.js` folds idle plan channels only on evidence: `activity(id).watched` and the known
  squelch for watched jobs, the mix's active set for unwatched ones.
- Every op field is required and unknown fields are rejected; AGENTS.md is generated from
  `ops.rs` doc text (`crates/zeta/tests/agents_md.rs` checks it), so the guide changes live there.
- The e2e suite (`crates/zeta-ui/tests/e2e/ui.py`, run by `crates/zeta/tests/ui.rs`) has the traps
  of the "UI e2e gotchas" note: `wait_for_function("() => …")` only (CSP), the `cb` fixture plays
  48 kHz (ch59, ch20, ch60 only; distinct bandwidths avoid `duplicate`), `view_value` returns
  `"<key> <value>"`, POLICIES are appended, the suite runs about 474 s of a 600 s deadline.

## 4. The design

### 4.1 The feed on the node

Every source attached to the node gets one `event` stream, its **feed**, registered where the
source is attached, `Node::add_port` (which `open` calls per port and which tests call after
`open`, so the bus's feed table is mutable after construction), with provenance
`{source: <id>, job: null, channel: null, span: 0..0}`. It lives as long as the node; a source
fault or heal does not touch it. There is no feed for `node`: the `ops` stream is the node's, and
the mix watch already costs one socket.

A feed is recognised in `stream_list` by its provenance: `kind == "event"`, `job == null` and a
source that is not `node` (the `ops` stream is the one with source `node` and no job). No new
field on `StreamInfo` (constraint above). `stream_list`'s doc text says so.

**Fan-in.** The bus does it: `StreamBus` keeps `feeds: HashMap<SourceId, StreamId>` and a set of
stream ids that are **not fed**. `publish` of a frame whose kind is `Event`, whose stream is fed and
whose `provenance.source` has a feed also sends a clone on the feed's sender with `stream` set to
the feed's id and everything else (provenance with its job, channel and span, payload) unchanged.
No task, no extra receiver, ordering as published. A frame published to the feed's own id directly
is dropped and counted in `dropped` (nothing but the bus writes a feed).

**Not fed:** every `Event` stream that leaves an `fft` node, and a scan's row streams. The fft
rule has two sites in `launch.rs`: the edge registration at the top of `launch_inner` (every edge
is registered before any stage is consulted, and a `watch` carries its rows on an `fft → detect`
edge), keyed by the `from` node's kind, and `node_outputs`' `fresh` closure for an fft's job
output (a `spectrum` job). The scan has two: `out` in `launch_scan` and the per-step temporary
stream `fft_step` makes with the scan's provenance. Everything else a job publishes (squelch, vad,
transcript, measure, detect's `signal`, the watch's own events, every decoder row, `candidate`) is
fed, inner edge streams included: an operator chain such as `decoder → correlate → bit_view` on a
radio job delivers an event once per stage that forwards it, and the page's counter counts each
(accepted; no built-in radio graph has a non-fft inner event edge today). `bits_start` and mix
jobs have source `node` and are never fed (`bits.js` keeps its own socket). Registration takes the
choice: `register_unfed` beside `register`, so the rule is at the places a stream is made. (My call
over inspecting payloads at publish time: a registration-time flag costs nothing per frame. The
test in 5.1 pins that a spectrum row never reaches a feed from a spectrum job, a watch or a scan,
whichever way a later change registers one.)

**Lag.** A feed is a broadcast channel like any other, `BUS_CAPACITY` deep; a slow socket sees
`{"type":"lagged","missed":N}` and continues. The feed's rate on the field host is small (squelch
transitions, vad edges and transcripts on 39 channels, one measure event a second per measure job,
at most 10 `signal` events a second per watch), so lag means a stalled page, not a busy radio.

**Ops and tails.** `stream_tail` on a feed works as on any stream, and is the agent's way to see
"what is happening on b210" in one call. The transcript tee (`tee_transcripts`) keeps reading each
job's own streams: it is unchanged, and `transcripts.log` is unchanged.

### 4.2 The page

`overlays.js` watches the **shown source's feed**, one `api.streamText` subscription, opened in
`ov.setSource(id)` and closed when the source changes (as every watch is dropped today). The id
is found in `api.streams()` by the rule in 4.1 (`kind === "event"`, `provenance.job === null`,
`provenance.source === id`); `onReconnect` resolves it again the same way; a node without a feed
for the source (an older node) is noted once in the transcript, `no event feed for <source>; job
events are not shown`, and retried on each sync, as the ops feed does.

A `lagged` message or a reconnect of the feed is a hole of unknown size in every job's events at
once, so the page treats it as it treats a job whose graph changed: every known squelch gate on the
shown source is forgotten (dots cleared) and re-seeded from `job_context` as below, the `pending`
list is cleared, and the counters stand (they are approximate on every stream). (My call: the
alternative, keeping the dots as they were, leaves an idle channel's dot wrong until its next
transition, which on CB can be an hour, and that is the class of bug this plan exists to fix.)

Each frame is rendered by `ov.render(job, payload)` exactly as today, the job found by the frame's
`job` in the last poll's list (`this.lastJobs`). The dispatch rules of the header comment stand
(spectrum rows ignored, `squelch` tags only a SQUELCH_KINDS job's row, every other event bumps the
counter, `label` labels, `signal` marks, `summary` and the protocol fallbacks ribbon, `vad` is
counted only; an attached job's ribbons take its input's colour and band). Jobs on the shown
source that are not `running` in the last poll are ignored: a stopped job's last frames are not
drawn.

**A frame before its job is known.** A job started between two polls, or a job whose mode row the
Subjects store has not yet settled (`rowKnown`), is not rendered yet. Such frames go in a bounded
`pending` list (200 frames, oldest dropped) keyed by job, and the first one asks for a poll now
(`app.js` exposes the poll the test hook `zetaTest.poll()` already drives; debounced to one request
in flight). After each `ov.sync`, pending frames whose job is now running and row-known are
rendered in order; frames of a job the poll does not list as running are dropped. (My call: hold
and replay rather than drop, so a decoder's first lines, which `lookup` says may be gone a poll
later, are not lost; the bound keeps a flood harmless.)

**What goes.** `MAX_WATCHED`, `overCap`, `watching()`, `noEventStream`, `lookupStream`,
`subscribe(rec, slot)`, `resolve`, `watchJob` for the shown source, the `—` cell and its
`not watched (limit 4)` title, and the socket-per-job accounting in the header comment. Every
running job on the shown source has a counter from its first fed event. The Subjects pane's
"not watched" line (`scenario_subjects_name_the_jobs_not_watched`) now names only positioned jobs
on other sources over `MAX_POSITIONED`; `ov.watching(jobId)` stays for that and answers true for
every job on the shown source.

**What stays.** `seedSquelch` for every SQUELCH_KINDS job on the shown source whose gate is
unknown (a page load of the CB plan: 39 `job_context` reads; after a lag or reconnect: 39 again),
at most 4 in flight (today's guard is per job, so up to `MAX_WATCHED` ran concurrently; the bound
keeps that shape and lands 39 reads in about ten round trips, within one 2 s poll on a LAN).
`ops.log` shows them as read-only ops without results. The positioned-jobs watch on other sources (`syncPositioned`,
`MAX_POSITIONED`) is unchanged. The mix watch (`mix.js`) is unchanged. `activity(id)` answers
`watched: true` for every running job on the shown source, which gives `foldPlan` rule (b) its
evidence for all of them.

**Sockets.** A page on the field host holds: ops, the waterfall's spectrum (binary), the feed, and
when "Listen to all" is on the mix's event and audio sockets: five, for 78 jobs or 160.

### 4.3 Wire and docs

- `stream_list` doc text (`ops.rs`, so AGENTS.md): "One is always the node's own `ops` event
  stream (provenance source `node` and no `job`), and one is each source's **feed** (kind `event`,
  provenance that source and no `job`): every event frame of every job on the source, each with
  its own job, channel and span, spectrum rows excepted. `stream_tail` a feed to see what a radio
  is doing in one call."
- `docs/guide.md`, Transports: the feed beside the ops stream in the stream table; the websocket
  client example gains one line showing a feed tail.
- Field craft, "Reading a source's health" or a new short paragraph under "Transcribing a
  channel": `stream_tail` the source's feed shows every transcript and squelch event on the radio
  without listing the jobs' streams.
- `/health` is unchanged. Nothing in `schema`.

### 4.4 TODO and UAT

- Remove "UI: the page watches at most 4 jobs, so a tag loses its squelch dot" and "UI: transcript
  ribbons stop at the 4-job watch cap on a plan" (this plan delivers them).
- "Node: stream sockets are a fixed pool": strike "the page releasing the streams it no longer
  shows"; keep `--max-streams` and per-client fairness.
- Add: "Page: positioned jobs on other sources onto their feeds (`MAX_POSITIONED` goes)".
- UAT: a 27.1 on the field host: the CB plan with transcribers running, open the page on the B210:
  every demodulator row has a squelch dot within a poll, a transmission on any channel draws its
  ribbon on that channel's line within a second of its transcript landing in `transcripts.log`,
  `GET /health` `ws_connections` reads 3 (5 with Listen to all) for one page, and a second page
  opens with no `stream refused`.

## 5. Tests

### 5.1 Node (`crates/zeta-core`)

- `feed_carries_every_event_of_a_source_with_its_job`: two jobs' event streams on source `a`
  (different job ids) publish; a subscriber of `a`'s feed receives both frames in order, each
  with `stream` = the feed id and its own `provenance.job`.
- `feed_does_not_carry_spectrum_rows_or_other_sources`: a `spectrum` job's rows, a `watch`'s
  `fft → detect` edge rows, a scan's `out` and its per-step rows, and a source `b` job's frame do
  not reach `a`'s feed, while the watch's `signal` events do.
- `feed_is_not_written_directly`: a frame published with the feed's id is dropped and counted.
- `stream_list_lists_one_feed_per_source`: with two sources attached, `stream_list` has exactly
  one `event` stream per source with `job: null`, plus the `ops` stream (source `node`), and a
  job start adds no second one.
- `node_source_frames_are_not_fed`: a mix's event frame (source `node`) reaches no feed.
- The attached-job test in `crates/zeta/tests/attach.rs` gains an assertion: the transcriber's
  `transcript` event appears on its source's feed with the transcriber's job id.

### 5.2 Websocket (`crates/zeta-ws/tests/stream.rs`)

- A text socket on a feed sees frames of two jobs; `lagged` arrives when the reader stalls past
  `BUS_CAPACITY` (as the existing lag test does for a job stream).

### 5.3 Page (`crates/zeta-ui/tests/e2e/ui.py`)

- `scenario_watched_jobs_are_capped` is replaced by `scenario_every_job_on_the_source_is_watched`:
  six `measure` jobs on `tone` (distinct bandwidths) all count events in their rows, no row has the
  `—` cell, and `ws_connections` stays at ops + waterfall + feed (3) through the run; four further
  `fm_demod`s get squelch dots too and the count stays 3.
- `scenario_ws_busy_is_noted_once`: a job start no longer opens a socket, so the trigger changes:
  the hog fills the pool (its own `/ui/` page now holds a feed socket too), then the page switches
  source (or is loaded) and its feed is refused; one note, and the feed opens once a slot frees.
- `scenario_transcript_ribbon_appears`, `scenario_quiet_channel_row_is_tagged_squelch`,
  `scenario_aprs_ribbon_appears`, `scenario_pocsag_ribbon_appears` pass unchanged (they are the
  proof that rendering did not change).
- `scenario_subjects_name_the_jobs_not_watched` narrows to positioned jobs on another source.
- `scenario_job_rows_fold_only_on_evidence`: a squelch-kind job on the shown source now folds on
  its own squelch evidence with no mix playing; the mix-evidence branch is kept for another
  source's jobs or removed with the test's assertion adjusted (builder's call; say which).
- New `scenario_frame_before_poll_is_drawn_after_it`: a job started and publishing before the
  page's next poll gets its first event counted (the pending replay), not lost.
- New `scenario_feed_lag_reseeds_squelch_dots`: with the feed's socket stalled past `lagged` (or
  closed and reconnected), a demodulator whose gate changed meanwhile shows the right dot after
  the re-seed, and a job's counter is not reset.
- Suite time must stay under the 600 s deadline; the replaced scenario was among the slower ones
  (it waits on sockets), so the budget should not worsen.

## 6. Left to the builder

- Where the `feeds` map and the unfed set live (on `StreamBus` as described, or a thin wrapper the
  node owns) and whether `register_unfed` is a flag or a second method; the bus tests are the
  contract.
- The pending list's exact replay point inside `ov.sync` and how the debounced poll request is
  wired through `app.js`.
- Whether `ov.render` keeps its `rec` argument or takes the job directly, and the header comment's
  rewrite.
- Order of work: node first (bus, registration, tests, doc text), then the page, then e2e; the
  node change is deployable alone (an old page keeps working on per-job streams until it is
  updated, and the feed is just one more stream in the list).
