// bits.js: the Bits pane (plan 19.2, layer 4), a bits job's stream worked by eye.
//
// mountBitsPane(el, api, view, transcript, {panes, jobs}) -> {render(), setJob(id), destroy()}
//   `jobs()` is the job list the page already polls (app.js hands each one to noteJobs too).
//   Registers the `bits` pane with the strip, eligible while any running job's
//   `context.slicer.stream` exists (a `bits_start` job, or a graph with a slicer). It shows
//   one job at a time: the one setJob(id) chose, else the newest eligible; a
//   `<select id="bits-job">` picks among several. While shown it polls the job's
//   `job_context` every BITS_POLL_MS for `clock` (`#bits-clock`: `<rate_est_baud> Bd
//   locked|searching`) and GET /streams for the job's event streams (`provenance.job`; the
//   slicer's, bit_view's hypotheses, a `correlate`'s syncs), subscribing to each one listed
//   and closing those no longer listed (a `graph_put` relaunches the job with new ids). It
//   holds no socket while hidden.
//   `bits` events append rows (`#bits-rows .bits-row`: the row's first bit offset, then
//   BITS_ROW_BYTES bytes of hex as `.byte` spans), at most BITS_ROWS_MAX rows, oldest
//   dropped; the byte under the pointer shows its 8 bits in `#bits-binary`. `sync` events
//   mark the bytes they cover `.sync` by `offset_bits` (on the newest row holding that
//   offset; a row scrolled off keeps nothing). The latest `hypotheses` event fills
//   `#bits-hyps`: one `<li>` per sync candidate (`0x2dd4 ×14 every 1120 bits`) with a Try
//   button (`data-action="try"`: graph_get, a `correlate` node {pattern, bits, max_errors: 0}
//   added or updated and fed by an `event` edge from the `slicer` node in bit_view's place
//   (see withCorrelate), graph_put; the rows clear, the hypotheses stay), then the frame
//   periods, the likely encoding and each CRC candidate (a `period_bits` of 0 reads `to next
//   sync`). `#bits-eye` draws the latest `constellation`
//   event: an eye of the symbols against ±1 lines on 1 channel, I/Q dots on 2.

export const BITS_POLL_MS = 1000;
export const BITS_ROWS_MAX = 64;
export const BITS_ROW_BYTES = 16;

// The pattern width a sync mark covers when the pane did not set the correlator itself.
const SYNC_BITS_DEFAULT = 16;

function running(j) {
  return j && (j.state?.status ?? j.status) === "running";
}

function slicerStream(j) {
  const s = j?.context?.slicer?.stream;
  return typeof s === "string" && s ? s : null;
}

export function bitsEligible(j) {
  return running(j) && slicerStream(j) !== null;
}

function hexBits(hex) {
  return parseInt(hex, 16).toString(2).padStart(8, "0");
}

// The `correlate` node set to `pattern` (hex, `bits` wide) and fed by an `event` edge from
// the slicer: the graph changed in place. A stage's output feeds one edge (the node gives each
// edge its own stream, and the slicer publishes on one), so the correlator takes the place of
// what the slicer fed (bit_view): the pane keeps the last hypotheses, and Try again only
// updates the correlator's params.
export function withCorrelate(graph, pattern, bits) {
  const params = { pattern, bits, max_errors: 0 };
  const node = graph.nodes.find((n) => n.id === "correlate");
  if (node) {
    node.kind = "correlate";
    node.params = params;
  } else graph.nodes.push({ id: "correlate", kind: "correlate", params });
  const fed = new Set(
    graph.edges.filter((e) => e.from === "slicer" && e.to !== "correlate").map((e) => e.to),
  );
  graph.nodes = graph.nodes.filter((n) => !fed.has(n.id));
  graph.edges = graph.edges.filter(
    (e) => e.to !== "correlate" && !fed.has(e.to) && !fed.has(e.from),
  );
  graph.edges.push({ from: "slicer", to: "correlate", kind: "event" });
  return graph;
}

export function mountBitsPane(el, api, view, transcript, { panes = null, jobs = () => [] } = {}) {
  return new BitsPane(el, api, view, transcript, panes, jobs);
}

class BitsPane {
  constructor(el, api, view, transcript, panes, jobs) {
    this.el = el;
    this.api = api;
    this.view = view;
    this.t = transcript;
    this.panes = panes;
    this.jobs = jobs;
    this.jobId = null; // the job chosen with setJob (or the select)
    this.shownJob = null; // the job whose state the pane holds
    this.subs = new Map(); // stream id -> {sub, closed}
    this.rows = []; // {offset, bytes: [hex], count}
    this.syncs = []; // {offset, bits}
    this.syncBits = SYNC_BITS_DEFAULT;
    this.hyps = null;
    this.constellation = null;
    this.clock = null;
    this.timer = null;
    this.polling = false;
    this.destroyed = false;
    this.trying = false;
    this.build();
    this.onClick = (ev) => this.click(ev);
    this.onOver = (ev) => this.over(ev);
    this.onChange = (ev) => {
      if (ev.target === this.selectEl) this.setJob(this.selectEl.value);
    };
    el.addEventListener("click", this.onClick);
    el.addEventListener("pointerover", this.onOver);
    el.addEventListener("change", this.onChange);
    panes?.register("bits", "bits", {
      eligible: () => this.eligible().length > 0,
      el,
      onShow: () => this.show(),
      onHide: () => this.hide(),
    });
  }

  build() {
    const head = document.createElement("div");
    head.className = "bits-head";
    this.selectEl = document.createElement("select");
    this.selectEl.id = "bits-job";
    this.selectEl.hidden = true;
    this.clockEl = document.createElement("span");
    this.clockEl.id = "bits-clock";
    this.binaryEl = document.createElement("span");
    this.binaryEl.id = "bits-binary";
    this.binaryEl.title = "the byte under the pointer";
    head.append(this.selectEl, this.clockEl, this.binaryEl);
    const body = document.createElement("div");
    body.className = "bits-body";
    this.rowsEl = document.createElement("div");
    this.rowsEl.id = "bits-rows";
    const side = document.createElement("div");
    side.className = "bits-side";
    this.eyeEl = document.createElement("canvas");
    this.eyeEl.id = "bits-eye";
    this.eyeEl.width = 192;
    this.eyeEl.height = 128;
    this.hypsEl = document.createElement("ul");
    this.hypsEl.id = "bits-hyps";
    side.append(this.eyeEl, this.hypsEl);
    body.append(this.rowsEl, side);
    this.el.replaceChildren(head, body);
  }

  // The running jobs with a slicer stream, newest first.
  eligible() {
    return (this.jobs() || [])
      .filter(bitsEligible)
      .sort((a, b) => (b.created_ns ?? 0) - (a.created_ns ?? 0));
  }

  // The job the pane shows: the chosen one while eligible, else the newest eligible.
  current() {
    const list = this.eligible();
    return list.find((j) => j.id === this.jobId) ?? list[0] ?? null;
  }

  shown() {
    return !this.el.hidden;
  }

  setJob(id) {
    this.jobId = id;
    if (this.shown()) this.poll();
    this.render();
  }

  // From each job_list (app.js): the select, and a shown pane follows its job.
  noteJobs() {
    this.renderJobs();
    if (this.shown() && (this.current()?.id ?? null) !== this.shownJob) this.poll();
  }

  show() {
    this.render();
    this.poll();
  }

  hide() {
    clearTimeout(this.timer);
    this.timer = null;
    this.closeAll();
  }

  closeAll() {
    for (const s of this.subs.values()) s.sub.close();
    this.subs.clear();
  }

  // The job's state dropped: a new job, or the same one relaunched by a graph_put.
  reset() {
    this.rows = [];
    this.syncs = [];
    this.hyps = null;
    this.constellation = null;
    this.clock = null;
  }

  schedule() {
    if (this.destroyed || this.timer !== null || !this.shown()) return;
    this.timer = setTimeout(() => {
      this.timer = null;
      this.poll();
    }, BITS_POLL_MS);
  }

  async poll() {
    if (this.destroyed || this.polling) return;
    const job = this.current();
    if ((job?.id ?? null) !== this.shownJob) {
      this.closeAll();
      this.reset();
      this.shownJob = job?.id ?? null;
      this.render();
    }
    if (!job || !this.shown()) return;
    this.polling = true;
    try {
      const [ctx, streams] = await Promise.all([
        this.api.op({ op: "job_context", job: job.id }).catch(() => null),
        this.api.streams().catch(() => null),
      ]);
      if (this.destroyed || this.shownJob !== job.id || !this.shown()) return;
      const c = ctx?.value;
      if (c && typeof c === "object") {
        this.clock = c.clock ?? null;
        if (!this.hyps && c.bit_view) this.hyps = c.bit_view;
      }
      if (Array.isArray(streams)) this.syncStreams(job.id, streams);
      this.renderClock();
      this.renderHyps();
    } finally {
      this.polling = false;
      this.schedule();
    }
  }

  // Subscribe to each event stream of the job listed, close those no longer listed.
  syncStreams(jobId, streams) {
    const want = new Set(
      streams
        .filter((s) => s.kind === "event" && s.provenance && s.provenance.job === jobId)
        .map((s) => s.id),
    );
    for (const [id, s] of [...this.subs]) {
      if (!want.has(id) || s.closed) {
        s.sub.close();
        this.subs.delete(id);
      }
    }
    for (const id of want) {
      if (this.subs.has(id)) continue;
      const rec = { sub: null, closed: false };
      rec.sub = this.api.streamText(
        id,
        (msg) => {
          if (!rec.closed && msg.type === "frame") this.event(msg.payload);
        },
        () => {
          rec.closed = true;
        },
        { reconnect: false },
      );
      this.subs.set(id, rec);
    }
  }

  event(p) {
    if (!p || typeof p !== "object") return;
    switch (p.protocol) {
      case "bits":
        this.addBits(p);
        break;
      case "sync":
        this.addSync(p);
        break;
      case "hypotheses":
        this.hyps = p;
        this.renderHyps();
        break;
      case "constellation":
        this.constellation = p;
        this.renderEye();
        break;
      default:
        break;
    }
  }

  addBits(p) {
    if (typeof p.bits !== "string" || typeof p.count !== "number") return;
    const base = typeof p.offset_bits === "number" ? p.offset_bits : 0;
    const bytes = p.bits.match(/../g) || [];
    const used = Math.ceil(p.count / 8);
    for (let i = 0; i < used; i += BITS_ROW_BYTES) {
      const row = bytes.slice(i, Math.min(i + BITS_ROW_BYTES, used));
      const count = Math.min(BITS_ROW_BYTES * 8, p.count - i * 8);
      this.rows.push({ offset: base + i * 8, bytes: row, count });
    }
    if (this.rows.length > BITS_ROWS_MAX) this.rows.splice(0, this.rows.length - BITS_ROWS_MAX);
    this.pruneSyncs();
    this.renderRows();
  }

  addSync(p) {
    if (typeof p.offset_bits !== "number") return;
    this.syncs.push({ offset: p.offset_bits, bits: this.syncBits });
    this.pruneSyncs();
    this.renderRows();
  }

  // A sync mark is kept only while a row holds it (a row scrolled off keeps nothing); a
  // sync published before its bits arrive waits, at most as many as a full screen can hold.
  pruneSyncs() {
    const last = this.rows[this.rows.length - 1];
    const end = last ? last.offset + last.count : 0;
    const held = (o) => this.rows.some((r) => o >= r.offset && o < r.offset + r.count);
    const max = (BITS_ROWS_MAX * BITS_ROW_BYTES * 8) / SYNC_BITS_DEFAULT;
    this.syncs = this.syncs.filter((s) => held(s.offset) || s.offset >= end).slice(-max);
  }

  // The newest row holding each sync's first bit marks the bytes the sync covers.
  syncMarks() {
    const marks = new Map(); // row index -> Set of byte indexes
    for (const s of this.syncs) {
      for (let r = this.rows.length - 1; r >= 0; r--) {
        const row = this.rows[r];
        if (s.offset < row.offset || s.offset >= row.offset + row.count) continue;
        for (let b = s.offset; b < s.offset + s.bits; b++) {
          // The bits run on into the next rows of the same run.
          let k = r;
          while (k < this.rows.length && b >= this.rows[k].offset + this.rows[k].count) k++;
          const at = this.rows[k];
          if (!at || b < at.offset) break;
          if (!marks.has(k)) marks.set(k, new Set());
          marks.get(k).add(Math.floor((b - at.offset) / 8));
        }
        break;
      }
    }
    return marks;
  }

  render() {
    if (this.destroyed) return;
    this.renderJobs();
    this.renderClock();
    this.renderRows();
    this.renderHyps();
    this.renderEye();
  }

  renderJobs() {
    const list = this.eligible();
    const cur = this.current();
    this.selectEl.hidden = list.length < 2;
    const key = list.map((j) => j.id).join(",") + "|" + (cur?.id ?? "");
    if (this.selectEl.dataset.key === key) return;
    this.selectEl.dataset.key = key;
    this.selectEl.replaceChildren(
      ...list.map((j) => {
        const o = document.createElement("option");
        o.value = j.id;
        o.textContent = `${j.kind} ${j.id.slice(-6)}`;
        o.selected = j.id === cur?.id;
        return o;
      }),
    );
  }

  renderClock() {
    const c = this.clock;
    if (!c || typeof c.rate_est_baud !== "number") {
      this.clockEl.textContent = this.shownJob ? "clock …" : "no bits job";
      return;
    }
    this.clockEl.textContent = `${Math.round(c.rate_est_baud)} Bd ${c.locked ? "locked" : "searching"}`;
    this.clockEl.title =
      typeof c.error_rms === "number" ? `timing error rms ${c.error_rms.toFixed(3)}` : "";
  }

  renderRows() {
    const marks = this.syncMarks();
    const frag = document.createDocumentFragment();
    this.rows.forEach((row, r) => {
      const div = document.createElement("div");
      div.className = "bits-row";
      const off = document.createElement("span");
      off.className = "off";
      off.textContent = String(row.offset).padStart(8, " ");
      div.append(off);
      const marked = marks.get(r);
      row.bytes.forEach((hex, i) => {
        const b = document.createElement("span");
        b.className = marked && marked.has(i) ? "byte sync" : "byte";
        b.textContent = hex;
        b.dataset.bin = hexBits(hex);
        b.dataset.bit = String(row.offset + i * 8);
        div.append(b);
      });
      frag.append(div);
    });
    const atEnd = this.rowsEl.scrollTop + this.rowsEl.clientHeight >= this.rowsEl.scrollHeight - 4;
    this.rowsEl.replaceChildren(frag);
    if (atEnd) this.rowsEl.scrollTop = this.rowsEl.scrollHeight;
  }

  renderHyps() {
    const h = this.hyps;
    const items = [];
    const li = (text, cls) => {
      const el = document.createElement("li");
      el.className = cls;
      el.textContent = text;
      items.push(el);
      return el;
    };
    if (h) {
      for (const s of h.sync_candidates || []) {
        const el = li(
          `0x${s.word} ×${s.count} every ${Math.round(s.mean_gap_bits)} bits`,
          "sync-candidate",
        );
        el.dataset.word = s.word;
        el.dataset.bits = String(s.bits);
        const b = document.createElement("button");
        b.type = "button";
        b.dataset.action = "try";
        b.textContent = "Try";
        b.title = `correlate on 0x${s.word} (${s.bits} bits)`;
        el.append(" ", b);
      }
      for (const p of h.period_bits || []) {
        li(`period ${p.lag} bits (score ${Number(p.score).toFixed(2)})`, "period");
      }
      if (h.encoding && h.encoding.likely) li(`encoding ${h.encoding.likely}`, "encoding");
      for (const c of h.crc_candidates || []) {
        const period = c.period_bits === 0 ? "to next sync" : `every ${c.period_bits} bits`;
        li(`${c.crc} after 0x${c.sync}, ${period}: ${c.frames}/${c.checked} frames check`, "crc");
      }
    }
    if (!items.length) li(this.shownJob ? "no hypotheses yet" : "", "empty");
    this.hypsEl.replaceChildren(...items);
  }

  renderEye() {
    const cv = this.eyeEl;
    const g = cv.getContext("2d");
    if (!g) return;
    const w = cv.width;
    const h = cv.height;
    g.clearRect(0, 0, w, h);
    const style = getComputedStyle(this.el);
    const ink = style.getPropertyValue("--text").trim() || "#ccc";
    const dim = style.getPropertyValue("--border").trim() || "#555";
    const c = this.constellation;
    const pts = c && Array.isArray(c.points) ? c.points : [];
    g.strokeStyle = dim;
    g.fillStyle = ink;
    if (c && c.channels === 2) {
      const m = Math.max(1e-9, ...pts.map(([x, y]) => Math.hypot(x, y)));
      const r = Math.min(w, h) * 0.45;
      g.beginPath();
      g.moveTo(w / 2 - r, h / 2);
      g.lineTo(w / 2 + r, h / 2);
      g.moveTo(w / 2, h / 2 - r);
      g.lineTo(w / 2, h / 2 + r);
      g.stroke();
      for (const [x, y] of pts) g.fillRect(w / 2 + (x / m) * r - 1, h / 2 - (y / m) * r - 1, 2, 2);
      return;
    }
    // One channel: the symbols in time order against ±1 (their mean magnitude).
    const mean = pts.length ? pts.reduce((a, [x]) => a + Math.abs(x), 0) / pts.length : 1;
    const scale = (h * 0.3) / Math.max(1e-9, mean);
    g.beginPath();
    for (const v of [-1, 1]) {
      g.moveTo(0, h / 2 - v * h * 0.3);
      g.lineTo(w, h / 2 - v * h * 0.3);
    }
    g.stroke();
    pts.forEach(([x], i) => {
      const y = Math.max(0, Math.min(h - 2, h / 2 - x * scale));
      g.fillRect((i / Math.max(1, pts.length - 1)) * (w - 2), y - 1, 2, 2);
    });
  }

  over(ev) {
    const b = ev.target.closest?.(".byte");
    if (!b || !this.rowsEl.contains(b)) return;
    this.binaryEl.textContent = `${b.dataset.bin} @ bit ${b.dataset.bit}`;
  }

  async click(ev) {
    const b = ev.target.closest("button[data-action]");
    if (!b || b.dataset.action !== "try") return;
    const li = b.closest("li");
    if (!li || !this.shownJob || this.trying) return;
    const job = this.shownJob;
    const word = li.dataset.word;
    const bits = Number(li.dataset.bits) || SYNC_BITS_DEFAULT;
    this.trying = true;
    try {
      const r = await this.api.op({ op: "graph_get", job });
      const graph = r && r.value;
      if (!graph || !Array.isArray(graph.nodes)) throw new Error("no graph");
      await this.api.op({ op: "graph_put", job, graph: withCorrelate(graph, word, bits) });
      this.t.note(`correlating ${job.slice(-6)} on 0x${word}`);
      // The job restarts under its id with new streams: the next poll subscribes to them.
      this.syncBits = bits;
      // The hypotheses stay: the correlator took bit_view's place (withCorrelate).
      const hyps = this.hyps;
      this.closeAll();
      this.reset();
      this.hyps = hyps;
      this.render();
    } catch (e) {
      this.t.note(`try 0x${word}: ${e.message}`, true);
    } finally {
      this.trying = false;
    }
  }

  destroy() {
    this.destroyed = true;
    clearTimeout(this.timer);
    this.closeAll();
    this.el.removeEventListener("click", this.onClick);
    this.el.removeEventListener("pointerover", this.onOver);
    this.el.removeEventListener("change", this.onChange);
    this.panes?.unregister("bits");
  }
}
