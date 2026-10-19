// overlays.js: channel-pinned overlays on the waterfall: layer labels, text ribbons and the
// decoded-event counters of the job rows.
//
// Exports:
//   mountOverlays(el, wf, api, {jobsEl}) -> Overlays
//       Adds a pointer-transparent layer over the waterfall canvas inside `el` (the
//       #waterfall element `wf` was mounted in). `jobsEl` (default #jobs) holds the job rows
//       agent.js renders; their `.events` cell carries the counters.
//   Overlays:
//     ov.setSource(id)       the selected source: changing it drops every watch, label and
//                            ribbon (they belong to the previous source's band)
//     ov.sync(jobs)          a `job_list` result (from the 2 s poll): watchJob for every
//                            running job on the selected source, unwatch the rest, refresh
//                            the job-row counters. `spectrum` jobs are skipped: their events
//                            are the waterfall's and are ignored here anyway.
//     ov.watchJob(job)       subscribes to the job's event stream (text mode, re-resolved by
//                            provenance after a drop) and renders each event by payload shape:
//         - `db` array (spectrum): ignored; every other event bumps the job row's counter;
//         - `label` and `center_hz` (measure): ov.label(center_hz, label, job.id), replacing
//           that job's previous label;
//         - `src` and `info` (APRS): ov.ribbon(tuning centre, "<src> > <dst>: <info>",
//           "aprs");
//         - anything else: ov.ribbon(tuning centre, first OTHER_EVENT_CHARS chars of the
//           payload's JSON, "event").
//     ov.ribbon(hz, text, cls) -> element
//         A text ribbon at wf.yForHz(hz) (the band's middle when hz or the band is unknown),
//         entering at the right edge and moving left with the waterfall's columns
//         (wf.onColumn); removed when it leaves the left edge or RIBBON_TTL_MS after it was
//         last refreshed. The same text and class still at the right edge is refreshed with a
//         ×N count instead of drawn again; a different one overlapping it is moved below.
//     ov.label(hz, {layer, name, confidence, evidence}, key = "") -> element
//         "L<layer> <name> <confidence, 2 dp>" at the left edge at wf.yForHz(hz); one label
//         per key (a job id), replaced on each call. `evidence` becomes its tooltip.
//     ov.jobRow(job, counter)  shows `counter` (when > 0) in that job's row `.events` cell
//     ov.destroy()

export const RIBBON_TTL_MS = 60_000;
export const OTHER_EVENT_CHARS = 80;
const EXPIRE_EVERY_MS = 1000;
const STREAM_LOOKUPS = 20;
const STREAM_LOOKUP_MS = 250;
const RIBBON_GAP_PX = 2;
const MAX_LANES = 8;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export function mountOverlays(el, wf, api, opts = {}) {
  return new Overlays(el, wf, api, opts);
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
    this.watched = new Map(); // job id -> {job, sub, closed}
    this.counters = new Map(); // job id -> non-spectrum events seen while watched
    this.labels = new Map(); // key -> {el, hz}
    this.ribbons = []; // {el, countEl, text, cls, hz, w, h, top, dy, right, born, count}
    this.destroyed = false;

    this.layer = document.createElement("div");
    this.layer.className = "ov-layer";
    el.append(this.layer);

    wf.onColumn((shift) => this.scroll(shift));
    this.timer = setInterval(() => this.expire(), EXPIRE_EVERY_MS);
    this.ro = new ResizeObserver(() => this.layout());
    this.ro.observe(this.layer);
  }

  // ---- jobs ----------------------------------------------------------------------------

  setSource(id) {
    if (id === this.sourceId) return;
    this.sourceId = id;
    for (const jobId of [...this.watched.keys()]) this.unwatch(jobId);
    for (const r of this.ribbons) r.el.remove();
    this.ribbons = [];
    for (const l of this.labels.values()) l.el.remove();
    this.labels.clear();
  }

  sync(jobs) {
    if (this.destroyed) return;
    const want = jobs.filter(
      (j) =>
        j.source === this.sourceId &&
        j.kind !== "spectrum" &&
        j.state &&
        j.state.status === "running",
    );
    const ids = new Set(want.map((j) => j.id));
    for (const id of [...this.watched.keys()]) if (!ids.has(id)) this.unwatch(id);
    for (const j of want) this.watchJob(j);
    for (const j of want) this.jobRow(j, this.counters.get(j.id) || 0);
  }

  watchJob(job) {
    if (this.destroyed || job.source !== this.sourceId) return;
    const known = this.watched.get(job.id);
    if (known) {
      known.job = job;
      return;
    }
    const rec = { job, sub: null, closed: false };
    this.watched.set(job.id, rec);
    this.subscribe(rec);
  }

  unwatch(jobId) {
    const rec = this.watched.get(jobId);
    if (!rec) return;
    rec.closed = true;
    if (rec.sub) rec.sub.close();
    this.watched.delete(jobId);
    this.counters.delete(jobId);
    const label = this.labels.get(jobId);
    if (label) {
      label.el.remove();
      this.labels.delete(jobId);
    }
  }

  async lookupStream(jobId) {
    const streams = await this.api.streams();
    const s = streams.find((x) => x.kind === "event" && x.provenance && x.provenance.job === jobId);
    return s ? s.id : null;
  }

  async subscribe(rec) {
    const jobId = rec.job.id;
    let streamId = null;
    for (let i = 0; i < STREAM_LOOKUPS && !rec.closed; i++) {
      try {
        streamId = await this.lookupStream(jobId);
      } catch {
        streamId = null;
      }
      if (streamId) break;
      await sleep(STREAM_LOOKUP_MS);
    }
    if (rec.closed) return;
    if (!streamId) {
      // Forget it so the next poll tries again.
      this.watched.delete(jobId);
      return;
    }
    rec.sub = this.api.streamText(
      streamId,
      (msg) => {
        if (!rec.closed && msg.type === "frame") this.render(rec, msg.payload);
      },
      null,
      { onReconnect: () => this.lookupStream(jobId) },
    );
  }

  render(rec, p) {
    const job = rec.job;
    const obj = p && typeof p === "object" && !Array.isArray(p);
    if (obj && Array.isArray(p.db)) return;
    // Every non-spectrum event counts in the job row, whatever its shape.
    const n = (this.counters.get(job.id) || 0) + 1;
    this.counters.set(job.id, n);
    this.jobRow(job, n);
    if (obj) {
      if (p.label && typeof p.label === "object" && typeof p.center_hz === "number") {
        this.label(p.center_hz, p.label, job.id);
        return;
      }
      if ("src" in p && "info" in p) {
        this.ribbon(tuningCentre(job), `${p.src} > ${p.dst}: ${p.info}`, "aprs");
        return;
      }
    }
    const json = JSON.stringify(p) ?? String(p);
    this.ribbon(tuningCentre(job), json.slice(0, OTHER_EVENT_CHARS), "event");
  }

  jobRow(job, counter) {
    if (!this.jobsEl) return;
    const row = this.jobsEl.querySelector(`.job[data-id="${CSS.escape(job.id)}"]`);
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
    l.el.style.top = `${this.yFor(hz) - l.h / 2}px`;
    return l.el;
  }

  // Re-places everything vertically (the band or the height changed).
  layout() {
    for (const r of this.ribbons) {
      r.top = this.yFor(r.hz) - r.h / 2 + r.dy;
      r.el.style.top = `${r.top}px`;
    }
    for (const l of this.labels.values()) l.el.style.top = `${this.yFor(l.hz) - l.h / 2}px`;
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
