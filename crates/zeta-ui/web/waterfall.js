// waterfall.js: the scrolling waterfall fed by a `spectrum` (or `scan`) job's event stream.
//
// Exports:
//   mountWaterfall(el, api, source = null) -> Waterfall
//   capHz(claim) -> the Hz of a `source_list` capability claim, or null
//       Builds the axis, the canvas, the selection overlay and the #wf-badge inside `el`.
//   Waterfall:
//     wf.setSource(sourceInfo)  a `source_list` record (or null). Reuses a running `spectrum`
//         job on that source from `job_list` (an elastic one first), else starts one, `elastic: true` (spec 4.1: it
//         follows the capture, holds no lease, never conflicts and is relaunched by the node
//         under the same id, with new streams, on every retune), on the source's `current`
//         band (its whole capture) or, when `current` is null, on the centre of its tune range
//         with its max_bandwidth_hz. While a `scan` job runs on the source, the waterfall follows
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
//     wf.release(src, span) -> Promise<bool>  checks `source_list` holders of `src` (rigid jobs;
//         the page's own elastic job is never one; a `pin` is one, see rigidHolders) and, when
//         there are none, stops following
//         (false, with a `conflict: …` notice, when a job holds it). The page's own job is
//         left to the node: a stepped scan on the source stops it, while under a self-sweep
//         (`span` {lo, hi} in Hz inside the source's `capabilities.sweep`, see sweepsItself)
//         the node takes no lease and stops nothing, so the page's own spectrum job keeps
//         running, unseen while the waterfall follows the scan, and the page does not refuse
//         for rigid holders: the node decides. The caller then runs
//         setSource(src) again (menu.js after starting a scan).
//     wf.painted    columns painted since mount (never reset; for tests)
//     wf.lastRowFormat  "q" (spectrum event v2) or "db" (older nodes) for the last painted
//         row; null before the first
//     wf.rows       ring of RING_COLUMNS painted rows, slot = column % RING_COLUMNS: {q:
//         Uint8Array, floorDb, stepDb} for v2 rows, {db: Float32Array} for older ones,
//         {coarseOnly: true} for a column of the coarse layer alone (see Layers)
//     wf.onSelect(cb)   cb({centerHz, bandwidthHz, startNs, endNs}) after a drag; the same
//         object is kept in window.zetaLastSelection.
//     wf.onColumn(cb)   cb(shiftCssPx) after each painted column: everything painted moved
//         left by that many CSS px (overlays.js scrolls its ribbons with it).
//     wf.plan       () -> the `plan` view policy (default auto); the axis marks the channels
//         of plans.js in view, when at least 14 px apart; wf.drawAxis() redraws it.
//     wf.onView(cb)     cb({lo, hi, outside}) after the band or the view changed (a zoom, a
//         pan, a retune, a scan band widening): yForHz maps differently (overlays.js lays its
//         items out again). lo..hi is the band shown (null before it is known); `outside` is
//         true when the view reaches past the capture (the coarse layer's part, coarse.js).
//     wf.coarseActive   null, or fn() -> bool set by coarse.js: true while the coarse layer
//         scans views past the capture (zoom_out "scan"); see Band control.
//     wf.follow     null, or fn() -> "target" | "free" set by app.js (the `follow` policy;
//         null counts as "target"); see Following the capture.
//     wf.addCoarseEvent(msg)  a frame of the coarse layer's scan (coarse.js onRow): stitched
//         into the coarse column buffer (see Layers); ignored in scan mode, where the
//         waterfall draws the shown source's scan itself, and before the band is known
//     wf.coarsePainted  columns painted with the coarse layer in them since mount (composed
//         over coarse rows, or the coarse layer alone; never reset; for tests)
//     wf.coarseMasked   coarse rows addCoarseEvent stored that carried `masked` (never
//         reset; for tests)
//     wf.clearCoarse()  forgets the coarse rows (the coarse layer went empty)
//     wf.syncSources(sources)  a `source_list` result (app.js hands it every jobs poll); see
//         Following the capture
//     wf.easeTo(lo, hi, ms = EASE_MS)  moves the view to lo..hi over `ms`, one step per
//         animation frame (requestAnimationFrame, ease-in-out); a second call, a zoom, a pan
//         or another source cancels the running one, which stops where it was
//     wf.easing     true while an easeTo runs
//     wf.clearSelection()   erases the drawn selection rectangle (the menu closed)
//     wf.centerHz() -> the centre of the band shown (null before the band is known)
//     wf.xForNs(ns) -> CSS x of a time on the painted columns (right edge when newer than
//         the newest; null when older than the oldest column or none is painted)
//     wf.oldestNs() -> the start of the oldest column on screen (null when none or untimed)
//     wf.yForHz(hz) -> CSS px from the canvas top (null before the band is known)
//     wf.rail       the `div#rail` before the axis, the canvas's height (0 wide and hidden in
//         the plain theme); overlays.js lays the job tags into it (ov.setRail)
//     wf.setRamp(stops)  the theme's ramp ([t, [r, g, b]] stops, theme.json `ramp`); paints
//         every stored column again with it
//     wf.refreshThemeColours()  reads --wf-bg, --wf-select and --wf-select-alpha (default
//         0.08, the drag rectangle's fill) from the theme; the caller redraws
//     wf.band       the capture {lo, hi} in Hz (a scan: its whole span); null before known
//     wf.view       the range {lo, hi} shown, or null for the whole capture; past the capture
//         (and wider than it) only while wf.coarseActive() holds
//     wf.isOutside() -> whether the view reaches past the capture
//     wf.requestView(lo, hi)  shows lo..hi Hz (see Band control)
//     wf.capturable(info, lo, hi) -> whether the source of `info` (a `source_list` record)
//         can capture lo..hi in one tune
//     wf.restoreView(lo, hi)  leaves scan mode for lo..hi: `source_ensure`, then the page's
//         elastic job on the new capture (coarse.js, once it stopped its own scan there)
//     wf.destroy()
//
// Reconnect: when the stream's socket closes (a `closed` message, an error) the #wf-badge
// "reconnecting…" shows and api.streamText reopens with backoff, re-resolving the stream id
// by provenance (event stream of the job) because it changes when the job restarts under
// `graph_put` or the node relaunches the elastic job on a new capture (same job id, new
// streams). If the job is no longer running (stopped by another tab, the CLI, its own row's
// Stop button), setSource runs again for the same source: it reuses another running spectrum
// job or starts a fresh elastic one of its own (a scan running there is followed instead, and
// the own job is started again only once it ended). The badge hides at the first frame after
// that, also one that comes while the map is shown, so a relaunch badge is clear on return.
//
// Layout: frequency is vertical (highest at the top), time runs right to left: each event
// becomes one CSS-pixel column (Math.round(devicePixelRatio) backing pixels, at least 1) at
// the right edge after the canvas is shifted left by that much with drawImage. At most
// MAX_ROWS_PER_S columns per second are painted; extra events are dropped, and so is every
// event while the map is shown in the waterfall's place (<html data-main="map">). Rows: spectrum
// event v2 (`q` of up to 8192 u8 levels with `floor_db` and `step_db`, db = floor_db +
// q * step_db) is decoded straight into colour through a 256-entry lookup table, without a
// float array; an older node's `db` array is still drawn. Each canvas pixel row shows the
// strongest bin it covers (bins are mapped onto the canvas height across the band). A row's
// `masked` ([[lo, hi], ...], bin indices, hi exclusive: a scan's DC notch and band edges)
// bins are never painted: a pixel row takes the strongest unmasked bin it covers, and one
// whose bins are all masked keeps what the previous row left there. Colour:
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
// A wheel step out that still fits the capture is shifted inside it (unless the view is
// already past the capture). A view leaving the capture: `source_list` first. With no rigid
// holder (a pin counts as one, rigidHolders), and the view no wider than the source's
// max_bandwidth_hz and inside its tune range,
// `source_ensure` asks for the view's band; the node relaunches the page's elastic job on the
// new capture, which the waterfall follows (refused, e.g. `out_of_range` on a file source,
// the view reverts with `view reverted: <error>` in the transcript). Otherwise (a rigid
// holder, a view too wide, or past the tune range) the page never retunes (spec 4.1): while
// wf.coarseActive() holds the view is left past the capture (outside: true) for the coarse
// layer to scan, the fine rows painted over their own band only; without it (zoom_out
// "none") a held source clamps the view into the capture with `conflict: source <id> held by
// <jobs>; the view stays inside its capture` in the transcript, and an unheld one is asked
// with `source_ensure` as above. In scan mode the view stays inside the scan's span and never
// retunes.
//
// Layers (spec 4.2): outside scan mode each column is composed from two buffers: the coarse
// column, into which every coarse row is stitched over its own step's band exactly as scan
// mode stitches its rows (the rest keeps what earlier rows left there), and on top of it the
// fine row over its own band. Without coarse rows the coarse column is background, so the
// fine column looks as before. Coarse rows are kept (RING_COLUMNS of them) with the column
// they arrived before, so a redraw replays them between the fine columns, up to two sweeps of
// the older ones first (counted by steps, so both the even and the shifted odd grid fill
// what the other's masks left). The axis spans the view, also when wider than the capture.
// When no fine column has been painted for COARSE_ALONE_MS (the fine job stopped, its socket is
// reconnecting, or its restart failed), each coarse row, at most MAX_ROWS_PER_S a second,
// paints a column of the coarse layer alone (kept in the ring as such for redraws). A
// restart on the same source keeps the band shown as the view while the band is unknown, so
// the coarse layer goes on across it; the band, once known again, sets the view as before.
//
// Following the capture (spec 4.3 `follow`): the capture moved by someone else (the node
// relaunched the page's elastic job on a new capture, seen as a fine row of another band, or
// a `source_list` whose `current` for the shown source differs from the previous poll's and
// from the band) moves the band; under "target" the view eases to the new capture
// (easeTo, EASE_MS), under "free" the band shown stays exactly where it was. The page's own
// retune (source_ensure) is not such a move: it restarts the waterfall on the new band. Rows
// still arriving for the band left behind by a `source_list` move do not move it back.
//
// Scan mode: the axis spans the scan job's `tuning`, widened to the union of the rows (a step
// can reach past it at the span's ends); each row covers only its step's band (`center_hz` ±
// size × bin_hz / 2) and paints that vertical segment of a new column, the rest of the column
// repeating the previous one, so a sweep fills the columns progressively. A redraw (a zoom or
// pan) replays two sweeps of older rows first (counted by steps: the even and the shifted odd
// grid, each filling what the other's masks left), so its oldest column is not left partly
// blank.
// When the page's own job_start is refused as a conflict and a scan now runs on the source,
// the waterfall follows that scan instead.
//
// Reload: pagehide stops the UI's jobs and records their ids under STOPPING_KEY; the next page
// in the tab never reuses those (their stop may still be in flight), stops them again in case
// the keepalive request was lost, and starts a fresh job instead. A page restored from the
// back/forward cache (`pageshow` with `persisted`) does the same and runs setSource again.

import { errorText } from "./agent.js";
import { channelsIn } from "./plans.js";

// The least spacing, in pixels, of the channel ticks on the axis (fewer are not drawn).
const CHANNEL_TICK_MIN_PX = 14;

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
// How long the view takes to ease to a new capture under `follow target`.
export const EASE_MS = 400;
// With no fine column painted for this long, each coarse row (at most MAX_ROWS_PER_S a second)
// paints a column of the coarse layer alone.
export const COARSE_ALONE_MS = 1000;
// The ring entry of a column painted from the coarse layer alone.
const COARSE_ONLY = Object.freeze({ coarseOnly: true });

// Whether the map is shown in the waterfall's place (app.js applyMain): nothing is painted then.
function behindMap() {
  return document.documentElement.dataset.main === "map";
}
// The theme's colour variable `name` (a custom property of <html>), trimmed.
export function themeColour(name) {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}
// Any CSS colour (named, #hex, rgb() in either syntax, hsl(), color-mix() ...) -> [r, g, b],
// by painting it on a 1x1 canvas; null when the browser does not parse it or it is not opaque.
let PARSE_CTX = null;
function parseColour(t) {
  if (!t) return null;
  PARSE_CTX ||= document.createElement("canvas").getContext("2d", { willReadFrequently: true });
  const x = PARSE_CTX;
  x.clearRect(0, 0, 1, 1);
  x.fillStyle = "rgba(0, 0, 0, 0)"; // the sentinel an unparsed value leaves in place
  x.fillStyle = t;
  x.fillRect(0, 0, 1, 1);
  const d = x.getImageData(0, 0, 1, 1).data;
  return d[3] === 255 ? [d[0], d[1], d[2]] : null;
}
// The canvas ground (--wf-bg, #000030 in both themes), read again by refreshThemeColours().
let BG = [0, 0, 48];
let BG_CSS = "#000030";
let SELECT_CSS = "#ffffff"; // --wf-select, the drag rectangle
let SELECT_ALPHA = 0.08; // --wf-select-alpha, the drag rectangle's fill
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

// The default ramp (console's and plain's); a theme's `ramp` replaces it (wf.setRamp).
const RAMP_STOPS = [
  [0, [0, 0, 48]],
  [0.35, [0, 90, 255]],
  [0.75, [255, 225, 0]],
  [1, [255, 255, 255]],
];

// 256 RGB triplets interpolated between `stops` ([t, [r, g, b]], t ascending from 0 to 1).
function buildRamp(stops) {
  const ramp = new Uint8ClampedArray(256 * 3);
  for (let i = 0; i < 256; i++) {
    const t = i / 255;
    let k = 0;
    while (k < stops.length - 2 && t > stops[k + 1][0]) k++;
    const [t0, c0] = stops[k];
    const [t1, c1] = stops[k + 1];
    const f = (t - t0) / (t1 - t0);
    for (let ch = 0; ch < 3; ch++) ramp[i * 3 + ch] = Math.round(c0[ch] + (c1[ch] - c0[ch]) * f);
  }
  return ramp;
}

let RAMP = buildRamp(RAMP_STOPS);

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

// The row of a spectrum event: {q, floorDb, stepDb} (v2), {db} (older nodes) or null. A
// `masked` array in the event adds `masked`, its [lo, hi] pairs of two numbers (when at
// least one is).
export function decodeRow(p) {
  if (!p || typeof p !== "object") return null;
  let row = null;
  if (
    Array.isArray(p.q) &&
    p.q.length &&
    typeof p.floor_db === "number" &&
    typeof p.step_db === "number"
  ) {
    row = { q: Uint8Array.from(p.q), floorDb: p.floor_db, stepDb: p.step_db };
  } else if (Array.isArray(p.db) && p.db.length) {
    row = { db: Float32Array.from(p.db) };
  }
  if (row && Array.isArray(p.masked)) {
    const masked = p.masked
      .filter((m) => Array.isArray(m) && m.length === 2 && m.every((x) => typeof x === "number"))
      .map((m) => [m[0], m[1]]);
    if (masked.length) row.masked = masked;
  }
  return row;
}

// Per-bin flags of `row.masked` (1: masked) over `size` bins.
function maskBins(masked, size) {
  const bins = new Uint8Array(size);
  for (const [lo, hi] of masked) {
    const a = Math.max(0, Math.ceil(lo));
    const b = Math.min(size, Math.ceil(hi));
    for (let k = a; k < b; k++) bins[k] = 1;
  }
  return bins;
}

// ---- source band -----------------------------------------------------------------------

// The number in a capability claim (`{asserted: n}` or a bare n), or null.
export function capHz(cap) {
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

// The rigid holders of a `source_list` record: its `holders` (job ids), then `pin` when the
// source is pinned (plan 12 spec 4.1: a pin holds the source like a rigid job).
export function rigidHolders(info) {
  const holders = info && Array.isArray(info.holders) ? [...info.holders] : [];
  if (info && info.pin) holders.push("pin");
  return holders;
}

// Whether `src` (a `source_list` record) sweeps `lo..hi` Hz for itself: its
// `capabilities.sweep` holds the span. Such a scan takes no lease (plan 36.2).
export function sweepsItself(src, lo, hi) {
  const sw = src && src.capabilities && src.capabilities.sweep;
  return !!sw && sw.min_hz <= lo && hi <= sw.max_hz;
}

// `t` when it is a {center_hz, bandwidth_hz} with both set, else null.
function usableTuning(t) {
  return t && t.center_hz && t.bandwidth_hz ? t : null;
}

// Whether bands a and b ({lo, hi}) are the same up to a thousandth of a's width (a capture
// from `source_list` and one from a row's bins may differ by rounding).
function sameBand(a, b) {
  if (!a || !b) return false;
  const tol = Math.max(1, (a.hi - a.lo) * 1e-3);
  return Math.abs(a.lo - b.lo) <= tol && Math.abs(a.hi - b.hi) <= tol;
}

// Sets row.lo..row.hi, the band the bins of `row` (decoded from event payload `p`) cover:
// from the event, else `tuning` ({center_hz, bandwidth_hz}); false when neither gives one.
function bandRow(row, p, tuning) {
  const size = row.q ? row.q.length : row.db.length;
  if (typeof p.center_hz === "number" && typeof p.bin_hz === "number") {
    row.lo = p.center_hz - (p.bin_hz * size) / 2;
    row.hi = p.center_hz + (p.bin_hz * size) / 2;
  } else if (tuning) {
    row.lo = tuning.center_hz - tuning.bandwidth_hz / 2;
    row.hi = tuning.center_hz + tuning.bandwidth_hz / 2;
  } else {
    return false;
  }
  return true;
}

// Steps in one sweep of a scan over `span` ({lo, hi}) whose newest row is `row`: the span over
// the row's width, rounded up, plus one (never fewer than a sweep's steps, the odd sweep's
// extra step included); 0 without either.
function sweepCount(row, span) {
  if (!row || !span || !(row.hi > row.lo)) return 0;
  return Math.ceil((span.hi - span.lo) / (row.hi - row.lo)) + 1;
}

// The first index from which the rows before `end` (exclusive) span at most `steps` scan
// steps: walks i from end - 1 down to `floor` over rowAt(i) (anything without a numeric
// lo/hi is skipped), counting a step each time a row's centre differs from the newer one's.
// `end` when `steps` is 0, `floor` when the rows run out first.
function stepsBack(rowAt, end, steps, floor = 0) {
  if (!(steps > 0)) return end;
  let count = 0;
  let prev = null;
  for (let i = end - 1; i >= floor; i--) {
    const r = rowAt(i);
    if (!r || typeof r.lo !== "number" || typeof r.hi !== "number") continue;
    const c = (r.lo + r.hi) / 2;
    if (c !== prev) {
      if (count === steps) return i + 1;
      count += 1;
      prev = c;
    }
  }
  return floor;
}

// ---- the waterfall ---------------------------------------------------------------------

export function mountWaterfall(el, api, source = null) {
  return new Waterfall(el, api, source);
}

class Waterfall {
  constructor(el, api, source) {
    this.el = el;
    this.api = api;
    this.refreshThemeColours();
    this.selectCbs = [];
    this.columnCbs = [];
    this.viewCbs = [];
    this.plan = () => "auto"; // the view's `plan` policy (app.js sets it)
    this.gen = 0;
    this.sub = null;
    this.src = null; // the source record being shown
    this.jobId = null;
    this.mode = "spectrum";
    this.band = null; // the capture {lo, hi} Hz (a scan: its span)
    this.view = null; // the sub-range shown {lo, hi}, null for the whole band
    this.pendingView = null; // the view to apply when the band is next set (a retune)
    this.retuning = false;
    this.coarseActive = null;
    this.follow = null;
    this.onNotice = null;
    this.coarseRows = []; // {row, col}: a coarse row stitched before fine column `col`
    this.coarseSpan = null; // the union of the stored coarse rows' bands
    this.coarsePainted = 0;
    this.coarseMasked = 0; // coarse rows stored that carried `masked` (for tests)
    this.lastCoarsePaint = -Infinity;
    this.easing = false;
    this.easeRaf = null;
    this.listed = null; // {id, lo, hi}: the shown source's `current` in the last source_list
    this.staleBand = null; // the band a source_list move left; its late rows do not move back
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

    this.rail = document.createElement("div");
    this.rail.id = "rail";
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
    el.replaceChildren(this.rail, this.axis, this.canvas, this.overlay, this.badge, this.bandForm);
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
    if (!src || !this.src || src.id !== this.src.id) {
      this.pendingView = null;
      this.listed = null;
    }
    // A restart on the same source keeps the band shown as the view until the band is known
    // again (or for good when the restart fails): the coarse layer goes on painting across it.
    const keep = restart && src && this.src && src.id === this.src.id ? this.shown() : null;
    this.src = src;
    this.clear();
    if (keep) {
      this.view = { lo: keep.lo, hi: keep.hi };
      this.drawAxis();
      this.showBandInputs();
    }
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

  // The `source_list` record of `src` (null when it is not listed). Throws when
  // `source_list` fails.
  async sourceInfo(src) {
    const sources = (await this.api.op({ op: "source_list" })).value || [];
    return sources.find((s) => s.id === src.id) || null;
  }

  // The rigid holders of `src` from `source_list` (the page's elastic job is never one; a
  // pin counts as one, named `pin`). Throws when `source_list` fails.
  async holders(src) {
    return rigidHolders(await this.sourceInfo(src));
  }

  // Lets go of `src` for a scan: true once nothing follows it; false (with a `conflict: …`
  // notice) when a rigid job holds it, unless the source sweeps `span` itself (the node
  // decides). The page's own elastic job keeps running: a stepped scan stops it on the node. Throws when `source_list` fails (nothing released then).
  async release(src, span = null) {
    const info = await this.sourceInfo(src);
    const holders = rigidHolders(info);
    if (holders.length && !(span && sweepsItself(info, span.lo, span.hi))) {
      this.notice(`conflict: source ${src.id} held by ${holders.join(", ")}`);
      return false;
    }
    this.gen += 1; // no frames, reconnects or restarts from the job being let go
    if (this.sub) this.sub.close();
    this.sub = null;
    this.jobId = null;
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
    // An elastic one first: a rigid one (an older page's, resumed rigid) holds the source.
    const reusable = jobs.filter(
      (j) =>
        j.kind === "spectrum" && j.source === src.id && isRunning(j) && !this.stopping.has(j.id),
    );
    const running = reusable.find((j) => j.elastic) || reusable[0];
    if (running) {
      // Without a usable tuning the band comes from the first event. An elastic job's tuning
      // is the capture, which `src.current` may know first (just after a retune).
      this.tuning = (running.elastic && usableTuning(src.current)) || usableTuning(running.tuning);
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
        elastic: true,
        replace: [],
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

  // The capture band; the view kept for it is `pendingView` when that fits inside. A band
  // already known that changes outside scan mode is the capture moving (captureMoved), unless
  // it is the band a `source_list` move left behind (late rows of the old capture).
  setBand(centerHz, spanHz) {
    const lo = centerHz - spanHz / 2;
    const hi = centerHz + spanHz / 2;
    const next = { lo, hi };
    // (No row painted yet: the band was the job's tuning, which the rows' bins correct.)
    if (this.band && this.mode !== "scan" && this.columns > 0) {
      if (sameBand(this.band, next)) {
        this.staleBand = null;
        return;
      }
      if (sameBand(this.staleBand, next)) return;
      this.captureMoved(next);
      return;
    }
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

  // A `source_list` result: the shown source's `current` differing from the previous poll's
  // and from the band is the capture moving (captureMoved). Not in scan mode (a scan's
  // `current` is its step) nor during the page's own retune.
  syncSources(sources) {
    const info = this.src && (sources || []).find((s) => s.id === this.src.id);
    const cur = info && usableTuning(info.current);
    if (!cur) return;
    const listed = {
      id: info.id,
      lo: cur.center_hz - cur.bandwidth_hz / 2,
      hi: cur.center_hz + cur.bandwidth_hz / 2,
    };
    const prev = this.listed;
    this.listed = listed;
    if (!prev || prev.id !== listed.id || sameBand(prev, listed)) return;
    if (this.mode !== "spectrum" || !this.band || this.retuning || sameBand(this.band, listed)) {
      return;
    }
    this.staleBand = this.band;
    this.captureMoved({ lo: listed.lo, hi: listed.hi });
  }

  // The capture is now `band` (moved by someone else): what is painted stays (each row keeps
  // its own band); under `follow` "target" the view eases to the new capture, under "free"
  // the band shown stays where it was.
  captureMoved(band) {
    const prev = this.shown();
    this.band = { lo: band.lo, hi: band.hi };
    if (sameBand(this.band, this.staleBand)) this.staleBand = null;
    // The band shown so far, explicitly: the view no longer defaults to the old capture.
    this.view = { lo: prev.lo, hi: prev.hi };
    const follow = this.follow ? this.follow() : "target";
    if (follow === "free") {
      this.cancelEase();
      this.view = this.keptView(prev);
      this.viewChanged();
      return;
    }
    this.easeTo(band.lo, band.hi);
  }

  // The view for band shown `v` on the current band: kept as given past the band, else
  // limited to it (null when it covers about all of it).
  keptView(v) {
    const b = this.band;
    if (!v || !b) return null;
    const tol = this.slackHz();
    if (v.lo < b.lo - tol || v.hi > b.hi + tol) return { lo: v.lo, hi: v.hi };
    return this.fitView(v);
  }

  easeTo(lo, hi, ms = EASE_MS) {
    this.cancelEase();
    const from = this.shown();
    if (!from || !(ms > 0)) {
      this.view = this.keptView({ lo, hi });
      this.viewChanged();
      return;
    }
    const start = { lo: from.lo, hi: from.hi };
    const t0 = performance.now();
    this.easing = true;
    const step = (now) => {
      const f = Math.min(1, Math.max(0, (now - t0) / ms));
      if (f >= 1) {
        this.easeRaf = null;
        this.easing = false;
        this.view = this.keptView({ lo, hi });
      } else {
        const e = f < 0.5 ? 2 * f * f : 1 - (2 - 2 * f) ** 2 / 2;
        this.view = { lo: start.lo + (lo - start.lo) * e, hi: start.hi + (hi - start.hi) * e };
        this.easeRaf = requestAnimationFrame(step);
      }
      this.viewChanged();
    };
    this.easeRaf = requestAnimationFrame(step);
  }

  // Stops a running easeTo where it is.
  cancelEase() {
    if (this.easeRaf !== null) cancelAnimationFrame(this.easeRaf);
    this.easeRaf = null;
    this.easing = false;
  }

  onView(cb) {
    this.viewCbs.push(cb);
  }

  fireView() {
    const s = this.shown();
    const v = { lo: s ? s.lo : null, hi: s ? s.hi : null, outside: this.isOutside() };
    for (const cb of this.viewCbs) cb(v);
  }

  isOutside() {
    const b = this.band;
    const v = this.view;
    if (!b || !v) return false;
    const tol = this.slackHz();
    return v.lo < b.lo - tol || v.hi > b.hi + tol;
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
    this.ctx.fillStyle = BG_CSS;
    this.ctx.fillRect(0, 0, w, h);
    if (old) this.ctx.drawImage(old, w - old.width, 0, old.width, h);
    this.column = this.ctx.createImageData(Math.max(1, Math.round(dpr)), h);
    this.coarseColumn = this.ctx.createImageData(this.column.width, h);
    this.blankColumn();
    this.replayCoarse(this.coarseRows.length);
    this.drawAxis();
  }

  clear() {
    this.cancelEase();
    this.columns = 0;
    this.band = null;
    this.view = null;
    this.staleBand = null;
    this.coarseRows = [];
    this.coarseSpan = null;
    this.ctx.fillStyle = BG_CSS;
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
    // Channel ticks (plans.js, the `plan` policy via wf.plan) when they are far enough apart.
    const chans = channelsIn(s.lo, s.hi, this.plan());
    const span = s.hi - s.lo;
    const minGap = chans.reduce(
      (m, c, i) => (i ? Math.min(m, ((c.center_hz - chans[i - 1].center_hz) / span) * h) : m),
      Infinity,
    );
    if (chans.length && minGap >= CHANNEL_TICK_MIN_PX) {
      for (const c of chans) {
        const t = document.createElement("span");
        t.className = "tick ch";
        t.textContent = String(c.number);
        t.style.top = `${((s.hi - c.center_hz) / span) * h}px`;
        t.style.right = "auto";
        t.style.left = "2px";
        ticks.push(t);
      }
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
    this.cancelEase();
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
    if (!s || !this.band || this.retuning || this.drag) return;
    this.cancelEase();
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
    const fits = hi - lo <= (this.band.hi - this.band.lo) * (1 + 1e-9);
    if (fits && !this.isOutside()) [lo, hi] = this.clampToBand(lo, hi);
    this.requestView(lo, hi);
  }

  // A drag on the axis moves the band shown with the pointer (down shows higher
  // frequencies); beyond the capture it shows background until the release decides.
  panStart(ev) {
    const s = this.shown();
    if (ev.button !== 0 || !s || this.retuning) return;
    this.cancelEase();
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

  // A view outside the capture (ruling R2, spec 4.1, ruling R6): `source_list` first. No
  // rigid holder and a view the source can capture: `source_ensure` asks for it, the node
  // relaunches the page's elastic job on what the source now captures and the waterfall
  // follows it again (refused: the view reverts). Otherwise the view stays past the capture
  // for the coarse layer while it is active; without it a held source clamps the view into
  // the capture, and an unheld one is asked anyway (the refusal reverts the view).
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
    let coarse = false;
    try {
      let info;
      try {
        info = await this.sourceInfo(src);
      } catch (e) {
        this.notice(`view reverted: source_list: ${e.message}`);
        if (this.src === src) revert();
        return;
      }
      const holders = rigidHolders(info);
      const coarseOn = !!this.coarseActive && this.coarseActive();
      if (coarseOn && (holders.length || !this.capturable(info, lo, hi))) {
        // The coarse layer's view (spec 4.3 zoom_out scan): left past the capture.
        coarse = this.src === src;
        return;
      }
      if (holders.length) {
        this.notice(
          `conflict: source ${src.id} held by ${holders.join(", ")}; the view stays inside its capture`,
        );
        if (this.src === src && this.band) {
          this.view = prev;
          this.setView(...this.clampToBand(lo, hi));
        }
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
          replace: [],
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
      // The view settled past the capture: tell the listeners (the coarse layer waits for it).
      if (coarse) this.fireView();
    }
  }

  // Leaves scan mode for lo..hi (coarse.js, ruling R23: the page's own scan on the shown
  // source was just stopped): the scan's stream is let go, `source_ensure` asks for the band
  // and the waterfall starts over on the source, the view lo..hi (refused: `view reverted:
  // <error>` in the transcript and the source's whole capture).
  async restoreView(lo, hi) {
    const src = this.src;
    if (!src) return;
    this.cancelEase();
    this.gen += 1; // no frames, reconnects or restarts from the scan being let go
    if (this.sub) this.sub.close();
    this.sub = null;
    this.jobId = null;
    const gen = this.gen;
    this.retuning = true;
    let current = null;
    try {
      const r = await this.api.op({
        op: "source_ensure",
        source: src.id,
        center_hz: Math.round((lo + hi) / 2),
        bandwidth_hz: Math.max(1, Math.round(hi - lo)),
        replace: [],
      });
      current = usableTuning(r.value);
    } catch (e) {
      this.notice(`view reverted: ${e.json ? errorText(e.json) : e.message}`);
    } finally {
      this.retuning = false;
    }
    if (gen !== this.gen) return; // another source was selected meanwhile
    this.pendingView = current ? { lo, hi } : null;
    this.setSource(current ? { ...src, current } : src);
  }

  // Whether the source of `info` (a `source_list` record) can capture lo..hi in one tune: no
  // wider than its max_bandwidth_hz and inside its tune range (a capability not known counts
  // as allowing it, as before; the node then decides).
  capturable(info, lo, hi) {
    const c = (info && info.capabilities) || {};
    const bw = capHz(c.max_bandwidth_hz);
    const min = capHz(c.tune_min_hz);
    const max = capHz(c.tune_max_hz);
    const tol = this.slackHz();
    if (bw !== null && hi - lo > bw + tol) return false;
    if (min !== null && lo < min - tol) return false;
    if (max !== null && hi > max + tol) return false;
    return true;
  }

  // ---- painting ------------------------------------------------------------------------

  addEvent(msg) {
    // Hidden behind the map (<html data-main="map">): no draw. The rows count as painted, so
    // the coarse layer does not move on its own meanwhile.
    if (behindMap()) {
      this.lastPaint = performance.now();
      return;
    }
    const row = decodeRow(msg.payload);
    if (!row) return;
    const now = performance.now();
    if (now - this.lastPaint < 1000 / MAX_ROWS_PER_S) return;
    this.lastPaint = now;
    // The band the row's bins cover: from the event, else the job's tuning.
    if (!bandRow(row, msg.payload, this.tuning)) return;
    // A spectrum row is the capture; a scan row is one step of the span (the job's tuning),
    // the band widened to the union of the steps (one may reach past the tuning).
    if (this.mode !== "scan" || !this.band) this.setBand((row.lo + row.hi) / 2, row.hi - row.lo);
    else this.widenBand(row);
    this.paintColumn(row);
    this.lastRowFormat = row.q ? "q" : "db";
    this.painted += 1;
    this.pushSlot(row, msg);
  }

  // Records the column just painted for `row` (a fine row, or COARSE_ONLY) and the time span
  // of `msg` in the ring, and tells onColumn listeners that everything moved left.
  pushSlot(row, msg) {
    const slot = this.columns % RING_COLUMNS;
    this.rows[slot] = row;
    this.ringStart[slot] = Number(msg.start_ns) || 0;
    this.ringEnd[slot] = Number(msg.end_ns) || this.ringStart[slot];
    this.columns += 1;
    if (this.columnCbs.length) {
      const dpr = this.canvas.width / (this.canvas.clientWidth || 1);
      const shift = Math.min(this.column.width, this.canvas.width) / dpr;
      for (const cb of this.columnCbs) cb(shift);
    }
  }

  onColumn(cb) {
    this.columnCbs.push(cb);
  }

  // A frame of the coarse layer's scan: stitched into the coarse column over its step's band
  // (as scan mode stitches its rows) and kept for redraws; the next fine column shows it.
  addCoarseEvent(msg) {
    if (this.mode === "scan" || !this.shown() || !this.coarseColumn) return;
    const row = decodeRow(msg.payload);
    if (!row || !bandRow(row, msg.payload, null)) return;
    this.coarseRows.push({ row, col: this.columns });
    if (this.coarseRows.length > RING_COLUMNS) this.coarseRows.shift();
    if (row.masked) this.coarseMasked += 1;
    const sp = this.coarseSpan;
    this.coarseSpan = sp
      ? { lo: Math.min(sp.lo, row.lo), hi: Math.max(sp.hi, row.hi) }
      : { lo: row.lo, hi: row.hi };
    this.fillColumn(row, false, this.coarseColumn);
    if (behindMap()) return;
    // The fine rows stopped (or never came): the coarse layer moves on its own.
    const now = performance.now();
    if (now - this.lastPaint < COARSE_ALONE_MS) return;
    if (now - this.lastCoarsePaint < 1000 / MAX_ROWS_PER_S) return;
    this.lastCoarsePaint = now;
    this.paintCoarseColumn();
    this.pushSlot(COARSE_ONLY, msg);
  }

  // Shifts the canvas left by one column and draws the coarse column alone at the right edge.
  paintCoarseColumn() {
    const w = this.canvas.width;
    const h = this.canvas.height;
    const cw = Math.min(this.coarseColumn.width, w);
    if (w > cw) this.ctx.drawImage(this.canvas, cw, 0, w - cw, h, 0, 0, w - cw, h);
    this.ctx.putImageData(this.coarseColumn, w - cw, 0);
    this.coarsePainted += 1;
  }

  clearCoarse() {
    if (!this.coarseRows.length) return;
    this.coarseRows = [];
    this.coarseSpan = null;
    this.blankBuffer(this.coarseColumn);
    this.scheduleRedraw();
  }

  // Stitches up to two sweeps (counted by steps, stepsBack) of the coarse rows before index
  // `end` (exclusive) into a blank coarse column, on the band shown.
  replayCoarse(end) {
    if (!this.coarseColumn) return;
    this.blankBuffer(this.coarseColumn);
    const newest = end > 0 ? this.coarseRows[end - 1].row : null;
    const steps = 2 * sweepCount(newest, this.coarseSpan);
    const from = stepsBack((i) => this.coarseRows[i].row, end, steps);
    for (let i = from; i < end; i++) {
      this.fillColumn(this.coarseRows[i].row, false, this.coarseColumn);
    }
  }

  blankColumn() {
    this.blankBuffer(this.column);
    this.blankBuffer(this.coarseColumn);
  }

  blankBuffer(buf) {
    if (!buf) return;
    const px = buf.data;
    for (let o = 0; o < px.length; o += 4) {
      px[o] = BG[0];
      px[o + 1] = BG[1];
      px[o + 2] = BG[2];
      px[o + 3] = 255;
    }
  }

  // Writes the fine `row` into this.column: in scan mode over the previous column (only the
  // row's band changes), else over the coarse column (see Layers).
  composeColumn(row) {
    if (this.mode !== "scan") this.column.data.set(this.coarseColumn.data);
    this.fillColumn(row, false);
  }

  // Shifts the canvas left by one column and draws `row` at the right edge (composeColumn).
  paintColumn(row) {
    const w = this.canvas.width;
    const h = this.canvas.height;
    const cw = Math.min(this.column.width, w); // backing pixels per event
    this.composeColumn(row);
    if (w > cw) this.ctx.drawImage(this.canvas, cw, 0, w - cw, h, 0, 0, w - cw, h);
    this.ctx.putImageData(this.column, w - cw, 0);
    if (this.mode !== "scan" && this.coarseRows.length) this.coarsePainted += 1;
  }

  // Writes `row` (bins spread over row.lo..row.hi Hz) into `buf` (this.column by default)
  // on the band shown. Levels are compared as stored (q is monotonic in dB) and turned into
  // a ramp index only for the strongest bin of each pixel row, or the nearest bin when a bin
  // spans several rows. Masked bins (row.masked) take no part: a pixel row takes the strongest
  // unmasked bin it covers, and one whose bins are all masked keeps its value (whatever
  // `fill`). Pixel rows outside the row's band get the background with `fill`, else keep
  // their value.
  fillColumn(row, fill, buf = this.column) {
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
    if (row.masked && !row.maskBin) row.maskBin = maskBins(row.masked, size);
    const mask = row.maskBin || null;
    const perBin = (row.hi - row.lo) / size;
    const span = s.hi - s.lo;
    const h = buf.height;
    const cw = buf.width;
    const px = buf.data;
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
          const k = Math.min(size - 1, Math.max(0, Math.floor((x0 + x1) / 2)));
          if (mask && mask[k]) continue;
          v = levels[k];
        } else if (mask) {
          let k = b0;
          while (k < b1 && mask[k]) k++;
          if (k === b1) continue; // every bin covered is masked
          v = levels[k];
          for (k += 1; k < b1; k++) if (!mask[k] && levels[k] > v) v = levels[k];
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
  // on the one before, as when they arrived). In scan mode up to two sweeps (counted by
  // steps, stepsBack) of the stored rows older than the oldest column shown are replayed into
  // the column first (not painted), so that column starts with every step's band, both
  // grids filling each other's masks, rather than background; outside it the coarse rows
  // are replayed the same way into the coarse column, and each later one before the fine
  // column it arrived before.
  redraw() {
    const w = this.canvas.width;
    const h = this.canvas.height;
    this.ctx.fillStyle = BG_CSS;
    this.ctx.fillRect(0, 0, w, h);
    this.blankColumn();
    if (!this.shown()) return;
    const cw = Math.max(1, Math.min(this.column.width, w));
    const stored = Math.min(this.columns, RING_COLUMNS);
    const n = Math.min(stored, Math.ceil(w / cw));
    const coarse = this.coarseRows;
    let ci = 0; // the next coarse row to stitch
    if (this.mode === "scan") {
      const oldest = this.columns - n; // the oldest column shown
      const rowAt = (c) => this.rows[c % RING_COLUMNS];
      const from = stepsBack(rowAt, oldest, 2 * this.sweepRows(), this.columns - stored);
      for (let c = from; c < oldest; c++) {
        const row = rowAt(c);
        if (row && row.lo !== undefined) this.fillColumn(row, false);
      }
    } else {
      const first = this.columns - n; // the oldest column shown
      while (ci < coarse.length && coarse[ci].col <= first) ci++;
      this.replayCoarse(ci);
    }
    for (let k = n - 1; k >= 0; k--) {
      const col = this.columns - 1 - k;
      for (; ci < coarse.length && coarse[ci].col <= col; ci++) {
        this.fillColumn(coarse[ci].row, false, this.coarseColumn);
      }
      const row = this.rows[col % RING_COLUMNS];
      if (row === COARSE_ONLY) {
        this.ctx.putImageData(this.coarseColumn, w - (k + 1) * cw, 0);
        continue;
      }
      if (!row || row.lo === undefined) continue;
      this.composeColumn(row);
      this.ctx.putImageData(this.column, w - (k + 1) * cw, 0);
    }
    // Coarse rows newer than the newest column: in the coarse column for the next one.
    if (this.mode !== "scan") {
      for (; ci < coarse.length; ci++) this.fillColumn(coarse[ci].row, false, this.coarseColumn);
    }
  }

  // Steps in one scan sweep of the band (sweepCount over the newest row).
  sweepRows() {
    return sweepCount(this.rows[(this.columns - 1) % RING_COLUMNS], this.band);
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

  // The theme's waterfall ramp (theme.json `ramp`: 2-16 stops [t, [r, g, b]], t from 0 to 1,
  // checked by the node); anything else keeps the default. Every stored column is painted
  // again with it.
  setRamp(stops) {
    const ok =
      Array.isArray(stops) &&
      stops.length >= 2 &&
      stops.every((s) => Array.isArray(s) && Array.isArray(s[1]) && s[1].length === 3);
    RAMP = buildRamp(ok ? stops : RAMP_STOPS);
    this.redraw();
  }

  // Read --wf-bg, --wf-select and --wf-select-alpha from the theme; the caller redraws.
  refreshThemeColours() {
    // A value that does not parse falls back to the default, never to the last theme's.
    BG = parseColour(themeColour("--wf-bg")) || [0, 0, 48];
    BG_CSS = `rgb(${BG[0]}, ${BG[1]}, ${BG[2]})`;
    const sel = parseColour(themeColour("--wf-select"));
    SELECT_CSS = sel ? `rgb(${sel[0]}, ${sel[1]}, ${sel[2]})` : "#ffffff";
    const alpha = parseFloat(themeColour("--wf-select-alpha"));
    SELECT_ALPHA = Number.isFinite(alpha) && alpha >= 0 && alpha <= 1 ? alpha : 0.08;
  }

  drawDrag() {
    const d = this.drag;
    const dpr = this.overlay.width / (this.overlay.clientWidth || 1);
    const o = this.octx;
    o.clearRect(0, 0, this.overlay.width, this.overlay.height);
    o.save();
    o.setLineDash([6 * dpr, 4 * dpr]);
    o.lineWidth = Math.max(1, dpr);
    o.strokeStyle = SELECT_CSS;
    o.fillStyle = SELECT_CSS;
    const x = Math.min(d.x0, d.x1) * dpr;
    const y = Math.min(d.y0, d.y1) * dpr;
    const w = Math.abs(d.x1 - d.x0) * dpr;
    const h = Math.abs(d.y1 - d.y0) * dpr;
    o.globalAlpha = SELECT_ALPHA;
    o.fillRect(x, y, w, h);
    o.globalAlpha = 1;
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

  // CSS x of the time `ns` (ns since the epoch) on the painted columns: the newest column is
  // at the right edge, each `column.width` backing px wide. A time newer than the newest
  // column clamps to the right edge; null when `ns` is older than the oldest column kept (or
  // on screen), or no timed column is painted. Inside a column the x is interpolated over
  // the column's own span.
  xForNs(ns) {
    if (!(this.columns > 0) || !Number.isFinite(ns)) return null;
    const dpr = this.canvas.width / (this.canvas.clientWidth || 1);
    const cw = Math.max(1, Math.min(this.column.width, this.canvas.width));
    const shown = Math.min(this.columns, RING_COLUMNS, Math.ceil(this.canvas.width / cw));
    const slotOf = (age) => (this.columns - 1 - age) % RING_COLUMNS;
    if (ns >= this.ringEnd[slotOf(0)]) return this.canvas.width / dpr;
    // The column starts fall with age (the ring is monotonic), so the youngest column that
    // starts at or before `ns` (or is untimed, which ends the walk) is a binary search.
    let lo = 0;
    let hi = shown;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      const start = this.ringStart[slotOf(mid)];
      if (!start || ns >= start) hi = mid;
      else lo = mid + 1;
    }
    if (lo >= shown) return null;
    const slot = slotOf(lo);
    const start = this.ringStart[slot];
    if (!start) return null;
    const span = this.ringEnd[slot] - start;
    const frac = span > 0 ? Math.min(1, (ns - start) / span) : 0;
    return (this.canvas.width - (lo + 1 - frac) * cw) / dpr;
  }

  // The start (ns since the epoch) of the oldest column on screen; null when none is painted
  // or it is untimed. A time before it has no x (xForNs answers null).
  oldestNs() {
    if (!(this.columns > 0)) return null;
    const cw = Math.max(1, Math.min(this.column.width, this.canvas.width));
    const shown = Math.min(this.columns, RING_COLUMNS, Math.ceil(this.canvas.width / cw));
    const start = this.ringStart[(this.columns - 1 - (shown - 1)) % RING_COLUMNS];
    return start || null;
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
    this.cancelEase();
    this.gen += 1;
    if (this.sub) this.sub.close();
    this.sub = null;
    this.ro.disconnect();
    window.removeEventListener("pagehide", this.onPageHide);
    window.removeEventListener("pageshow", this.onPageShow);
    this.el.replaceChildren();
  }
}
