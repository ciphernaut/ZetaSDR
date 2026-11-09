// video.js: a running `video_decode` job's picture on the page (plan 28.2 spec 5.3).
//
// The tag thumbnail: overlays.js puts a `canvas.video-thumb[data-job]` (64x48) in a video
// job's tag and hands it over (openThumb/closeThumb); one `thumb` socket while the canvas is
// laid out (not in a `.thin` or hidden rail tag) and the pane is not shown. The Video pane
// (#video, a `full` pane, panes.js): the selected feed at the view's `video` profile, a status
// line and a strip of every running feed on every source (`thumb` sockets; the selected cell is
// drawn from the main socket); leaving it closes them and reopens the tags'.
//
// Exports: stats (window.zetaVideo, e2e): {drawn, sockets} and sockets_main, sockets_thumb,
// sockets_events (the pane's per-radio feeds; `sockets` counts the pictures') and
//   mountVideo(api, {panes, view, el, jobs, shownSource}) -> Video   jobs(): every source's
//       job_list; shownSource(): the waterfall's (overlays.js hands over its lock events;
//       while shown, each other source with a video job has its event feed subscribed)
//     eligible()   any running video_decode job;  openThumb(jobId, canvas) / closeThumb(jobId)
//     onLock(jobId, event)   `nolock` on the job's thumbnails while `locked` is false; status
//     sync()   after each jobs poll: strip, selection (its job stopped: the next feed, none:
//         panes.select(null)), feeds, and a new look-up for each picture without a socket
//     refresh()   on a source switch;  show() / hide()   panes.js;  select(jobId);  destroy()
// The pane's DOM: `.video-status` (`span.text` `<source> · <centre MHz> · <system> <lock> ·
// <n> f/s · <snr> dB · <profile>`, `button.video-profile` naming the other profile: sets the
// view's `video`), `.video-stage > canvas.video-main[data-job]`, `.video-strip >
// .thumb-cell[data-job] > canvas.video-thumb[data-job]` (`.selected` on the shown feed's;
// `nolock` on cell and canvas while the last lock event says so). A frame is a PNG in the
// envelope, decoded with createImageBitmap (the CSP allows no blob: images); one decode at a
// time, a newer frame replacing a waiting one.
import { parseEnvelopeRaw } from "./audio.js";

export const STRIP_W = 120;
export const STRIP_H = 90;
// A job runs before its streams exist: a picture looks its stream up this many times this far
// apart (as audio.js), then again at each sync() until it has one.
export const STREAM_LOOKUPS = 20;
export const STREAM_LOOKUP_MS = 250;

export const stats = { drawn: 0, sockets: 0, sockets_main: 0, sockets_thumb: 0, sockets_events: 0 };
window.zetaVideo = stats;

const isVideo = (j) => !!(j && j.kind === "video_decode" && j.state && j.state.status === "running");
const make = (tag, cls) => Object.assign(document.createElement(tag), { className: cls });
const mhz = (job) => (job.tuning ? `${(job.tuning.center_hz / 1e6).toFixed(3)} MHz` : "?");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// The id of the first GET /streams entry of `kind` whose provenance passes `match`, or null.
async function streamWhere(api, kind, match) {
  const list = await api.streams();
  const s = list.find((x) => x.kind === kind && x.provenance && match(x.provenance));
  return s ? s.id : null;
}
const videoOf = (api, jobId) => streamWhere(api, "video", (p) => p.job === jobId);
const feedOf = (api, source) => streamWhere(api, "event", (p) => p.job === null && p.source === source);

// One video socket of `jobId` at `profile`, each frame drawn on every canvas of `canvases()`,
// only while `alive()` (its job runs). A stream that ended (`closed`) is let go and never
// subscribed again (GET /streams may list it a while after); the next sync() looks again, for
// a new stream of a job that still runs (a restarted graph's).
class Picture {
  constructor(api, jobId, profile, canvases, { onFrame = () => {}, alive = () => true } = {}) {
    Object.assign(this, { api, jobId, profile, canvases, onFrame, alive });
    this.kind = profile === "thumb" ? "sockets_thumb" : "sockets_main";
    Object.assign(this, { sub: null, id: null, ended: new Set(), looking: false, closed: false });
    Object.assign(this, { busy: false, waiting: null });
    this.open();
  }

  // Looks the stream up and subscribes; a no-op while subscribed or looking.
  async open() {
    if (this.closed || this.sub || this.looking || !this.alive()) return;
    this.looking = true;
    let id = null;
    for (let i = 0; i < STREAM_LOOKUPS && id === null && !this.closed; i++) {
      if (i) await sleep(STREAM_LOOKUP_MS);
      id = await this.look();
    }
    this.looking = false;
    if (this.closed || id === null || !this.alive()) return;
    stats.sockets++;
    stats[this.kind]++;
    this.id = id;
    this.sub = this.api.streamBinary(id, (buf) => this.frame(buf), {
      video: this.profile,
      onText: (msg) => msg && msg.type === "closed" && this.ended.add(this.id) && this.letGo(),
      onReconnect: async () => (this.id = await this.look()) ?? this.letGo(),
    });
  }

  // The running job's video stream id, unless it is one that ended; null.
  async look() {
    const id = this.alive() ? await videoOf(this.api, this.jobId).catch(() => null) : null;
    return id !== null && !this.ended.has(id) ? id : null;
  }

  // Closes the socket for good (the stream is gone); null, for onReconnect.
  letGo() {
    if (!this.sub) return null;
    this.sub.close();
    this.sub = null;
    stats.sockets--;
    stats[this.kind]--;
    return null;
  }

  frame(buf) {
    if (this.closed) return;
    let body = null;
    try {
      body = parseEnvelopeRaw(buf).body;
    } catch {}
    if (!body) return;
    this.onFrame();
    if (this.busy) this.waiting = body;
    else this.decode(body);
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

  // Closes it for good; null (`pic = pic?.close() ?? null`).
  close() {
    this.closed = true;
    this.waiting = null;
    return this.letGo();
  }
}

export function mountVideo(api, opts) {
  return new Video(api, opts);
}

class Video {
  constructor(api, { panes, view, el, jobs = () => [], shownSource = () => null }) {
    // thumbs: jobId -> {canvas, pic | null} (the tags'); locks: jobId -> the last video event;
    // cells: jobId -> {cell, canvas, pic} (the strip's); feeds: radio -> its event feed while
    // shown; main: the main Picture; frames: received by it in the current second.
    Object.assign(this, { api, panes, view, el, jobs, shownSource, selected: null, main: null });
    Object.assign(this, { thumbs: new Map(), locks: new Map(), cells: new Map(), feeds: new Map() });
    Object.assign(this, { shown: false, polled: false, frames: 0, fps: 0, timer: null });
    this.build();
    Object.assign(this, {
      eligible: () => this.running().length > 0,
      show: () => this.onShow(),
      hide: () => this.onHide(),
      onLock: (jobId, ev) => this.lock(jobId, ev),
    });
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

  running = () => (this.jobs() || []).filter(isVideo);
  picture = (id, profile, canvases, onFrame) =>
    new Picture(this.api, id, profile, canvases, { onFrame, alive: () => isVideo(this.job(id)) });
  job = (id) => (this.jobs() || []).find((j) => j.id === id) || null;

  openThumb(jobId, canvas) {
    this.closeThumb(jobId);
    this.thumbs.set(jobId, { canvas, pic: null });
    this.paintLock(jobId);
    // After the tag pass that made it has laid the tags out (a rail tag may go thin).
    setTimeout(() => this.syncThumbs(), 0);
  }

  closeThumb(jobId) {
    this.thumbs.get(jobId)?.pic?.close();
    this.thumbs.delete(jobId);
  }

  // A tag's socket only while its canvas is laid out (offsetParent: not in a hidden or `.thin`
  // tag) and the pane is not over the waterfall; a socket still without a stream looks again.
  syncThumbs() {
    for (const [id, t] of this.thumbs) {
      const want = !this.shown && t.canvas.isConnected && t.canvas.offsetParent !== null;
      if (want && t.pic) t.pic.open();
      else if (want) t.pic = this.picture(id, "thumb", () => [t.canvas]);
      else t.pic = t.pic?.close() ?? null;
    }
  }

  lock(jobId, ev) {
    this.locks.set(jobId, ev);
    this.paintLock(jobId);
    if (jobId === this.selected) this.paintStatus();
  }

  paintLock(jobId) {
    const ev = this.locks.get(jobId);
    const off = !!ev && ev.locked === false;
    const [t, c] = [this.thumbs.get(jobId), this.cells.get(jobId)];
    for (const x of [t?.canvas, c?.cell, c?.canvas]) x?.classList.toggle("nolock", off);
  }

  onShow() {
    this.shown = true;
    this.syncThumbs(); // the waterfall is hidden: its tags' sockets close
    if (!isVideo(this.job(this.selected))) this.selected = this.running()[0]?.id ?? null;
    this.fps = this.frames = 0;
    this.timer = setInterval(() => {
      [this.fps, this.frames] = [this.frames, 0];
      this.paintStatus();
    }, 1000);
    this.openMain();
    this.reconcile();
  }

  onHide() {
    this.shown = false;
    clearInterval(this.timer);
    this.timer = null;
    this.main?.close();
    this.main = null;
    for (const id of [...this.cells.keys()]) this.dropCell(id);
    this.syncFeeds();
    this.syncThumbs();
  }

  select(jobId) {
    this.selected = jobId;
    if (!this.shown) return;
    this.openMain();
    this.reconcile();
  }

  openMain() {
    this.main?.close();
    this.main = null;
    this.canvas.dataset.job = this.selected ?? "";
    this.canvas.getContext("2d").clearRect(0, 0, this.canvas.width, this.canvas.height);
    this.paintStatus();
    if (!this.selected) return;
    const id = this.selected;
    const canvases = () => [this.fitMain(), this.cells.get(id)?.canvas];
    this.main = this.picture(id, this.view.get("video"), canvases, () => this.frames++);
  }

  // The main canvas at the largest 4:3 that fits the stage (device pixels).
  fitMain() {
    const c = this.canvas;
    const dpr = window.devicePixelRatio || 1;
    const w = Math.max(4, Math.round(Math.min(this.stage.clientWidth, (this.stage.clientHeight * 4) / 3) * dpr));
    const h = Math.max(3, Math.round((w * 3) / 4));
    if (c.width !== w || c.height !== h) Object.assign(c, { width: w, height: h });
    return c;
  }

  paintStatus() {
    const profile = this.view.get("video");
    this.profileBtn.textContent = profile === "smooth" ? "sharp" : "smooth";
    this.profileBtn.title = `view set video ${this.profileBtn.textContent}`;
    const job = this.job(this.selected);
    if (!job) return void (this.statusText.textContent = "no video feed");
    const ev = this.locks.get(job.id) || {};
    const lock = ev.lock ?? (ev.locked === true ? "locked" : ev.locked === false ? "no lock" : "waiting");
    const snr = Number.isFinite(ev.snr_db) ? `${ev.snr_db} dB` : "? dB";
    const parts = [job.source, mhz(job), `${ev.system ?? "?"} ${lock}`, `${this.fps} f/s`, snr, profile];
    this.statusText.textContent = parts.join(" · ");
  }

  // After each jobs poll.
  sync() {
    this.polled = true;
    const ids = new Set(this.running().map((j) => j.id));
    for (const id of [...this.locks.keys()]) if (!ids.has(id)) this.locks.delete(id);
    this.syncThumbs();
    this.main?.open();
    for (const c of this.cells.values()) c.pic?.open();
    this.reconcile();
  }

  refresh() {
    this.syncThumbs();
    this.syncFeeds();
  }

  // The strip and the selection against the jobs. With no feed left once a poll has answered
  // the pane goes (a pane restored over a reload waits for the first poll).
  reconcile() {
    if (!this.shown) return;
    const run = this.running();
    const ids = new Set(run.map((j) => j.id));
    if (!this.selected || !ids.has(this.selected)) {
      if (run.length) return this.select(run[0].id);
      this.selected = null;
      if (this.polled) return this.panes.select(null); // runs onHide
    }
    for (const id of [...this.cells.keys()]) if (!ids.has(id)) this.dropCell(id);
    for (const j of run) this.cellFor(j);
    this.syncFeeds();
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
    if (selected) c.pic = c.pic?.close() ?? null;
    else c.pic ??= this.picture(job.id, "thumb", () => [c.canvas]);
  }

  dropCell(id) {
    const c = this.cells.get(id);
    c?.pic?.close();
    c?.cell.remove();
    this.cells.delete(id);
  }

  // Lock events of the feeds on a source the waterfall does not show (overlays.js has the
  // shown source's): that source's event feed (plan 27), one socket per source, while shown.
  syncFeeds() {
    const here = this.shownSource();
    const want = new Set(this.shown ? this.running().map((j) => j.source).filter((s) => s !== here) : []);
    for (const src of [...this.feeds.keys()]) if (!want.has(src)) this.dropFeed(src);
    for (const src of want) {
      if (this.feeds.has(src)) continue;
      const sub = { inner: null, closed: false };
      sub.close = () => {
        if (sub.inner && !sub.closed) stats.sockets_events--;
        sub.closed = true;
        sub.inner?.close();
      };
      this.feeds.set(src, sub);
      const onMessage = (msg) => {
        if (msg && msg.type === "closed") return this.feeds.get(src) === sub && this.dropFeed(src);
        const p = msg && msg.type === "frame" ? msg.payload : null;
        if (p && p.protocol === "video" && typeof p.locked === "boolean" && msg.job) this.lock(msg.job, p);
      };
      feedOf(this.api, src)
        .catch(() => null)
        .then((sid) => {
          if (sub.closed) return;
          // No feed listed (yet): forgotten, so the next sync looks again.
          if (sid === null) this.feeds.delete(src);
          else {
            stats.sockets_events++;
            sub.inner = this.api.streamText(sid, onMessage, null, { onReconnect: () => feedOf(this.api, src) });
          }
        });
    }
  }

  dropFeed(src) {
    this.feeds.get(src)?.close();
    this.feeds.delete(src);
  }

  destroy() {
    for (const id of [...this.thumbs.keys()]) this.closeThumb(id);
    this.onHide();
    this.el.replaceChildren();
  }
}
