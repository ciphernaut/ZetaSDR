// coarse.js: the coarse layer's scans (spec 4.4): a view past the fine capture is swept by a
// `scan` job on a free "scanner" source.
//
// Exports:
//   SCAN_DEBOUNCE_MS   wait after the last view change before a scan is started or replaced
//   SPAN_CHANGE        an edge of the wanted span moving by more than this fraction of the
//                      running scan's width replaces the scan
//   mountCoarse(wf, api, view, {transcript, sources, jobs}) -> Coarse
//       `sources()` / `jobs()` return the last `source_list` / `job_list` results; each
//       decision fetches both afresh and falls back to them when a fetch fails.
//       Listens to wf.onView ({lo, hi, outside}) and view.onChange (zoom_out, auto_scan_stop,
//       scanner). SCAN_DEBOUNCE_MS after the last change, with the view outside the fine
//       capture and zoom_out "scan", it chooses the scanner (view.get("scanner")):
//         auto   the first source in `source_list` order that is not the shown one, is not
//                faulted, has no rigid holder (a running scan holding it, the page's own or a
//                manual one, is not rigid) and whose tune range contains the view's centre;
//                else the shown source itself when it has no holder at all (today's scan
//                mode: the scan stops the page's elastic job, which restarts when it ends)
//         <id>   that source, as given
//       The span is the view clamped to the scanner's tune_min_hz..tune_max_hz. A manual scan
//       (one not in sessionStorage[AUTO_SCAN_KEY]) running on the scanner is followed
//       read-only and nothing is started. Otherwise the page's scan is started:
//       `job_start {kind: "scan", source, center_hz, bandwidth_hz, deviation_hz: null,
//       sideband: null, elastic: false, replace: []}`, its id kept in
//       sessionStorage[AUTO_SCAN_KEY]; an own scan on another source, or one whose span moved
//       by more than SPAN_CHANGE, is stopped first. With no scanner, or an empty span, the
//       own scan is stopped and the transcript gets `coarse layer: no free source can scan
//       <lo>-<hi> MHz`, once per view. Back inside the capture under auto_scan_stop "inside",
//       or under zoom_out "none", the own scan is stopped. A scan the page did not start is
//       never stopped. While the waterfall itself follows the own scan (the shown-source
//       fallback, scan mode) nothing is decided.
//       It also sets wf.coarseActive = () => zoom_out is "scan": the waterfall then lets the
//       view leave the capture instead of clamping it or retuning past the source.
//       The followed scan's (own or manual) event stream is subscribed in text mode and each
//       frame handed to the onRow callbacks. Its own scan is stopped on `pagehide` (fetch
//       keepalive, with the bearer), the id kept so the next page in the tab knows it.
//   Coarse:
//     coarse.scanJobId   the page's own running scan id, or null
//     coarse.followed    the scan whose frames are followed (own or manual), or null
//     coarse.source      the source of the followed scan, or null
//     coarse.rows        frames handed on since mount (for tests)
//     coarse.onRow(cb)   cb(msg) per `frame` message of the followed scan
//     coarse.destroy()

import { AUTO_SCAN_KEY, errorText } from "./agent.js";

export const SCAN_DEBOUNCE_MS = 300;
export const SPAN_CHANGE = 0.05;
const STREAM_LOOKUPS = 20;
const STREAM_LOOKUP_MS = 250;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const isRunning = (j) => !!(j && j.state && j.state.status === "running");
const mhz = (hz) => (hz / 1e6).toFixed(3);

function capHz(cap) {
  if (typeof cap === "number") return cap;
  if (cap && typeof cap === "object") {
    const v = Object.values(cap).find((x) => typeof x === "number");
    return v === undefined ? null : v;
  }
  return null;
}

// The {lo, hi} tune range of a `source_list` record, or null when it is not known.
function tuneRange(src) {
  const c = (src && src.capabilities) || {};
  const lo = capHz(c.tune_min_hz);
  const hi = capHz(c.tune_max_hz);
  return lo === null || hi === null ? null : { lo, hi };
}

function loadAutoId() {
  try {
    return sessionStorage.getItem(AUTO_SCAN_KEY);
  } catch {
    return null;
  }
}

function storeAutoId(id) {
  try {
    if (id) sessionStorage.setItem(AUTO_SCAN_KEY, id);
    else sessionStorage.removeItem(AUTO_SCAN_KEY);
  } catch {
    /* storage unavailable: the id holds for this page load */
  }
}

export function mountCoarse(wf, api, view, { transcript, sources = () => [], jobs = () => [] }) {
  return new Coarse(wf, api, view, transcript, sources, jobs);
}

class Coarse {
  constructor(wf, api, view, transcript, sources, jobs) {
    this.wf = wf;
    this.api = api;
    this.view = view;
    this.transcript = transcript;
    this.lastSources = sources;
    this.lastJobs = jobs;
    this.scanJobId = loadAutoId();
    this.span = null; // {source, lo, hi} of the own scan, when this page started it
    this.followed = null;
    this.source = null;
    this.sub = null;
    this.subGen = 0;
    this.rows = 0;
    this.rowCbs = [];
    this.timer = null;
    this.busy = false;
    this.again = false;
    this.noted = null; // the view the last "no free source" line was for
    this.destroyed = false;

    wf.coarseActive = () => view.get("zoom_out") === "scan";
    wf.onView(() => this.schedule());
    view.onChange((key) => {
      if (key === "zoom_out" || key === "scanner" || key === "auto_scan_stop") {
        this.noted = null;
        this.schedule();
      }
    });
    this.onPageHide = () => this.stopOnPageHide();
    window.addEventListener("pagehide", this.onPageHide);
    this.schedule();
  }

  onRow(cb) {
    this.rowCbs.push(cb);
  }

  note(text) {
    if (this.transcript) this.transcript.note(text);
    else console.warn(text);
  }

  schedule() {
    if (this.destroyed) return;
    clearTimeout(this.timer);
    this.timer = setTimeout(() => this.run(), SCAN_DEBOUNCE_MS);
  }

  // One decision at a time; a change during one runs another after it.
  async run() {
    if (this.busy) {
      this.again = true;
      return;
    }
    this.busy = true;
    try {
      await this.decide();
    } catch (e) {
      console.warn(`coarse layer: ${e.message}`);
    } finally {
      this.busy = false;
    }
    if (this.again && !this.destroyed) {
      this.again = false;
      this.schedule();
    }
  }

  // The view {lo, hi} when it lies (partly) outside the fine capture, else null.
  outsideView() {
    const wf = this.wf;
    if (!wf.band || !wf.view) return null;
    const b = wf.band;
    const v = wf.view;
    const tol = wf.slackHz();
    return v.lo < b.lo - tol || v.hi > b.hi + tol ? { lo: v.lo, hi: v.hi } : null;
  }

  async fetchList(op, fallback) {
    try {
      return (await this.api.op({ op })).value || [];
    } catch {
      return fallback() || [];
    }
  }

  async decide() {
    const wf = this.wf;
    if (this.destroyed) return;
    // No capture known (a source being selected): wait for its band, which fires onView.
    // A retune or an axis drag in progress: its end fires onView again.
    if (!wf.band) return;
    if (wf.retuning || wf.pan) {
      return;
    }
    const jobs = await this.fetchList("job_list", this.lastJobs);
    if (this.destroyed) return;
    // The own scan ended elsewhere (its row's Stop, another client): forget it.
    if (this.scanJobId && !isRunning(jobs.find((j) => j.id === this.scanJobId))) {
      this.forgetOwn();
    }
    // The shown-source fallback: the waterfall itself follows the own scan in scan mode.
    if (this.scanJobId && wf.jobId === this.scanJobId) return;
    const v = this.outsideView();
    const scanning = this.view.get("zoom_out") === "scan";
    if (!v || !scanning) {
      this.noted = null;
      if (this.scanJobId && (!scanning || this.view.get("auto_scan_stop") === "inside")) {
        await this.stopOwn();
      }
      if (!this.scanJobId) this.unfollow();
      return;
    }
    const srcs = await this.fetchList("source_list", this.lastSources);
    if (this.destroyed) return;
    const pick = this.chooseScanner(srcs, jobs, v);
    const range = pick && tuneRange(pick);
    const lo = range ? Math.max(v.lo, range.lo) : 0;
    const hi = range ? Math.min(v.hi, range.hi) : 0;
    if (!pick || !(hi > lo)) {
      await this.stopOwn();
      this.unfollow();
      const key = `${v.lo}-${v.hi}`;
      if (this.noted !== key) {
        this.noted = key;
        this.note(`coarse layer: no free source can scan ${mhz(v.lo)}-${mhz(v.hi)} MHz`);
      }
      return;
    }
    this.noted = null;
    const manual = jobs.find(
      (j) => j.kind === "scan" && j.source === pick.id && isRunning(j) && j.id !== this.scanJobId,
    );
    if (manual) {
      // Someone else's scan on the scanner: shown, never stopped or retuned.
      await this.stopOwn();
      this.follow(manual.id, pick.id);
      return;
    }
    const s = this.span;
    if (this.scanJobId && s && s.source === pick.id && !this.moved(s, lo, hi)) {
      this.follow(this.scanJobId, pick.id);
      return;
    }
    await this.stopOwn();
    await this.startOwn(pick.id, lo, hi);
  }

  // Whether lo..hi differs from the running span by more than SPAN_CHANGE of its width.
  moved(s, lo, hi) {
    const tol = (s.hi - s.lo) * SPAN_CHANGE;
    return Math.abs(lo - s.lo) > tol || Math.abs(hi - s.hi) > tol;
  }

  // The scanner for view `v` under the `scanner` policy (see the header), or null.
  chooseScanner(srcs, jobs, v) {
    const want = this.view.get("scanner");
    if (want !== "auto") return srcs.find((s) => s.id === want) || null;
    const shown = this.wf.sourceId;
    const scans = new Set(jobs.filter((j) => j.kind === "scan" && isRunning(j)).map((j) => j.id));
    if (this.scanJobId) scans.add(this.scanJobId);
    const rigid = (s) => (Array.isArray(s.holders) ? s.holders : []).filter((h) => !scans.has(h));
    const centre = (v.lo + v.hi) / 2;
    const other = srcs.find((s) => {
      if (s.id === shown || s.fault || rigid(s).length) return false;
      const r = tuneRange(s);
      return !!r && r.lo <= centre && centre <= r.hi;
    });
    if (other) return other;
    const self = srcs.find((s) => s.id === shown);
    if (self && !self.fault && (!Array.isArray(self.holders) || self.holders.length === 0)) {
      return self;
    }
    return null;
  }

  async startOwn(source, lo, hi) {
    let r;
    try {
      r = await this.api.op({
        op: "job_start",
        kind: "scan",
        source,
        center_hz: Math.round((lo + hi) / 2),
        bandwidth_hz: Math.max(1, Math.round(hi - lo)),
        deviation_hz: null,
        sideband: null,
        elastic: false,
        replace: [],
      });
    } catch (e) {
      this.note(`coarse layer: scan on ${source}: ${e.json ? errorText(e.json) : e.message}`);
      return;
    }
    if (this.destroyed) {
      this.api.op({ op: "job_stop", job: r.value }).catch(() => {});
      return;
    }
    this.scanJobId = r.value;
    this.span = { source, lo, hi };
    storeAutoId(r.value);
    this.follow(r.value, source);
  }

  // Stops the page's own scan (never another).
  async stopOwn() {
    const id = this.scanJobId;
    if (!id) return;
    if (this.followed === id) this.unfollow();
    this.forgetOwn();
    try {
      await this.api.op({ op: "job_stop", job: id });
    } catch {
      /* already gone */
    }
  }

  forgetOwn() {
    if (this.followed === this.scanJobId) this.unfollow();
    this.scanJobId = null;
    this.span = null;
    storeAutoId(null);
  }

  stopOnPageHide() {
    const id = this.scanJobId;
    if (!id) return;
    fetch(this.api.base + "/op", {
      method: "POST",
      keepalive: true,
      headers: { Authorization: "Bearer " + this.api.secret, "Content-Type": "application/json" },
      body: JSON.stringify({ op: "job_stop", job: id }),
    }).catch(() => {});
  }

  // ---- the followed scan's frames ------------------------------------------------------

  unfollow() {
    this.subGen += 1;
    if (this.sub) this.sub.close();
    this.sub = null;
    this.followed = null;
    this.source = null;
  }

  async lookupStream(jobId) {
    const streams = await this.api.streams();
    const s = streams.find((x) => x.kind === "event" && x.provenance && x.provenance.job === jobId);
    return s ? s.id : null;
  }

  async follow(jobId, source) {
    if (this.followed === jobId) return;
    this.unfollow();
    this.followed = jobId;
    this.source = source;
    const gen = this.subGen;
    let streamId = null;
    for (let i = 0; i < STREAM_LOOKUPS && gen === this.subGen && !streamId; i++) {
      try {
        streamId = await this.lookupStream(jobId);
      } catch {
        /* retried */
      }
      if (!streamId) await sleep(STREAM_LOOKUP_MS);
    }
    if (gen !== this.subGen || !streamId) return;
    this.sub = this.api.streamText(
      streamId,
      (msg) => {
        if (gen !== this.subGen || msg.type !== "frame") return;
        this.rows += 1;
        for (const cb of this.rowCbs) cb(msg);
      },
      () => {},
      {
        onReconnect: async () => {
          const id = await this.lookupStream(jobId);
          if (id || gen !== this.subGen) return id;
          // The scan ended: decide again (a manual one gone, or the own one stopped).
          this.unfollow();
          this.schedule();
          return null;
        },
      },
    );
  }

  destroy() {
    this.destroyed = true;
    clearTimeout(this.timer);
    this.unfollow();
    window.removeEventListener("pagehide", this.onPageHide);
    if (this.wf.coarseActive) this.wf.coarseActive = null;
  }
}
