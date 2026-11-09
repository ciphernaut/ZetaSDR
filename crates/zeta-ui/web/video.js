// video.js: a running `video_decode` job's picture on the page (plan 28.2 spec 5.3).
//
// The thumbnail on the band: overlays.js puts a `canvas.video-thumb[data-job]` (64x48) in a
// video job's tag on the shown source's waterfall and hands it over (openThumb/closeThumb), one
// `thumb` socket each. The Video pane (#video, a `full` pane in panes.js: the whole of #main):
// the selected feed at the view's `video` profile (one socket), a status line over it and a
// strip of every running video feed on every source, one `thumb` socket per other feed (the
// selected one's cell is drawn from the main socket). Leaving the pane closes them all.
//
// Exports:
//   stats   {drawn, sockets}: pictures drawn and video sockets open (window.zetaVideo, e2e)
//   mountVideo(api, {panes, view, el, jobs, shownSource}) -> Video
//       jobs() the last job_list (every source's); shownSource() the waterfall's source id
//       (overlays.js hands over its jobs' lock events; a feed of another source subscribes
//       its job's own event stream while the pane is shown)
//     video.eligible()   any running video_decode job on any source
//     video.openThumb(jobId, canvas) / video.closeThumb(jobId)   a tag's thumbnail
//     video.onLock(jobId, event)   a `video` event: class `nolock` on the job's thumbnails
//         while `locked` is false, and the status line
//     video.sync()   after each jobs poll: the strip, and the selection when its job stopped
//         (the next running feed; none: panes.select(null))
//     video.show() / video.hide()   panes.js onShow/onHide
//     video.select(jobId)   the feed the main picture shows
//     video.destroy()
// The pane's DOM: `.video-status` (`span.text` `<source> · <centre MHz> · <system> <lock> ·
// <n> f/s · <snr> dB · <profile>`, `button.video-profile` naming the other profile: sets the
// view's `video`), `.video-stage > canvas.video-main[data-job]`, `.video-strip >
// .thumb-cell[data-job] > canvas.video-thumb[data-job]` (`.selected` on the shown feed's;
// `nolock` on cell and canvas while the last lock event says so).
// A frame is a PNG in the stream envelope, decoded with createImageBitmap (the CSP allows no
// blob: images) onto the canvas; one decode at a time, a newer frame replacing a waiting one.
import { parseEnvelopeRaw } from "./audio.js";

export const STRIP_W = 120;
export const STRIP_H = 90;

export const stats = { drawn: 0, sockets: 0 };
window.zetaVideo = stats;

const isVideo = (j) => !!(j && j.kind === "video_decode" && j.state && j.state.status === "running");
const make = (tag, cls) => Object.assign(document.createElement(tag), { className: cls });
const mhz = (job) => (job.tuning ? `${(job.tuning.center_hz / 1e6).toFixed(3)} MHz` : "?");

// The id of `jobId`'s stream of `kind` ("video" | "event") in GET /streams, or null.
async function streamOf(api, jobId, kind) {
  const list = await api.streams();
  const s = list.find((x) => x.kind === kind && x.provenance && x.provenance.job === jobId);
  return s ? s.id : null;
}

// One video socket of `jobId` at `profile`, each frame drawn on every canvas of `canvases()`.
class Picture {
  constructor(api, jobId, profile, canvases, onFrame = () => {}) {
    this.jobId = jobId;
    this.canvases = canvases;
    this.onFrame = onFrame;
    this.sub = null;
    this.closed = false;
    this.busy = false;
    this.waiting = null;
    streamOf(api, jobId, "video")
      .then((id) => {
        if (this.closed || id === null) return;
        stats.sockets++;
        this.sub = api.streamBinary(id, (buf) => this.frame(buf), {
          video: profile,
          onReconnect: () => streamOf(api, jobId, "video"),
        });
      })
      .catch(() => {});
  }

  frame(buf) {
    if (this.closed) return;
    let body;
    try {
      body = parseEnvelopeRaw(buf).body;
    } catch {
      return;
    }
    this.onFrame();
    if (this.busy) {
      this.waiting = body;
      return;
    }
    this.decode(body);
  }

  decode(body) {
    this.busy = true;
    createImageBitmap(new Blob([body], { type: "image/png" }))
      .then((bmp) => {
        if (!this.closed) {
          for (const c of this.canvases()) {
            if (c) c.getContext("2d").drawImage(bmp, 0, 0, c.width, c.height);
          }
          stats.drawn++;
        }
        bmp.close();
      })
      .catch(() => {})
      .finally(() => {
        this.busy = false;
        const next = this.waiting;
        this.waiting = null;
        if (next && !this.closed) this.decode(next);
      });
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    this.waiting = null;
    if (this.sub) {
      this.sub.close();
      stats.sockets--;
    }
  }
}

export function mountVideo(api, opts) {
  return new Video(api, opts);
}

class Video {
  constructor(api, { panes, view, el, jobs = () => [], shownSource = () => null }) {
    this.api = api;
    this.panes = panes;
    this.view = view;
    this.el = el;
    this.jobs = jobs;
    this.shownSource = shownSource;
    this.thumbs = new Map(); // jobId -> {canvas, pic}: the tags'
    this.locks = new Map(); // jobId -> the last video event
    this.cells = new Map(); // jobId -> {cell, canvas, pic}: the strip's
    this.events = new Map(); // jobId -> another source's event subscription, while shown
    this.selected = null;
    this.main = null; // the main Picture
    this.shown = false;
    this.polled = false;
    this.frames = 0; // received by the main socket in the current second
    this.fps = 0;
    this.timer = null;
    this.build();
    this.eligible = () => this.running().length > 0;
    this.show = () => this.onShow();
    this.hide = () => this.onHide();
    this.onLock = (jobId, ev) => this.lock(jobId, ev);
    view.onChange((key) => {
      if (key !== "video") return;
      this.paintStatus();
      if (this.shown) this.openMain();
    });
  }

  build() {
    this.statusText = make("span", "text");
    this.profileBtn = make("button", "video-profile");
    this.profileBtn.type = "button";
    this.profileBtn.addEventListener("click", () => {
      this.view.set("video", this.view.get("video") === "smooth" ? "sharp" : "smooth");
    });
    const status = make("div", "video-status");
    status.append(this.statusText, this.profileBtn);
    this.stage = make("div", "video-stage");
    this.canvas = make("canvas", "video-main");
    this.stage.append(this.canvas);
    this.strip = make("div", "video-strip");
    this.el.replaceChildren(status, this.stage, this.strip);
  }

  running() {
    return (this.jobs() || []).filter(isVideo);
  }

  job(id) {
    return (this.jobs() || []).find((j) => j.id === id) || null;
  }

  // ---- the tags' thumbnails -----------------------------------------------------------

  openThumb(jobId, canvas) {
    this.closeThumb(jobId);
    const pic = new Picture(this.api, jobId, "thumb", () => [canvas]);
    this.thumbs.set(jobId, { canvas, pic });
    this.paintLock(jobId);
  }

  closeThumb(jobId) {
    const t = this.thumbs.get(jobId);
    if (!t) return;
    t.pic.close();
    this.thumbs.delete(jobId);
  }

  lock(jobId, ev) {
    this.locks.set(jobId, ev);
    this.paintLock(jobId);
    if (jobId === this.selected) this.paintStatus();
  }

  paintLock(jobId) {
    const ev = this.locks.get(jobId);
    const off = !!ev && ev.locked === false;
    const t = this.thumbs.get(jobId);
    if (t) t.canvas.classList.toggle("nolock", off);
    const c = this.cells.get(jobId);
    if (c) {
      c.cell.classList.toggle("nolock", off);
      c.canvas.classList.toggle("nolock", off);
    }
  }

  // ---- the pane -----------------------------------------------------------------------

  onShow() {
    this.shown = true;
    if (!this.job(this.selected) || !isVideo(this.job(this.selected))) {
      this.selected = this.running()[0]?.id ?? null;
    }
    this.fps = 0;
    this.frames = 0;
    this.timer = setInterval(() => {
      this.fps = this.frames;
      this.frames = 0;
      this.paintStatus();
    }, 1000);
    this.openMain();
    this.reconcile();
  }

  onHide() {
    this.shown = false;
    clearInterval(this.timer);
    this.timer = null;
    if (this.main) this.main.close();
    this.main = null;
    for (const id of [...this.cells.keys()]) this.dropCell(id);
    for (const sub of this.events.values()) sub.close();
    this.events.clear();
  }

  select(jobId) {
    this.selected = jobId;
    if (!this.shown) return;
    this.openMain();
    this.reconcile();
  }

  openMain() {
    if (this.main) this.main.close();
    this.main = null;
    this.canvas.dataset.job = this.selected ?? "";
    this.canvas.getContext("2d").clearRect(0, 0, this.canvas.width, this.canvas.height);
    this.paintStatus();
    if (!this.selected) return;
    const id = this.selected;
    const canvases = () => [this.fitMain(), this.cells.get(id)?.canvas];
    this.main = new Picture(this.api, id, this.view.get("video"), canvases, () => this.frames++);
  }

  // The main canvas at the largest 4:3 that fits the stage (device pixels).
  fitMain() {
    const c = this.canvas;
    const dpr = window.devicePixelRatio || 1;
    const sw = this.stage.clientWidth;
    const sh = this.stage.clientHeight;
    const w = Math.max(4, Math.round(Math.min(sw, (sh * 4) / 3) * dpr));
    const h = Math.max(3, Math.round((w * 3) / 4));
    if (c.width !== w || c.height !== h) {
      c.width = w;
      c.height = h;
    }
    return c;
  }

  paintStatus() {
    const profile = this.view.get("video");
    this.profileBtn.textContent = profile === "smooth" ? "sharp" : "smooth";
    this.profileBtn.title = `view set video ${this.profileBtn.textContent}`;
    const job = this.job(this.selected);
    if (!job) {
      this.statusText.textContent = "no video feed";
      return;
    }
    const ev = this.locks.get(job.id) || {};
    const centre = mhz(job);
    const lock = ev.lock ?? (ev.locked === true ? "locked" : ev.locked === false ? "no lock" : "waiting");
    const snr = Number.isFinite(ev.snr_db) ? `${ev.snr_db} dB` : "? dB";
    const parts = [job.source, centre, `${ev.system ?? "?"} ${lock}`, `${this.fps} f/s`, snr, profile];
    this.statusText.textContent = parts.join(" · ");
  }

  // After each jobs poll.
  sync() {
    this.polled = true;
    const ids = new Set(this.running().map((j) => j.id));
    for (const id of [...this.locks.keys()]) if (!ids.has(id)) this.locks.delete(id);
    this.reconcile();
  }

  // The strip and the selection against the jobs. With no feed left once a poll has answered
  // the pane goes (a pane restored over a reload waits for the first poll).
  reconcile() {
    if (!this.shown) return;
    const run = this.running();
    const ids = new Set(run.map((j) => j.id));
    if (!this.selected || !ids.has(this.selected)) {
      const next = run[0]?.id ?? null;
      if (next !== null) {
        this.select(next);
        return;
      }
      this.selected = null;
      if (this.polled) {
        this.panes.select(null); // runs onHide
        return;
      }
    }
    for (const id of [...this.cells.keys()]) if (!ids.has(id)) this.dropCell(id);
    for (const j of run) this.cellFor(j);
    this.syncEvents(run);
    this.paintStatus();
  }

  cellFor(job) {
    let c = this.cells.get(job.id);
    if (!c) {
      const cell = make("div", "thumb-cell");
      const canvas = Object.assign(make("canvas", "video-thumb"), { width: STRIP_W, height: STRIP_H });
      cell.dataset.job = canvas.dataset.job = job.id;
      canvas.title = `${job.source} ${mhz(job)}`;
      canvas.addEventListener("click", () => this.select(job.id));
      cell.append(canvas, Object.assign(make("span", "label"), { textContent: job.id.slice(-6) }));
      this.strip.append(cell);
      c = { cell, canvas, pic: null };
      this.cells.set(job.id, c);
      this.paintLock(job.id);
    }
    const selected = job.id === this.selected;
    c.canvas.classList.toggle("selected", selected);
    // The selected feed's cell is drawn by the main socket; every other one has its own.
    if (selected && c.pic) {
      c.pic.close();
      c.pic = null;
    } else if (!selected && !c.pic) {
      const canvas = c.canvas;
      c.pic = new Picture(this.api, job.id, "thumb", () => [canvas]);
    }
  }

  dropCell(id) {
    const c = this.cells.get(id);
    if (!c) return;
    if (c.pic) c.pic.close();
    c.cell.remove();
    this.cells.delete(id);
  }

  // Lock events of the feeds on a source the waterfall does not show (overlays.js has the
  // shown source's): each job's own event stream while the pane is shown.
  syncEvents(run) {
    const here = this.shownSource();
    const want = new Set(run.filter((j) => j.source !== here).map((j) => j.id));
    for (const [id, sub] of [...this.events]) {
      if (!want.has(id)) {
        sub.close();
        this.events.delete(id);
      }
    }
    for (const id of want) {
      if (this.events.has(id)) continue;
      const sub = { inner: null, closed: false };
      sub.close = () => {
        sub.closed = true;
        if (sub.inner) sub.inner.close();
      };
      this.events.set(id, sub);
      const onMessage = (msg) => {
        const p = msg && msg.type === "frame" ? msg.payload : null;
        if (p && p.protocol === "video" && typeof p.locked === "boolean") this.lock(id, p);
      };
      streamOf(this.api, id, "event")
        .then((sid) => {
          if (sub.closed || sid === null) return;
          sub.inner = this.api.streamText(sid, onMessage, null, {
            onReconnect: () => streamOf(this.api, id, "event"),
          });
        })
        .catch(() => {});
    }
  }

  destroy() {
    this.onHide();
    for (const id of [...this.thumbs.keys()]) this.closeThumb(id);
    this.el.replaceChildren();
  }
}
