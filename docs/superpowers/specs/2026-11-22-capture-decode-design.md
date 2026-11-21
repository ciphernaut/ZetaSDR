# Replaying a stored capture into a mode's handler: what the code settles

Design note for plan 44. The design itself is the operator's, approved 2026-11-21:
`2026-11-21-alternative-handlers-design.md`, section "Send to: a stored IQ capture"
(delivery item 3). This note does not restate it. It records what a survey of the code as
plan 43 left it (main `f5e5eb3b`) found that the design did not say, and the call made for
each. Where this note and the parent spec differ, this note is the later word; every such
place is marked **differs**.

## Intent, unchanged

From a burst capture in Explore, run any mode that fits the recording, through any of its
handlers, and see what it decodes, with the recording's own times, and without any of it
passing for live traffic. Two handlers replaying one capture are the repeatable form of the
comparison plan 43 made live.

## What the code is today

- `capture_in` (`zeta-dsp/src/capture_in.rs`) replays a list of captures in order, paced by
  a 50 ms sleep per 50 ms block, and stamps every frame with the recorded time. A pass that
  does not loop ends cleanly. It takes capture ids, not a set.
- `bits_start` is the precedent: it resolves the capture's set (`set` equal, sorted by start
  time), builds a graph rooted on `capture_in`, and launches a record on source `node`,
  which makes it leaseless (no port, no lease, no IQ stream of a radio). Bits jobs loop,
  resume after a restart and stay `running` across a shutdown.
- A leaseless job drains: when its first stage ends cleanly the rest get 2 s
  (`DRAIN_TIMEOUT_MS`), then are cancelled, and the job ends `stopped`. A program gets 5 s
  after its input closes (`EXIT_GRACE`), so the job's drain cuts it short. The packet reader
  of a program is aborted with the run and drains nothing (a plan 42 follow-up).
- Every mode row's graph is rooted on a `channel` stage, fed on a radio job by the job's IQ
  stream. A leaseless job has none.
- A decoder publishes its event envelope (`summary`, `handler`, `t_ns`) only when the
  record's kind is a mode row (`launch.rs`, `ctx.mode`). Events and packets already take the
  time of the input frame, so a replay's carry the recording's time with no change.
- Nothing carries a `replay` mark: not the record, the stage context or an event.
- Event frames whose provenance source is `node` enter no feed and no `events.log`
  (`stream.rs`). The page hears events only through a source's feed, so a job on `node` is
  invisible to Subjects and the map. A per-job socket (as the Bits pane opens) has no
  backlog and would miss the first events of a one-second replay.
- The page drops a subject group and a map track when none of its jobs runs, never shows a
  record's `context.note`, has menus only on the waterfall (`box` and `job` surfaces), and
  Explore holds no mode rows. Explore's capture rows carry buttons.
- SondeHub is reached only through `sink_sondehub` graph nodes, which come from a watch's
  sinks or a `graph_put`. Identify reads only its own child; a watch reads lease holders.
  Neither can see a leaseless job.
- A decoder job whose kind is a mode row supersedes learned signatures on its band, keeps a
  session (frame rows) and tallies stations (`tee_rules`).

## Calls made

### The node

1. **The op is `capture_decode { capture, kind, handler, packets }`.** `capture` is any
   capture of the set; `kind` is a mode row's id; `handler` is `null` (the policy picks) or
   a handler id; `packets` is `false` or `true`. The answer is the job, as `job_start`'s.
   **Differs:** `packets` is added (call 9).
2. **The whole set is replayed,** resolved as `bits_start` resolves it; the two share one
   helper. A capture that is gone is `unknown` on `capture`, with the known ids.
3. **The record's kind is the mode's id and its source is `node`.** The mode id is what
   gives the events their envelope and lets `packets` find the row; `node` is what makes
   the job leaseless. Its tuning is the first capture's centre and width. It is an
   operator-graph record (the node built its graph; nothing re-expands it).
4. **The graph is the mode's, with `capture_in` in front:** the row expanded as for a radio
   whose sample rate is the capture's and which has no channel bank, then a `capture_in`
   node (`loop: false`, `realtime: true`, the set's ids) with an IQ edge into the row's
   `channel`. What does not fit is refused by the expansion's own errors, before anything
   starts: a band narrower than the row's minimum, a centre outside its range, a rate its
   channel cannot divide.
5. **The mark is `replay: <capture id>`,** the id the request named. On the record it is a
   field, absent when not a replay (so a node rolled back past this build still reads every
   record that is not a replay). On events it is a member of the envelope, set from a new
   `StageContext.replay`; `capture_in`'s own `segment` events carry it too. Every event the
   job publishes has it.
6. **A replay's streams belong to the source `replay`.** The job's base provenance names
   `replay`, not `node`, and the node registers one event feed for `replay` when it opens.
   So a replay's events reach one feed that a page holds open before any replay starts, and
   none of them can be taken for a radio's. The `replay` feed has no `events.log` tee: the
   log stays a record of what was received. `replay` joins `node` as a name no source may
   take. The record's source stays `node`.
7. **A replay ends `stopped` with what it decoded.** While it runs, the node counts its
   events (envelope events; `segment` and diagnostics are not counted). At the end the
   record's context has `replay: { capture, captures, source, start_ns, end_ns, events }`
   and `note: "replayed <n> capture(s) from <source>: <events> event(s)"`. A failed stage
   fails the job as it does any job.
8. **A replay is given time to finish.** Its drain bound is 8 s, not 2 s, past a program's
   5 s exit grace and the 1 s event drain; a job whose stages all end sooner ends sooner.
   The packet reader gains the drain the events have: when the input is done it reads until
   the program closes the socket or 1 s passes. This closes the plan 42 follow-up.
9. **`packets: true` starts the packet capture before the first sample.** An attachment
   made after a replay starts misses whatever was decoded first, which for a short burst is
   everything. With `packets: true` the handler is resolved as with `needs: ["packet"]`,
   `capture_in` waits a lead of 300 ms before its first block (a new `lead_ms` param,
   default 0), and the node attaches `packets` inside that lead, under the same call. A
   failed attachment stops the replay and is the op's error. `job_attach` of `packets` to a
   running replay still works and captures what follows.
10. **A replay never resumes.** It is not resumable, and a replay record found `running`
    when the node opens is stopped with the note `a replay does not resume: run it again`,
    as a packet capture is.
11. **A replay proves nothing live.** It supersedes no signature, keeps no session, tallies
    no station, and `sink_sondehub` refuses to start in a job whose context is a replay
    (`a replay uploads nothing`). `graph_put` of a replay's job is refused (`a replay's
    graph is its mode's`), which also keeps sinks out. Identify and the watch already cannot
    see it.
12. **One replay of a capture per mode and handler at a time.** A second while the first
    runs is a `duplicate` naming it. The history keeps one finished record per capture, mode
    and handler: the prune key gains `replay`.
13. **Refused, each by name:** a `kind` that is no mode row (`<kind> has no handlers`, as
    Send to says); a handler that is not ready or gives no packets when asked (plan 42's
    wording); `packets: true` on a mode none of whose handlers gives packets.

### The CLI and the agent

14. **`zeta capture decode --capture <id> --kind <mode> [--handler <h>] [--packets]`,**
    locally and with `--node`. It prints the job. There is no `--then-tail`: the result is
    the record's note (`zeta job show`), and the capture file when `--packets` was given.
15. **The agent's note** says what a replay is for (checking a decoder against a recording,
    comparing handlers), that it proves nothing live, and that its events are on the
    `replay` feed with the recording's times.

### The page

16. **Explore's capture row gains a `Send to ›` button** that opens a popup of the same
    kind as the waterfall's menus, from a new `capture` surface in the entry table. First
    level: each ready mode row that fits the capture (`rowFits` against its centre and
    width), by label. A row with two or more ready handlers opens a submenu: `<pick>
    (policy)` and each handler, a handler that is not ready greyed with its reason. Rows
    that do not fit are under one `elsewhere ›` submenu, greyed, each titled with why.
    **Differs** from "as Decode does" only in that the unfit rows cannot be chosen: a
    recording cannot be moved.
17. **A capture row whose mode has a packet-giving handler** offers, in that mode's submenu,
    `… with packets`, which sends `packets: true`. The capture then appears in Explore's
    packet captures as any does.
18. **The page opens the `replay` feed once, at load,** beside the shown radio's. Its events
    draw no ribbon and no map track. They go to Subjects only.
19. **A replay's subjects are a group of their own,** keyed `replay:<capture id>` and titled
    `replay of <capture's time and band>`. Every replay of that capture joins it, so two
    handlers replaying one capture show merged and marked, as live siblings do. The group
    outlives its jobs: it stays until the operator closes it (a `×` on its title) or the
    page reloads. Its first-seen and last-seen times are the events' `t_ns`, and its rows do
    not dim or drop with the page's clock.
20. **A replay is no sibling of a live job.** It is left out of Send to's group, the merged
    view across live jobs, the outlines and the positioned-jobs feeds.
21. **The job list** shows a replay with the source `replay`, its handler, and when finished
    its note in place of `stopped`.

## Not in this plan

- A band inside the capture (a narrow mode at an offset in a wide recording). The replay
  takes the capture's own centre and width.
- An unpaced replay. A program fed faster than real time loses frames today.
- A replay superseding the signature its own capture taught: worth having, a later call.
- A handler flagged as unable to take a replay (none known).
- Replaying a capture held by another node other than through `--node`.
