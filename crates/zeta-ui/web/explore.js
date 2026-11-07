// explore.js: the Explore pane (plan 19 spec 5.7), the captures the node holds.
//
// mountExplorePane(el, api, view, transcript, {panes, ov, wf, onBits})
//   -> {render(), eligible(), destroy()}
//   Polls `capture_list` once at load, then every EXPLORE_POLL_MS while the pane is shown, has
//   been shown once this load, a `capture` job runs (fed by noteJobs(jobs) from each job_list;
//   one more poll when the last one ends) or a bracket is drawn (so a deleted capture's goes;
//   brackets scroll off with the columns), and never otherwise: a page that never opens
//   Explore, runs no capture job and has no capture on screen costs one op per load. Lists the captures newest first (time, band, size, the feature summary,
//   the nearest transmitter's licensee) as `tr.capture` rows with the buttons What is this
//   (`capture_classify`, labels to the transcript), Download (both SigMF files fetched with the
//   bearer and saved as Blobs: the secret never goes into a URL) and Delete (`capture_delete`,
//   a second click within DELETE_CONFIRM_MS confirms; the button reads `sure?` between) and
//   Work the bits (`bits_start {capture, modulation: null, symbol_rate_baud, order: null}`,
//   `symbol_rate_baud` the row's `input.bits-rate`, prefilled from the features' symbol rate,
//   null when empty so the node fills it; the transcript says `started bits <id> on <kHz> kHz`
//   or the error, then the Bits pane is selected; a rate typed and the focus in it survive
//   the 2 s redraw).
//   Above the rows a `.explore-tools` bar holds Delete all (`button[data-action="delete-all"]`,
//   armed like Delete: `sure?` for DELETE_CONFIRM_MS, then a second click deletes every
//   capture that is not pinned, one `capture_delete` each, and notes
//   `deleted <n> captures` with `(<m> pinned kept)` or the failures appended).
//   Draws the cached region (ov.span, tagged with the id's last 6 characters) of each capture
//   still on the waterfall's columns (one that ended before the oldest column is skipped).
//   Each poll also lists `signature_list {state: null}`; under the captures a
//   `table.signatures` holds the records in EXPLORE_SIGNATURE_STATES, newest first, one
//   `tr.signature[data-id]` each: `td.name` (an `input.sig-name` prefilled with the name,
//   `readonly` once accepted; a name typed, and the focus and caret in it, survive the
//   redraw), `td.confidence` (two decimals), `td.state` (`proposed`, `accepted`, or `accepted · learned` and the row id
//   when `decoder` is set), `td.evidence` (signatureText) and `td.actions`: Accept
//   (proposed only: `signature_accept {id, name}`, `name` the input when it was changed,
//   else null; the transcript notes `accepted <name> (<id tail>)` and the `decoder_note`
//   when set) and Reject (armed like Delete: `sure?` for DELETE_CONFIRM_MS, then
//   `signature_reject`, noting `rejected <name> (<id tail>)`).
//   Registers the `explore` pane with the strip, eligible while any capture or any listed
//   signature exists, and selects it when a capture appears that the load's first poll did
//   not list (once per load): a
//   reload never takes the pane back from the stored one (spec 5.7, auto-selected the first
//   time a capture appears, never again).

import { bandText, khzText, registerText } from "./units.js";

export const EXPLORE_POLL_MS = 2000;
export const DELETE_CONFIRM_MS = 3000;
// The signature states the pane lists (a rejected or superseded record is history).
export const EXPLORE_SIGNATURE_STATES = ["proposed", "accepted"];
// The characters of an id the transcript and the brackets name it by.
const ID_TAIL = 6;
// The row fields the operator types in; a redraw keeps the focus and caret in them.
const FIELDS = "input.sig-name, input.bits-rate";

// A signature's evidence cell: its `evidence`, else `<modulation> <kHz> kHz`.
export function signatureText(rec) {
  if (!rec) return "";
  if (typeof rec.evidence === "string" && rec.evidence) return rec.evidence;
  const bw = typeof rec.bandwidth_hz === "number" ? ` ${khzText(rec.bandwidth_hz)} kHz` : "";
  return `${rec.modulation ?? "?"}${bw}`;
}

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

export function mountExplorePane(
  el,
  api,
  view,
  transcript,
  { panes = null, ov = () => null, wf = () => null, onBits = null } = {},
) {
  const pane = new ExplorePane(el, api, view, transcript, panes, ov, wf);
  pane.onBits = onBits; // (job id) after a bits_start: the Bits pane shows that job
  return pane;
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
    this.baseline = null; // the ids the first poll listed (null before it answers)
    this.selected = false; // the auto-select happened (once per load)
    this.everShown = false; // the pane was shown this load
    this.capturing = false; // a capture job runs (noteJobs)
    this.timer = null;
    this.destroyed = false;
    this.armed = new Map(); // capture id -> timer of the pending delete confirmation
    this.armedAll = null; // timer of the pending Delete all confirmation
    this.deletingAll = false;
    this.busy = new Set();
    this.rates = new Map(); // capture id -> the symbol rate typed in its row
    this.signatures = []; // the listed records in EXPLORE_SIGNATURE_STATES, newest first
    this.names = new Map(); // signature id -> the name typed in its row
    this.armedReject = new Map(); // signature id -> timer of the pending reject confirmation
    this.onInput = (ev) => {
      const tr = ev.target.closest("tr.capture");
      if (tr && ev.target.classList.contains("bits-rate")) this.rates.set(tr.dataset.id, ev.target.value);
      const sig = ev.target.closest("tr.signature");
      if (sig && ev.target.classList.contains("sig-name")) this.names.set(sig.dataset.id, ev.target.value);
    };
    el.addEventListener("input", this.onInput);
    this.onClick = (ev) => this.click(ev);
    el.addEventListener("click", this.onClick);
    panes?.register("explore", "explore", {
      eligible: () => this.eligible(),
      el,
      onShow: () => {
        this.everShown = true;
        this.poll();
      },
      onHide: () => {},
    });
    this.poll();
  }

  eligible() {
    return this.captures.length > 0 || this.signatures.length > 0;
  }

  shown() {
    return !this.el.hidden;
  }

  // Whether the 2 s poll runs (see the header).
  polling() {
    return this.shown() || this.everShown || this.capturing || this.spans.size > 0;
  }

  // From each job_list: whether a capture job runs; its end gets one more poll.
  noteJobs(jobs) {
    const was = this.capturing;
    this.capturing = (jobs || []).some(
      (j) => j && j.kind === "capture" && (j.state?.status ?? j.status) === "running",
    );
    if (this.capturing && !was) this.schedule();
    else if (was && !this.capturing && !this.polling()) this.poll();
  }

  schedule() {
    if (this.destroyed || this.timer !== null || !this.polling()) return;
    this.timer = setTimeout(() => {
      this.timer = null;
      this.poll();
    }, EXPLORE_POLL_MS);
  }

  async poll() {
    if (this.destroyed) return;
    let listed = false;
    const [caps, sigs] = await Promise.allSettled([
      this.api.op({ op: "capture_list", signal: null }),
      this.api.op({ op: "signature_list", state: null }),
    ]);
    if (this.destroyed) return;
    // A failed list keeps the last one; the next poll tries again.
    const r = caps.status === "fulfilled" ? caps.value : null;
    if (r && r.result === "captures" && Array.isArray(r.value)) {
      this.captures = r.value;
      listed = true;
    }
    const s = sigs.status === "fulfilled" ? sigs.value : null;
    if (s && s.result === "signatures" && Array.isArray(s.value)) {
      this.signatures = s.value
        .filter((x) => x && EXPLORE_SIGNATURE_STATES.includes(x.state))
        .sort((a, b) => (b.provenance?.proposed_ns ?? 0) - (a.provenance?.proposed_ns ?? 0));
      const ids = new Set(this.signatures.map((x) => x.id));
      for (const id of this.names.keys()) if (!ids.has(id)) this.names.delete(id);
    }
    this.render();
    this.syncSpans();
    const ids = this.captures.map((c) => c.id);
    if (!listed) {
      // Nothing new is known: no baseline, no auto-select.
    } else if (this.baseline === null) this.baseline = new Set(ids);
    else if (!this.selected && ids.some((id) => !this.baseline.has(id))) {
      this.selected = true;
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
    const wf = this.wf();
    const sourceId = wf?.sourceId;
    const oldest = wf?.oldestNs?.() ?? null;
    const want = new Set();
    for (const c of this.captures) {
      if (c.source !== sourceId) continue;
      if (oldest !== null && c.end_ns < oldest) continue;
      const key = `capture:${c.id}`;
      const lo = c.center_hz - c.bandwidth_hz / 2;
      const hi = c.center_hz + c.bandwidth_hz / 2;
      if (ov.span(lo, hi, c.start_ns, c.end_ns, c.id.slice(-ID_TAIL), key)) want.add(key);
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
        ["bits", "Work the bits"],
      ]) {
        if (action === "bits") td.append(this.rateInput(c));
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
    const tools = document.createElement("div");
    tools.className = "explore-tools";
    const all = document.createElement("button");
    all.type = "button";
    all.dataset.action = "delete-all";
    all.textContent = this.armedAll !== null ? "sure?" : "Delete all";
    all.title = "capture_delete every capture that is not pinned";
    all.disabled = this.deletingAll || !this.captures.some((c) => !c.pinned);
    tools.append(all);
    const parts = [tools, table];
    if (this.signatures.length) parts.push(this.signatureTable());
    const place = this.focusedField();
    this.el.replaceChildren(...parts);
    this.restoreField(place);
  }

  // The operator's place in a row's name or rate field (the element, its row and class,
  // the selection), when one has focus: the redraw replaces it, so restoreField puts the
  // focus and the caret back in the new one.
  focusedField() {
    const a = document.activeElement;
    if (!a || !this.el.contains(a) || !a.matches(FIELDS)) return null;
    const tr = a.closest("tr[data-id]");
    if (!tr) return null;
    let sel = null;
    try {
      // A number input has no selection (null, or a throw in older engines).
      if (a.selectionStart !== null) sel = [a.selectionStart, a.selectionEnd, a.selectionDirection];
    } catch {
      sel = null;
    }
    return { row: tr.className.split(" ")[0], id: tr.dataset.id, cls: a.className, sel };
  }

  restoreField(place) {
    if (!place) return;
    const tr = [...this.el.querySelectorAll(`tr.${place.row}`)].find((r) => r.dataset.id === place.id);
    const input = tr?.querySelector(`input.${place.cls}`);
    if (!input) return;
    input.focus({ preventScroll: true });
    if (place.sel && !input.readOnly) {
      try {
        input.setSelectionRange(...place.sel);
      } catch {
        // No selection on this input type.
      }
    }
  }

  // The `table.signatures` under the captures (see the header).
  signatureTable() {
    const body = document.createElement("tbody");
    for (const s of this.signatures) {
      const tr = document.createElement("tr");
      tr.className = `signature ${s.state}`;
      tr.dataset.id = s.id;
      const accepted = s.state === "accepted";
      const name = document.createElement("td");
      name.className = "name";
      const input = document.createElement("input");
      input.type = "text";
      input.className = "sig-name";
      input.value = accepted ? s.name : (this.names.get(s.id) ?? s.name);
      input.readOnly = accepted;
      input.title = accepted ? s.name : "the name Accept gives it";
      name.append(input);
      const conf = document.createElement("td");
      conf.className = "confidence";
      conf.textContent = typeof s.confidence === "number" ? s.confidence.toFixed(2) : "";
      const state = document.createElement("td");
      state.className = "state";
      if (accepted && s.decoder) {
        const row = document.createElement("code");
        row.textContent = s.decoder;
        state.append("accepted · learned ", row);
      } else state.textContent = s.state;
      state.title = [s.decoder_file, s.decoder_note].filter(Boolean).join(" · ") || state.textContent;
      const evidence = document.createElement("td");
      evidence.className = "evidence";
      evidence.textContent = signatureText(s);
      evidence.title = evidence.textContent;
      const actions = document.createElement("td");
      actions.className = "actions";
      const buttons = accepted ? [] : [
        ["accept", "Accept"],
        ["reject", this.armedReject.has(s.id) ? "sure?" : "Reject"],
      ];
      for (const [action, label] of buttons) {
        const b = document.createElement("button");
        b.type = "button";
        b.dataset.action = action;
        b.textContent = label;
        actions.append(b);
      }
      tr.append(name, conf, state, evidence, actions);
      body.append(tr);
    }
    const table = document.createElement("table");
    table.className = "signatures";
    table.append(body);
    return table;
  }

  // The symbol rate Work the bits sends: what was typed, else the features' (empty: none).
  rateInput(c) {
    const input = document.createElement("input");
    input.type = "number";
    input.className = "bits-rate";
    input.min = "1";
    input.step = "1";
    input.placeholder = "Bd";
    input.title = "symbol rate for Work the bits (empty: the node's measurement)";
    const rate = c.features?.symbol_rate_baud;
    input.value = this.rates.get(c.id) ?? (typeof rate === "number" ? String(Math.round(rate)) : "");
    return input;
  }

  async click(ev) {
    const b = ev.target.closest("button[data-action]");
    if (b && b.dataset.action === "delete-all") return this.removeAll();
    const sig = ev.target.closest("tr.signature");
    if (b && sig) {
      if (b.dataset.action === "reject") return this.reject(sig.dataset.id);
      if (b.dataset.action === "accept") return this.accept(sig.dataset.id, sig.querySelector("input.sig-name")?.value);
      return;
    }
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
      else if (action === "bits") await this.bits(id, tr.querySelector("input.bits-rate")?.value);
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
      const reg = registerText(v.allocations);
      if (reg) this.t.note(`${id}: ${reg}`);
    } catch (e) {
      this.t.note(`capture_classify: ${e.message}`, true);
    }
  }

  async bits(id, rateText) {
    const c = this.captures.find((x) => x.id === id);
    const rate = Number.parseInt(rateText, 10);
    try {
      const r = await this.api.op({
        op: "bits_start",
        capture: id,
        modulation: null,
        symbol_rate_baud: Number.isFinite(rate) && rate > 0 ? rate : null,
        order: null,
      });
      const job = r && r.result === "job" ? r.value : null;
      if (!job) return;
      this.t.note(`started bits ${job} on ${c ? khzText(c.bandwidth_hz) : "?"} kHz`);
      this.onBits?.(job);
      this.panes?.select("bits");
    } catch (e) {
      this.t.note(`bits_start: ${e.message}`, true);
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

  // Accept (see the header): `name` is the row's input, sent only when it was changed.
  async accept(id, typed) {
    const key = `${id}accept`;
    if (this.busy.has(key)) return;
    this.busy.add(key);
    const s = this.signatures.find((x) => x.id === id);
    const text = (typed ?? "").trim();
    const name = text && text !== s?.name ? text : null;
    try {
      const r = await this.api.op({ op: "signature_accept", id, name });
      const rec = r && r.result === "signature" ? r.value : null;
      if (rec) {
        this.names.delete(id);
        this.t.note(`accepted ${rec.name} (${rec.id.slice(-ID_TAIL)})`);
        if (rec.decoder_note) this.t.note(`${rec.id.slice(-ID_TAIL)}: ${rec.decoder_note}`);
      }
    } catch (e) {
      this.t.note(`signature_accept: ${e.message}`, true);
    } finally {
      this.busy.delete(key);
    }
    await this.poll();
  }

  // Reject (see the header): arm, then a second click within DELETE_CONFIRM_MS rejects.
  async reject(id) {
    if (!this.armedReject.has(id)) {
      this.armedReject.set(
        id,
        setTimeout(() => {
          this.armedReject.delete(id);
          if (!this.destroyed) this.render();
        }, DELETE_CONFIRM_MS),
      );
      this.render();
      return;
    }
    clearTimeout(this.armedReject.get(id));
    this.armedReject.delete(id);
    try {
      const r = await this.api.op({ op: "signature_reject", id });
      const rec = r && r.result === "signature" ? r.value : null;
      if (rec) this.t.note(`rejected ${rec.name} (${rec.id.slice(-ID_TAIL)})`);
    } catch (e) {
      this.t.note(`signature_reject: ${e.message}`, true);
    }
    await this.poll();
  }

  // Delete all (see the header): arm, then every unpinned capture of the last list.
  async removeAll() {
    if (this.deletingAll) return;
    if (this.armedAll === null) {
      this.armedAll = setTimeout(() => {
        this.armedAll = null;
        if (!this.destroyed) this.render();
      }, DELETE_CONFIRM_MS);
      this.render();
      return;
    }
    clearTimeout(this.armedAll);
    this.armedAll = null;
    this.deletingAll = true;
    this.render();
    const pinned = this.captures.filter((c) => c.pinned).length;
    let deleted = 0;
    const failed = [];
    try {
      for (const c of this.captures.filter((x) => !x.pinned)) {
        try {
          await this.api.op({ op: "capture_delete", capture: c.id });
          deleted += 1;
        } catch (e) {
          failed.push(`${c.id}: ${e.message}`);
        }
      }
    } finally {
      this.deletingAll = false;
    }
    const kept = pinned ? ` (${pinned} pinned kept)` : "";
    this.t.note(`deleted ${deleted} capture${deleted === 1 ? "" : "s"}${kept}`);
    for (const f of failed) this.t.note(`capture_delete ${f}`, true);
    await this.poll();
  }

  destroy() {
    this.destroyed = true;
    clearTimeout(this.timer);
    clearTimeout(this.armedAll);
    for (const t of this.armed.values()) clearTimeout(t);
    for (const t of this.armedReject.values()) clearTimeout(t);
    this.el.removeEventListener("click", this.onClick);
    this.el.removeEventListener("input", this.onInput);
    this.panes?.unregister("explore");
  }
}
