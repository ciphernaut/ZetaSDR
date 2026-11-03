// graph.js: a job's graph document as an SVG in a modal, with its nodes' params editable.
//
// Exports:
//   layout(graph) -> {order, column}
//       `order`: node ids in a topological order over `edges` (Kahn's algorithm, ties in
//       insertion order); nodes left over by a cycle follow in insertion order. `column`: id ->
//       column, 0 for a node with no earlier predecessor, else one right of its rightmost
//       predecessor earlier in `order`. Edges naming a missing node are ignored.
//   showGraph(api, jobId) -> Promise
//       graph_get {job}; opens the modal #graph (replacing an open one) with an SVG: nodes as
//       boxes (`g.graph-node[data-id][data-kind]`: kind and short id) in columns by `layout`,
//       stacked in `order` within a column; edges as lines with an arrow
//       (`g.graph-edge[data-kind]`) labelled with the stream kind. Clicking a node shows its
//       `params` JSON in #graph-params; #graph-apply parses it and sends graph_put {job,
//       graph} with that node's params replaced and the rest of the document unchanged
//       (restarting the job under the same id), then graph_get and re-renders with the node
//       still selected; `.graph-status` says "applied". Failures (bad JSON, the node's
//       op error with its corrective detail) show in `.graph-error`. Escape or the × button
//       closes it.
//   closeGraph()
//       Closes the open graph modal, if any (the app calls it when the secret is rejected).

const NODE_W = 160;
const NODE_H = 58;
// The params each stage kind shows under its name (up to two, as `key value`).
const HEADLINE_PARAMS = {
  channel: ["center_hz", "bandwidth_hz"],
  nbfm_demod: ["deviation_hz", "squelch_db"],
  wfm_demod: ["deviation_hz"],
  am_demod: ["squelch_db"],
  ssb_demod: ["sideband"],
  transcribe: ["language"],
  fft: ["size", "window"],
  detect: ["threshold_db", "energy_db"],
  measure: ["bandwidth_hz"],
  burst_capture: ["bursts", "window_ms"],
  sink_sondehub: ["callsign"],
  mix: ["sources"],
  stream_in: ["job"],
  external_decode: ["mode"],
  passthrough: ["kind"],
};
const HEADLINE_MAX = 26;

function headline(n) {
  const params = n.params || {};
  const parts = [];
  for (const key of HEADLINE_PARAMS[n.kind] || []) {
    if (!(key in params) || params[key] === null || params[key] === undefined) continue;
    const v = params[key];
    parts.push(`${key} ${Array.isArray(v) ? `[${v.length}]` : v}`);
    if (parts.length === 2) break;
  }
  const text = parts.join(" ");
  return text.length > HEADLINE_MAX ? text.slice(0, HEADLINE_MAX - 1) + "\u2026" : text;
}

function nodeRadius() {
  const r = parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--radius-bar"));
  return Number.isFinite(r) ? r : 4;
}
const COL_GAP = 70;
const ROW_GAP = 24;
const PAD = 16;
const SVG_NS = "http://www.w3.org/2000/svg";

export function layout(graph) {
  const ids = graph.nodes.map((n) => n.id);
  const known = new Set(ids);
  const edges = graph.edges.filter((e) => known.has(e.from) && known.has(e.to));
  const indeg = new Map(ids.map((id) => [id, 0]));
  for (const e of edges) indeg.set(e.to, indeg.get(e.to) + 1);
  const order = [];
  const ready = ids.filter((id) => indeg.get(id) === 0);
  while (ready.length) {
    const id = ready.shift();
    order.push(id);
    for (const e of edges) {
      if (e.from !== id) continue;
      indeg.set(e.to, indeg.get(e.to) - 1);
      if (indeg.get(e.to) === 0) ready.push(e.to);
    }
  }
  // A cycle leaves nodes behind: they follow in insertion order.
  for (const id of ids) if (!order.includes(id)) order.push(id);
  const at = new Map(order.map((id, i) => [id, i]));
  const column = new Map();
  for (const id of order) {
    let c = 0;
    for (const e of edges) {
      if (e.to === id && at.get(e.from) < at.get(id)) c = Math.max(c, column.get(e.from) + 1);
    }
    column.set(id, c);
  }
  return { order, column };
}

function svgEl(name, attrs = {}, text = null) {
  const el = document.createElementNS(SVG_NS, name);
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, String(v));
  if (text !== null) el.textContent = text;
  return el;
}

const short = (id) => String(id).slice(-6);

// An op failure as one line, with the node's corrective detail (graph cause and reason,
// range and suggestion) when it gave one.
export function errorMessage(e) {
  const j = e && e.json;
  if (!j || typeof j !== "object") return e && e.message ? e.message : String(e);
  const fmt = (v) => (typeof v === "object" ? JSON.stringify(v) : v);
  const parts = [j.error || "error"];
  let why = j.message || "";
  const d = j.detail;
  if (d && typeof d === "object") {
    if (d.cause) parts.push(d.cause);
    if (d.node) parts.push(`on node ${short(d.node)}`);
    for (const [k, v] of Object.entries(d)) {
      if (!["cause", "node", "reason"].includes(k)) parts.push(`${k}=${fmt(v)}`);
    }
    if (d.reason) why = d.reason;
  } else if (d !== undefined) {
    parts.push(String(d));
  }
  for (const [k, v] of Object.entries(j)) {
    if (!["error", "detail", "message"].includes(k) && v !== null) parts.push(`${k}=${fmt(v)}`);
  }
  return why ? `${parts.join(" ")}: ${why}` : parts.join(" ");
}

let current = null; // the open modal

class GraphModal {
  constructor(api, jobId) {
    this.api = api;
    this.jobId = jobId;
    this.doc = null;
    this.selected = null;

    this.el = document.createElement("div");
    this.el.id = "graph";
    this.el.setAttribute("role", "dialog");
    this.el.setAttribute("aria-modal", "true");
    this.el.setAttribute("aria-label", `graph of job ${jobId}`);
    const panel = document.createElement("div");
    panel.className = "graph-panel";
    const head = document.createElement("div");
    head.className = "graph-head";
    const title = document.createElement("span");
    title.className = "graph-title";
    title.textContent = `graph · job ${short(jobId)}`;
    title.title = jobId;
    const close = document.createElement("button");
    close.className = "graph-close";
    close.type = "button";
    close.textContent = "×";
    close.title = "close (Escape)";
    close.onclick = () => this.close();
    head.append(title, close);

    this.svgBox = document.createElement("div");
    this.svgBox.className = "graph-canvas";

    this.edit = document.createElement("div");
    this.edit.className = "graph-edit";
    this.edit.hidden = true;
    this.nodeTitle = document.createElement("div");
    this.nodeTitle.className = "graph-node-title";
    this.params = document.createElement("textarea");
    this.params.id = "graph-params";
    this.params.spellcheck = false;
    this.params.rows = 8;
    const actions = document.createElement("div");
    actions.className = "graph-actions";
    this.apply = document.createElement("button");
    this.apply.id = "graph-apply";
    this.apply.type = "button";
    this.apply.textContent = "Apply";
    this.apply.title = "graph_put with this node's params";
    this.apply.onclick = () => this.put();
    this.status = document.createElement("span");
    this.status.className = "graph-status";
    actions.append(this.apply, this.status);
    this.edit.append(this.nodeTitle, this.params, actions);

    this.error = document.createElement("div");
    this.error.className = "graph-error";
    this.error.setAttribute("role", "alert");

    panel.append(head, this.svgBox, this.edit, this.error);
    this.el.append(panel);
    this.el.addEventListener("pointerdown", (ev) => {
      if (ev.target === this.el) this.close();
    });
    this.onKey = (ev) => {
      if (ev.key === "Escape") {
        ev.preventDefault();
        this.close();
      }
    };
    document.addEventListener("keydown", this.onKey);
    document.body.append(this.el);
  }

  close() {
    document.removeEventListener("keydown", this.onKey);
    this.el.remove();
    if (current === this) current = null;
  }

  async load() {
    try {
      const r = await this.api.op({ op: "graph_get", job: this.jobId });
      this.doc = r.value;
      this.render();
      return true;
    } catch (e) {
      this.error.textContent = errorMessage(e);
      return false;
    }
  }

  render() {
    const g = this.doc;
    const { order, column } = layout(g);
    const rows = new Map(); // column -> next row
    const pos = new Map(); // id -> {x, y}
    for (const id of order) {
      const c = column.get(id);
      const r = rows.get(c) || 0;
      rows.set(c, r + 1);
      pos.set(id, { x: PAD + c * (NODE_W + COL_GAP), y: PAD + r * (NODE_H + ROW_GAP) });
    }
    const cols = Math.max(1, ...[...column.values()].map((c) => c + 1));
    const maxRows = Math.max(1, ...rows.values());
    const w = 2 * PAD + cols * NODE_W + (cols - 1) * COL_GAP;
    const h = 2 * PAD + maxRows * NODE_H + (maxRows - 1) * ROW_GAP;
    const svg = svgEl("svg", {
      viewBox: `0 0 ${w} ${h}`,
      width: w,
      height: h,
      role: "img",
      "aria-label": `${g.nodes.length} nodes, ${g.edges.length} edges`,
    });
    const defs = svgEl("defs");
    const marker = svgEl("marker", {
      id: "graph-arrow",
      viewBox: "0 0 8 8",
      refX: 8,
      refY: 4,
      markerWidth: 8,
      markerHeight: 8,
      orient: "auto",
    });
    marker.append(svgEl("path", { d: "M0,0 L8,4 L0,8 z", class: "graph-arrowhead" }));
    defs.append(marker);
    svg.append(defs);

    for (const e of g.edges) {
      const a = pos.get(e.from);
      const b = pos.get(e.to);
      if (!a || !b) continue;
      const x1 = a.x + NODE_W;
      const y1 = a.y + NODE_H / 2;
      const x2 = b.x;
      const y2 = b.y + NODE_H / 2;
      const grp = svgEl("g", { class: "graph-edge", "data-kind": e.kind });
      grp.append(
        svgEl("line", { x1, y1, x2, y2, "marker-end": "url(#graph-arrow)" }),
        svgEl("text", { x: (x1 + x2) / 2, y: (y1 + y2) / 2 - 6, "text-anchor": "middle" }, e.kind),
      );
      svg.append(grp);
    }
    const rx = nodeRadius();
    for (const n of g.nodes) {
      const p = pos.get(n.id);
      const grp = svgEl("g", {
        class: "graph-node" + (n.id === this.selected ? " selected" : ""),
        "data-id": n.id,
        "data-kind": n.kind,
        tabindex: 0,
        role: "button",
      });
      grp.append(
        svgEl("rect", { x: p.x, y: p.y, width: NODE_W, height: NODE_H, rx }),
        svgEl("text", { x: p.x + 10, y: p.y + 19, class: "kind" }, n.kind),
        svgEl("text", { x: p.x + 10, y: p.y + 36, class: "nid" }, short(n.id)),
        svgEl("text", { x: p.x + 10, y: p.y + 50, class: "graph-node-headline" }, headline(n)),
      );
      const title = svgEl("title", {}, `${n.kind} ${n.id}\n${JSON.stringify(n.params)}`);
      grp.append(title);
      grp.addEventListener("click", () => this.select(n.id));
      grp.addEventListener("keydown", (ev) => {
        if (ev.key === "Enter" || ev.key === " ") {
          ev.preventDefault();
          this.select(n.id);
        }
      });
      svg.append(grp);
    }
    this.svgBox.replaceChildren(svg);
  }

  select(id) {
    const n = this.doc && this.doc.nodes.find((x) => x.id === id);
    if (!n) return;
    this.selected = id;
    for (const g of this.svgBox.querySelectorAll(".graph-node")) {
      g.classList.toggle("selected", g.dataset.id === id);
    }
    this.nodeTitle.textContent = `${n.kind} ${short(n.id)} params`;
    this.params.value = JSON.stringify(n.params, null, 2);
    this.status.textContent = "";
    this.error.textContent = "";
    this.edit.hidden = false;
    this.params.focus();
  }

  async put() {
    const id = this.selected;
    if (!this.doc || !id) return;
    this.status.textContent = "";
    this.error.textContent = "";
    let params;
    try {
      params = JSON.parse(this.params.value);
    } catch (e) {
      this.error.textContent = `params: not valid JSON: ${e.message}`;
      return;
    }
    const graph = {
      ...this.doc,
      nodes: this.doc.nodes.map((n) => (n.id === id ? { ...n, params } : n)),
    };
    this.apply.disabled = true;
    try {
      await this.api.op({ op: "graph_put", job: this.jobId, graph });
    } catch (e) {
      this.error.textContent = errorMessage(e);
      return;
    } finally {
      this.apply.disabled = false;
    }
    if (await this.load()) {
      this.select(id);
      this.status.textContent = "applied; job restarted";
    }
  }
}

export function closeGraph() {
  if (current) current.close();
}

export async function showGraph(api, jobId) {
  if (current) current.close();
  const modal = new GraphModal(api, jobId);
  current = modal;
  await modal.load();
  return modal;
}
