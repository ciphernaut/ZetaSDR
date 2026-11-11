// overlays.js: channel-pinned overlays on the waterfall: layer labels, text ribbons, the
// outlined bands of the rigid holders, the decoded-event counters of the job rows and their
// squelch tags.
//
// Exports:
//   mountOverlays(el, wf, api, {jobsEl, plan, applyOp, footprint, notice, requestPoll}) -> Overlays
//       Adds a pointer-transparent layer over the waterfall canvas inside `el` (the
//       #waterfall element `wf` was mounted in). `jobsEl` (default #jobs) holds the job rows
//       agent.js renders; their `.events` cell carries the counters. `plan` (default `auto`)
//       returns the view's `plan` policy: an outline on a plan channel reads `ch20 fm_demod`
//       (plans.js); `ov.syncOutlines` is called again when the policy changes. `applyOp(op)
//       -> Promise<result | null>` is app.js's (null: failed and already reported, a
//       `conflict` with its retry buttons); a dragged outline's `job_retune` goes through it.
//       `footprint()` returns "wide" or "narrow" (<html data-footprint>): only the wide
//       footprint's outlines can be dragged. `onOutlineMenu(job, x, y)` is called with a
//       retunable outline's job and a client point: by `contextmenu` on the outline (wide), or
//       by a LONG_PRESS_MS press on its `.job-tag` in the narrow footprint (cancelled by
//       pointerup/pointercancel or a move over LONG_PRESS_MOVE_PX). `notice(text)` puts an
//       error line in the transcript (app.js: the shown source has no feed).
//   Job tags: each running rigid job's outline holds a `.job-tag[data-job]` (ov.tag/untag):
//       `.kind`, `.short` (the last 6 characters of the id), a `.dot.open|closed` (only for a
//       SQUELCH_KINDS job with a known squelch state; refreshed wherever the squelch is
//       set), `.channel` (the plan channel, else empty) and `.snippet` (the last transcript
//       text, SNIPPET_CHARS at most, of a `transcribe` job attached to it; absent until one
//       arrives). Tags sit at their outline's vertical centre and ov.layoutTags stacks them
//       (a tag under the previous one's bottom + TAG_GAP_PX is pushed down, MAX_LANES at
//       most). Hovering a tag or its outline puts class `hot` on both and on the job's row;
//       hovering the row (agent.js `onHot` -> ov.setHot(id, on)) does the same the other way,
//       and `ov.hotJob` is re-applied after each sync, as agent.js re-renders the rows. A tag
//       click scrolls the row into view. `speaking` is the same on the jobs a mix hears now
//       (app.js hands mix.js activeIds() to ov.setSpeaking(ids): class `speaking` on their
//       outline, tag and row, styled apart from `hot`; `ov.speaking` is re-applied after each
//       sync and whenever an outline or tag is made, as `hotJob`). The pin, the capture and
//       the ghost keep a plain `.outline-label`; job outlines have none. Each tag also has a
//       `.centre` (the tuning centre in MHz, three decimals). A `video_decode` job's tag also
//       holds a `canvas.video-thumb[data-job]` (64x48) for its live picture: made with the tag,
//       it is handed to `ov.onTagMedia(id, canvas, true)` and taken back with
//       `ov.onTagMedia(id, null, false)` when the tag goes (video.js draws it); a click on it
//       calls `ov.onThumbClick(id)` instead of the tag's. Its `video` events (a boolean
//       `locked`) are the tag's snippet (their `summary`) and go to `ov.onVideoLock(id, event)`,
//       never a ribbon.
//   The rail: ov.setRail(el) (app.js hands it wf.rail in the console theme, null in plain)
//       moves every tag out of its outline into `el` at once (null moves them back), and new
//       tags are made there. A rail tag's top is its outline's vertical centre less half its
//       height, clamped to the rail; a tag whose outline is hidden or wholly off the shown
//       band is hidden (never pinned at the edge). Sorted by centre, a tag whose full extent
//       overlaps the last full tag's (TAG_GAP_PX apart at least) gets class `thin`
//       (RAIL_THIN_PX high, centred on its frequency, in the rail's end strip, text hidden by
//       CSS), unless it is `hot` or `speaking`: then it stays full and the full tags it
//       overlaps go thin instead. Every thin tag is named (title and aria-label: its channel
//       on a plan, else its kind, and its centre); the first of a run of more than
//       RAIL_GROUP_MIN thin tags has the run's count appended. ov.setHot and ov.setSpeaking
//       lay the rail out again. A pass over many outlines (ov.layout, ov.sync,
//       ov.syncOutlines) lays the tags out once at its end (ov.batchTags).
//   Colour: channelColour(jobId) -> 1..6 (the sum of the id's char codes mod 6, plus 1) is
//       the `data-colour` of the job's outline, tag and ribbons (an attached job's ribbons take
//       its input's), and agent.js puts it on the job's row; style.css maps it to
//       --c-channel-<n>.
//   Overlays:
//     ov.syncOutlines(jobs, source)  the outline half of ov.sync alone (app.js calls it after
//                            each `source_list` poll, so a pin shows without waiting for jobs)
//     ov.setSource(id)       the selected source: changing it closes the old source's feed,
//                            drops every counter, squelch gate, label, ribbon and outline
//                            (they belong to the previous source's band) and opens the new
//                            source's feed (below)
//     ov.sync(jobs, source)  a `job_list` result (from the 2 s poll) and the selected
//                            source's `source_list` record: outlines (ov.outline,
//                            key = job id, text = kind) the tuning of every running rigid job
//                            on the selected source (spec 4.2: not elastic, not attached to
//                            another job's output, not a time-shared `scan`) and the source's
//                            `pin` (key "pin", text "pin"; plan 12 spec 4.1), and unoutlines
//                            the rest; with none the capture itself (wf.band, key
//                            "capture", text "capture"; not in scan or sweep mode) is outlined,
//                            following the band as it changes. Then the job-row counters
//                            of every running job on the selected source (`spectrum` jobs
//                            skipped: their events are the waterfall's) are refreshed, a
//                            job no longer running is forgotten (counter, gate, label,
//                            marks), and the squelch gates are seeded (below).
//                            The feed: every event of every job on the selected source
//                            comes on one stream, the source's feed (in GET /streams the
//                            stream of kind `event` with provenance `job` null and `source`
//                            the selected one), one text-mode socket however many jobs run
//                            (an attached `transcribe` job's too: its source is its
//                            input's). It is opened by setSource; a node that lists no
//                            feed for the source (an older node) is noted once per source,
//                            `no event feed for <id>; job events are not shown`, through
//                            `notice`, and asked again on each ov.sync (outlines still
//                            draw). After a drop the feed id is resolved again by the same
//                            rule. Each frame names its job: a job running on the source in
//                            the last poll (ov.lastJobs) whose mode row is settled is
//                            rendered (below); any other frame goes to ov.hold: it joins
//                            `pending` (at most PENDING_MAX, the oldest out first) and a
//                            poll is asked for (opts.requestPoll, one in flight); after
//                            each sync the held frames whose job is now running with its
//                            row settled are rendered in order, and those of a job a
//                            fresh poll does not list as running are dropped. A `lagged`
//                            message, or an open of the feed after its first (a
//                            reconnect), is a hole in every job's events at once:
//                            ov.reseedAll forgets every known squelch gate and seeds them
//                            again; the counters stand. Seeding: each SQUELCH_KINDS job
//                            whose gate is unknown has its `job_context` asked, once the
//                            feed is open, at most SEED_IN_FLIGHT reads in flight, each
//                            completion sending the next (so the reads chain within a
//                            poll); a job whose context holds no gate (its squelch off) or
//                            whose read failed is not asked again until its graph changes
//                            or ov.reseedAll runs.
//                            Then the positioned jobs: the MAX_POSITIONED most recent
//                            running jobs on the other sources whose kind is a mode row
//                            with a `position` map or a `subject` (spec 7.2: the map follows
//                            every positioned job, e.g. a decoder on a second radio while
//                            this page shows the watching one; plan 23: so does the Subjects
//                            pane) are subscribed too, for ov.onEvent only: no outline,
//                            ribbon, label, mark or counter. Every running kind goes to
//                            the `subjects` store's row cache (store.lookup); a job is
//                            subscribed only once its row is known, so an event never
//                            arrives before it, and what the lookup learns is applied at
//                            once by a second sync. A positioned job whose source becomes
//                            the selected one is left to the feed.
//     ov.follows(jobId) -> bool  whether the job's events reach ov.onEvent: a running job on
//                            the selected source (the feed), or a positioned job on another
//                            source subscribed here; false for any other (a `bits` job on
//                            source `node`, a plain job on another source)
//     ov.watching(jobId) -> bool  false for a positioned job on another source over
//                            MAX_POSITIONED, so its events are not seen (the Subjects pane
//                            says so); true for every job on the selected source
//     ov.render(job, payload, {positionOnly})  draws one event of `job` by payload shape (so
//                            by `protocol`); `positionOnly` (a positioned job's) stops after
//                            ov.onEvent:
//         - `db` array (spectrum): ignored;
//         - `protocol: squelch`: not counted, never drawn; on an fm_demod or am_demod job
//           (agent.js SQUELCH_KINDS; any other job's is ignored) kept as the
//           job's last squelch event (ov.squelch, forgotten when the job's graph changes)
//           and the row's tag set from it at once (agent.js setSquelchTag; the 2 s
//           re-render keeps it through agent.js); the seed (above) reads the latched
//           `squelch` event of the job's `job_context` unless a live one came first (the
//           gate changes rarely, so a page that came late would otherwise miss it);
//         - `protocol: origin` (the node's origin answer for the job): never drawn, not
//           counted, not ov.lastEvent (ov.onEvent hands it to the map and the transcript);
//         - every other event bumps the job row's counter;
//         - `label` and `center_hz` (measure): ov.label(center_hz, label, job.id), replacing
//           that job's previous label;
//         - `summary` (a string; every decoder event the node envelopes from its mode
//           row, with `protocol` and `t_ns`): ov.ribbon(tuning centre, summary, class =
//           `protocol`); the node filled it, the page renders it as is;
//         - `protocol` (an event without a `summary`: a stage with no mode row; the
//           fallback texts below), at the tuning centre, class = the protocol:
//             `pocsag`: "<address>: <text>" ("<address>: tone" for kind tone);
//             `rds`:    "<ps ?? pi> · <rt ?? ''>", trimmed;
//             `acars`:  "<reg> <label>: <text>";
//             `transcript`: "<text>", class `transcript`, tooltip "<language> · <duration_ms>
//                       ms"; a `dropped` one "[dropped]" and an `error` one
//                       "[error] <error>", class `transcript-error`;
//             `vad`:    counted only, never drawn;
//           any other protocol falls through to the branches below. An attached job's
//           ribbons sit at its own tuning, which equals its input's;
//         - `src` and `info` (APRS): ov.ribbon(tuning centre, "<src> > <dst>: <info>",
//           "aprs");
//         - anything else: ov.ribbon(tuning centre, first OTHER_EVENT_CHARS chars of the
//           payload's JSON, "event").
//     ov.ribbon(hz, text, cls) -> element
//         A text ribbon at wf.yForHz(hz) (the band's middle when hz or the band is unknown;
//         ribbons and labels are placed again when the waterfall's view changes, wf.onView),
//         entering at the right edge and moving left with the waterfall's columns
//         (wf.onColumn); removed when it leaves the left edge or RIBBON_TTL_MS after it was
//         last refreshed. The same text and class still at the right edge is refreshed with a
//         ×N count instead of drawn again. `top` is the ribbon's centre line (style.css's
//         translateY(-50%) centres the box on it, rotated by --ribbon-tilt about its centre;
//         `right` and `top` stay the untilted box). A ribbon is in lane `lane` (0: on the band
//         line, yFor(hz)); its top is yFor(hz) + lane * RIBBON_STEP_DIR * (h + RIBBON_GAP_PX).
//         A new one takes lane 0 and every unkeyed ribbon it overlaps (at the right edge,
//         and, in turn, every one those overlap) steps one lane on, animated over
//         RIBBON_STEP_MS; one that would pass MAX_LANES or reach another job band's base lane
//         (its centre line plus or minus half a lane) keeps its lane and gets class `clipped`,
//         its tail under the newer ribbon (newer ribbons draw higher). On each layout a ribbon
//         whose next lane down is clear falls back one lane. A keyed ribbon never steps nor
//         falls: a new one takes the first lane free of overlap. A job's ribbon (an attached
//         job's: its input's) carries that job's `data-colour` (channelColour) and band; one
//         with no job takes the narrowest job outline holding hz, none when no job band does.
//         The layer isolates its stacking (style.css), so the newer-on-top z-index stays in it.
//         At most MAX_RIBBONS are kept: the oldest goes first.
//     ov.label(hz, {layer, name, confidence, evidence}, key = "") -> element
//         "L<layer> <name> <confidence, 2 dp>" at the left edge at wf.yForHz(hz); a layer-2
//         label one line (its height + LABEL_GAP_PX) lower, so it stacks under the layer-1
//         label drawn at the same hz. One label per key (a job id, a classification slot),
//         replaced on each call. `evidence` becomes its tooltip; `data-layer` the layer.
//     ov.unlabel(key)        removes the label drawn under `key`, if any
//     ov.outline(lo, hi, text, key, job = null) -> element
//         A band lo..hi Hz across the waterfall (`.outline`, data-key = key) bordered at its
//         edges, `text` its label (`.outline-label`) at its top right; one per key, replaced
//         on each call, placed again when the view changes. Drawn under labels and ribbons,
//         pointer-transparent. With a `job` that ov.retunable allows, in the wide footprint,
//         it is draggable (class `retunable`, cursor grab): two `.outline-handle` children
//         (data-edge `hi` at the top edge, `lo` at the bottom, HANDLE_PX tall, centred on the
//         edge, ns-resize) resize that edge, the body moves the band. A band shorter than
//         3 × HANDLE_PX (12.5 kHz at a 1 MHz view) gets class `handles-out`: its handles sit
//         outside it, `hi` ending at its top edge and `lo` starting at its bottom one, so the
//         whole band stays body. A press (button 0)
//         captures the pointer; each move maps the pointer's y through wf.hzForY: a move
//         shifts both edges by the Hz travelled, an edge follows it, clamped so the band
//         stays MIN_WIDTH_HZ wide (no snapping to any raster). Past DRAG_CLICK_PX of travel a
//         `.outline-ghost` (dashed, no label) stays on the old band and a `.outline-readout`
//         under the outline reads "<bandText> · release to retune". A release before that is
//         a click and sends nothing; after it `job_retune` (the dragged centre and width,
//         rounded to Hz, replace []) goes through applyOp: null (a refusal or a conflict,
//         whose line offers "stop <kind> <id> and retry") snaps the outline back to the
//         ghost's band, a result keeps the dragged band until the next `job_list`; ghost and
//         readout go either way. `pointercancel` snaps back and sends nothing. While a drag
//         runs (ov.drag) the polls and the view changes leave that outline where the pointer
//         has it, and a wheel over a draggable outline still zooms the waterfall. In the
//         narrow footprint no handles are made and the body stays pointer-transparent; pins,
//         the capture, elastic jobs, scans, watches and identify runs are never draggable.
//     ov.retunable(job) -> bool  a running rigid job (as outlined) the node can move with
//                            `job_retune`: no operator graph, kind not in NOT_RETUNABLE
//     ov.unoutline(key)      removes the outline drawn under `key`, if any
//     ov.span(lo, hi, startNs, endNs, text, key) -> element|null
//                            a thin bracket (class ov-span) over lo..hi Hz between the columns
//                            of startNs and endNs (wf.xForNs), tagged `text`; re-laid on every
//                            column and view change and removed (null) once endNs left the columns
//     ov.unspan(key)         removes the span drawn under `key`, if any
//     ov.squelch(jobId) -> event|null  the last `squelch` event seen for a job on the
//                            selected source
//     ov.lastEvent(jobId) -> ms|null  performance.now() of the last event counted in the job's
//                            row (agent.js folds idle plan channels on it)
//     ov.lastStationEvent(jobId) -> ms|null  performance.now() of the last counted event of a
//                            job whose row has `subject.station` (plan 35) that is a fixed
//                            station's frame: matching `station.when` (subjects.js
//                            matchesPairs) or with a non-blank `addressed` field; null before
//                            one and for every other row (app.js fades the map's lit sets
//                            from it, else from ov.lastEvent)
//     ov.activity(jobId) -> {watched, speaking, hot}  what the page knows of the job now
//                            (watched: running on the selected source, so on the feed)
//     ov.onEvent(fn)         fn(job, payload) for every non-spectrum event of a job on the
//                            selected source,
//                            before it is drawn, and of a positioned job on another source
//                            (the map reads `position` from them)
//     ov.jobRow(job, counter)  shows `counter` (when > 0) in that job's row `.events` cell
//     ov.destroy()

import { SQUELCH_KINDS, setSquelchTag } from "./agent.js";
import { channelLabel } from "./plans.js";
import { matchesPairs, subjectLabel, subjectSummary } from "./subjects.js";
import { bandText } from "./units.js";

export const RIBBON_TTL_MS = 60_000;
export const OTHER_EVENT_CHARS = 80;
export const MAX_POSITIONED = 4;
// At most this many `job_context` reads seeding squelch gates are in flight at once; the
// reads chain, each answer sends the next.
export const SEED_IN_FLIGHT = 4;
// Feed frames held for a job no poll has listed yet; the oldest go first.
export const PENDING_MAX = 200;
export const MAX_RIBBONS = 200;
const EXPIRE_EVERY_MS = 1000;
const KEYED_RIBBON_MS = 500;
const RIBBON_GAP_PX = 2;
const LABEL_GAP_PX = 2;
const MAX_LANES = 8;
// Ribbon collisions. A new ribbon takes its band's line; the ribbons it overlaps step one lane
// away in RIBBON_STEP_DIR (-1 up, +1 down) over RIBBON_STEP_MS, at most MAX_LANES lanes and never
// into another band's base lane (they clip instead), and fall back as the line clears. Keyed
// (subject) ribbons never step. To return to the pre-plan-24 behaviour set RIBBON_STEP_DIR = 1
// and RIBBON_STEP_MS = 0.
export const RIBBON_STEP_DIR = -1;
export const RIBBON_STEP_MS = 160;
const TAG_GAP_PX = 2;
const RAIL_THIN_PX = 5; // a collapsed rail tag's height (style.css --rail-thin-px)
const RAIL_EXPAND_MS = 3000; // a tapped narrow rail segment shows its label this long
const RAIL_GROUP_MIN = 6; // a cluster with more thin tags than this titles the first with the count
export const SNIPPET_CHARS = 40;
const CAPTURE_KEY = "capture";
const PIN_KEY = "pin";
// Kinds job_retune refuses (an attached `transcribe` follows its input instead).
export const NOT_RETUNABLE = ["spectrum", "scan", "identify", "watch", "transcribe", "mix"];
export const HANDLE_PX = 8; // an edge handle's height (style.css .outline-handle)
export const DRAG_CLICK_PX = 3; // less pointer travel than this is a click, not a drag
export const LONG_PRESS_MS = 500; // a press this long on a narrow outline's tag opens its menu
export const LONG_PRESS_MOVE_PX = 10; // a press that moves further than this is no long press
export const MIN_WIDTH_HZ = 1000; // the narrowest band an edge drag leaves

// The colour slot 1..6 of a job: stable across reloads and the same in every view.
export function channelColour(jobId) {
  const id = String(jobId ?? "");
  let sum = 0;
  for (let i = 0; i < id.length; i++) sum += id.charCodeAt(i);
  return (sum % 6) + 1;
}

const isRunning = (j) => !!(j && j.state && j.state.status === "running");

// Whether `job` (a running `job_list` record) holds its source rigidly with a usable tuning.
// A job on source `node` (a mix, a `bits` job replaying a capture) holds no radio and draws
// no outline, whatever band its record carries.
function rigidBand(job) {
  const t = job.tuning;
  return (
    job.source !== "node" &&
    !job.elastic &&
    !job.input &&
    job.kind !== "scan" &&
    !job.owner &&
    !!t &&
    typeof t.center_hz === "number" &&
    typeof t.bandwidth_hz === "number" &&
    t.bandwidth_hz > 0
  );
}

export function mountOverlays(el, wf, api, opts = {}) {
  return new Overlays(el, wf, api, opts);
}

// What identifies a job's streams besides its id: `graph_put` restarts it with new ones.
function graphKey(job) {
  return JSON.stringify(job.graph ?? null);
}

const present = (v) => v !== null && v !== undefined && v !== "";

// The ribbon text for a decoder event by its `protocol` (null for a protocol not known here).
export function protocolText(p) {
  switch (p.protocol) {
    case "pocsag":
      return p.kind === "tone" ? `${p.address}: tone` : `${p.address}: ${p.text ?? ""}`;
    case "rds":
      return `${p.ps ?? p.pi} · ${p.rt ?? ""}`.trim();
    case "acars":
      return `${p.reg} ${p.label}: ${p.text ?? ""}`;
    default:
      return null;
  }
}

// The ribbon for a `transcript` event: {text, cls, title}.
export function transcriptRibbon(p) {
  if (p.dropped) return { text: "[dropped]", cls: "transcript-error", title: "segment dropped" };
  if (present(p.error)) return { text: `[error] ${p.error}`, cls: "transcript-error", title: "" };
  const ms = typeof p.duration_ms === "number" ? `${p.duration_ms} ms` : "? ms";
  return { text: String(p.text ?? ""), cls: "transcript", title: `${p.language ?? "?"} · ${ms}` };
}

function tuningCentre(job) {
  return job && job.tuning && typeof job.tuning.center_hz === "number"
    ? job.tuning.center_hz
    : null;
}

class Overlays {
  constructor(
    el,
    wf,
    api,
    {
      subjects = null,
      jobsEl = document.getElementById("jobs"),
      plan = () => "auto",
      applyOp = async () => null,
      footprint = () => "wide",
      onOutlineMenu = () => {},
      notice = () => {},
      requestPoll = () => Promise.resolve(),
    } = {},
  ) {
    this.plan = plan;
    this.notice = notice;
    this.requestPoll = requestPoll;
    this.applyOp = applyOp;
    this.footprint = footprint;
    this.onOutlineMenu = onOutlineMenu;
    this.press = null; // the narrow long press in progress: {timer, x, y}
    // The outline drag in progress: {key, job, mode: "move"|"lo"|"hi", y0 (layer px), lo0,
    // hi0 (the ghost's band), lo, hi (the dragged band), moved, sending (job_retune sent)}.
    this.drag = null;
    this.ghostEl = null; // the dragged outline's `.outline-ghost`, past DRAG_CLICK_PX
    this.readoutEl = null; // its `.outline-readout`
    this.lastJobs = [];
    this.wf = wf;
    this.api = api;
    this.jobsEl = jobsEl;
    this.sourceId = null;
    // The shown source's feed: {id, source, sub, opened (it has opened once)}, or null.
    this.feed = null;
    this.feedOpening = null; // the source whose feed a GET /streams is resolving
    this.feedNoted = new Set(); // sources noted as having no feed
    this.hideFeeds = false; // zetaTest.hideFeeds: resolve as if no feed were listed
    this.pending = []; // feed frames held until their job is known (ov.hold)
    this.pollAsked = false; // a poll requested by ov.hold is in flight
    this.pollAgain = false; // another is wanted once it ends
    this.seeding = new Set(); // job ids whose `job_context` squelch read is in flight
    // job id -> graphKey of a job whose context answered with no gate (its squelch off) or
    // whose read failed: not asked again until its graph changes or ov.reseedAll runs.
    this.gateless = new Map();
    this.reseeds = 0; // reseedAll runs (zetaTest.reseeds)
    this.staleSeeds = 0; // reads discarded for answering after a reseedAll
    this.seedHold = null; // zetaTest.holdSeeds: a promise every read awaits once answered
    this.seedGen = 0; // bumped by ov.reseedAll: a read issued before it is discarded
    this.counters = new Map(); // job id -> non-spectrum events seen on the feed
    this.lastEvents = new Map(); // job id -> performance.now() of its last counted event
    this.stationEvents = new Map(); // job id -> performance.now() of its last station frame
    this.hotJob = null; // the job id whose tag, outline and row show `hot`
    this.speaking = []; // the job ids a mix is playing now: `speaking` on each
    this.snippets = new Map(); // input job id -> the last transcript text of its attached job
    this.squelches = new Map(); // job id -> {event: the last `squelch` event seen, graph}
    this.labels = new Map(); // key -> {el, hz}
    this.outlines = new Map(); // key -> {el, labelEl (pin, capture) or tag, lo, hi, job, handles}
    this.spans = new Map(); // key -> {el, lo, hi, startNs, endNs}: a capture's cached region
    this.marks = new Map(); // `${job id}:${rounded centre}` -> {el, hz}: a watch's `signal new`
    this.captureOutlined = false; // the capture is outlined (no rigid holder, no pin)
    this.sourceRec = null; // the selected source's last `source_list` record (its `pin`)
    this.ribbons = []; // {el, bodyEl, countEl, text, cls, hz, band, w, h, top, lane, right, born, count, key, held}
    this.eventFns = []; // ov.onEvent listeners
    this.positioned = new Map(); // job id -> {job, ids, subs, closed, positionOnly: true}
    this.resolvingPositioned = false; // a GET /streams for unwatched positioned jobs
    this.subjects = subjects; // the row cache and the subjects (subjects.js)
    this.overCap = new Set(); // positioned job ids on other sources over MAX_POSITIONED
    this.destroyed = false;
    // Above zero inside a pass over many outlines (ov.layout, ov.sync, ov.syncOutlines):
    // layoutTags only marks the tags dirty, and the pass lays them out once at its end.
    this.tagBatch = 0;
    this.tagsDirty = false;
    this.rail = null; // the element the job tags live in (ov.setRail), null: their outlines

    this.layer = document.createElement("div");
    this.layer.className = "ov-layer";
    this.layer.style.setProperty("--ribbon-step-ms", `${RIBBON_STEP_MS}ms`);
    el.append(this.layer);

    wf.onColumn((shift) => this.scroll(shift));
    wf.onView(() => this.layout());
    this.timer = setInterval(() => this.expire(), EXPIRE_EVERY_MS);
    this.ro = new ResizeObserver(() => this.layout());
    this.ro.observe(this.layer);
  }

  // ---- jobs ----------------------------------------------------------------------------

  setSource(id) {
    if (id === this.sourceId) return;
    this.sourceId = id;
    this.seedGen++; // a read issued on an earlier visit must not apply
    this.closeFeed();
    this.pending = [];
    this.counters.clear();
    this.lastEvents.clear();
    this.stationEvents.clear();
    this.squelches.clear();
    this.gateless.clear();
    for (const [jobId, rec] of [...this.positioned]) {
      if (rec.job.source === id) this.unwatchPositioned(jobId);
    }
    for (const r of this.ribbons) r.el.remove();
    this.ribbons = [];
    for (const l of this.labels.values()) l.el.remove();
    this.labels.clear();
    for (const key of [...this.marks.keys()]) this.unmark(key);
    for (const key of [...this.outlines.keys()]) this.unoutline(key);
    for (const key of [...this.spans.keys()]) this.unspan(key);
    this.captureOutlined = false;
    this.openFeed();
  }

  sync(jobs, source) {
    // A poll's list is a new array; the lookup's second sync passes the last one again.
    const fresh = jobs !== this.lastJobs;
    this.batchTags(() => {
      if (this.destroyed) return;
      this.syncOutlines(jobs, source);
      const want = jobs.filter(
        (j) => j.source === this.sourceId && j.kind !== "spectrum" && isRunning(j),
      );
      this.syncMarks(want);
      const ids = new Set(want.map((j) => j.id));
      const held = [...this.counters.keys(), ...this.squelches.keys(), ...this.gateless.keys()];
      for (const id of new Set(held)) {
        if (!ids.has(id)) this.forget(id);
      }
      this.lookup(jobs);
      // A source without a feed (an older node, or one listed late) is asked again each poll.
      if (!this.feed) this.openFeed();
      for (const j of want) {
        // A `graph_put` restarted the job (perhaps with its squelch off): forget the old state.
        const sq = this.squelches.get(j.id);
        if (sq && sq.graph !== graphKey(j)) {
          this.squelches.delete(j.id);
          setSquelchTag(this.jobRowEl(j), null);
          this.refreshTag(this.outlines.get(j.id));
        }
        this.jobRow(j, this.counters.get(j.id) || 0);
      }
      this.seedPass(want);
      // agent.js re-rendered the rows: the hovered job's row is hot again.
      if (this.hotJob) this.setHot(this.hotJob, true);
      this.setSpeaking(this.speaking);
      this.syncPositioned(jobs);
      this.replayPending(fresh);
    });
  }

  // What the page holds for a job on the shown source that is no longer running.
  forget(jobId) {
    this.counters.delete(jobId);
    this.lastEvents.delete(jobId);
    this.stationEvents.delete(jobId);
    this.squelches.delete(jobId);
    this.gateless.delete(jobId);
    this.unlabel(jobId);
    for (const key of [...this.marks.keys()]) if (key.startsWith(`${jobId}:`)) this.unmark(key);
  }

  // ---- the shown source's feed ---------------------------------------------------------

  // The id of `source`'s feed in a GET /streams answer: kind `event`, no job, that source.
  async findFeed(source) {
    const streams = await this.api.streams();
    if (this.hideFeeds) return null;
    const f = streams.find(
      (x) =>
        x.kind === "event" &&
        x.provenance &&
        x.provenance.job === null &&
        x.provenance.source === source,
    );
    return f ? f.id : null;
  }

  // Subscribes the shown source's feed, once; a source the node lists no feed for is noted
  // once and tried again on each sync.
  async openFeed() {
    const source = this.sourceId;
    if (this.destroyed || source === null || this.feed || this.feedOpening === source) return;
    this.feedOpening = source;
    let id;
    try {
      id = await this.findFeed(source);
    } catch {
      return; // a failed request: the next sync asks again
    } finally {
      if (this.feedOpening === source) this.feedOpening = null;
    }
    if (this.destroyed || this.sourceId !== source || this.feed) return;
    if (id === null) {
      if (!this.feedNoted.has(source)) {
        this.feedNoted.add(source);
        this.notice(`no event feed for ${source}; job events are not shown`);
      }
      return;
    }
    const feed = { id, source, sub: null, opened: false };
    feed.sub = this.api.streamText(id, (msg) => this.onFeedMessage(feed, msg), null, {
      onReconnect: async () => {
        const next = await this.findFeed(source);
        if (next) feed.id = next;
        return next;
      },
      // A reopened feed missed what was sent while it was down: every gate is read again.
      onOpen: () => {
        if (this.feed !== feed) return;
        if (feed.opened) {
          this.reseedAll();
          return;
        }
        feed.opened = true;
        this.seedPass(this.shownJobs());
      },
    });
    this.feed = feed;
  }

  closeFeed() {
    if (this.feed) this.feed.sub.close();
    this.feed = null;
  }

  onFeedMessage(feed, msg) {
    if (this.destroyed || this.feed !== feed) return;
    if (msg.type === "frame") {
      const job = this.feedJob(msg.job);
      if (job) this.render(job, msg.payload);
      else this.hold(msg);
    } else if (msg.type === "lagged") {
      this.reseedAll();
    }
  }

  // The running job on the shown source a feed frame belongs to, once its row is settled.
  feedJob(jobId) {
    const j = this.lastJobs.find(
      (x) => x.id === jobId && x.source === this.sourceId && isRunning(x),
    );
    return j && j.kind !== "spectrum" && this.rowKnown(j) ? j : null;
  }

  // Keeps a frame whose job the last poll did not list as running (or whose row is not yet
  // settled) and asks for a poll; ov.replayPending draws it once the job is known.
  hold(frame) {
    this.pending.push({ frame, seen: 0 });
    if (this.pending.length > PENDING_MAX) this.pending.splice(0, this.pending.length - PENDING_MAX);
    this.askPoll();
  }

  // Asks for a poll; one in flight at a time. A request made while one is in flight (which
  // may have started before the frame was held) is repeated once it ends.
  askPoll() {
    if (this.pollAsked) {
      this.pollAgain = true;
      return;
    }
    this.pollAsked = true;
    Promise.resolve()
      .then(() => this.requestPoll())
      .catch(() => {})
      .finally(() => {
        this.pollAsked = false;
        if (this.pollAgain && !this.destroyed) {
          this.pollAgain = false;
          this.askPoll();
        }
      });
  }

  // Renders the held frames whose job is known now, in order. A frame stays while its job's
  // row is unsettled. A fresh poll that does not list its job counts against it: the first
  // keeps it (the poll may have started before the job did) and asks for one more poll, the
  // second drops it.
  replayPending(fresh) {
    if (!this.pending.length) return;
    const held = this.pending;
    this.pending = [];
    let again = false;
    for (const h of held) {
      const job = this.feedJob(h.frame.job);
      if (job) {
        this.render(job, h.frame.payload);
        continue;
      }
      const j = this.lastJobs.find((x) => x.id === h.frame.job);
      const listed = j && j.source === this.sourceId && isRunning(j) && j.kind !== "spectrum";
      if (!listed && fresh && ++h.seen >= 2) continue;
      if (!listed && fresh) again = true;
      this.pending.push(h);
    }
    if (again) this.askPoll();
  }

  // A lag or a reconnect of the feed: every gate may have changed unseen. Forget them, clear
  // every row's and tag's dot, drop the held frames (the counters stay) and read the gates
  // again. A read issued before this is discarded when it answers (ov.seedGen).
  reseedAll() {
    this.seedGen++;
    this.squelches.clear();
    this.gateless.clear();
    this.reseeds++;
    this.pending = [];
    for (const j of this.shownJobs()) {
      setSquelchTag(this.jobRowEl(j), null);
      this.refreshTag(this.outlines.get(j.id));
    }
    this.seedPass(this.shownJobs());
  }

  // The running non-spectrum jobs on the shown source in the last poll.
  shownJobs() {
    return this.lastJobs.filter(
      (j) => j.source === this.sourceId && j.kind !== "spectrum" && isRunning(j),
    );
  }

  // A `job_context` read for each SQUELCH_KINDS job among `jobs` whose gate is unknown, at most
  // SEED_IN_FLIGHT at once; only once the feed is open, so a gate change after the read is seen.
  // Each read that completes starts the next (seedSquelch), so the reads chain within a poll.
  // A job whose context held no gate is skipped until its graph changes (ov.gateless).
  seedPass(jobs) {
    if (!this.feed || !this.feed.opened) return;
    for (const j of jobs) {
      if (this.seeding.size >= SEED_IN_FLIGHT) return;
      if (!SQUELCH_KINDS.includes(j.kind)) continue;
      if (this.squelches.has(j.id) || this.seeding.has(j.id)) continue;
      const g = this.gateless.get(j.id);
      if (g !== undefined) {
        if (g === graphKey(j)) continue;
        this.gateless.delete(j.id);
      }
      this.seedSquelch(j);
    }
  }

  // Whether the job's mode row is settled (a row, or no row): no event before that.
  rowKnown(job) {
    return !this.subjects || this.subjects.rowOf(job.kind) !== undefined;
  }

  // Every running kind's row looked up; what is learned is applied at once, since a poll
  // later a decoder's first lines may be gone.
  async lookup(jobs) {
    if (!this.subjects) return;
    const learned = await this.subjects.lookup(jobs.filter(isRunning).map((j) => j.kind));
    if (learned && !this.destroyed) this.sync(this.lastJobs, this.sourceRec);
  }

  watching(jobId) {
    return !this.overCap.has(jobId);
  }

  follows(jobId) {
    return this.activity(jobId).watched || this.positioned.has(jobId);
  }

  // The running jobs on the other sources whose mode row carries a `position` or a `subject`,
  // newest first, up to MAX_POSITIONED: subscribed for ov.onEvent alone.
  syncPositioned(jobs) {
    if (this.destroyed) return;
    const others = jobs.filter(
      (j) => j.source !== this.sourceId && j.kind !== "spectrum" && isRunning(j),
    );
    const followed = others
      .filter((j) => {
        const row = this.subjects ? this.subjects.rowOf(j.kind) : null;
        return row && (row.position || row.subject);
      })
      .sort((a, b) => (b.created_ns ?? 0) - (a.created_ns ?? 0));
    const want = followed.slice(0, MAX_POSITIONED);
    this.overCap = new Set(followed.slice(MAX_POSITIONED).map((j) => j.id));
    const ids = new Set(want.map((j) => j.id));
    for (const id of [...this.positioned.keys()]) if (!ids.has(id)) this.unwatchPositioned(id);
    for (const j of want) {
      const known = this.positioned.get(j.id);
      if (known) known.job = j;
    }
    const fresh = want.filter((j) => !this.positioned.has(j.id));
    if (fresh.length > 0) this.resolvePositioned(fresh);
  }

  // One GET /streams for the positioned jobs not yet subscribed.
  async resolvePositioned(jobs) {
    if (this.resolvingPositioned) return;
    this.resolvingPositioned = true;
    let streams = null;
    try {
      streams = await this.api.streams();
    } catch {
      streams = null;
    } finally {
      this.resolvingPositioned = false;
    }
    if (!streams || this.destroyed) return;
    for (const j of jobs) {
      if (j.source === this.sourceId || this.positioned.has(j.id)) continue;
      const ids = streams
        .filter((x) => x.kind === "event" && x.provenance && x.provenance.job === j.id)
        .map((x) => x.id);
      if (ids.length === 0) continue;
      const rec = { job: j, ids, subs: [], closed: false, positionOnly: true };
      this.positioned.set(j.id, rec);
      ids.forEach((_, slot) => this.subscribe(rec, slot));
    }
  }

  unwatchPositioned(jobId) {
    const rec = this.positioned.get(jobId);
    if (!rec) return;
    rec.closed = true;
    for (const sub of rec.subs) sub.close();
    this.positioned.delete(jobId);
  }

  // The rigid holders' bands (the source's pin among them) outlined, or the capture when
  // there is none. `source` is the selected source's `source_list` record; without one (or
  // another source's) the last one given is kept.
  syncOutlines(jobs, source) {
    this.batchTags(() => {
      if (this.destroyed) return;
      if (source !== undefined) this.sourceRec = source;
      this.lastJobs = jobs;
      const rec = this.sourceRec && this.sourceRec.id === this.sourceId ? this.sourceRec : null;
      const pin = rec && rec.pin && typeof rec.pin.center_hz === "number" ? rec.pin : null;
      const rigid = jobs.filter((j) => j.source === this.sourceId && isRunning(j) && rigidBand(j));
      const keys = new Set(rigid.map((j) => j.id));
      if (pin) keys.add(PIN_KEY);
      for (const key of [...this.outlines.keys()]) {
        if (key !== CAPTURE_KEY && !keys.has(key)) this.unoutline(key);
      }
      for (const j of rigid) {
        const { center_hz: c, bandwidth_hz: bw } = j.tuning;
        const ch = channelLabel(c, this.plan());
        this.outline(c - bw / 2, c + bw / 2, ch ? `${ch.text} ${j.kind}` : j.kind, j.id, j);
      }
      if (pin) {
        const { center_hz: c, bandwidth_hz: bw } = pin;
        this.outline(c - bw / 2, c + bw / 2, "pin", PIN_KEY);
      }
      this.captureOutlined = rigid.length === 0 && !pin;
      this.outlineCapture();
      this.setSpeaking(this.speaking);
    });
  }

  // The capture's outline follows wf.band while no rigid holder is outlined (a spectrum
  // job's capture only: a scan's or a sweep's band is the view, not a capture).
  outlineCapture() {
    const b = this.wf.band;
    if (this.captureOutlined && b && this.wf.mode === "spectrum") {
      const o = this.outlines.get(CAPTURE_KEY);
      if (!o || o.lo !== b.lo || o.hi !== b.hi) this.outline(b.lo, b.hi, "capture", CAPTURE_KEY);
    } else {
      this.unoutline(CAPTURE_KEY);
    }
  }

  // The gate the job's context latched (its last `squelch` event), asked once the feed is
  // open: a job whose only `squelch` event came before the page subscribed is still tagged.
  // A live event seen meanwhile is newer and wins. A failed request or an answer without a
  // gate puts the job in ov.gateless (with its graph): not asked again until its graph
  // changes or ov.reseedAll runs. Each completion runs the seed pass again for the next job.
  async seedSquelch(job) {
    this.seeding.add(job.id);
    const source = this.sourceId;
    const graph = graphKey(job);
    const gen = this.seedGen;
    let r = null;
    try {
      r = await this.api.op({ op: "job_context", job: job.id });
    } catch {
      r = null;
    }
    // The id stays in ov.seeding until the answer is processed, so the cap stays honest.
    if (this.seedHold) await this.seedHold;
    this.seeding.delete(job.id);
    if (this.destroyed || this.sourceId !== source) return;
    if (gen !== this.seedGen) {
      this.staleSeeds++;
      this.seedPass(this.shownJobs());
      return;
    }
    const now = this.lastJobs.find((x) => x.id === job.id && isRunning(x));
    const p = r && r.value && r.value.squelch;
    if (now && !this.squelches.has(job.id)) {
      if (p && p.protocol === "squelch") {
        this.squelches.set(job.id, { event: p, graph: graphKey(now) });
        setSquelchTag(this.jobRowEl(now), p);
        this.refreshTag(this.outlines.get(job.id));
      } else {
        this.gateless.set(job.id, graph);
      }
    }
    this.seedPass(this.shownJobs());
  }

  // A positioned job's event stream `slot` (another source's job: the map's alone).
  subscribe(rec, slot) {
    rec.subs[slot] = this.api.streamText(
      rec.ids[slot],
      (msg) => {
        if (!rec.closed && msg.type === "frame") {
          this.render(rec.job, msg.payload, { positionOnly: rec.positionOnly });
        }
      },
      null,
      { onReconnect: () => this.lookupStream(rec, slot) },
    );
  }

  // The id for subscription `slot` of `rec` after a drop: its stream if still listed, else
  // one of the job's event streams no other slot holds (the job restarted with new ids).
  async lookupStream(rec, slot) {
    const streams = await this.api.streams();
    const ev = streams
      .filter((x) => x.kind === "event" && x.provenance && x.provenance.job === rec.job.id)
      .map((x) => x.id);
    if (ev.includes(rec.ids[slot])) return rec.ids[slot];
    const held = new Set(rec.ids.filter((_, i) => i !== slot));
    const id = ev.find((x) => !held.has(x)) ?? null;
    if (id) rec.ids[slot] = id;
    return id;
  }

  render(job, p, { positionOnly = false } = {}) {
    const owner = job.input ?? job.id; // whose colour and band its ribbons take
    const obj = p && typeof p === "object" && !Array.isArray(p);
    if (obj && (Array.isArray(p.db) || Array.isArray(p.q))) return; // spectrum rows
    for (const fn of this.eventFns) fn(job, p);
    if (positionOnly) return; // another source's job: the map's alone
    // The node's annotation of the job (plan 32 ruling N), not a decode: the map and the
    // transcript have it (ov.onEvent); no ribbon, no count, not the job's activity.
    if (obj && p.protocol === "origin") return;
    if (obj && p.protocol === "squelch") {
      // Only a demodulator job's own squelch tags its row (never, say, a decoder's demod).
      if (SQUELCH_KINDS.includes(job.kind)) {
        this.squelches.set(job.id, { event: p, graph: graphKey(job) });
        setSquelchTag(this.jobRowEl(job), p);
        this.refreshTag(this.outlines.get(job.id));
      }
      return;
    }
    // Every non-spectrum event counts in the job row, whatever its shape.
    const n = (this.counters.get(job.id) || 0) + 1;
    this.counters.set(job.id, n);
    this.lastEvents.set(job.id, performance.now());
    if (this.isStationFrame(job, p)) this.stationEvents.set(job.id, performance.now());
    this.jobRow(job, n);
    if (obj) {
      if (p.label && typeof p.label === "object" && typeof p.center_hz === "number") {
        this.label(p.center_hz, p.label, job.id);
        return;
      }
      if (p.protocol === "vad") return;
      if (p.protocol === "video" && typeof p.locked === "boolean") {
        // A lock report: the tag's snippet and the video view's, never a ribbon.
        if (typeof p.summary === "string") {
          this.snippets.set(job.id, p.summary.slice(0, SNIPPET_CHARS));
          this.refreshTag(this.outlines.get(job.id));
        }
        this.onVideoLock?.(job.id, p);
        return;
      }
      if (p.protocol === "signal" && typeof p.center_hz === "number") {
        const key = `${job.id}:${Math.round(p.center_hz)}`;
        if (p.state === "gone") this.unmark(key);
        else if (p.state === "new") this.mark(p.center_hz, p.summary, key);
        // A mark only: a busy band's new/gone ribbons would bury the watch's verdicts.
        return;
      }
      if (p.protocol === "transcript") {
        if (job.input && typeof p.text === "string" && !p.dropped && !present(p.error)) {
          this.snippets.set(job.input, p.text.slice(0, SNIPPET_CHARS));
          this.refreshTag(this.outlines.get(job.input));
        }
        const t = transcriptRibbon(p);
        const el = this.ribbon(tuningCentre(job), t.text, t.cls, owner);
        if (el && t.title) el.title = `${t.text}\n${t.title}`;
        return;
      }
      if (typeof p.summary === "string") {
        if (this.subjectRibbon(job, p)) return;
        this.ribbon(tuningCentre(job), p.summary, typeof p.protocol === "string" ? p.protocol : "event", owner);
        return;
      }
      if (typeof p.protocol === "string") {
        const text = protocolText(p);
        if (text !== null) {
          this.ribbon(tuningCentre(job), text, p.protocol, owner);
          return;
        }
      }
      if ("src" in p && "info" in p) {
        this.ribbon(tuningCentre(job), `${p.src} > ${p.dst}: ${p.info}`, "aprs", owner);
        return;
      }
    }
    const json = JSON.stringify(p) ?? String(p);
    this.ribbon(tuningCentre(job), json.slice(0, OTHER_EVENT_CHARS), "event", owner);
  }

  onEvent(fn) {
    this.eventFns.push(fn);
  }

  squelch(jobId) {
    const s = this.squelches.get(jobId);
    return s ? s.event : null;
  }

  lastEvent(jobId) {
    return this.lastEvents.get(jobId) ?? null;
  }

  lastStationEvent(jobId) {
    return this.stationEvents.get(jobId) ?? null;
  }

  // A frame from or to a fixed station (plan 35 spec 4.5), on a job whose row has
  // `subject.station`: one matching `station.when` (subjects.js matchesPairs, the table's
  // comparison), or one whose `addressed` field names a station as the node's tally keys
  // it (origin.rs station_key): a non-blank string, or a number.
  isStationFrame(job, p) {
    if (!p || typeof p !== "object" || Array.isArray(p)) return false;
    const st = this.subjects?.rowOf(job.kind)?.subject?.station;
    if (!st || typeof st !== "object") return false;
    if (matchesPairs(p, st.when)) return true;
    if (typeof st.addressed !== "string") return false;
    const to = p[st.addressed];
    return (typeof to === "string" && to.trim() !== "") || typeof to === "number";
  }

  // What the page knows of a job's activity: watched (a running job on the shown source, so
  // its squelch and events come on the feed), speaking (a mix hears it), hot (hovered).
  activity(jobId) {
    return {
      watched: this.lastJobs.some(
        (j) => j.id === jobId && j.source === this.sourceId && isRunning(j),
      ),
      speaking: this.speaking.includes(jobId),
      hot: this.hotJob === jobId,
    };
  }

  jobRowEl(job) {
    return this.jobsEl
      ? this.jobsEl.querySelector(`.job[data-id="${CSS.escape(job.id)}"]`)
      : null;
  }

  jobRow(job, counter) {
    const row = this.jobRowEl(job);
    const cell = row && row.querySelector(".events");
    if (!cell) return;
    cell.textContent = counter > 0 ? String(counter) : "";
    cell.title = counter > 0 ? `${counter} events since this page started watching` : "";
  }

  // ---- drawing -------------------------------------------------------------------------

  // CSS px from the layer top for `hz`; the middle when unknown.
  yFor(hz) {
    const y = hz === null || hz === undefined ? null : this.wf.yForHz(hz);
    return y === null || !Number.isFinite(y) ? this.layer.clientHeight / 2 : y;
  }

  // The one line a subject job draws: `waterfall: summary` keeps a single ribbon for the job
  // (`<protocol> <n> <name>, last <label>`), `fold` one per subject, each updated in place.
  // False for an event with no subject (no row, a row without one, a blank key): the ordinary
  // ribbon takes it.
  subjectRibbon(job, p) {
    const store = this.subjects;
    const k = store ? store.keyed(job, p) : null;
    if (!k) return false;
    const { row } = k;
    const hz = tuningCentre(job);
    if (row.subject.waterfall === "fold") {
      const s = store.subjectOf(job, p);
      if (!s) return false;
      this.keyedRibbon(
        `job:${job.id}:${k.key}`, hz, () => subjectSummary(s), row.protocol, s.count,
        () => store.held(job.id, k.key), job.id,
      );
      return true;
    }
    const line = () => {
      const last = store.lastHeard(job.id);
      const label = last ? subjectLabel(last) : "";
      return `${row.protocol} ${store.size(job.id)} ${row.subject.name}, last ${label}`;
    };
    this.keyedRibbon(`job:${job.id}`, hz, line, row.protocol, 1, () => store.held(job.id, null), job.id);
    return true;
  }

  // The ribbon for `key`, `text` a string or a function asked when it is drawn. A new one is
  // drawn at once; a message to one only notes what it says, and a timer draws it at most every
  // KEYED_RIBBON_MS, so no message measures anything. `held()` true keeps it from scrolling or
  // expiring while its subject is heard.
  keyedRibbon(key, hz, text, cls, count, held, job = null) {
    if (this.destroyed) return null;
    let r = this.ribbons.find((o) => o.key === key);
    const fresh = !r;
    if (fresh) {
      r = this.makeRibbon(hz, typeof text === "function" ? String(text()) : String(text), cls, key, job);
      r.countText = "";
      r.drawn = 0;
    }
    r.held = held;
    r.hz = hz;
    r.nextText = text;
    r.count = count;
    r.born = performance.now();
    if (fresh && count <= 1) {
      r.drawn = r.born;
      return r.el;
    }
    if (r.keyedTimer === undefined) {
      const wait = Math.max(0, r.drawn + KEYED_RIBBON_MS - r.born);
      r.keyedTimer = setTimeout(() => this.drawKeyed(r), wait);
    }
    return r.el;
  }

  // A keyed ribbon as last told: the text replaced, `×count` when `count` is over 1, the width
  // measured again if either changed, the ribbon back at the right edge and placed again if it
  // had moved.
  drawKeyed(r) {
    r.keyedTimer = undefined;
    if (this.destroyed || !r.el.isConnected) return;
    r.drawn = performance.now();
    const text = String(typeof r.nextText === "function" ? r.nextText() : r.nextText);
    const countText = r.count > 1 ? ` ×${r.count}` : "";
    const moved = r.right > 0;
    if (text === r.text && countText === r.countText && !moved) return;
    r.text = text;
    r.bodyEl.textContent = text;
    r.el.title = text;
    r.countText = countText;
    r.countEl.textContent = countText;
    r.w = r.el.offsetWidth;
    r.right = 0;
    r.el.style.right = "0px";
    if (moved) this.place(r);
  }

  ribbon(hz, text, cls = "", job = null) {
    if (this.destroyed) return null;
    text = String(text);
    const same = this.ribbons.find(
      (r) => !r.key && r.text === text && r.cls === cls && r.hz === hz && r.right < r.w,
    );
    if (same) {
      same.count += 1;
      same.countEl.textContent = ` ×${same.count}`;
      same.born = performance.now();
      return same.el;
    }
    return this.makeRibbon(hz, text, cls, undefined, job).el;
  }

  // A new ribbon record, drawn and placed, never folded into another by its text. `job` (the
  // id whose ribbon it is: an attached job's input) gives its colour (channelColour) and band;
  // a ribbon with no job (the agent's notes, a test's) takes the job outline holding hz, if any.
  makeRibbon(hz, text, cls, key = undefined, job = null) {
    const el = document.createElement("div");
    el.className = cls ? `ov-ribbon ${cls}` : "ov-ribbon";
    el.dir = "auto";
    const band = job ? { key: job, colour: String(channelColour(job)) } : this.bandOf(hz);
    if (band) el.dataset.colour = band.colour;
    const body = document.createElement("span");
    body.textContent = text;
    const countEl = document.createElement("span");
    countEl.className = "count";
    el.append(body, countEl);
    el.title = text;
    el.style.right = "0px";
    this.layer.append(el);
    const r = {
      el, bodyEl: body, countEl, text, cls, hz, band: band ? band.key : null, lane: 0, right: 0,
      born: performance.now(), count: 1, key,
    };
    r.w = el.offsetWidth;
    r.h = el.offsetHeight;
    el.style.zIndex = String(this.ribbons.length);
    this.place(r);
    this.ribbons.push(r);
    while (this.ribbons.length > MAX_RIBBONS) this.ribbons.shift().el.remove();
    return r;
  }

  // The narrowest job outline whose band holds `hz` ({key, colour, o}), or null (no job band does,
  // or hz unknown): the band a ribbon at hz belongs to.
  bandOf(hz) {
    if (typeof hz !== "number") return null;
    let best = null;
    for (const [key, o] of this.outlines) {
      if (!o.job || !o.el.dataset.colour || hz < o.lo || hz > o.hi) continue;
      if (!best || o.hi - o.lo < best.o.hi - best.o.lo) best = { key, colour: o.el.dataset.colour, o };
    }
    return best;
  }

  // The centre line of ribbon `r` in lane `lane` (layer px).
  laneTop(r, lane) {
    return this.yFor(r.hz) + lane * RIBBON_STEP_DIR * (r.h + RIBBON_GAP_PX);
  }

  // Whether ribbons a and b overlap, a's centre line at `top` (a.top when not given).
  ribbonsOverlap(a, b, top = a.top) {
    return (
      a.right < b.right + b.w &&
      b.right < a.right + a.w &&
      Math.abs(top - b.top) < (a.h + b.h) / 2
    );
  }

  // Whether ribbon r, its centre line at `top`, would reach another job band's base lane
  // (that band's centre line plus or minus half a lane).
  crossesOtherBand(r, top) {
    const half = (r.h + RIBBON_GAP_PX) / 2;
    for (const [key, o] of this.outlines) {
      if (!o.job || key === r.band) continue;
      const y = this.yFor((o.lo + o.hi) / 2);
      if (Math.abs(top - y) < r.h / 2 + half) return true;
    }
    return false;
  }

  setLane(r, lane) {
    r.lane = lane;
    r.top = this.laneTop(r, lane);
    r.el.style.top = `${r.top}px`;
  }

  // Places a new (or redrawn) ribbon at the right edge. An unkeyed one takes the band line and
  // steps the unkeyed ribbons it overlaps one lane on, and in turn those they then overlap; a
  // ribbon that cannot step (MAX_LANES, another band's base lane) is clipped where it is. A
  // keyed one takes the first lane free of overlap and moves nobody.
  place(r) {
    if (r.key) {
      let lane = 0;
      for (let l = 0; l < MAX_LANES; l++) {
        const top = this.laneTop(r, l);
        if (!this.ribbons.some((o) => o !== r && this.ribbonsOverlap(r, o, top))) {
          lane = l;
          break;
        }
      }
      this.setLane(r, lane);
      return;
    }
    this.setLane(r, 0);
    r.el.classList.remove("clipped");
    const seen = new Set([r]);
    const queue = [r];
    while (queue.length > 0) {
      const x = queue.shift();
      for (const o of this.ribbons) {
        if (seen.has(o) || o.key || !this.ribbonsOverlap(x, o)) continue;
        seen.add(o);
        const lane = o.lane + 1;
        const top = this.laneTop(o, lane);
        if (lane >= MAX_LANES || this.crossesOtherBand(o, top)) {
          o.el.classList.add("clipped");
          continue;
        }
        o.el.classList.remove("clipped");
        this.setLane(o, lane);
        queue.push(o);
      }
    }
  }

  // Each unkeyed ribbon off its band line whose next lane down is clear falls back one lane.
  fallBack() {
    for (const r of this.ribbons) {
      if (r.key || r.lane <= 0) continue;
      const top = this.laneTop(r, r.lane - 1);
      if (this.ribbons.some((o) => o !== r && this.ribbonsOverlap(r, o, top))) continue;
      r.el.classList.remove("clipped");
      this.setLane(r, r.lane - 1);
    }
  }

  label(hz, { layer = 1, name = "?", confidence = null, evidence = null } = {}, key = "") {
    if (this.destroyed) return null;
    const conf = typeof confidence === "number" ? ` ${confidence.toFixed(2)}` : "";
    let l = this.labels.get(key);
    if (!l) {
      const el = document.createElement("div");
      el.className = "ov-label";
      this.layer.append(el);
      l = { el, hz };
      this.labels.set(key, l);
    }
    l.hz = hz;
    l.el.textContent = `L${layer} ${name}${conf}`;
    l.el.title = evidence ? String(evidence) : "";
    l.el.dataset.layer = String(layer);
    l.h = l.el.offsetHeight;
    l.dy = layer === 2 ? l.h + LABEL_GAP_PX : 0;
    l.el.style.top = `${this.yFor(hz) - l.h / 2 + l.dy}px`;
    return l.el;
  }

  unlabel(key) {
    const l = this.labels.get(key);
    if (!l) return;
    l.el.remove();
    this.labels.delete(key);
  }

  // A watch's markers follow its context (`watch.signals` still present), so a signal
  // announced before this page watched the job is marked too; `signal` events keep them live
  // between polls.
  syncMarks(jobs) {
    const keep = new Set();
    for (const j of jobs) {
      const sigs = j.kind === "watch" && j.context && j.context.watch && j.context.watch.signals;
      if (!Array.isArray(sigs)) continue;
      for (const s of sigs) {
        if (!s || s.present === false || typeof s.center_hz !== "number") continue;
        const key = `${j.id}:${Math.round(s.center_hz)}`;
        keep.add(key);
        this.mark(s.center_hz, `signal ${(s.center_hz / 1e6).toFixed(4)} MHz`, key);
      }
    }
    for (const key of [...this.marks.keys()]) if (!keep.has(key)) this.unmark(key);
  }

  // A small marker at `hz` for a signal a watch announced; `unmark(key)` removes it.
  mark(hz, text, key) {
    if (this.destroyed) return null;
    let m = this.marks.get(key);
    if (!m) {
      const el = document.createElement("div");
      el.className = "ov-mark";
      this.layer.append(el);
      m = { el, hz };
      this.marks.set(key, m);
    }
    m.hz = hz;
    m.el.title = typeof text === "string" ? text : "";
    m.el.style.top = `${this.yFor(hz)}px`;
    return m.el;
  }

  unmark(key) {
    const m = this.marks.get(key);
    if (!m) return;
    m.el.remove();
    this.marks.delete(key);
  }

  outline(lo, hi, text, key, job = null) {
    if (this.destroyed) return null;
    let o = this.outlines.get(key);
    if (!o) {
      const el = document.createElement("div");
      el.className = "outline";
      el.dataset.key = key;
      o = { el, labelEl: null, tag: null, parts: null, lo, hi, job: null, handles: null };
      if (job) {
        this.tag(o, key, job);
      } else {
        o.labelEl = document.createElement("span");
        o.labelEl.className = "outline-label";
        el.append(o.labelEl);
      }
      this.layer.prepend(el); // under the labels and ribbons
      this.outlines.set(key, o);
      this.listenDrag(o, key);
    }
    o.job = job;
    this.setDraggable(o);
    if (o.labelEl) o.labelEl.textContent = String(text);
    this.refreshTag(o);
    if (this.drag && this.drag.key === key) return o.el; // the pointer has it
    o.lo = lo;
    o.hi = hi;
    o.el.title = `${text} ${(lo / 1e6).toFixed(3)}-${(hi / 1e6).toFixed(3)} MHz`;
    this.placeOutline(o);
    return o.el;
  }

  unoutline(key) {
    const o = this.outlines.get(key);
    if (!o) return;
    if (this.drag && this.drag.key === key) this.endDrag();
    this.untag(o, key);
    o.el.remove();
    this.outlines.delete(key);
    this.layoutTags();
  }

  // ---- spans ---------------------------------------------------------------------------

  // A thin bracket over the band lo..hi spanning the times startNs..endNs (a capture's
  // cached region), tagged `text`; re-laid as the columns scroll and the view changes, and
  // removed once its end has scrolled off. Returns the element, or null when it is off
  // screen already.
  span(lo, hi, startNs, endNs, text, key) {
    if (this.destroyed) return null;
    let sp = this.spans.get(key);
    if (!sp) {
      const el = document.createElement("div");
      el.className = "ov-span";
      el.dataset.key = key;
      el.append(document.createElement("span"));
      this.layer.prepend(el);
      sp = { el };
      this.spans.set(key, sp);
    }
    Object.assign(sp, { lo, hi, startNs, endNs });
    sp.el.firstChild.textContent = String(text);
    sp.el.title = `${text} ${(lo / 1e6).toFixed(3)}-${(hi / 1e6).toFixed(3)} MHz`;
    return this.placeSpan(key, sp) ? sp.el : null;
  }

  unspan(key) {
    const sp = this.spans.get(key);
    if (!sp) return;
    sp.el.remove();
    this.spans.delete(key);
  }

  // Positions a span's element; false (and the span removed) when its end left the columns.
  placeSpan(key, sp) {
    const x1 = this.wf.xForNs(sp.endNs);
    if (x1 === null) {
      this.unspan(key);
      return false;
    }
    const x0 = this.wf.xForNs(sp.startNs);
    const left = x0 === null ? 0 : x0;
    const y0 = this.wf.yForHz(sp.hi);
    const y1 = this.wf.yForHz(sp.lo);
    if (y0 === null || y1 === null || !Number.isFinite(y0 + y1)) {
      sp.el.hidden = true;
      return true;
    }
    sp.el.hidden = false;
    const s = sp.el.style;
    s.left = `${left}px`;
    s.width = `${Math.max(2, x1 - left)}px`;
    s.top = `${y0}px`;
    s.height = `${Math.max(2, y1 - y0)}px`;
    return true;
  }

  layoutSpans() {
    for (const [key, sp] of [...this.spans]) this.placeSpan(key, sp);
  }

  // ---- job tags ------------------------------------------------------------------------

  // The tag of a job's outline `o`: kind, short id, [squelch dot], channel, centre, [snippet];
  // in the rail when there is one, else in the outline.
  tag(o, key, job = null) {
    const el = document.createElement("div");
    el.className = "job-tag";
    el.dataset.job = key;
    const span = (cls) => {
      const s = document.createElement("span");
      s.className = cls;
      return s;
    };
    o.parts = {
      kind: span("kind"),
      short: span("short"),
      channel: span("channel"),
      centre: span("centre"),
      dot: null,
      snippet: null,
      thumb: null,
    };
    el.append(o.parts.kind, " ", o.parts.short, " ", o.parts.channel, " ", o.parts.centre, " ");
    if (job && job.kind === "video_decode") {
      const c = document.createElement("canvas");
      c.className = "video-thumb";
      c.width = 64;
      c.height = 48;
      c.dataset.job = key;
      c.addEventListener("click", (ev) => {
        ev.stopPropagation();
        this.onThumbClick?.(key);
      });
      el.append(c);
      o.parts.thumb = c;
    }
    el.dataset.colour = o.el.dataset.colour = String(channelColour(key));
    (this.rail ?? o.el).append(el);
    o.tag = el;
    if (o.parts.thumb) this.onTagMedia?.(key, o.parts.thumb, true);
    // Narrow: a tap expands the thin segment into its label for RAIL_EXPAND_MS. It only adds a
    // class, so the long press (listenPress) on the same tag is untouched.
    el.addEventListener("pointerdown", () => {
      if (document.documentElement.dataset.footprint !== "narrow") return;
      el.classList.add("expanded");
      clearTimeout(o.expandTimer);
      o.expandTimer = setTimeout(() => el.classList.remove("expanded"), RAIL_EXPAND_MS);
    });
    el.addEventListener("click", () => {
      const row = o.job && this.jobRowEl(o.job);
      if (row) row.scrollIntoView({ block: "nearest" });
    });
    for (const target of [el, o.el]) {
      target.addEventListener("pointerenter", () => this.hoverTag(o, true));
      target.addEventListener("pointerleave", (ev) => {
        // Moving between the tag and its outline is still hovering the job.
        if (!ev.relatedTarget || !o.el.contains(ev.relatedTarget)) this.hoverTag(o, false);
      });
    }
  }

  untag(o, key) {
    if (!o.tag) return;
    if (o.parts && o.parts.thumb) this.onTagMedia?.(key, null, false);
    this.snippets.delete(key);
    if (this.hotJob === key) this.hotJob = null;
    clearTimeout(o.expandTimer);
    o.tag.remove();
    o.tag = null;
    o.parts = null;
  }

  // The text of `o`'s tag from its job, the squelch state, the plan and the last snippet.
  refreshTag(o) {
    if (!o || !o.tag || !o.job) return;
    const job = o.job;
    const parts = o.parts;
    parts.kind.textContent = job.kind;
    parts.short.textContent = job.id.slice(-6);
    const sq = SQUELCH_KINDS.includes(job.kind) ? this.squelch(job.id) : null;
    if (sq && typeof sq.open === "boolean") {
      if (!parts.dot) {
        parts.dot = document.createElement("span");
        o.tag.insertBefore(parts.dot, parts.channel);
        o.tag.insertBefore(document.createTextNode(" "), parts.channel);
      }
      parts.dot.className = `dot ${sq.open ? "open" : "closed"}`;
    } else if (parts.dot) {
      parts.dot.nextSibling.remove();
      parts.dot.remove();
      parts.dot = null;
    }
    const ch = job.tuning ? channelLabel(job.tuning.center_hz, this.plan()) : null;
    parts.channel.textContent = ch ? ch.text : "";
    parts.centre.textContent = tuningCentre(job) === null ? "" : (job.tuning.center_hz / 1e6).toFixed(3);
    const text = this.snippets.get(job.id);
    if (text !== undefined) {
      if (!parts.snippet) {
        parts.snippet = document.createElement("span");
        parts.snippet.className = "snippet";
        o.tag.append(parts.snippet);
      }
      parts.snippet.textContent = text;
    }
    this.layoutTags();
  }

  // Moves every job tag into `el` (the rail), or back into its outline with null.
  setRail(el) {
    this.rail = el ?? null;
    for (const o of this.outlines.values()) {
      const t = o.tag;
      if (!t) continue;
      t.classList.remove("thin");
      t.hidden = false;
      t.removeAttribute("title");
      t.removeAttribute("aria-label");
      t.style.top = "";
      (this.rail ?? o.el).append(t);
    }
    this.layoutTags();
  }

  // Runs `fn` as one pass: every layoutTags it causes (one per outline placed or tag
  // refreshed) is deferred and done once when the outermost pass ends, so a pass over 80
  // outlines forces one rail reflow, not 80. A lone call outside a pass lays out at once.
  batchTags(fn) {
    this.tagBatch++;
    try {
      return fn();
    } finally {
      this.tagBatch--;
      if (this.tagBatch === 0 && this.tagsDirty) {
        this.tagsDirty = false;
        this.layoutTags();
      }
    }
  }

  // Tags at their outline's vertical centre. In the rail: see `The rail` above. On the
  // outlines: stacked, sorted by top, a tag under the previous one's bottom + TAG_GAP_PX is
  // pushed down (at most MAX_LANES in a row).
  layoutTags() {
    if (this.tagBatch > 0) {
      this.tagsDirty = true;
      return;
    }
    if (this.rail) {
      this.layoutRail();
      return;
    }
    const items = [];
    for (const o of this.outlines.values()) {
      if (!o.tag || o.el.hidden) continue;
      const oTop = parseFloat(o.el.style.top) || 0;
      const h = parseFloat(o.el.style.height) || 0;
      const th = o.tag.offsetHeight;
      items.push({ o, oTop, th, top: oTop + h / 2 - th / 2 });
    }
    items.sort((a, b) => a.top - b.top);
    let prev = null;
    let lane = 0;
    for (const it of items) {
      if (prev && it.top < prev.top + prev.th + TAG_GAP_PX && lane < MAX_LANES) {
        it.top = prev.top + prev.th + TAG_GAP_PX;
        lane++;
      } else {
        lane = 0;
      }
      it.o.tag.style.top = `${it.top - it.oTop}px`;
      prev = it;
    }
  }

  layoutRail() {
    const height = this.rail.clientHeight;
    const items = [];
    for (const o of this.outlines.values()) {
      const t = o.tag;
      if (!t) continue;
      t.classList.remove("thin");
      const top = parseFloat(o.el.style.top);
      const h = parseFloat(o.el.style.height) || 0;
      const off = o.el.hidden || !Number.isFinite(top) || top + h <= 0 || top >= height;
      t.hidden = off;
      if (!off) items.push({ o, t, c: top + h / 2 });
    }
    for (const it of items) it.th = it.t.offsetHeight; // full height: every `thin` is off
    items.sort((a, b) => a.c - b.c);
    const clamp = (top, h) => Math.max(0, Math.min(top, height - h));
    const wins = (it) => it.t.classList.contains("hot") || it.t.classList.contains("speaking");
    const overlaps = (a, b) => b.c - b.th / 2 < a.c + a.th / 2 + TAG_GAP_PX; // a above b
    const fulls = []; // the full tags so far, top first
    for (const it of items) {
      it.thin = false;
      // A hot or speaking tag keeps its space: the full tags it overlaps go thin instead.
      while (wins(it) && fulls.length && !wins(fulls.at(-1)) && overlaps(fulls.at(-1), it)) {
        fulls.pop().thin = true;
      }
      if (fulls.length && overlaps(fulls.at(-1), it)) it.thin = true;
      else fulls.push(it);
    }
    // A thin segment shows no text, so it is named: its channel (on a plan) or kind, and its
    // centre, as title and aria-label.
    const name = (x, text) => {
      x.t.title = text;
      x.t.setAttribute("aria-label", text);
    };
    const thinText = (x) => {
      const p = x.o.parts;
      return [p.channel.textContent || p.kind.textContent, p.centre.textContent]
        .filter((s) => s)
        .join(" ");
    };
    let run = []; // consecutive thin tags: more than RAIL_GROUP_MIN adds the count to the first
    const endRun = () => {
      if (run.length > RAIL_GROUP_MIN) name(run[0], `${thinText(run[0])} (${run.length} jobs)`);
      run = [];
    };
    for (const x of items) {
      x.t.removeAttribute("title");
      x.t.removeAttribute("aria-label");
      if (x.thin) {
        name(x, thinText(x));
        x.t.classList.add("thin");
        x.t.style.top = `${clamp(x.c - RAIL_THIN_PX / 2, RAIL_THIN_PX)}px`;
        run.push(x);
      } else {
        x.t.style.top = `${clamp(x.c - x.th / 2, x.th)}px`;
        endRun();
      }
    }
    endRun();
  }

  // `hot` on the job's outline, tag and row (agent.js calls it when its row is hovered).
  setHot(jobId, on) {
    this.hotJob = on ? jobId : this.hotJob === jobId ? null : this.hotJob;
    const o = this.outlines.get(jobId);
    if (o) {
      o.el.classList.toggle("hot", on);
      if (o.tag) o.tag.classList.toggle("hot", on);
    }
    const row = this.jobsEl && this.jobsEl.querySelector(`.job[data-id="${CSS.escape(jobId)}"]`);
    if (row) row.classList.toggle("hot", on);
    if (this.rail) this.layoutTags(); // the hot tag wins the rail's space
  }

  // `speaking` on the outline, tag and row of each job in `ids` (and off the rest): the jobs a
  // mix hears now. Kept in this.speaking and applied again after a sync.
  setSpeaking(ids) {
    this.speaking = [...ids];
    const on = new Set(ids);
    for (const [id, o] of this.outlines) {
      o.el.classList.toggle("speaking", on.has(id));
      if (o.tag) o.tag.classList.toggle("speaking", on.has(id));
    }
    if (this.jobsEl) {
      for (const row of this.jobsEl.querySelectorAll(".job[data-id]")) {
        row.classList.toggle("speaking", on.has(row.dataset.id));
      }
    }
    if (this.rail) this.layoutTags(); // a speaking tag wins the rail's space
  }

  // The pointer entered or left a tag or outline: its row follows.
  hoverTag(o, on) {
    if (!o.job) return;
    this.setHot(o.job.id, on);
  }

  // ---- dragging an outline (retune) ----------------------------------------------------

  retunable(job) {
    return (
      isRunning(job) &&
      rigidBand(job) &&
      !job.operator_graph &&
      !NOT_RETUNABLE.includes(job.kind)
    );
  }

  // Class `retunable` and the two edge handles on `o` when its job can be dragged here.
  setDraggable(o) {
    const on = !!o.job && this.footprint() === "wide" && this.retunable(o.job);
    const d = this.drag;
    if (!on && d && d.key === o.el.dataset.key && !d.sending) this.endDrag(true);
    o.el.classList.toggle("retunable", on);
    // Narrow: the label of a retunable job is the long-press target.
    o.el.classList.toggle(
      "pressable",
      !!o.job && this.footprint() === "narrow" && this.retunable(o.job),
    );
    if (on && !o.handles) {
      o.handles = ["hi", "lo"].map((edge) => {
        const h = document.createElement("div");
        h.className = "outline-handle";
        h.dataset.edge = edge;
        o.el.append(h);
        return h;
      });
    } else if (!on && o.handles) {
      for (const h of o.handles) h.remove();
      o.handles = null;
    }
  }

  listenDrag(o, key) {
    const el = o.el;
    el.addEventListener("pointerdown", (ev) => this.dragStart(ev, key));
    el.addEventListener("pointermove", (ev) => this.dragMove(ev, key));
    el.addEventListener("pointerup", () => this.dragUp(key));
    el.addEventListener("pointercancel", () => this.dragCancel(key));
    el.addEventListener("contextmenu", (ev) => {
      if (this.footprint() !== "wide" || !el.classList.contains("retunable")) return;
      ev.preventDefault();
      this.onOutlineMenu(o.job, ev.clientX, ev.clientY);
    });
    // Narrow: a long press on the tag opens the menu.
    const label = o.tag;
    if (label) this.listenPress(o, label);
    // A wheel over a draggable outline still zooms the waterfall under it.
    el.addEventListener(
      "wheel",
      (ev) => {
        const target = this.wf.overlay;
        if (!target || !el.classList.contains("retunable")) return;
        ev.preventDefault();
        target.dispatchEvent(new WheelEvent("wheel", ev));
      },
      { passive: false },
    );
  }

  listenPress(o, label) {
    label.addEventListener("pointerdown", (ev) => {
      if (this.footprint() !== "narrow" || !o.job || !this.retunable(o.job)) return;
      this.cancelPress();
      const { clientX: x, clientY: y } = ev;
      this.press = {
        x,
        y,
        timer: setTimeout(() => {
          this.press = null;
          if (o.job && this.retunable(o.job)) this.onOutlineMenu(o.job, x, y);
        }, LONG_PRESS_MS),
      };
    });
    label.addEventListener("pointermove", (ev) => {
      const p = this.press;
      if (p && Math.hypot(ev.clientX - p.x, ev.clientY - p.y) > LONG_PRESS_MOVE_PX) {
        this.cancelPress();
      }
    });
    label.addEventListener("pointerup", () => this.cancelPress());
    label.addEventListener("pointercancel", () => this.cancelPress());
    label.addEventListener("contextmenu", (ev) => {
      // A touch long press may raise contextmenu too: ours is the timer's.
      if (this.footprint() === "narrow") ev.preventDefault();
    });
  }

  cancelPress() {
    if (this.press) clearTimeout(this.press.timer);
    this.press = null;
  }

  layerY(ev) {
    return ev.clientY - this.layer.getBoundingClientRect().top;
  }

  dragStart(ev, key) {
    const o = this.outlines.get(key);
    if (!o || ev.button !== 0 || this.drag || !o.el.classList.contains("retunable")) return;
    ev.preventDefault();
    const handle = ev.target.closest(".outline-handle");
    const mode = handle ? handle.dataset.edge : "move";
    try {
      ev.target.setPointerCapture(ev.pointerId);
    } catch {
      // no capture (a synthetic event): the drag still runs while the pointer is over it
    }
    const { lo, hi } = o;
    this.drag = {
      key,
      job: o.job,
      mode,
      y0: this.layerY(ev),
      lo0: lo,
      hi0: hi,
      lo,
      hi,
      moved: false,
    };
  }

  dragMove(ev, key) {
    const d = this.drag;
    const o = this.outlines.get(key);
    if (!d || d.key !== key || d.sending || !o || this.wf.yForHz(d.lo0) === null) return;
    const y = this.layerY(ev);
    if (!d.moved && Math.abs(y - d.y0) < DRAG_CLICK_PX) return;
    const delta = this.wf.hzForY(y) - this.wf.hzForY(d.y0);
    if (!Number.isFinite(delta)) return;
    if (d.mode === "move") {
      d.lo = d.lo0 + delta;
      d.hi = d.hi0 + delta;
    } else if (d.mode === "hi") {
      d.lo = d.lo0;
      d.hi = Math.max(d.hi0 + delta, d.lo0 + MIN_WIDTH_HZ);
    } else {
      d.lo = Math.min(d.lo0 + delta, d.hi0 - MIN_WIDTH_HZ);
      d.hi = d.hi0;
    }
    if (!d.moved) {
      d.moved = true;
      this.ghostEl = document.createElement("div");
      this.ghostEl.className = "outline-ghost";
      this.layer.prepend(this.ghostEl);
      this.readoutEl = document.createElement("span");
      this.readoutEl.className = "outline-readout";
      o.el.append(this.readoutEl);
      this.placeGhost();
    }
    o.lo = d.lo;
    o.hi = d.hi;
    this.placeOutline(o);
    this.readoutEl.textContent = `${bandText((d.lo + d.hi) / 2, d.hi - d.lo)} · release to retune`;
  }

  // The release: a click (under DRAG_CLICK_PX) sends nothing; a drag retunes the job.
  async dragUp(key) {
    const d = this.drag;
    if (!d || d.key !== key || d.sending) return;
    if (!d.moved) {
      this.drag = null;
      return;
    }
    d.sending = true;
    let result = null;
    try {
      result = await this.applyOp({
        op: "job_retune",
        job: d.job.id,
        center_hz: Math.round((d.lo + d.hi) / 2),
        bandwidth_hz: Math.round(d.hi - d.lo),
        replace: [],
      });
    } catch {
      result = null;
    }
    if (this.drag !== d) return; // the outline went meanwhile
    this.endDrag(result === null);
  }

  dragCancel(key) {
    const d = this.drag;
    if (d && d.key === key && !d.sending) this.endDrag(true);
  }

  // Ends the drag, its outline back on the ghost's band with `snapBack`.
  endDrag(snapBack = false) {
    const d = this.drag;
    this.drag = null;
    if (this.ghostEl) this.ghostEl.remove();
    if (this.readoutEl) this.readoutEl.remove();
    this.ghostEl = null;
    this.readoutEl = null;
    const o = d && this.outlines.get(d.key);
    if (o && snapBack) {
      o.lo = d.lo0;
      o.hi = d.hi0;
      this.placeOutline(o);
    }
  }

  placeGhost() {
    const d = this.drag;
    if (!d || !this.ghostEl) return;
    const top = this.wf.yForHz(d.hi0);
    const bottom = this.wf.yForHz(d.lo0);
    if (top === null || bottom === null) return;
    this.ghostEl.style.top = `${top}px`;
    this.ghostEl.style.height = `${Math.max(1, bottom - top)}px`;
  }

  placeOutline(o) {
    const top = this.wf.yForHz(o.hi);
    const bottom = this.wf.yForHz(o.lo);
    if (top === null || bottom === null || !Number.isFinite(top) || !Number.isFinite(bottom)) {
      o.el.hidden = true;
      return;
    }
    o.el.hidden = false;
    const h = Math.max(1, bottom - top);
    o.el.style.top = `${top}px`;
    o.el.style.height = `${h}px`;
    // A band too short for two handles and a body keeps its handles outside it.
    o.el.classList.toggle("handles-out", h < 3 * HANDLE_PX);
    this.layoutTags();
  }

  // Re-places everything vertically (the band or the height changed).
  layout() {
    this.batchTags(() => {
      this.layoutSpans();
      this.outlineCapture();
      for (const [key, o] of this.outlines) {
        if (!this.drag || this.drag.key !== key) this.placeOutline(o);
      }
      this.placeGhost();
      for (const r of this.ribbons) this.setLane(r, r.lane);
      this.fallBack();
      for (const m of this.marks.values()) m.el.style.top = `${this.yFor(m.hz)}px`;
      for (const l of this.labels.values()) {
        l.el.style.top = `${this.yFor(l.hz) - l.h / 2 + (l.dy || 0)}px`;
      }
      this.layoutTags();
    });
  }

  scroll(shift) {
    const width = this.layer.clientWidth;
    this.ribbons = this.ribbons.filter((r) => {
      if (r.held?.()) return true;
      r.right += shift;
      if (r.right > width) {
        r.el.remove();
        return false;
      }
      r.el.style.right = `${r.right}px`;
      return true;
    });
    this.layout();
  }

  expire() {
    const now = performance.now();
    this.ribbons = this.ribbons.filter((r) => {
      if (r.held?.()) r.born = now;
      if (now - r.born <= RIBBON_TTL_MS) return true;
      r.el.remove();
      return false;
    });
  }

  destroy() {
    this.destroyed = true;
    clearInterval(this.timer);
    this.ro.disconnect();
    this.closeFeed();
    for (const id of [...this.positioned.keys()]) this.unwatchPositioned(id);
    for (const [key, o] of this.outlines) if (o.tag) this.untag(o, key); // the rail's
    this.layer.remove();
  }
}
