// waterfall.js: the scrolling waterfall fed by a `spectrum` job's event stream.
//
// Exports:
//   mountWaterfall(el, api, source = null) -> Waterfall
//       Builds the axis, the canvas, the selection overlay and the #wf-badge inside `el`.
//   Waterfall:
//     wf.setSource(sourceInfo)  a `source_list` record (or null). Reuses a running `spectrum`
//         job on that source from `job_list`, else starts one on the source's `current` band
//         or, when `current` is null, on the centre of its tune range with
//         min(max_bandwidth_hz, 1 MHz). UI-started job ids go in sessionStorage[UI_JOBS_KEY];
//         they are stopped on `pagehide` (fetch keepalive, with the bearer). The job's event
//         stream is followed in text mode.
//     wf.sourceId   id of the source being shown (null when none, or after setSource failed)
//     wf.jobId      the followed job's id (null until known)
//     wf.painted    columns painted since mount (never reset; for tests)
//     wf.onSelect(cb)   cb({centerHz, bandwidthHz, startNs, endNs}) after a drag; the same
//         object is kept in window.zetaLastSelection.
//     wf.yForHz(hz) -> CSS px from the canvas top (null before the band is known)
//     wf.destroy()
//
// Reconnect: when the stream's socket closes (a `closed` message, an error) the #wf-badge
// "reconnecting…" shows and api.streamText reopens with backoff, re-resolving the stream id
// by provenance (event stream of the job) because it changes when the job restarts under
// `graph_put`. If the job is no longer running (stopped by another tab, the CLI, a Stop
// button), setSource runs again for the same source: it reuses another running spectrum job
// or starts its own. The badge hides at the first frame after that.
//
// Layout: frequency is vertical (highest at the top), time runs right to left: each event
// becomes one CSS-pixel column (Math.round(devicePixelRatio) backing pixels, at least 1) at
// the right edge after the canvas is shifted left by that much with drawImage. At most
// MAX_ROWS_PER_S columns per second are painted; extra events are dropped. Colour: dB
// clamped to [floor - 10, floor + 60], floor being the 15th percentile of the column's own
// dB values, through a 256-entry blue-yellow-white ramp. The frequency
// band comes from each event (`center_hz`, `size * bin_hz`), falling back to the job's tuning.
// Per-column start_ns/end_ns are kept in a ring of RING_COLUMNS for the selection's time span.
//
// Reload: pagehide stops the UI's jobs and records their ids under STOPPING_KEY; the next page
// in the tab never reuses those (their stop may still be in flight), stops them again in case
// the keepalive request was lost, and starts a fresh job instead. A page restored from the
// back/forward cache (`pageshow` with `persisted`) does the same and runs setSource again.

export const MAX_ROWS_PER_S = 30;
export const RING_COLUMNS = 2048;
export const AXIS_TICKS = 5;
export const UI_JOBS_KEY = "zeta.uiJobs";
export const STOPPING_KEY = "zeta.uiJobsStopping";
const BELOW_FLOOR_DB = 10;
const ABOVE_FLOOR_DB = 60;
const FLOOR_PERCENTILE = 0.15;
const MAX_UI_BANDWIDTH_HZ = 1_000_000;
const STREAM_LOOKUPS = 20;
const STREAM_LOOKUP_MS = 250;

// ---- storage ---------------------------------------------------------------------------

function loadIds(key) {
  try {
    const v = JSON.parse(sessionStorage.getItem(key) || "[]");
    return Array.isArray(v) ? v.filter((x) => typeof x === "string") : [];
  } catch {
    return [];
  }
}

function storeIds(key, ids) {
  try {
    if (ids.length) sessionStorage.setItem(key, JSON.stringify(ids));
    else sessionStorage.removeItem(key);
  } catch {
    /* storage unavailable: cleanup then only covers this page load */
  }
}

// ---- colour ----------------------------------------------------------------------------

const RAMP_STOPS = [
  [0, [0, 0, 48]],
  [0.35, [0, 90, 255]],
  [0.75, [255, 225, 0]],
  [1, [255, 255, 255]],
];

function buildRamp() {
  const ramp = new Uint8ClampedArray(256 * 3);
  for (let i = 0; i < 256; i++) {
    const t = i / 255;
    let k = 0;
    while (k < RAMP_STOPS.length - 2 && t > RAMP_STOPS[k + 1][0]) k++;
    const [t0, c0] = RAMP_STOPS[k];
    const [t1, c1] = RAMP_STOPS[k + 1];
    const f = (t - t0) / (t1 - t0);
    for (let ch = 0; ch < 3; ch++) ramp[i * 3 + ch] = Math.round(c0[ch] + (c1[ch] - c0[ch]) * f);
  }
  return ramp;
}

const RAMP = buildRamp();

function percentile(values, p) {
  const sorted = Float64Array.from(values).sort();
  return sorted[Math.min(sorted.length - 1, Math.floor(p * sorted.length))];
}

// ---- source band -----------------------------------------------------------------------

function capHz(cap) {
  if (typeof cap === "number") return cap;
  if (cap && typeof cap === "object") {
    const v = Object.values(cap).find((x) => typeof x === "number");
    return v === undefined ? null : v;
  }
  return null;
}

// The {center_hz, bandwidth_hz} a UI spectrum job uses on `src`, or null.
export function sourceBand(src) {
  if (src.current && src.current.center_hz && src.current.bandwidth_hz) {
    return { center_hz: src.current.center_hz, bandwidth_hz: src.current.bandwidth_hz };
  }
  const c = src.capabilities || {};
  const lo = capHz(c.tune_min_hz);
  const hi = capHz(c.tune_max_hz);
  const bw = capHz(c.max_bandwidth_hz);
  if (lo === null || hi === null || bw === null) return null;
  return {
    center_hz: Math.round((lo + hi) / 2),
    bandwidth_hz: Math.min(bw, MAX_UI_BANDWIDTH_HZ),
  };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---- the waterfall ---------------------------------------------------------------------

export function mountWaterfall(el, api, source = null) {
  return new Waterfall(el, api, source);
}

class Waterfall {
  constructor(el, api, source) {
    this.el = el;
    this.api = api;
    this.selectCbs = [];
    this.gen = 0;
    this.sub = null;
    this.src = null; // the source record being shown
    this.jobId = null;
    this.band = null; // {lo, hi} Hz
    this.lastPaint = -Infinity;
    this.columns = 0; // columns painted since the last clear
    this.painted = 0; // columns painted since mount (never reset)
    this.ringStart = new Array(RING_COLUMNS).fill(0);
    this.ringEnd = new Array(RING_COLUMNS).fill(0);
    this.drag = null;

    this.axis = document.createElement("div");
    this.axis.className = "wf-axis";
    this.canvas = document.createElement("canvas");
    this.canvas.className = "wf-canvas";
    this.overlay = document.createElement("canvas");
    this.overlay.className = "wf-overlay";
    this.badge = document.createElement("div");
    this.badge.id = "wf-badge";
    this.badge.textContent = "reconnecting…";
    this.badge.hidden = true;
    el.replaceChildren(this.axis, this.canvas, this.overlay, this.badge);
    this.ctx = this.canvas.getContext("2d", { willReadFrequently: true });
    this.octx = this.overlay.getContext("2d");

    this.resize();
    this.ro = new ResizeObserver(() => this.resize());
    this.ro.observe(el);

    this.onDown = (ev) => this.dragStart(ev);
    this.onMove = (ev) => this.dragMove(ev);
    this.onUp = (ev) => this.dragEnd(ev);
    this.overlay.addEventListener("pointerdown", this.onDown);
    this.overlay.addEventListener("pointermove", this.onMove);
    this.overlay.addEventListener("pointerup", this.onUp);
    this.overlay.addEventListener("pointercancel", this.onUp);

    this.onPageHide = () => this.stopUiJobs();
    window.addEventListener("pagehide", this.onPageHide);
    this.onPageShow = (ev) => {
      if (!ev.persisted) return;
      this.cleanupStopping();
      if (this.src) this.setSource(this.src);
    };
    window.addEventListener("pageshow", this.onPageShow);

    this.cleanupStopping();
    if (source) this.setSource(source);
  }

  // ---- jobs and stream -----------------------------------------------------------------

  // Jobs a previous page in this tab asked to stop on its pagehide: never reused; stopped
  // again in case that keepalive request was lost.
  cleanupStopping() {
    const ids = loadIds(STOPPING_KEY);
    this.stopping = new Set([...(this.stopping || []), ...ids]);
    storeIds(STOPPING_KEY, []);
    for (const id of ids) this.api.op({ op: "job_stop", job: id }).catch(() => {});
  }

  get sourceId() {
    return this.src ? this.src.id : null;
  }

  stopUiJobs() {
    const ids = loadIds(UI_JOBS_KEY);
    if (!ids.length) return;
    storeIds(STOPPING_KEY, [...new Set([...loadIds(STOPPING_KEY), ...ids])]);
    storeIds(UI_JOBS_KEY, []);
    for (const id of ids) {
      fetch(this.api.base + "/op", {
        method: "POST",
        keepalive: true,
        headers: { Authorization: "Bearer " + this.api.secret, "Content-Type": "application/json" },
        body: JSON.stringify({ op: "job_stop", job: id }),
      }).catch(() => {});
    }
  }

  // `restart`: the followed job went away; keep the badge up until frames flow again.
  async setSource(src, { restart = false } = {}) {
    const gen = ++this.gen;
    if (this.sub) this.sub.close();
    this.sub = null;
    this.jobId = null;
    this.src = src;
    this.clear();
    if (!restart) this.badge.hidden = true;
    if (!src) return;
    try {
      const jobId = await this.findOrStartJob(src);
      if (gen !== this.gen || !jobId) return;
      this.jobId = jobId;
      const streamId = await this.findStream(jobId, gen);
      if (gen !== this.gen) return;
      if (!streamId) throw new Error(`no event stream for job ${jobId}`);
      this.sub = this.api.streamText(
        streamId,
        (msg) => {
          if (gen !== this.gen || msg.type !== "frame") return;
          this.badge.hidden = true;
          this.addEvent(msg);
        },
        ({ reconnecting }) => {
          if (gen === this.gen && reconnecting) this.badge.hidden = false;
        },
        { onReconnect: () => this.reresolve(src, jobId, gen) },
      );
    } catch (e) {
      if (gen === this.gen) this.src = null; // selecting the source again retries
      console.warn(`waterfall ${src.id}: ${e.message}`);
    }
  }

  // The followed job's current event stream id; null to try again after the next backoff.
  // When the job no longer runs, setSource starts over (closing this subscription).
  async reresolve(src, jobId, gen) {
    const id = await this.lookupStream(jobId);
    if (gen !== this.gen) return null;
    if (id) return id;
    const jobs = (await this.api.op({ op: "job_list" })).value || [];
    if (gen !== this.gen) return null;
    const job = jobs.find((j) => j.id === jobId);
    if (job && job.state && job.state.status === "running") return null; // restarting
    this.forgetUiJob(jobId);
    this.setSource(src, { restart: true });
    return null;
  }

  forgetUiJob(id) {
    const ids = loadIds(UI_JOBS_KEY);
    if (ids.includes(id)) storeIds(UI_JOBS_KEY, ids.filter((x) => x !== id));
  }

  async findOrStartJob(src) {
    const jobs = (await this.api.op({ op: "job_list" })).value || [];
    const running = jobs.find(
      (j) =>
        j.kind === "spectrum" &&
        j.source === src.id &&
        j.state &&
        j.state.status === "running" &&
        !this.stopping.has(j.id),
    );
    if (running) {
      // Without a usable tuning the band comes from the first event.
      const t = running.tuning;
      this.tuning = t && t.center_hz && t.bandwidth_hz ? t : null;
      if (this.tuning) this.setBand(this.tuning.center_hz, this.tuning.bandwidth_hz);
      return running.id;
    }
    const band = sourceBand(src);
    if (!band) throw new Error("no tuning or tune range to show");
    const r = await this.api.op({
      op: "job_start",
      kind: "spectrum",
      source: src.id,
      center_hz: band.center_hz,
      bandwidth_hz: band.bandwidth_hz,
      deviation_hz: null,
    });
    const id = r.value;
    storeIds(UI_JOBS_KEY, [...new Set([...loadIds(UI_JOBS_KEY), id])]);
    this.tuning = band;
    this.setBand(band.center_hz, band.bandwidth_hz);
    return id;
  }

  async lookupStream(jobId) {
    const streams = await this.api.streams();
    const s = streams.find((x) => x.kind === "event" && x.provenance && x.provenance.job === jobId);
    return s ? s.id : null;
  }

  // The job's event stream id; it can appear a moment after job_start.
  async findStream(jobId, gen) {
    for (let i = 0; i < STREAM_LOOKUPS && gen === this.gen; i++) {
      const id = await this.lookupStream(jobId);
      if (id) return id;
      await sleep(STREAM_LOOKUP_MS);
    }
    return null;
  }

  // ---- geometry ------------------------------------------------------------------------

  setBand(centerHz, spanHz) {
    const lo = centerHz - spanHz / 2;
    const hi = centerHz + spanHz / 2;
    if (this.band && this.band.lo === lo && this.band.hi === hi) return;
    if (this.band) this.clear();
    this.band = { lo, hi };
    this.drawAxis();
  }

  cssHeight() {
    return this.canvas.clientHeight || 1;
  }

  yForHz(hz) {
    if (!this.band) return null;
    return ((this.band.hi - hz) / (this.band.hi - this.band.lo)) * this.cssHeight();
  }

  hzForY(y) {
    return this.band.hi - (y / this.cssHeight()) * (this.band.hi - this.band.lo);
  }

  resize() {
    const dpr = window.devicePixelRatio || 1;
    const w = Math.max(1, Math.round(this.canvas.clientWidth * dpr));
    const h = Math.max(1, Math.round(this.canvas.clientHeight * dpr));
    if (w === this.canvas.width && h === this.canvas.height) return;
    // Keep what is painted: anchored right, stretched to the new height.
    let old = null;
    if (this.columns > 0 && this.canvas.width > 1) {
      old = document.createElement("canvas");
      old.width = this.canvas.width;
      old.height = this.canvas.height;
      old.getContext("2d").drawImage(this.canvas, 0, 0);
    }
    this.canvas.width = w;
    this.canvas.height = h;
    this.overlay.width = w;
    this.overlay.height = h;
    this.ctx.fillStyle = "#000030";
    this.ctx.fillRect(0, 0, w, h);
    if (old) this.ctx.drawImage(old, w - old.width, 0, old.width, h);
    this.column = this.ctx.createImageData(Math.max(1, Math.round(dpr)), h);
    this.drawAxis();
  }

  clear() {
    this.columns = 0;
    this.band = null;
    this.ctx.fillStyle = "#000030";
    this.ctx.fillRect(0, 0, this.canvas.width, this.canvas.height);
    this.octx.clearRect(0, 0, this.overlay.width, this.overlay.height);
    this.axis.replaceChildren();
  }

  drawAxis() {
    if (!this.band) return;
    const h = this.cssHeight();
    const ticks = [];
    for (let i = 0; i < AXIS_TICKS; i++) {
      const hz = this.band.hi - (i / (AXIS_TICKS - 1)) * (this.band.hi - this.band.lo);
      const t = document.createElement("span");
      t.className = "tick";
      t.textContent = (hz / 1e6).toFixed(3);
      t.style.top = `${(i / (AXIS_TICKS - 1)) * h}px`;
      t.dataset.edge = i === 0 ? "top" : i === AXIS_TICKS - 1 ? "bottom" : "";
      ticks.push(t);
    }
    this.axis.replaceChildren(...ticks);
  }

  // ---- painting ------------------------------------------------------------------------

  addEvent(msg) {
    const p = msg.payload;
    if (!p || !Array.isArray(p.db) || !p.db.length) return;
    const now = performance.now();
    if (now - this.lastPaint < 1000 / MAX_ROWS_PER_S) return;
    this.lastPaint = now;
    const size = p.db.length;
    if (typeof p.center_hz === "number" && typeof p.bin_hz === "number") {
      this.setBand(p.center_hz, p.bin_hz * size);
    } else if (this.tuning) {
      this.setBand(this.tuning.center_hz, this.tuning.bandwidth_hz);
    }
    this.paintColumn(p.db);
    const slot = this.columns % RING_COLUMNS;
    this.ringStart[slot] = Number(msg.start_ns) || 0;
    this.ringEnd[slot] = Number(msg.end_ns) || this.ringStart[slot];
    this.columns += 1;
    this.painted += 1;
  }

  paintColumn(db) {
    const w = this.canvas.width;
    const h = this.canvas.height;
    const cw = Math.min(this.column.width, w); // backing pixels per event
    const size = db.length;
    const floor = percentile(db, FLOOR_PERCENTILE);
    const lo = floor - BELOW_FLOOR_DB;
    const scale = 255 / (BELOW_FLOOR_DB + ABOVE_FLOOR_DB);
    const px = this.column.data;
    for (let y = 0; y < h; y++) {
      // Row y covers bins [b0, b1) counted from the top (highest frequency, last bin);
      // the strongest bin wins so a narrow carrier stays visible when bins outnumber rows.
      const b0 = Math.floor((1 - (y + 1) / h) * size);
      const b1 = Math.max(b0 + 1, Math.floor((1 - y / h) * size));
      let v = -Infinity;
      for (let b = b0; b < b1 && b < size; b++) if (db[b] > v) v = db[b];
      const idx = Math.max(0, Math.min(255, Math.round((v - lo) * scale))) * 3;
      for (let x = 0; x < cw; x++) {
        const o = (y * this.column.width + x) * 4;
        px[o] = RAMP[idx];
        px[o + 1] = RAMP[idx + 1];
        px[o + 2] = RAMP[idx + 2];
        px[o + 3] = 255;
      }
    }
    if (w > cw) this.ctx.drawImage(this.canvas, cw, 0, w - cw, h, 0, 0, w - cw, h);
    this.ctx.putImageData(this.column, w - cw, 0);
  }

  // ---- selection -----------------------------------------------------------------------

  onSelect(cb) {
    this.selectCbs.push(cb);
  }

  local(ev) {
    const r = this.overlay.getBoundingClientRect();
    return {
      x: Math.max(0, Math.min(r.width, ev.clientX - r.left)),
      y: Math.max(0, Math.min(r.height, ev.clientY - r.top)),
    };
  }

  dragStart(ev) {
    if (ev.button !== 0) return;
    this.overlay.setPointerCapture(ev.pointerId);
    const p = this.local(ev);
    this.drag = { x0: p.x, y0: p.y, x1: p.x, y1: p.y };
    this.drawDrag();
  }

  dragMove(ev) {
    if (!this.drag) return;
    const p = this.local(ev);
    this.drag.x1 = p.x;
    this.drag.y1 = p.y;
    this.drawDrag();
  }

  dragEnd(ev) {
    if (!this.drag) return;
    this.dragMove(ev);
    const d = this.drag;
    this.drag = null;
    if (Math.abs(d.y1 - d.y0) < 1 || !this.band) {
      this.octx.clearRect(0, 0, this.overlay.width, this.overlay.height);
      return;
    }
    const sel = this.resolve(d);
    if (!sel) return;
    window.zetaLastSelection = sel;
    for (const cb of this.selectCbs) cb(sel);
  }

  drawDrag() {
    const d = this.drag;
    const dpr = this.overlay.width / (this.overlay.clientWidth || 1);
    const o = this.octx;
    o.clearRect(0, 0, this.overlay.width, this.overlay.height);
    o.save();
    o.setLineDash([6 * dpr, 4 * dpr]);
    o.lineWidth = Math.max(1, dpr);
    o.strokeStyle = "#ffffff";
    o.fillStyle = "rgba(255,255,255,0.08)";
    const x = Math.min(d.x0, d.x1) * dpr;
    const y = Math.min(d.y0, d.y1) * dpr;
    const w = Math.abs(d.x1 - d.x0) * dpr;
    const h = Math.abs(d.y1 - d.y0) * dpr;
    o.fillRect(x, y, w, h);
    o.strokeRect(x + 0.5, y + 0.5, w, h);
    o.restore();
  }

  // The column painted at CSS x (its ring slot), clamped to the painted columns; null if none.
  // Each column is `this.column.width` backing pixels wide, the newest at the right edge.
  slotAt(x) {
    if (this.columns === 0) return null;
    const dpr = this.canvas.width / (this.canvas.clientWidth || 1);
    const cw = Math.max(1, Math.min(this.column.width, this.canvas.width));
    const px = Math.max(0, this.canvas.width - 1 - Math.floor(x * dpr));
    const age = Math.floor(px / cw);
    const oldest = Math.min(this.columns, RING_COLUMNS, Math.ceil(this.canvas.width / cw)) - 1;
    return (this.columns - 1 - Math.min(age, oldest)) % RING_COLUMNS;
  }

  resolve(d) {
    const fHi = this.hzForY(Math.min(d.y0, d.y1));
    const fLo = this.hzForY(Math.max(d.y0, d.y1));
    const left = this.slotAt(Math.min(d.x0, d.x1));
    const right = this.slotAt(Math.max(d.x0, d.x1));
    if (left === null || right === null) return null;
    const startNs = Math.round(this.ringStart[left]);
    return {
      centerHz: Math.round((fHi + fLo) / 2),
      bandwidthHz: Math.max(1, Math.round(fHi - fLo)),
      startNs,
      endNs: Math.max(startNs, Math.round(this.ringEnd[right])),
    };
  }

  destroy() {
    this.gen += 1;
    if (this.sub) this.sub.close();
    this.sub = null;
    this.ro.disconnect();
    window.removeEventListener("pagehide", this.onPageHide);
    window.removeEventListener("pageshow", this.onPageShow);
    this.el.replaceChildren();
  }
}
