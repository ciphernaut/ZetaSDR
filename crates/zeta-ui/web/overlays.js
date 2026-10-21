// overlays.js: channel-pinned overlays on the waterfall: layer labels, text ribbons, the
// outlined bands of the rigid holders, the decoded-event counters of the job rows and their
// squelch tags.
//
// Exports:
//   mountOverlays(el, wf, api, {jobsEl}) -> Overlays
//       Adds a pointer-transparent layer over the waterfall canvas inside `el` (the
//       #waterfall element `wf` was mounted in). `jobsEl` (default #jobs) holds the job rows
//       agent.js renders; their `.events` cell carries the counters.
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
//                            selected source, unwatch the rest, refresh the job-row counters
//                            (`—`, tooltip "not watched (limit 4)", on the rows over the cap;
//                            each watch holds one of the node's `max_ws` sockets). `spectrum` jobs are skipped: their events
//                            are the waterfall's and are ignored here anyway. Event streams
//                            of jobs not yet watched are resolved with one GET /streams per
//                            poll; a job whose streams are listed but none is an `event`
//                            stream (e.g. `wfm_demod`) goes in `noEventStream` (job id ->
//                            graph JSON) and is not looked up again until it leaves the
//                            running list or its graph changes. An attached `transcribe`
//                            job is watched like any other (its source is its input's).
//     ov.watchJob(job, streamIds)  subscribes to the job's event streams (an id or an array:
//                            an attached `transcribe` job has two, its `vad` and its
//                            `transcript` stage's; text mode, each re-resolved by provenance
//                            after a drop) and renders each event by payload shape (so by
//                            `protocol`, whichever stream it came on):
//         - `db` array (spectrum): ignored;
//         - `protocol: squelch` (fm_demod, am_demod): not counted, never drawn; kept as the
//           job's last squelch event (ov.squelch, forgotten when the job's graph changes)
//           and the row's tag set from it at once (agent.js setSquelchTag; the 2 s
//           re-render keeps it through agent.js);
//         - every other event bumps the job row's counter;
//         - `label` and `center_hz` (measure): ov.label(center_hz, label, job.id), replacing
//           that job's previous label;
//         - `protocol` (the decoders), at the tuning centre, class = the protocol:
//             `pocsag`: "<address>: <text>" ("<address>: tone" for kind tone);
//             `rds`:    "<ps ?? pi> · <rt ?? ''>", trimmed;
//             `adsb`:   "<icao> <callsign> <altitude_ft> ft <lat>,<lon>", each absent (null)
//                       field omitted ("ft" with the altitude, lat,lon only as a pair);
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
//     ov.outline(lo, hi, text, key) -> element
//         A band lo..hi Hz across the waterfall (`.outline`, data-key = key) bordered at its
//         edges, `text` its label (`.outline-label`) at its top right; one per key, replaced
//         on each call, placed again when the view changes. Drawn under labels and ribbons.
//     ov.unoutline(key)      removes the outline drawn under `key`, if any
//     ov.squelch(jobId) -> event|null  the last `squelch` event seen for a watched job
//     ov.jobRow(job, counter)  shows `counter` (when > 0, or the string `—` for a job over
//                            the cap) in that job's row `.events` cell
//     ov.destroy()

import { setSquelchTag } from "./agent.js";

export const RIBBON_TTL_MS = 60_000;
export const OTHER_EVENT_CHARS = 80;
export const MAX_WATCHED = 4;
export const MAX_RIBBONS = 200;
const EXPIRE_EVERY_MS = 1000;
const RIBBON_GAP_PX = 2;
const LABEL_GAP_PX = 2;
const MAX_LANES = 8;
const CAPTURE_KEY = "capture";
const PIN_KEY = "pin";

const isRunning = (j) => !!(j && j.state && j.state.status === "running");

// Whether `job` (a running `job_list` record) holds its source rigidly with a usable tuning.
function rigidBand(job) {
  const t = job.tuning;
  return (
    !job.elastic &&
    !job.input &&
    job.kind !== "scan" &&
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
    case "adsb": {
      const parts = [p.icao];
      if (present(p.callsign)) parts.push(String(p.callsign).trim());
      if (present(p.altitude_ft)) parts.push(`${p.altitude_ft} ft`);
      if (present(p.lat) && present(p.lon)) parts.push(`${p.lat},${p.lon}`);
      return parts.filter(present).join(" ");
    }
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
  constructor(el, wf, api, { jobsEl = document.getElementById("jobs") } = {}) {
    this.wf = wf;
    this.api = api;
    this.jobsEl = jobsEl;
    this.sourceId = null;
    this.watched = new Map(); // job id -> {job, ids, subs, closed}
    this.noEventStream = new Map(); // job id -> JSON of the graph that has no event stream
    this.resolving = false; // a GET /streams for unwatched jobs is in flight
    this.counters = new Map(); // job id -> non-spectrum events seen while watched
    this.squelches = new Map(); // job id -> {event: the last `squelch` event seen, graph}
    this.labels = new Map(); // key -> {el, hz}
    this.outlines = new Map(); // key -> {el, labelEl, lo, hi}
    this.captureOutlined = false; // the capture is outlined (no rigid holder, no pin)
    this.sourceRec = null; // the selected source's last `source_list` record (its `pin`)
    this.ribbons = []; // {el, countEl, text, cls, hz, w, h, top, dy, right, born, count}
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
    this.noEventStream.clear();
    for (const r of this.ribbons) r.el.remove();
    this.ribbons = [];
    for (const l of this.labels.values()) l.el.remove();
    this.labels.clear();
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
    for (const [id, graph] of [...this.noEventStream]) {
      const j = want.find((x) => x.id === id);
      if (!j || graphKey(j) !== graph) this.noEventStream.delete(id);
    }
    // Jobs known to have no event stream take no socket and so no place under the cap.
    const capped = want
      .filter((j) => !this.noEventStream.has(j.id))
      .sort((a, b) => (b.created_ns ?? 0) - (a.created_ns ?? 0))
      .slice(0, MAX_WATCHED);
    const ids = new Set(capped.map((j) => j.id));
    for (const id of [...this.watched.keys()]) if (!ids.has(id)) this.unwatch(id);
    for (const j of capped) {
      const known = this.watched.get(j.id);
      if (known) known.job = j;
    }
    const fresh = capped.filter((j) => !this.watched.has(j.id));
    if (fresh.length > 0) this.resolve(fresh);
    for (const j of want) {
      // A `graph_put` restarted the job (perhaps with its squelch off): forget the old state.
      const sq = this.squelches.get(j.id);
      if (sq && sq.graph !== graphKey(j)) {
        this.squelches.delete(j.id);
        setSquelchTag(this.jobRowEl(j), null);
      }
      const over = !ids.has(j.id) && !this.noEventStream.has(j.id);
      this.jobRow(j, over ? "—" : this.counters.get(j.id) || 0);
    }
  }

  // The rigid holders' bands (the source's pin among them) outlined, or the capture when
  // there is none. `source` is the selected source's `source_list` record; without one (or
  // another source's) the last one given is kept.
  syncOutlines(jobs, source) {
    if (this.destroyed) return;
    if (source !== undefined) this.sourceRec = source;
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
      this.outline(c - bw / 2, c + bw / 2, j.kind, j.id);
    }
    if (pin) {
      const { center_hz: c, bandwidth_hz: bw } = pin;
      this.outline(c - bw / 2, c + bw / 2, "pin", PIN_KEY);
    }
    this.captureOutlined = rigid.length === 0 && !pin;
    this.outlineCapture();
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
    if (obj && p.protocol === "squelch") {
      this.squelches.set(job.id, { event: p, graph: graphKey(job) });
      setSquelchTag(this.jobRowEl(job), p);
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
      if (p.protocol === "transcript") {
        const t = transcriptRibbon(p);
        const el = this.ribbon(tuningCentre(job), t.text, t.cls);
        if (el && t.title) el.title = `${t.text}\n${t.title}`;
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
    const r = { el, countEl, text, cls, hz, dy: 0, right: 0, born: performance.now(), count: 1 };
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
      const hit = this.ribbons.find((o) => o.right < r.w && top < o.top + o.h && o.top < top + r.h);
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

  outline(lo, hi, text, key) {
    if (this.destroyed) return null;
    let o = this.outlines.get(key);
    if (!o) {
      const el = document.createElement("div");
      el.className = "outline";
      el.dataset.key = key;
      const labelEl = document.createElement("span");
      labelEl.className = "outline-label";
      el.append(labelEl);
      this.layer.prepend(el); // under the labels and ribbons
      o = { el, labelEl, lo, hi };
      this.outlines.set(key, o);
    }
    o.lo = lo;
    o.hi = hi;
    o.labelEl.textContent = String(text);
    o.el.title = `${text} ${(lo / 1e6).toFixed(3)}-${(hi / 1e6).toFixed(3)} MHz`;
    this.placeOutline(o);
    return o.el;
  }

  unoutline(key) {
    const o = this.outlines.get(key);
    if (!o) return;
    o.el.remove();
    this.outlines.delete(key);
  }

  placeOutline(o) {
    const top = this.wf.yForHz(o.hi);
    const bottom = this.wf.yForHz(o.lo);
    if (top === null || bottom === null || !Number.isFinite(top) || !Number.isFinite(bottom)) {
      o.el.hidden = true;
      return;
    }
    o.el.hidden = false;
    o.el.style.top = `${top}px`;
    o.el.style.height = `${Math.max(1, bottom - top)}px`;
  }

  // Re-places everything vertically (the band or the height changed).
  layout() {
    this.outlineCapture();
    for (const o of this.outlines.values()) this.placeOutline(o);
    for (const r of this.ribbons) {
      r.top = this.yFor(r.hz) - r.h / 2 + r.dy;
      r.el.style.top = `${r.top}px`;
    }
    for (const l of this.labels.values()) {
      l.el.style.top = `${this.yFor(l.hz) - l.h / 2 + (l.dy || 0)}px`;
    }
  }

  scroll(shift) {
    const width = this.layer.clientWidth;
    this.ribbons = this.ribbons.filter((r) => {
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
    this.layer.remove();
  }
}
