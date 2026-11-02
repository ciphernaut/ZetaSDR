// explore.js: the Explore pane (plan 19 spec 5.7), the captures the node holds.
//
// mountExplorePane(el, api, view, transcript, {panes}) -> {render(), eligible(), destroy()}
//   Polls `capture_list` every EXPLORE_POLL_MS; lists the captures newest first (time, band, size, the feature summary,
//   the nearest transmitter's licensee) as `tr.capture` rows with the buttons What is this
//   (`capture_classify`, labels to the transcript), Download (both SigMF files fetched with the
//   bearer and saved as Blobs: the secret never goes into a URL) and Delete (`capture_delete`,
//   a second click within DELETE_CONFIRM_MS confirms; the button reads `sure?` between).
//   Draws the cached region (ov.span, tagged with the id's last 6 characters) of each capture
//   still on the waterfall's columns; polls while hidden too, for that.
//   Registers the `explore` pane with the strip, eligible while any capture exists, and selects
//   it the first time a capture appears (once per page load).

import { bandText, khzText } from "./units.js";

export const EXPLORE_POLL_MS = 2000;
export const DELETE_CONFIRM_MS = 3000;

function timeText(ns) {
  const d = new Date(Math.round(ns / 1e6));
  return d.toISOString().slice(0, 19).replace("T", " ") + "Z";
}

function sizeText(bytes) {
  if (bytes >= 1048576) return `${(bytes / 1048576).toFixed(1)} MB`;
  return `${Math.max(1, Math.round(bytes / 1024))} kB`;
}

// `<bw> kHz, duty <d>, <baud> Bd` from a FeatureRecord (parts the record lacks are left out).
export function featureText(f) {
  if (!f) return "";
  const parts = [];
  if (typeof f.bandwidth_hz === "number") parts.push(`${khzText(f.bandwidth_hz)} kHz`);
  if (typeof f.duty === "number") parts.push(`duty ${f.duty.toFixed(2)}`);
  if (typeof f.symbol_rate_baud === "number") parts.push(`${Math.round(f.symbol_rate_baud)} Bd`);
  return parts.join(", ");
}

function nearestTx(c) {
  const tx = (c.allocations || [])
    .filter((a) => a && a.role === "tx" && a.licensee)
    .sort((a, b) => (a.distance_km ?? Infinity) - (b.distance_km ?? Infinity));
  return tx.length ? tx[0].licensee : "";
}

export function mountExplorePane(el, api, view, transcript, { panes = null, ov = () => null, wf = () => null } = {}) {
  return new ExplorePane(el, api, view, transcript, panes, ov, wf);
}

class ExplorePane {
  constructor(el, api, view, transcript, panes, ov, wf) {
    this.ov = ov; // () => the overlays (or null)
    this.wf = wf; // () => the waterfall (or null)
    this.spans = new Set(); // keys of the cached-region brackets drawn
    this.el = el;
    this.api = api;
    this.view = view;
    this.t = transcript;
    this.panes = panes;
    this.captures = [];
    this.seen = false; // a capture has been seen (the auto-select happens once)
    this.timer = null;
    this.destroyed = false;
    this.armed = new Map(); // capture id -> timer of the pending delete confirmation
    this.busy = new Set();
    this.onClick = (ev) => this.click(ev);
    el.addEventListener("click", this.onClick);
    panes?.register("explore", "explore", {
      eligible: () => this.eligible(),
      el,
      onShow: () => this.poll(),
      onHide: () => {},
    });
    this.poll();
  }

  eligible() {
    return this.captures.length > 0;
  }

  shown() {
    return !this.el.hidden;
  }

  schedule() {
    if (this.destroyed || this.timer !== null) return;
    this.timer = setTimeout(() => {
      this.timer = null;
      this.poll();
    }, EXPLORE_POLL_MS);
  }

  async poll() {
    if (this.destroyed) return;
    try {
      const r = await this.api.op({ op: "capture_list", signal: null });
      if (this.destroyed) return;
      if (r && r.result === "captures" && Array.isArray(r.value)) this.captures = r.value;
    } catch {
      // The next poll tries again.
    }
    this.render();
    this.syncSpans();
    if (!this.seen && this.captures.length > 0) {
      this.seen = true;
      this.panes?.select("explore");
    }
    this.panes?.render();
    this.schedule();
  }

  // The cached region: a bracket (ov.span) over each capture of the selected source whose end
  // is still on the waterfall's columns, tagged with the tail of its id; the overlays drop it
  // when it scrolls off, so a bracket is only kept here while it is drawn.
  syncSpans() {
    const ov = this.ov();
    if (!ov) return;
    const sourceId = this.wf()?.sourceId;
    const want = new Set();
    for (const c of this.captures) {
      if (c.source !== sourceId) continue;
      const key = `capture:${c.id}`;
      const lo = c.center_hz - c.bandwidth_hz / 2;
      const hi = c.center_hz + c.bandwidth_hz / 2;
      if (ov.span(lo, hi, c.start_ns, c.end_ns, c.id.slice(-6), key)) want.add(key);
    }
    for (const key of this.spans) if (!want.has(key)) ov.unspan(key);
    this.spans = want;
  }

  render() {
    const body = document.createElement("tbody");
    for (const c of this.captures) {
      const tr = document.createElement("tr");
      tr.className = "capture";
      tr.dataset.id = c.id;
      const cells = [
        timeText(c.start_ns),
        bandText(c.center_hz, c.bandwidth_hz),
        sizeText(c.bytes),
        featureText(c.features),
        nearestTx(c),
      ];
      for (const text of cells) {
        const td = document.createElement("td");
        td.textContent = text;
        td.title = text;
        tr.append(td);
      }
      const td = document.createElement("td");
      td.className = "actions";
      for (const [action, label] of [
        ["classify", "What is this"],
        ["download", "Download"],
        ["delete", this.armed.has(c.id) ? "sure?" : "Delete"],
      ]) {
        const b = document.createElement("button");
        b.type = "button";
        b.dataset.action = action;
        b.textContent = label;
        td.append(b);
      }
      tr.append(td);
      body.append(tr);
    }
    const table = document.createElement("table");
    table.append(body);
    this.el.replaceChildren(table);
  }

  async click(ev) {
    const b = ev.target.closest("button[data-action]");
    const tr = ev.target.closest("tr.capture");
    if (!b || !tr) return;
    const id = tr.dataset.id;
    const action = b.dataset.action;
    if (action === "delete") return this.remove(id);
    if (this.busy.has(id + action)) return;
    this.busy.add(id + action);
    try {
      if (action === "classify") await this.classify(id);
      else if (action === "download") await this.download(id);
    } finally {
      this.busy.delete(id + action);
    }
  }

  async classify(id) {
    this.t.note(`${id}: classify…`);
    try {
      const r = await this.api.op({ op: "capture_classify", capture: id });
      const v = r && r.result === "classification" ? r.value : null;
      if (!v) return;
      if (!(v.labels || []).length) this.t.note(`${id}: no labels`);
      for (const l of v.labels || []) {
        const conf = typeof l.confidence === "number" ? ` ${l.confidence.toFixed(2)}` : "";
        this.t.note(`${id}: L${l.layer} ${l.name}${conf}${l.evidence ? ` (${l.evidence})` : ""}`);
      }
      for (const t of v.tried || []) {
        this.t.note(`tried ${t.mode} ${Number(t.score).toFixed(2)} ${t.outcome}`);
      }
    } catch (e) {
      this.t.note(`capture_classify: ${e.message}`, true);
    }
  }

  async download(id) {
    try {
      for (const ext of ["sigmf-meta", "sigmf-data"]) {
        const resp = await fetch(`${this.api.base}/captures/${encodeURIComponent(id)}.${ext}`, {
          headers: { Authorization: "Bearer " + this.api.secret },
        });
        if (!resp.ok) throw new Error(`${id}.${ext}: HTTP ${resp.status}`);
        const url = URL.createObjectURL(await resp.blob());
        const a = document.createElement("a");
        a.href = url;
        a.download = `${id}.${ext}`;
        document.body.append(a);
        a.click();
        a.remove();
        setTimeout(() => URL.revokeObjectURL(url), 10000);
      }
    } catch (e) {
      this.t.note(`download: ${e.message}`, true);
    }
  }

  async remove(id) {
    if (!this.armed.has(id)) {
      this.armed.set(
        id,
        setTimeout(() => {
          this.armed.delete(id);
          if (!this.destroyed) this.render();
        }, DELETE_CONFIRM_MS),
      );
      this.render();
      return;
    }
    clearTimeout(this.armed.get(id));
    this.armed.delete(id);
    try {
      await this.api.op({ op: "capture_delete", capture: id });
    } catch (e) {
      this.t.note(`capture_delete: ${e.message}`, true);
    }
    await this.poll();
  }

  destroy() {
    this.destroyed = true;
    clearTimeout(this.timer);
    for (const t of this.armed.values()) clearTimeout(t);
    this.el.removeEventListener("click", this.onClick);
    this.panes?.unregister("explore");
  }
}
