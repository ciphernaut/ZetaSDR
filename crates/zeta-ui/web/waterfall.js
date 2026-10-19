// waterfall.js: the scrolling waterfall fed by a `spectrum` (or `scan`) job's event stream.
//
// Exports:
//   mountWaterfall(el, api, source = null) -> Waterfall
//       Builds the axis, the canvas, the selection overlay and the #wf-badge inside `el`.
//   Waterfall:
//     wf.setSource(sourceInfo)  a `source_list` record (or null). Reuses a running `spectrum`
//         job on that source from `job_list`, else starts one on the source's `current` band
//         (its whole capture) or, when `current` is null, on the centre of its tune range with
//         its max_bandwidth_hz. While a `scan` job runs on the source, the waterfall follows
//         that job instead and starts none (scan mode, below). UI-started job ids go in
//         sessionStorage[UI_JOBS_KEY]; they are stopped on `pagehide` (fetch keepalive, with
//         the bearer), and when the waterfall moves to another source (a job it started for
//         the source it leaves; one it reused is left running). The job's event stream is
//         followed in text mode.
//     wf.sourceId   id of the source being shown (null when none, or after setSource failed)
//     wf.jobId      the followed job's id (null until known)
//     wf.mode       "spectrum" (a spectrum job's capture) or "scan" (a running scan job)
//     wf.syncJobs(jobs)  a `job_list` result (app.js hands it every poll): follows a scan job
//         that started on the shown source, or goes back to a spectrum job when it ended
//     wf.onNotice   fn(text) for transcript lines (a refused retune); set by app.js
//     wf.release(src) -> Promise<bool>  checks `source_list` holders of `src` and, when
//         nothing but this page's own spectrum job holds it, stops following and stops
//         that job (false, with a `conflict: …` notice, when another job holds it). The
//         caller then runs setSource(src) again (menu.js before a scan; retune below).
//     wf.painted    columns painted since mount (never reset; for tests)
//     wf.lastRowFormat  "q" (spectrum event v2) or "db" (older nodes) for the last painted
//         row; null before the first
//     wf.rows       ring of RING_COLUMNS painted rows, slot = column % RING_COLUMNS: {q:
//         Uint8Array, floorDb, stepDb} for v2 rows, {db: Float32Array} for older ones
//     wf.onSelect(cb)   cb({centerHz, bandwidthHz, startNs, endNs}) after a drag; the same
//         object is kept in window.zetaLastSelection.
//     wf.onColumn(cb)   cb(shiftCssPx) after each painted column: everything painted moved
//         left by that many CSS px (overlays.js scrolls its ribbons with it).
//     wf.onView(cb)     cb() after the band or the view changed (a zoom, a pan, a retune, a
//         scan band widening): yForHz maps differently (overlays.js lays its items out again).
//     wf.clearSelection()   erases the drawn selection rectangle (the menu closed)
//     wf.centerHz() -> the centre of the band shown (null before the band is known)
//     wf.yForHz(hz) -> CSS px from the canvas top (null before the band is known)
//     wf.band       the capture {lo, hi} in Hz (a scan: its whole span); null before known
//     wf.view       the sub-range {lo, hi} shown, or null for the whole capture
//     wf.requestView(lo, hi)  shows lo..hi Hz (see Band control)
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
// MAX_ROWS_PER_S columns per second are painted; extra events are dropped. Rows: spectrum
// event v2 (`q` of up to 8192 u8 levels with `floor_db` and `step_db`, db = floor_db +
// q * step_db) is decoded straight into colour through a 256-entry lookup table, without a
// float array; an older node's `db` array is still drawn. Each canvas pixel row shows the
// strongest bin it covers (bins are mapped onto the canvas height across the band). Colour:
// dB clamped to [floor - 10, floor + 60], floor being the 15th percentile of the row's own
// values, through a 256-entry blue-yellow-white ramp. The frequency
// band comes from each event (`center_hz`, `size * bin_hz`), falling back to the job's tuning.
// Per-column start_ns/end_ns are kept in a ring of RING_COLUMNS for the selection's time span.
//
// Band control (ruling R2): #band holds the centre (MHz) and width (kHz) of the band shown and
// an Apply button; a drag on the frequency axis pans it and the wheel over the canvas zooms
// it x2 about the pointer. A view inside the capture is rendering only: every stored row is
// drawn again through the sub-range of its bins (the strongest bin per pixel row, or the
// nearest bin when a bin spans several rows) and the axis is rescaled; no operation is issued.
// A wheel step out that still fits the capture is shifted inside it. A view leaving the
// capture retunes: `source_list` first; when another job holds the source the transcript
// shows `conflict: source <id> held by <jobs>` and the view reverts; else the page's own
// spectrum job is stopped, `source_ensure` asks for the view's band, and the spectrum job is
// started again on the capture it returns (or, refused, e.g. `out_of_range` on a file source,
// on the old one, with `view reverted: <error>` in the transcript). In scan mode the view
// stays inside the scan's span and never retunes.
//
// Scan mode: the axis spans the scan job's `tuning`, widened to the union of the rows (a step
// can reach past it at the span's ends); each row covers only its step's band (`center_hz` ±
// size × bin_hz / 2) and paints that vertical segment of a new column, the rest of the column
// repeating the previous one, so a sweep fills the columns progressively. A redraw (a zoom or
// pan) replays one sweep of older rows first, so its oldest column is not left partly blank.
// When the page's own job_start is refused as a conflict and a scan now runs on the source,
// the waterfall follows that scan instead.
//
// Reload: pagehide stops the UI's jobs and records their ids under STOPPING_KEY; the next page
// in the tab never reuses those (their stop may still be in flight), stops them again in case
// the keepalive request was lost, and starts a fresh job instead. A page restored from the
// back/forward cache (`pageshow` with `persisted`) does the same and runs setSource again.

import { errorText } from "./agent.js";

export const MAX_ROWS_PER_S = 30;
export const RING_COLUMNS = 2048;
export const AXIS_TICKS = 5;
export const UI_JOBS_KEY = "zeta.uiJobs";
export const STOPPING_KEY = "zeta.uiJobsStopping";
const BELOW_FLOOR_DB = 10;
const ABOVE_FLOOR_DB = 60;
const FLOOR_PERCENTILE = 0.15;
export const ZOOM_FACTOR = 2;
// The narrowest view the wheel zooms to.
export const MIN_VIEW_HZ = 100;
const BG = [0, 0, 48]; // #000030
// Wheel deltas (px; a line counts WHEEL_LINE_PX, a page WHEEL_PAGE_PX) add up to one zoom
// step per WHEEL_NOTCH_PX, so a touchpad's stream of small deltas does not zoom at every
// event; the sum restarts after WHEEL_IDLE_MS without wheel events.
const WHEEL_NOTCH_PX = 40;
const WHEEL_LINE_PX = 16;
const WHEEL_PAGE_PX = 800;
const WHEEL_IDLE_MS = 250;
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

const toRamp = (v) => Math.max(0, Math.min(255, Math.round(v)));
const COLOUR_SCALE = 255 / (BELOW_FLOOR_DB + ABOVE_FLOOR_DB);

// Ramp index per level 0..255 of a v2 row: the 15th-percentile level found by counting.
function qLut(q, floorDb, stepDb) {
  const hist = new Uint32Array(256);
  for (let i = 0; i < q.length; i++) hist[q[i]]++;
  const target = Math.min(q.length - 1, Math.floor(FLOOR_PERCENTILE * q.length));
  let acc = 0;
  let q15 = 0;
  for (; q15 < 256; q15++) {
    acc += hist[q15];
    if (acc > target) break;
  }
  const lut = new Uint8Array(256);
  for (let v = 0; v < 256; v++) lut[v] = toRamp((v - q15) * stepDb * COLOUR_SCALE + BELOW_FLOOR_DB * COLOUR_SCALE);
  return lut;
}

// The row of a spectrum event: {q, floorDb, stepDb} (v2), {db} (older nodes) or null.
export function decodeRow(p) {
  if (!p || typeof p !== "object") return null;
  if (
    Array.isArray(p.q) &&
    p.q.length &&
    typeof p.floor_db === "number" &&
    typeof p.step_db === "number"
  ) {
    return { q: Uint8Array.from(p.q), floorDb: p.floor_db, stepDb: p.step_db };
  }
  if (Array.isArray(p.db) && p.db.length) return { db: Float32Array.from(p.db) };
  return null;
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

// The {center_hz, bandwidth_hz} a UI spectrum job uses on `src`: its current capture, else
// the whole max_bandwidth_hz at the centre of its tune range; null when neither is known.
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
    bandwidth_hz: bw,
  };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const isRunning = (j) => !!(j && j.state && j.state.status === "running");

// The running `scan` job on `sourceId` in a `job_list` result, or null.
function runningScan(jobs, sourceId) {
  return jobs.find((j) => j.kind === "scan" && j.source === sourceId && isRunning(j)) || null;
}

// `t` when it is a {center_hz, bandwidth_hz} with both set, else null.
function usableTuning(t) {
  return t && t.center_hz && t.bandwidth_hz ? t : null;
}

// ---- the waterfall ---------------------------------------------------------------------

export function mountWaterfall(el, api, source = null) {
  return new Waterfall(el, api, source);
}

class Waterfall {
  constructor(el, api, source) {
    this.el = el;
    this.api = api;
    this.selectCbs = [];
    this.columnCbs = [];
    this.viewCbs = [];
    this.gen = 0;
    this.sub = null;
    this.src = null; // the source record being shown
    this.jobId = null;
    this.mode = "spectrum";
    this.band = null; // the capture {lo, hi} Hz (a scan: its span)
    this.view = null; // the sub-range shown {lo, hi}, null for the whole band
    this.pendingView = null; // the view to apply when the band is next set (a retune)
    this.retuning = false;
    this.onNotice = null;
    this.lastPaint = -Infinity;
    this.columns = 0; // columns painted since the last clear
    this.painted = 0; // columns painted since mount (never reset)
    this.ringStart = new Array(RING_COLUMNS).fill(0);
    this.ringEnd = new Array(RING_COLUMNS).fill(0);
    this.rows = new Array(RING_COLUMNS).fill(null);
    this.lastRowFormat = null;
    this.drag = null;
    this.pan = null;
    this.wheelAcc = 0;
    this.wheelAt = 0;
    this.redrawPending = false;

    this.axis = document.createElement("div");
    this.axis.className = "wf-axis";
    this.axis.title = "drag to pan";
    this.canvas = document.createElement("canvas");
    this.canvas.className = "wf-canvas";
    this.overlay = document.createElement("canvas");
    this.overlay.className = "wf-overlay";
    this.badge = document.createElement("div");
    this.badge.id = "wf-badge";
    this.badge.textContent = "reconnecting…";
    this.badge.hidden = true;
    this.bandForm = this.buildBandForm();
    el.replaceChildren(this.axis, this.canvas, this.overlay, this.badge, this.bandForm);
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
    this.overlay.addEventListener("wheel", (ev) => this.wheel(ev), { passive: false });
    this.axis.addEventListener("pointerdown", (ev) => this.panStart(ev));
    this.axis.addEventListener("pointermove", (ev) => this.panMove(ev));
    this.axis.addEventListener("pointerup", (ev) => this.panEnd(ev, true));
    this.axis.addEventListener("pointercancel", (ev) => this.panEnd(ev, false));

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

  notice(text) {
    if (this.onNotice) this.onNotice(text);
    else console.warn(text);
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
    if (this.jobId && this.src && (!src || src.id !== this.src.id)) this.stopOwnJob(this.jobId);
    if (this.sub) this.sub.close();
    this.sub = null;
    this.jobId = null;
    // A pending view (a retune) only carries over to the same source.
    if (!src || !this.src || src.id !== this.src.id) this.pendingView = null;
    this.src = src;
    this.clear();
    if (!restart) this.badge.hidden = true;
    if (!src) return;
    try {
      const jobId = await this.findOrStartJob(src);
      if (gen !== this.gen) {
        // Moved to another source while this one's job was being found or started.
        if (jobId && this.sourceId !== src.id) this.stopOwnJob(jobId);
        return;
      }
      if (!jobId) return;
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

  // Follows the shown source again after release().
  resume() {
    if (this.src) this.setSource(this.src);
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
    if (job && isRunning(job)) return null; // restarting
    this.forgetUiJob(jobId);
    this.setSource(src, { restart: true });
    return null;
  }

  // A `job_list` result: follow a scan that started on the shown source, or leave scan mode
  // when the followed scan is gone (the stream's close usually gets there first).
  syncJobs(jobs) {
    if (!this.src || !this.sub || this.retuning) return;
    const scan = runningScan(jobs, this.src.id);
    if (scan && scan.id !== this.jobId && !this.stopping.has(scan.id)) {
      if (this.mode === "spectrum" && this.jobId) this.stopOwnJob(this.jobId);
      this.setSource(this.src, { restart: true });
    } else if (!scan && this.mode === "scan") {
      this.setSource(this.src, { restart: true });
    }
  }

  // Lets go of `src` for a retune or a scan: true once nothing follows it and this page's own
  // spectrum job on it is stopped; false (with a `conflict: …` notice) when another job holds
  // it. Throws when `source_list` fails (nothing released then).
  async release(src) {
    const own =
      this.mode === "spectrum" && this.jobId && loadIds(UI_JOBS_KEY).includes(this.jobId)
        ? this.jobId
        : null;
    const sources = (await this.api.op({ op: "source_list" })).value || [];
    const info = sources.find((s) => s.id === src.id);
    const holders = info && Array.isArray(info.holders) ? info.holders : [];
    const others = holders.filter((h) => h !== own);
    if (others.length) {
      this.notice(`conflict: source ${src.id} held by ${others.join(", ")}`);
      return false;
    }
    this.gen += 1; // no frames, reconnects or restarts from the job being let go
    if (this.sub) this.sub.close();
    this.sub = null;
    this.jobId = null;
    if (own) {
      this.forgetUiJob(own);
      try {
        await this.api.op({ op: "job_stop", job: own });
      } catch {
        /* already gone */
      }
    }
    return true;
  }

  // Stops `id` when this tab started it (UI_JOBS_KEY); a reused job is left alone.
  stopOwnJob(id) {
    if (!loadIds(UI_JOBS_KEY).includes(id)) return;
    this.forgetUiJob(id);
    this.api.op({ op: "job_stop", job: id }).catch(() => {});
  }

  forgetUiJob(id) {
    const ids = loadIds(UI_JOBS_KEY);
    if (ids.includes(id)) storeIds(UI_JOBS_KEY, ids.filter((x) => x !== id));
  }

  // `retried`: a job_start conflict already sent us round once (a scan took the source).
  async findOrStartJob(src, retried = false) {
    const jobs = (await this.api.op({ op: "job_list" })).value || [];
    const scan = runningScan(jobs, src.id);
    if (scan && !this.stopping.has(scan.id)) {
      this.mode = "scan";
      this.tuning = usableTuning(scan.tuning);
      if (this.tuning) this.setBand(this.tuning.center_hz, this.tuning.bandwidth_hz);
      return scan.id;
    }
    this.mode = "spectrum";
    const running = jobs.find(
      (j) =>
        j.kind === "spectrum" && j.source === src.id && isRunning(j) && !this.stopping.has(j.id),
    );
    if (running) {
      // Without a usable tuning the band comes from the first event.
      this.tuning = usableTuning(running.tuning);
      if (this.tuning) this.setBand(this.tuning.center_hz, this.tuning.bandwidth_hz);
      return running.id;
    }
    const band = sourceBand(src);
    if (!band) throw new Error("no tuning or tune range to show");
    let r;
    try {
      r = await this.api.op({
        op: "job_start",
        kind: "spectrum",
        source: src.id,
        center_hz: band.center_hz,
        bandwidth_hz: band.bandwidth_hz,
        deviation_hz: null,
        sideband: null,
      });
    } catch (e) {
      // A scan started on the source since job_list (or this page loaded while one was
      // starting): follow it rather than fail.
      if (!retried && e.json && e.json.error === "conflict") {
        const now = (await this.api.op({ op: "job_list" })).value || [];
        if (runningScan(now, src.id)) return this.findOrStartJob(src, true);
      }
      throw e;
    }
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

  // The capture band; the view kept for it is `pendingView` when that fits inside.
  setBand(centerHz, spanHz) {
    const lo = centerHz - spanHz / 2;
    const hi = centerHz + spanHz / 2;
    if (this.band && this.band.lo === lo && this.band.hi === hi) return;
    if (this.band) this.clear();
    this.band = { lo, hi };
    this.view = this.fitView(this.pendingView);
    this.pendingView = null;
    this.drawAxis();
    this.showBandInputs();
    this.fireView();
  }

  // Scan mode: the band grows to take in `row` (a step may reach past the scan's tuning),
  // keeping what is painted: the stored rows are drawn again on the wider band.
  widenBand(row) {
    const b = this.band;
    if (row.lo >= b.lo && row.hi <= b.hi) return;
    this.band = { lo: Math.min(b.lo, row.lo), hi: Math.max(b.hi, row.hi) };
    this.view = this.fitView(this.view);
    this.viewChanged();
  }

  onView(cb) {
    this.viewCbs.push(cb);
  }

  fireView() {
    for (const cb of this.viewCbs) cb();
  }

  // The band shown: the view, else the whole capture.
  shown() {
    return this.view || this.band;
  }

  // Hz of one CSS pixel of the whole capture: views closer than that to its edges count as
  // inside it.
  slackHz() {
    return this.band ? (this.band.hi - this.band.lo) / this.cssHeight() : 0;
  }

  // `v` limited to the capture, or null when it covers (about) all of it or lies outside.
  fitView(v) {
    const b = this.band;
    if (!v || !b) return null;
    const tol = this.slackHz();
    if (v.lo < b.lo - tol || v.hi > b.hi + tol) return null;
    if (v.hi - v.lo >= b.hi - b.lo - tol) return null;
    return { lo: Math.max(v.lo, b.lo), hi: Math.min(v.hi, b.hi) };
  }

  cssHeight() {
    return this.canvas.clientHeight || 1;
  }

  centerHz() {
    const s = this.shown();
    return s ? (s.lo + s.hi) / 2 : null;
  }

  yForHz(hz) {
    const s = this.shown();
    if (!s) return null;
    return ((s.hi - hz) / (s.hi - s.lo)) * this.cssHeight();
  }

  hzForY(y) {
    const s = this.shown();
    return s.hi - (y / this.cssHeight()) * (s.hi - s.lo);
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
    this.blankColumn();
    this.drawAxis();
  }

  clear() {
    this.columns = 0;
    this.band = null;
    this.view = null;
    this.ctx.fillStyle = "#000030";
    this.ctx.fillRect(0, 0, this.canvas.width, this.canvas.height);
    this.octx.clearRect(0, 0, this.overlay.width, this.overlay.height);
    this.axis.replaceChildren();
    this.blankColumn();
    this.showBandInputs();
  }

  drawAxis() {
    const s = this.shown();
    if (!s) return;
    const h = this.cssHeight();
    const ticks = [];
    for (let i = 0; i < AXIS_TICKS; i++) {
      const hz = s.hi - (i / (AXIS_TICKS - 1)) * (s.hi - s.lo);
      const t = document.createElement("span");
      t.className = "tick";
      t.textContent = (hz / 1e6).toFixed(3);
      t.style.top = `${(i / (AXIS_TICKS - 1)) * h}px`;
      t.dataset.edge = i === 0 ? "top" : i === AXIS_TICKS - 1 ? "bottom" : "";
      ticks.push(t);
    }
    this.axis.replaceChildren(...ticks);
  }

  // ---- band control --------------------------------------------------------------------

  buildBandForm() {
    const form = document.createElement("form");
    form.id = "band";
    form.autocomplete = "off";
    const field = (name, label, unit) => {
      const wrap = document.createElement("label");
      const input = document.createElement("input");
      input.name = name;
      input.type = "text";
      input.inputMode = "decimal";
      input.spellcheck = false;
      input.setAttribute("aria-label", label);
      const u = document.createElement("span");
      u.className = "unit";
      u.textContent = unit;
      wrap.append(input, u);
      form.append(wrap);
      return input;
    };
    this.centreIn = field("center", "centre (MHz)", "MHz");
    this.widthIn = field("width", "width (kHz)", "kHz");
    const apply = document.createElement("button");
    apply.type = "submit";
    apply.textContent = "Apply";
    form.append(apply);
    form.addEventListener("submit", (ev) => {
      ev.preventDefault();
      this.applyBandForm();
    });
    return form;
  }

  applyBandForm() {
    const c = parseFloat(this.centreIn.value) * 1e6;
    const w = parseFloat(this.widthIn.value) * 1e3;
    if (!Number.isFinite(c) || !Number.isFinite(w) || w <= 0) {
      this.notice("band: the centre (MHz) and width (kHz) must be positive numbers");
      this.showBandInputs(true);
      return;
    }
    this.requestView(c - w / 2, c + w / 2);
  }

  // The inputs show the band shown (an input being edited is left alone unless `force`).
  showBandInputs(force = false) {
    if (!this.centreIn) return;
    const s = this.shown();
    const set = (input, text) => {
      if (force || document.activeElement !== input) input.value = text;
    };
    set(this.centreIn, s ? ((s.lo + s.hi) / 2 / 1e6).toFixed(6) : "");
    set(this.widthIn, s ? ((s.hi - s.lo) / 1e3).toFixed(3) : "");
  }

  // Shows lo..hi Hz: a re-render inside the capture (a scan: kept inside its span), else a
  // retune (ruling R2).
  requestView(lo, hi) {
    if (!this.band || this.retuning || !(hi > lo)) return;
    const b = this.band;
    const tol = this.slackHz();
    if (lo >= b.lo - tol && hi <= b.hi + tol) {
      this.setView(lo, hi);
    } else if (this.mode === "scan") {
      this.setView(...this.clampToBand(lo, hi));
    } else {
      this.retune(lo, hi);
    }
  }

  setView(lo, hi) {
    this.view = this.fitView({ lo, hi });
    this.viewChanged();
  }

  viewChanged() {
    this.drawAxis();
    this.showBandInputs();
    this.scheduleRedraw();
    this.fireView();
  }

  // lo..hi moved inside the capture; the whole capture when it is wider.
  clampToBand(lo, hi) {
    const b = this.band;
    const span = hi - lo;
    if (span >= b.hi - b.lo) return [b.lo, b.hi];
    if (lo < b.lo) return [b.lo, b.lo + span];
    if (hi > b.hi) return [b.hi - span, b.hi];
    return [lo, hi];
  }

  // x2 per wheel notch (WHEEL_NOTCH_PX of accumulated delta) about the pointer; a step out
  // that fits the capture is moved inside it rather than retuning.
  wheel(ev) {
    ev.preventDefault();
    const s = this.shown();
    if (!s || this.retuning || this.drag) return;
    const now = performance.now();
    if (now - this.wheelAt > WHEEL_IDLE_MS) this.wheelAcc = 0;
    this.wheelAt = now;
    const unit = ev.deltaMode === 1 ? WHEEL_LINE_PX : ev.deltaMode === 2 ? WHEEL_PAGE_PX : 1;
    this.wheelAcc += ev.deltaY * unit;
    if (Math.abs(this.wheelAcc) < WHEEL_NOTCH_PX) return;
    const k = this.wheelAcc < 0 ? 1 / ZOOM_FACTOR : ZOOM_FACTOR;
    this.wheelAcc = 0;
    const f = this.hzForY(this.local(ev).y);
    let lo = f - (f - s.lo) * k;
    let hi = f + (s.hi - f) * k;
    if (hi - lo < MIN_VIEW_HZ) return;
    if (hi - lo <= (this.band.hi - this.band.lo) * (1 + 1e-9)) [lo, hi] = this.clampToBand(lo, hi);
    this.requestView(lo, hi);
  }

  // A drag on the axis moves the band shown with the pointer (down shows higher
  // frequencies); beyond the capture it shows background until the release decides.
  panStart(ev) {
    const s = this.shown();
    if (ev.button !== 0 || !s || this.retuning) return;
    this.axis.setPointerCapture(ev.pointerId);
    this.pan = { y0: ev.clientY, from: { ...s }, prev: this.view, target: null };
  }

  panMove(ev) {
    const p = this.pan;
    if (!p) return;
    const off = ((ev.clientY - p.y0) * (p.from.hi - p.from.lo)) / this.cssHeight();
    if (off === 0) return;
    p.target = [p.from.lo + off, p.from.hi + off];
    this.view = { lo: p.target[0], hi: p.target[1] };
    this.viewChanged();
  }

  panEnd(ev, commit) {
    const p = this.pan;
    if (!p) return;
    this.pan = null;
    this.view = p.prev;
    if (commit && p.target) this.requestView(p.target[0], p.target[1]);
    else this.viewChanged();
  }

  // Retunes the source for a view outside the capture (ruling R2): holders first; then the
  // page's own spectrum job stops, `source_ensure` asks for the view's band and the spectrum
  // job starts again on what the source captures. Refused: the view reverts.
  async retune(lo, hi) {
    const src = this.src;
    if (!src) return;
    const prev = this.view;
    this.retuning = true;
    this.view = { lo, hi }; // shown while the retune runs
    this.viewChanged();
    const revert = () => {
      this.view = prev;
      this.viewChanged();
    };
    try {
      let released = false;
      try {
        released = await this.release(src);
      } catch (e) {
        this.notice(`view reverted: source_list: ${e.message}`);
      }
      if (!released) {
        if (this.src === src) revert();
        return;
      }
      const gen = this.gen;
      let current = null;
      try {
        const r = await this.api.op({
          op: "source_ensure",
          source: src.id,
          center_hz: Math.round((lo + hi) / 2),
          bandwidth_hz: Math.max(1, Math.round(hi - lo)),
        });
        current = usableTuning(r.value);
      } catch (e) {
        this.notice(`view reverted: ${e.json ? errorText(e.json) : e.message}`);
      }
      if (gen !== this.gen) return; // another source was selected meanwhile
      this.pendingView = current ? { lo, hi } : prev;
      this.setSource(current ? { ...src, current } : src);
    } finally {
      this.retuning = false;
    }
  }

  // ---- painting ------------------------------------------------------------------------

  addEvent(msg) {
    const p = msg.payload;
    const row = decodeRow(p);
    if (!row) return;
    const now = performance.now();
    if (now - this.lastPaint < 1000 / MAX_ROWS_PER_S) return;
    this.lastPaint = now;
    // The band the row's bins cover: from the event, else the job's tuning.
    const size = row.q ? row.q.length : row.db.length;
    if (typeof p.center_hz === "number" && typeof p.bin_hz === "number") {
      row.lo = p.center_hz - (p.bin_hz * size) / 2;
      row.hi = p.center_hz + (p.bin_hz * size) / 2;
    } else if (this.tuning) {
      row.lo = this.tuning.center_hz - this.tuning.bandwidth_hz / 2;
      row.hi = this.tuning.center_hz + this.tuning.bandwidth_hz / 2;
    } else {
      return;
    }
    // A spectrum row is the capture; a scan row is one step of the span (the job's tuning),
    // the band widened to the union of the steps (one may reach past the tuning).
    if (this.mode !== "scan" || !this.band) this.setBand((row.lo + row.hi) / 2, row.hi - row.lo);
    else this.widenBand(row);
    this.paintColumn(row);
    this.lastRowFormat = row.q ? "q" : "db";
    const slot = this.columns % RING_COLUMNS;
    this.rows[slot] = row;
    this.ringStart[slot] = Number(msg.start_ns) || 0;
    this.ringEnd[slot] = Number(msg.end_ns) || this.ringStart[slot];
    this.columns += 1;
    this.painted += 1;
    if (this.columnCbs.length) {
      const dpr = this.canvas.width / (this.canvas.clientWidth || 1);
      const shift = Math.min(this.column.width, this.canvas.width) / dpr;
      for (const cb of this.columnCbs) cb(shift);
    }
  }

  onColumn(cb) {
    this.columnCbs.push(cb);
  }

  blankColumn() {
    if (!this.column) return;
    const px = this.column.data;
    for (let o = 0; o < px.length; o += 4) {
      px[o] = BG[0];
      px[o + 1] = BG[1];
      px[o + 2] = BG[2];
      px[o + 3] = 255;
    }
  }

  // Shifts the canvas left by one column and draws `row` at the right edge: in scan mode
  // over the previous column (only the row's band changes), else on its own.
  paintColumn(row) {
    const w = this.canvas.width;
    const h = this.canvas.height;
    const cw = Math.min(this.column.width, w); // backing pixels per event
    this.fillColumn(row, this.mode !== "scan");
    if (w > cw) this.ctx.drawImage(this.canvas, cw, 0, w - cw, h, 0, 0, w - cw, h);
    this.ctx.putImageData(this.column, w - cw, 0);
  }

  // Writes `row` (bins spread over row.lo..row.hi Hz) into this.column on the band shown.
  // Levels are compared as stored (q is monotonic in dB) and turned into a ramp index only
  // for the strongest bin of each pixel row, or the nearest bin when a bin spans several
  // rows. Pixel rows outside the row's band get the background with `fill`, else keep
  // their value.
  fillColumn(row, fill) {
    const s = this.shown() || { lo: row.lo, hi: row.hi };
    let levels;
    let colour;
    if (row.q) {
      levels = row.q;
      if (!row.lut) row.lut = qLut(row.q, row.floorDb, row.stepDb);
      const lut = row.lut;
      colour = (v) => lut[v];
    } else {
      levels = row.db;
      if (row.dbLo === undefined) row.dbLo = percentile(row.db, FLOOR_PERCENTILE) - BELOW_FLOOR_DB;
      const lo = row.dbLo;
      colour = (v) => toRamp((v - lo) * COLOUR_SCALE);
    }
    const size = levels.length;
    const perBin = (row.hi - row.lo) / size;
    const span = s.hi - s.lo;
    const h = this.column.height;
    const cw = this.column.width;
    const px = this.column.data;
    for (let y = 0; y < h; y++) {
      // Pixel row y covers bins x0..x1 (fractional, counted from the band's bottom).
      const x0 = (s.hi - ((y + 1) / h) * span - row.lo) / perBin;
      const x1 = (s.hi - (y / h) * span - row.lo) / perBin;
      let r = BG[0];
      let g = BG[1];
      let b = BG[2];
      if (x1 <= 0 || x0 >= size) {
        if (!fill) continue;
      } else {
        let v;
        const b0 = Math.max(0, Math.floor(x0));
        const b1 = Math.min(size, Math.floor(x1));
        if (b1 <= b0) {
          v = levels[Math.min(size - 1, Math.max(0, Math.floor((x0 + x1) / 2)))];
        } else {
          v = levels[b0];
          for (let k = b0 + 1; k < b1; k++) if (levels[k] > v) v = levels[k];
        }
        const idx = colour(v) * 3;
        r = RAMP[idx];
        g = RAMP[idx + 1];
        b = RAMP[idx + 2];
      }
      for (let x = 0; x < cw; x++) {
        const o = (y * cw + x) * 4;
        px[o] = r;
        px[o + 1] = g;
        px[o + 2] = b;
        px[o + 3] = 255;
      }
    }
  }

  scheduleRedraw() {
    if (this.redrawPending) return;
    this.redrawPending = true;
    requestAnimationFrame(() => {
      this.redrawPending = false;
      this.redraw();
    });
  }

  // Paints every stored column again on the band shown, oldest first (scan columns build
  // on the one before, as when they arrived). In scan mode up to one sweep of the stored rows
  // older than the oldest column shown is replayed into the column first (not painted), so
  // that column starts with every step's band rather than background.
  redraw() {
    const w = this.canvas.width;
    const h = this.canvas.height;
    this.ctx.fillStyle = "#000030";
    this.ctx.fillRect(0, 0, w, h);
    this.blankColumn();
    if (!this.columns || !this.shown()) return;
    const cw = Math.max(1, Math.min(this.column.width, w));
    const stored = Math.min(this.columns, RING_COLUMNS);
    const n = Math.min(stored, Math.ceil(w / cw));
    if (this.mode === "scan") {
      const extra = Math.min(stored - n, this.sweepRows());
      for (let k = n + extra - 1; k >= n; k--) {
        const row = this.rows[(this.columns - 1 - k) % RING_COLUMNS];
        if (row && row.lo !== undefined) this.fillColumn(row, false);
      }
    }
    for (let k = n - 1; k >= 0; k--) {
      const row = this.rows[(this.columns - 1 - k) % RING_COLUMNS];
      if (!row || row.lo === undefined) continue;
      this.fillColumn(row, this.mode !== "scan");
      this.ctx.putImageData(this.column, w - (k + 1) * cw, 0);
    }
  }

  // Rows in one scan sweep: the band over the newest row's width, rounded up (the steps
  // overlap at most at the span's ends, so this is never fewer than the sweep).
  sweepRows() {
    const row = this.rows[(this.columns - 1) % RING_COLUMNS];
    if (!row || !this.band || !(row.hi > row.lo)) return 0;
    return Math.ceil((this.band.hi - this.band.lo) / (row.hi - row.lo)) + 1;
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

  clearSelection() {
    if (this.drag) return; // a new drag is being drawn
    this.octx.clearRect(0, 0, this.overlay.width, this.overlay.height);
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
