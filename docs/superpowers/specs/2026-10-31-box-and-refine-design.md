# Plan 18: box a signal, fit it, refine it on the waterfall

Design spec, 2026-10-31. Brainstormed with the operator (the drag interaction and the fit-first
start are theirs; the mockup `.superpowers/brainstorm/*/content/drag-outline.html` is what they
approved); a reviewer read it as the builder before hand-over. Builds on plan 7 (the web page:
the box menu in `menu.js`, outlines and ribbons in `overlays.js`), plan 12 (`job_context.squelch`,
the plan-channel outline text) and plan 13 (channel plans). Written in the terms of
`crates/zeta-ui/web` (`menu.js`, `overlays.js`, `waterfall.js`, `agent.js` job rows, `graph.js`)
and `crates/zeta-core/src/node` (the lease manager, `job_graph_set`, the attached-job resume in
`recover.rs`).

## 1. Intent and why

The page can start a demodulator from a box today: draw a box on the waterfall, pick "Decode ›
NBFM audio" or a Transcribe entry, and an `fm_demod` starts at the box's exact width. That is where
the operator's crackly jobs come from: a box drawn by hand over a 12.5 kHz channel is 110-140 kHz
wide, which is about 10 dB of extra noise in the audio, and a little off the signal's centre. The
only way to fix either today is the graph modal's raw params JSON, which restarts the job but
cannot move its lease, or stopping the job and boxing again, which loses the transcribe attached to
it and its ribbons.

The operator's rule, stated twice in `TODO.md`: never assume channels sit on a fixed grid. The fix
therefore snaps to the signal, never to a raster, and otherwise leaves the band exactly where the
operator puts it.

With more than about three jobs on a capture it is also hard to see which job is on which
frequency; the outlines carry only the kind.

Success: a demod started from a box lands on the signal's measured centre and width; a running job
can be moved and resized by dragging its outline on the waterfall, keeping its id, its attached
transcribe and its ribbons; and every job on the capture is labelled where it sits, linked to its
row in the job list.

## 2. Goals, non-goals, anti-goals

### Goals

- Fit first: a demodulator started from a box (the `fm_demod`, `wfm_demod`, `am_demod`, `ssb_*`
  entries and their Transcribe twins) measures the box for a second and starts on the measured
  centre and occupied width. A box with only noise in it starts at the box as drawn, and the
  transcript says so.
- Refine by dragging: a running job's outline has a handle at each edge; dragging a handle resizes
  the band, dragging the body moves it. On release the job is retuned under the same id. Nothing
  snaps.
- A same-id retune in the node (`job_retune`), with the CLI, MCP and `ask` forms every operation
  has, so the agent can centre a demod too.
- An attached job survives its input's retune.
- Fit and width presets from a small menu on the outline or its tag: "Fit to signal", 2.5, 5,
  6.25, 12.5 and 25 kHz about the current centre, and Stop.
- A tag at every rigid job's frequency (kind, short id, squelch state, plan channel, last
  transcript snippet), linked both ways by hover with its row in the job list.

### Non-goals

- Live retune while dragging (a retune restarts the demodulator; the audio would stutter on every
  pixel). The ghost and the readout show where the job will land; release commits.
- Staircase transcript ribbons, fold-by-key for state-stream decoders, mode picking by band through
  `knowledge_lookup`, the LCARS theme: the other items of the UI wave, each its own plan.
- Keeping a lone job's capture centre off its channel in the lease (the DC problem): plan 15's
  follow-on; "Fit to signal" and dragging give the operator the manual fix meanwhile.
- Dragging a pin, a watch, a scan, an identify, an elastic job, or the capture outline. Pins move
  with `source pin`; the others are not retunable (section 3).
- Fitting a decoder (`aprs_decode`, `pocsag_decode`, ...): those start at their row's band as
  today. Only the audio demodulators fit, because only they take any width.

### Anti-goals

- A retune that silently retunes the radio under another job: a band outside the current capture
  goes through the lease manager like a `job_start` and a conflict is reported, the job left as it
  was.
- A grid: no snapping of centre or width to any raster, plan or preset while dragging. Presets are
  explicit menu choices.
- Losing the transcribe: the attached jobs of a retuned job must come back under their ids.

## 3. The node: `job_retune`

A new operation:

```
{"op":"job_retune","job":"<id>","center_hz":476875000,"bandwidth_hz":16000,"replace":[]}
zeta job retune --job <id> --center-hz 476875000 --bandwidth-hz 16000 [--replace ID,ID]
```

`replace` is as on `job_start`: holders that may be stopped to make room, stopped under the gate
right before the request, so a retune's `conflict` offers the same `retry with replace` option the
others do and the page shows the same retry buttons (my call, after the builder check: every field
is required and unknown fields are rejected, so the field is decided now, not added later).

Applies to a `running` job on the radio (not attached) that holds a rigid lease and is of a kind
the node expanded and can re-expand: the four demodulators, the decoders, `measure`, `passthrough`,
and the mode rows. Refused, naming the kind, for `scan`, `watch`, `identify`, `spectrum` (elastic
or not: the page's own follows the capture; a rigid spectrum is a `job_start` away), `transcribe`
(attached), and a job whose graph was replaced by an operator `graph put` (it launches as stored;
the error says to put a new graph or start a new job). A job that is not running is `job <id> is
<state>`; an unknown id is `unknown_job`.

Under the source's gate, the node:

1. Validates the band as `job_start` does for that kind (range, the kind's width rules, the
   source's capability record).
2. Releases the job's lease and leases the new band as a rigid request under today's rules:
   inside the current capture it is granted without a tune, whether or not another rigid job's
   band overlaps it (two demods may share a channel inside a capture today, and `identify`'s
   children rely on it; no overlap rule is added); outside it, it tunes when no other rigid job or
   pin holds the source, else it is a `conflict` with today's holders and options, and the old
   lease is put back, the job untouched.
3. Stops the job's run, re-expands the graph for the new tuning at the capture's rate, carrying
   `squelch`, `squelch_db`, `deviation_hz` and `sideband` from the stored graph, and relaunches
   under the same id. The adjacent-channel guard (`guard_hz`, `guard_db`, `channel_hz`, the
   channel's `wide_hz`) is re-derived from the channel plan at the new band, not carried: a
   carried `channel_hz` would keep the old channel's filter width, so a width preset would do
   nothing audible and a move to a neighbouring channel would keep the old geometry. A guard an
   operator turned off with `graph_put` (`guard_hz: null`) comes back on a retune onto a plan
   channel; the guide says so (my call). The record's `tuning` is the new band;
   `capture_rate_hz` stays the node's to re-expand later.
4. Relaunches every attached job whose `input` is this job (the `transcribe` chain) under its own
   id, after the input is running, as `recover.rs` does for a healed source. An attached job that
   cannot relaunch is `failed` with the reason, the retune still succeeds.

The result is the job's id, as `job_start` answers (its record is in `job_list`). The op is logged
on the `ops` stream like the others. A one-shot node that loaded a `running` record it did not
launch refuses with `job <id> is not running in this node (one-shot: use --node against zeta serve)`.

Today an attached job ends `stopped` when its input's stream closes for any reason, a `graph_put`
restart included; this plan narrows that: an input that is being retuned (or restarted by
`graph_put`, the same path) keeps its attached jobs, which are relaunched with it. The guide's
`job_attach` paragraph changes accordingly.

## 4. The page: fit first

The box menu's demodulator entries (`fm_demod`, `wfm_demod`, `am_demod`, `ssb_usb`, `ssb_lsb` and
their Transcribe twins) fit before they start:

1. The page starts a `measure` job on the box (centre and width as drawn), reads one `measure`
   event from its stream (up to 3 s), and stops it. The box's footer reads "measuring…".
2. From the event: `label.name`, `center_hz`, `bandwidth_hz` (the occupied band) and, for an
   `ssb` label, the dial frequency in its evidence.
3. If the label is `noise` or `unknown` with an SNR under 6 dB, or no event arrived, the demod
   starts at the box as drawn and the transcript gets `no signal found in <lo>-<hi> MHz; started
   <kind> at the box as drawn`.
4. Otherwise the demod starts at the measured centre (the dial for SSB) with `bandwidth_hz` the
   occupied width times 1.25, floored at the kind's minimum: 5 000 Hz for `fm_demod` and
   `am_demod`, 3 000 Hz for `ssb_demod`, 150 000 Hz for `wfm_demod`; and capped at the box as drawn
   (a box narrower than the signal is the operator's choice). The transcript gets `fitted <kind> to
   <centre> MHz, <width> kHz (<label>)`. The Transcribe twins attach as today once the demod runs.

   *Amended 2026-11-01 (plan 18's review):* `ssb_demod` is not fitted in width. Its band is
   centred on the dial and the `channel` stage low-passes at half the band, while USB voice
   occupies about dial+300..dial+2700 Hz: a 3 000 Hz band about the dial would pass ±1.5 kHz
   and cut the voice above 1.5 kHz. An SSB fit moves the dial only (the evidence's dial, else
   the occupied centre ∓ 1 500 Hz) and keeps the width the entry uses today (6 000 Hz), or a
   running job's own width for the outline menu's Fit.

`deviation_hz` and `sideband` are as the entry sets them today. A `measure` on a band outside the
capture retunes the radio like any rigid job; a conflict is reported as the entries report it
today and nothing starts.

The receiver's DC spike sits at the capture centre, and a box that contains it (ch20 under the
UHF CB pin is 18.75 kHz from it) would fit onto the spike whenever it is louder than the signal
during the measured second. `measure` therefore ignores the bins near a local hardware receiver's
capture centre; that change lives in plan 15 (`2026-10-31-scan-grid-mask-design.md`, section 5,
the same local-device gate as the scan's notch), and this plan's fit relies on it. The two plans
run side by side; the fit acceptance (section 10) is run once both are merged.

## 5. The page: drag to refine

Every outline the overlays draw for a running rigid job the node can retune (section 3's list;
the page knows the kind from `job_list`) becomes draggable in the wide footprint:

- A handle (`.outline-handle[data-edge=lo|hi]`, 8 px, `ew-resize` or `ns-resize` to match the
  waterfall's orientation) at each edge resizes; the body (`grab`) moves. Pointer events, with
  pointer capture, so the drag survives leaving the element.
- While dragging: the outline follows the pointer; a dashed ghost (`.outline-ghost`) stays on the
  old band; a readout (`.outline-readout`) under the outline gives the centre in MHz and the width
  in kHz, formatted as the box footer formats them, and "release to retune". The width cannot go
  under 1 000 Hz; the band is otherwise unconstrained (the node decides what fits).
- On release with a change: `job_retune` with the new band. Success: the ghost and readout go,
  the outline follows the next `job_list`. Failure (a conflict, a refusal): the outline snaps back
  to the ghost and the transcript gets the error with its corrective detail, as the box menu
  reports job errors. A drag of under 3 px is a click, not a retune.
- A right-click (or long press in the narrow footprint) on an outline or its tag opens a small
  context menu (`menu.js` infrastructure, a second entry table): "Fit to signal", "2.5 kHz", "5
  kHz", "6.25 kHz", "12.5 kHz", "25 kHz", "Stop". Fit runs section 4's measure on the job's
  current band and retunes to the result (or reports `no signal found in the band; left as it
  is`); a width entry retunes to that width about the current centre; Stop is `job_stop`.
- Elastic jobs, pins, the capture outline, watches, scans and identify runs draw as today, without
  handles.

In the narrow footprint (phones) dragging is off (the touch gestures pan the waterfall); the
long-press menu carries Fit, the presets and Stop.

## 6. The page: tags and row linking

Each running rigid job on the shown source gets a tag (`.job-tag[data-job]`) at its centre
frequency, replacing the outline's bare kind label: `<kind> <short id> <squelch dot> <channel>
<snippet>`:

- `short id`: the last six characters of the job id, which is what the job list shows.
- the squelch dot: green while `job_context.squelch.open` is true, grey while false, absent for a
  kind without a squelch; the overlays already watch the job's event stream and see the `squelch`
  events, and read `job_context.squelch` once on first sight for a job whose gate changed before.
- `channel`: the plan channel text the outline carries today (`ch20`), when the view's `plan`
  policy and the band give one.
- `snippet`: the last transcript text of a transcribe attached to the job (the overlays already
  route transcript events to ribbons), truncated to about 40 characters; absent until one arrives.

Tags that would overlap stack like the layer-1 labels do. Hovering a tag or an outline adds `hot`
to its job row in the list (the row's `data-id`, with `data-job` added as an alias); hovering a row
adds `hot` to its tag and outline. Clicking a tag scrolls the row into view. Job rows gain the job's centre in MHz
and width in kHz if they do not show them today (the builder checks `agent.js`).

## 7. Documentation

- `AGENTS.md` (`crates/zeta/src/agents_md.rs`): `job_retune` under Operations with its CLI form
  (`replace`, the carried params, the re-derived guard);
  `job_attach` says an attached job follows its input through a retune or a `graph_put`; field
  craft "Decoding APRS and reading measurements" or a new short "Fitting a demod to a signal":
  measure, then start or retune on the measured centre with the occupied width times 1.25, never
  on a raster.
- `zeta-mcp` exposes `job_retune`; `zeta ask`'s tool list follows the schema automatically (check).
- The plan-series table marks plan 18; the status section records the live acceptance.
- `TODO.md`: the "box a signal" gist closes; the remaining UI-wave gists stay.

## 8. Tests

Node (Rust, `crates/zeta-core` and `crates/zeta/tests`):

- `job_retune` inside the capture: no tune, same id, `tuning` updated, the channel node's params
  follow, `squelch_db` set by an earlier `graph_put` carried.
- Outside the capture with no other holder: the source tunes; with another rigid holder: a
  `conflict` offering `replace`, the job still running on its old band, the old lease back; with
  `replace` naming that holder, the holder stops and the retune succeeds.
- Inside the capture onto a band another rigid job holds: granted (today's rule).
- A retune onto a plan channel re-derives the guard; a width preset on a plan channel changes the
  channel node's `wide_hz` and the demod's `channel_hz`.
- Refusals: `scan`, `watch`, `identify`, an elastic `spectrum`, an attached `transcribe`, an
  operator-graph job, a stopped job, an unknown id.
- An attached `transcribe` survives its input's retune under its own id (and, since the path is
  shared, a `graph_put` on the input); the ops stream shows the retune.
- CLI: `zeta job retune` one-shot and `--node`; MCP tool present; `schema` lists the op.

Page (UI e2e, `crates/zeta-ui/tests/e2e/ui.py`, Playwright):

- Fit first: a box drawn wide around the `tone` fixture's signal and "Decode › NBFM audio"
  produces an `fm_demod` whose `tuning` is the measured centre with a width well under the box;
  a box on a new `quiet` white-noise fixture (the `retune` fixture is a pure tone, whose filter
  leakage would not measure as `noise` deterministically) starts at the box and the transcript says
  so.
- Drag: dragging the `hi` handle of the outline inwards sends `job_retune` (read from the ops
  stream) with the narrower band and the outline follows; dragging the body moves the centre; a
  drag outside the capture while another rigid job holds the `retune` fixture snaps back, the
  transcript shows the conflict and the "stop X and retry" button retunes with `replace`.
- Tags: a job's tag shows the kind and short id; hovering it marks its row `hot` and back.
- Menu: "12.5 kHz" retunes to that width about the centre.

The suite runs near its 300 s deadline today (plan 17 left it at about 235 s). The builder either
raises the harness deadline with the added scenarios' measured cost or splits the suite into two
`cargo test` targets; either is acceptable, say which in the plan.

## 9. Left to the builder

- The exact pointer-event wiring; the waterfall's `hzForY` is the inverse mapping to use.
- How the attached-job relaunch is factored between `job_graph_set` and the retune path (one
  helper, two callers; `recover.rs` is record-driven and stays as it is).
- Whether `job_retune` reuses `job_start`'s validation by constructing a `JobStart` op internally
  or by a shared function.
- The tag stacking rule when more than three tags fall within one tag's width.
- Whether a `measure` for fit runs for one event or until a `partial: false` one (at most 3 s
  either way).

## 10. Live acceptance

On the desktop HackRF with the 2 m / 70 cm antenna, UHF CB pinned at 476 918 750 / 1 000 000 Hz:

- Box a transmitting channel generously (about 100 kHz) and start "Transcribe › NBFM": the job
  lands within 500 Hz of the channel's centre with a width of 10-16 kHz, the audio is clean, the
  transcript arrives. The transcript pane shows the `fitted` line.
- Drag the outline a channel over; the audio follows after the retune, the transcribe stays
  attached (same id in the job list), the ribbons keep scrolling.
- Drag its edge to 25 kHz and back to 12.5 kHz with the presets; `zeta ops tail` shows each
  `job_retune`.
- Unpin, then drag a job outside the capture while another demod holds the radio: the conflict
  shows in the transcript, the outline snaps back, and the retry button moves it.
- With four demods running, each tag reads its kind, id, squelch dot and channel; hovering a row
  lights its tag.
- `zeta job retune` from the CLI against the serving node moves a job the page shows.
