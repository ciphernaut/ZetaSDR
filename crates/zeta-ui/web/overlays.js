// overlays.js: channel-pinned overlays on the waterfall: layer labels, text ribbons, the
// outlined bands of the rigid holders, the decoded-event counters of the job rows and their
// squelch tags.
//
// Exports:
//   mountOverlays(el, wf, api, {jobsEl, plan, applyOp, footprint}) -> Overlays
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
//       pointerup/pointercancel or a move over LONG_PRESS_MOVE_PX).
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
//       the ghost keep a plain `.outline-label`; job outlines have none.
//   Overlays:
//     ov.syncOutlines(jobs, source)  the outline half of ov.sync alone (app.js calls it after
//                            each `source_list` poll, so a pin shows without waiting for jobs)
//     ov.setSource(id)       the selected source: changing it drops every watch, label,
//                            ribbon and outline (they belong to the previous source's band)
//     ov.sync(jobs, source)  a `job_list` result (from the 2 s poll) and the selected
//                            source's `source_list` record: outlines (ov.outline,
//                            key = job id, text = kind) the tuning of every running rigid job
//                            on the selected source (spec 4.2: not elastic, not attached to
//                            another job's output, not a time-shared `scan`) and the source's
//                            `pin` (key "pin", text "pin"; plan 12 spec 4.1), and unoutlines
//                            the rest; with none the capture itself (wf.band, key
//                            "capture", text "capture"; not in scan mode) is outlined,
//                            following the band as it changes. Then watchJob for the
//                            MAX_WATCHED most recent (created_ns) running jobs on the
//                            selected source, those of agent.js SQUELCH_KINDS (fm/am
//                            rows, squelch tag only) only in slots no other job takes,
//                            unwatch the rest, refresh the job-row counters
//                            (`—`, tooltip "not watched (limit 4)", on the rows over the cap;
//                            each watch holds one of the node's `max_ws` sockets). `spectrum` jobs are skipped: their events
//                            are the waterfall's and are ignored here anyway. Event streams
//                            of jobs not yet watched are resolved with one GET /streams per
//                            poll; a job whose streams are listed but none is an `event`
//                            stream (e.g. `wfm_demod`) goes in `noEventStream` (job id ->
//                            graph JSON) and is not looked up again until it leaves the
//                            running list or its graph changes. An attached `transcribe`
//                            job is watched like any other (its source is its input's).
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
//                            the selected one is handed to the ordinary watch.
//     ov.watching(jobId) -> bool  false for a running job over either cap, so its events
//                            are not seen (the Subjects pane says so)
//     ov.watchJob(job, streamIds)  subscribes to the job's event streams (an id or an array:
//                            an attached `transcribe` job has two, its `vad` and its
//                            `transcript` stage's; text mode, each re-resolved by provenance
//                            after a drop) and renders each event by payload shape (so by
//                            `protocol`, whichever stream it came on):
//         - `db` array (spectrum): ignored;
//         - `protocol: squelch`: not counted, never drawn; on an fm_demod or am_demod job
//           (agent.js SQUELCH_KINDS; any other job's is ignored) kept as the
//           job's last squelch event (ov.squelch, forgotten when the job's graph changes)
//           and the row's tag set from it at once (agent.js setSquelchTag; the 2 s
//           re-render keeps it through agent.js); when such a job's watch starts, its
//           `job_context` is asked once and its latched `squelch` event seeds that state
//           unless a live one came first (the gate changes rarely, so a late watch would
//           otherwise miss it);
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
//         ×N count instead of drawn again; a different one overlapping it is moved below.
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
//     ov.squelch(jobId) -> event|null  the last `squelch` event seen for a watched job
//     ov.onEvent(fn)         fn(job, payload) for every non-spectrum event of a watched job,
//                            before it is drawn, and of a positioned job on another source
//                            (the map reads `position` from them)
//     ov.jobRow(job, counter)  shows `counter` (when > 0, or the string `—` for a job over
//                            the cap) in that job's row `.events` cell
//     ov.destroy()

import { SQUELCH_KINDS, setSquelchTag } from "./agent.js";
import { channelLabel } from "./plans.js";
import { subjectLabel, subjectSummary } from "./subjects.js";
import { bandText } from "./units.js";

export const RIBBON_TTL_MS = 60_000;
export const OTHER_EVENT_CHARS = 80;
export const MAX_WATCHED = 4;
export const MAX_POSITIONED = 4;
export const MAX_RIBBONS = 200;
const EXPIRE_EVERY_MS = 1000;
const RIBBON_GAP_PX = 2;
const LABEL_GAP_PX = 2;
const MAX_LANES = 8;
const TAG_GAP_PX = 2;
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

const isRunning = (j) => !!(j && j.state && j.state.status === "running");

// Whether `job` (a running `job_list` record) holds its source rigidly with a usable tuning.
function rigidBand(job) {
  const t = job.tuning;
  return (
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
    } = {},
  ) {
    this.plan = plan;
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
    this.watched = new Map(); // job id -> {job, ids, subs, closed}
    this.noEventStream = new Map(); // job id -> JSON of the graph that has no event stream
    this.resolving = false; // a GET /streams for unwatched jobs is in flight
    this.counters = new Map(); // job id -> non-spectrum events seen while watched
    this.hotJob = null; // the job id whose tag, outline and row show `hot`
    this.speaking = []; // the job ids a mix is playing now: `speaking` on each
    this.snippets = new Map(); // input job id -> the last transcript text of its attached job
    this.squelches = new Map(); // job id -> {event: the last `squelch` event seen, graph}
    this.labels = new Map(); // key -> {el, hz}
    this.outlines = new Map(); // key -> {el, labelEl (pin, capture) or tag, lo, hi, job, handles}
    this.marks = new Map(); // `${job id}:${rounded centre}` -> {el, hz}: a watch's `signal new`
    this.captureOutlined = false; // the capture is outlined (no rigid holder, no pin)
    this.sourceRec = null; // the selected source's last `source_list` record (its `pin`)
    this.ribbons = []; // {el, bodyEl, countEl, text, cls, hz, w, h, top, dy, right, born, count, key, held}
    this.eventFns = []; // ov.onEvent listeners
    this.positioned = new Map(); // job id -> {job, ids, subs, closed, positionOnly: true}
    this.resolvingPositioned = false; // a GET /streams for unwatched positioned jobs
    this.subjects = subjects; // the row cache and the subjects (subjects.js)
    this.overCap = new Set(); // running job ids over MAX_WATCHED or MAX_POSITIONED
    this.destroyed = false;

    this.layer = document.createElement("div");
    this.layer.className = "ov-layer";
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
    for (const jobId of [...this.watched.keys()]) this.unwatch(jobId);
    for (const [jobId, rec] of [...this.positioned]) {
      if (rec.job.source === id) this.unwatchPositioned(jobId);
    }
    this.noEventStream.clear();
    for (const r of this.ribbons) r.el.remove();
    this.ribbons = [];
    for (const l of this.labels.values()) l.el.remove();
    this.labels.clear();
    for (const key of [...this.marks.keys()]) this.unmark(key);
    for (const key of [...this.outlines.keys()]) this.unoutline(key);
    this.captureOutlined = false;
  }

  sync(jobs, source) {
    if (this.destroyed) return;
    this.syncOutlines(jobs, source);
    const want = jobs.filter(
      (j) =>
        j.source === this.sourceId &&
        j.kind !== "spectrum" &&
        j.state &&
        j.state.status === "running",
    );
    this.syncMarks(want);
    for (const [id, graph] of [...this.noEventStream]) {
      const j = want.find((x) => x.id === id);
      if (!j || graphKey(j) !== graph) this.noEventStream.delete(id);
    }
    // Jobs known to have no event stream take no socket and so no place under the cap.
    // Decoders first, then the SQUELCH_KINDS jobs (whose stream only tags their row), each
    // newest first: a squelch tag never crowds a decoder's ribbons and counter out.
    const squelchOnly = (j) => (SQUELCH_KINDS.includes(j.kind) ? 1 : 0);
    const capped = want
      .filter((j) => !this.noEventStream.has(j.id))
      .sort((a, b) => squelchOnly(a) - squelchOnly(b) || (b.created_ns ?? 0) - (a.created_ns ?? 0))
      .slice(0, MAX_WATCHED);
    const ids = new Set(capped.map((j) => j.id));
    for (const id of [...this.watched.keys()]) if (!ids.has(id)) this.unwatch(id);
    for (const j of capped) {
      const known = this.watched.get(j.id);
      if (known) known.job = j;
    }
    this.lookup(jobs);
    const fresh = capped.filter((j) => !this.watched.has(j.id) && this.rowKnown(j));
    if (fresh.length > 0) this.resolve(fresh);
    this.overCap = new Set(
      want.filter((j) => !ids.has(j.id) && !this.noEventStream.has(j.id)).map((j) => j.id),
    );
    for (const j of want) {
      // A `graph_put` restarted the job (perhaps with its squelch off): forget the old state.
      const sq = this.squelches.get(j.id);
      if (sq && sq.graph !== graphKey(j)) {
        this.squelches.delete(j.id);
        setSquelchTag(this.jobRowEl(j), null);
        this.refreshTag(this.outlines.get(j.id));
      }
      const over = !ids.has(j.id) && !this.noEventStream.has(j.id);
      this.jobRow(j, over ? "—" : this.counters.get(j.id) || 0);
    }
    // agent.js re-rendered the rows: the hovered job's row is hot again.
    if (this.hotJob) this.setHot(this.hotJob, true);
    this.setSpeaking(this.speaking);
    this.syncPositioned(jobs);
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
    for (const j of followed.slice(MAX_POSITIONED)) this.overCap.add(j.id);
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
  }

  // The capture's outline follows wf.band while no rigid holder is outlined.
  outlineCapture() {
    const b = this.wf.band;
    if (this.captureOutlined && b && this.wf.mode !== "scan") {
      const o = this.outlines.get(CAPTURE_KEY);
      if (!o || o.lo !== b.lo || o.hi !== b.hi) this.outline(b.lo, b.hi, "capture", CAPTURE_KEY);
    } else {
      this.unoutline(CAPTURE_KEY);
    }
  }

  // One GET /streams for the jobs not yet watched: watch those with an event stream, cache
  // those whose streams are listed without one. A job with no stream listed yet (just
  // started), or a failed request, is tried again on the next poll.
  async resolve(jobs) {
    if (this.resolving) return;
    this.resolving = true;
    let streams = null;
    try {
      streams = await this.api.streams();
    } catch {
      streams = null;
    } finally {
      this.resolving = false;
    }
    if (!streams || this.destroyed) return;
    for (const j of jobs) {
      if (j.source !== this.sourceId || this.watched.has(j.id)) continue;
      const mine = streams.filter((x) => x.provenance && x.provenance.job === j.id);
      const ev = mine.filter((x) => x.kind === "event").map((x) => x.id);
      if (ev.length > 0) this.watchJob(j, ev);
      else if (mine.length > 0) this.noEventStream.set(j.id, graphKey(j));
    }
  }

  watchJob(job, streamIds) {
    if (this.destroyed || job.source !== this.sourceId) return;
    const known = this.watched.get(job.id);
    if (known) {
      known.job = job;
      return;
    }
    const ids = Array.isArray(streamIds) ? [...streamIds] : [streamIds];
    const rec = { job, ids, subs: [], closed: false };
    this.watched.set(job.id, rec);
    ids.forEach((_, slot) => this.subscribe(rec, slot));
    if (SQUELCH_KINDS.includes(job.kind)) this.seedSquelch(rec);
  }

  // The gate the job's context latched (its last `squelch` event), asked once the
  // subscriptions are open: a job whose only event came before this watch is still tagged.
  // A live event seen meanwhile is newer and wins; a failed request leaves the live path.
  async seedSquelch(rec) {
    const job = rec.job;
    let r = null;
    try {
      r = await this.api.op({ op: "job_context", job: job.id });
    } catch {
      return;
    }
    const p = r && r.value && r.value.squelch;
    if (rec.closed || this.destroyed || this.watched.get(job.id) !== rec) return;
    if (!p || p.protocol !== "squelch" || this.squelches.has(job.id)) return;
    this.squelches.set(job.id, { event: p, graph: graphKey(rec.job) });
    setSquelchTag(this.jobRowEl(rec.job), p);
    this.refreshTag(this.outlines.get(job.id));
  }

  unwatch(jobId) {
    const rec = this.watched.get(jobId);
    if (!rec) return;
    rec.closed = true;
    for (const sub of rec.subs) sub.close();
    this.watched.delete(jobId);
    this.counters.delete(jobId);
    this.squelches.delete(jobId);
    this.unlabel(jobId);
    for (const key of [...this.marks.keys()]) if (key.startsWith(`${jobId}:`)) this.unmark(key);
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

  subscribe(rec, slot) {
    rec.subs[slot] = this.api.streamText(
      rec.ids[slot],
      (msg) => {
        if (!rec.closed && msg.type === "frame") this.render(rec, msg.payload);
      },
      null,
      { onReconnect: () => this.lookupStream(rec, slot) },
    );
  }

  render(rec, p) {
    const job = rec.job;
    const obj = p && typeof p === "object" && !Array.isArray(p);
    if (obj && (Array.isArray(p.db) || Array.isArray(p.q))) return; // spectrum rows
    for (const fn of this.eventFns) fn(job, p);
    if (rec.positionOnly) return; // another source's job: the map's alone
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
    this.jobRow(job, n);
    if (obj) {
      if (p.label && typeof p.label === "object" && typeof p.center_hz === "number") {
        this.label(p.center_hz, p.label, job.id);
        return;
      }
      if (p.protocol === "vad") return;
      if (p.protocol === "signal" && typeof p.center_hz === "number") {
        const key = `${job.id}:${Math.round(p.center_hz)}`;
        if (p.state === "gone") this.unmark(key);
        else if (p.state === "new") this.mark(p.center_hz, p.summary, key);
      }
      if (p.protocol === "transcript") {
        if (job.input && typeof p.text === "string" && !p.dropped && !present(p.error)) {
          this.snippets.set(job.input, p.text.slice(0, SNIPPET_CHARS));
          this.refreshTag(this.outlines.get(job.input));
        }
        const t = transcriptRibbon(p);
        const el = this.ribbon(tuningCentre(job), t.text, t.cls);
        if (el && t.title) el.title = `${t.text}\n${t.title}`;
        return;
      }
      if (typeof p.summary === "string") {
        if (this.subjectRibbon(job, p)) return;
        this.ribbon(tuningCentre(job), p.summary, typeof p.protocol === "string" ? p.protocol : "event");
        return;
      }
      if (typeof p.protocol === "string") {
        const text = protocolText(p);
        if (text !== null) {
          this.ribbon(tuningCentre(job), text, p.protocol);
          return;
        }
      }
      if ("src" in p && "info" in p) {
        this.ribbon(tuningCentre(job), `${p.src} > ${p.dst}: ${p.info}`, "aprs");
        return;
      }
    }
    const json = JSON.stringify(p) ?? String(p);
    this.ribbon(tuningCentre(job), json.slice(0, OTHER_EVENT_CHARS), "event");
  }

  onEvent(fn) {
    this.eventFns.push(fn);
  }

  squelch(jobId) {
    const s = this.squelches.get(jobId);
    return s ? s.event : null;
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
    if (typeof counter === "string") {
      cell.textContent = counter;
      cell.title = `not watched (limit ${MAX_WATCHED})`;
      return;
    }
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
        `job:${job.id}:${k.key}`, hz, subjectSummary(s), row.protocol, s.count,
        () => store.held(job.id, k.key),
      );
      return true;
    }
    const n = store.list(job.id).length;
    const line = `${row.protocol} ${n} ${row.subject.name}, last ${subjectLabel(store.lastHeard(job.id))}`;
    this.keyedRibbon(`job:${job.id}`, hz, line, row.protocol, 1, () => store.held(job.id, null));
    return true;
  }

  // The ribbon for `key`, made or updated in place: the text replaced, `×count` when `count`
  // is over 1, the width measured again, the ribbon back at the right edge and placed again if
  // it had moved. `held()` true keeps it from scrolling or expiring while its subject is heard.
  keyedRibbon(key, hz, text, cls, count, held) {
    if (this.destroyed) return null;
    text = String(text);
    let r = this.ribbons.find((o) => o.key === key);
    if (!r) {
      const el = this.ribbon(hz, text, cls);
      r = this.ribbons[this.ribbons.length - 1];
      if (!r || r.el !== el) return el;
      r.key = key;
    }
    const moved = r.right > 0;
    r.held = held;
    r.hz = hz;
    r.text = text;
    r.bodyEl.textContent = text;
    r.el.title = text;
    r.count = count;
    r.countEl.textContent = count > 1 ? ` ×${count}` : "";
    r.w = r.el.offsetWidth;
    r.right = 0;
    r.el.style.right = "0px";
    r.born = performance.now();
    if (moved) this.place(r);
    return r.el;
  }

  ribbon(hz, text, cls = "") {
    if (this.destroyed) return null;
    text = String(text);
    const same = this.ribbons.find(
      (r) => r.text === text && r.cls === cls && r.hz === hz && r.right < r.w,
    );
    if (same) {
      same.count += 1;
      same.countEl.textContent = ` ×${same.count}`;
      same.born = performance.now();
      return same.el;
    }
    const el = document.createElement("div");
    el.className = cls ? `ov-ribbon ${cls}` : "ov-ribbon";
    const body = document.createElement("span");
    body.textContent = text;
    const countEl = document.createElement("span");
    countEl.className = "count";
    el.append(body, countEl);
    el.title = text;
    el.style.right = "0px";
    this.layer.append(el);
    const r = { el, bodyEl: body, countEl, text, cls, hz, dy: 0, right: 0, born: performance.now(), count: 1 };
    r.w = el.offsetWidth;
    r.h = el.offsetHeight;
    this.place(r);
    this.ribbons.push(r);
    while (this.ribbons.length > MAX_RIBBONS) this.ribbons.shift().el.remove();
    return el;
  }

  // Moves a new ribbon below any ribbon it would overlap at the right edge.
  place(r) {
    const base = this.yFor(r.hz) - r.h / 2;
    let top = base;
    for (let lane = 0; lane < MAX_LANES; lane++) {
      const hit = this.ribbons.find((o) => o !== r && o.right < r.w && top < o.top + o.h && o.top < top + r.h);
      if (!hit) break;
      top = hit.top + hit.h + RIBBON_GAP_PX;
    }
    r.dy = top - base;
    r.top = top;
    r.el.style.top = `${top}px`;
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
        this.tag(o, key);
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

  // ---- job tags ------------------------------------------------------------------------

  // The tag of a job's outline `o`: kind, short id, [squelch dot], channel, [snippet].
  tag(o, key) {
    const el = document.createElement("div");
    el.className = "job-tag";
    el.dataset.job = key;
    const span = (cls) => {
      const s = document.createElement("span");
      s.className = cls;
      return s;
    };
    o.parts = { kind: span("kind"), short: span("short"), channel: span("channel"), dot: null, snippet: null };
    el.append(o.parts.kind, " ", o.parts.short, " ", o.parts.channel, " ");
    o.el.append(el);
    o.tag = el;
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
    this.snippets.delete(key);
    if (this.hotJob === key) this.hotJob = null;
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

  // Tags at their outline's vertical centre, stacked: sorted by top, a tag under the
  // previous one's bottom + TAG_GAP_PX is pushed down (at most MAX_LANES in a row).
  layoutTags() {
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
    this.outlineCapture();
    for (const [key, o] of this.outlines) {
      if (!this.drag || this.drag.key !== key) this.placeOutline(o);
    }
    this.placeGhost();
    for (const r of this.ribbons) {
      r.top = this.yFor(r.hz) - r.h / 2 + r.dy;
      r.el.style.top = `${r.top}px`;
    }
    for (const m of this.marks.values()) m.el.style.top = `${this.yFor(m.hz)}px`;
    for (const l of this.labels.values()) {
      l.el.style.top = `${this.yFor(l.hz) - l.h / 2 + (l.dy || 0)}px`;
    }
    this.layoutTags();
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
    for (const id of [...this.watched.keys()]) this.unwatch(id);
    for (const id of [...this.positioned.keys()]) this.unwatchPositioned(id);
    this.layer.remove();
  }
}
