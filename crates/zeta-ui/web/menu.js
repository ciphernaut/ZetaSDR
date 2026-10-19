// menu.js: the context menu on a waterfall selection. Every entry applies the same operation
// the agent would (through the page's op path, so its ops-stream line shows in the transcript)
// or asks the agent.
//
// Exports:
//   mountMenu(wf, {applyOp, ask, transcript, ov}) -> Menu
//       Opens #ctx at the pointer after each wf.onSelect. `applyOp(op) -> Promise<result|null>`
//       is app.js's command-line op path (shows its outcome per ruling R6; null when it
//       failed, the failure already reported); `ask(text)` is its /ask path.
//   Menu:
//     menu.open(sel, x, y)  shows the menu for a waterfall selection at client (x, y)
//     menu.close()          hides it and erases the selection rectangle
//     menu.destroy()
//
// Entries (`.ctx-item[data-action]`):
//   classify   What is this?                  notes "classify: running…", then classify
//                                             {selection}; the layer-1 labels are drawn with
//                                             ov.label at the selection's centre (stacked
//                                             LABEL_STEP_PX apart, or LABEL_FALLBACK_STEP_HZ
//                                             while the band is unknown), the top layer-2
//                                             label one line under the last of them; every
//                                             label is listed in the transcript as
//                                             `L<layer> <name> <confidence>`
//   aprs_decode  Decode › APRS                job_start kind=aprs_decode, deviation_hz null
//   fm_demod   Decode › NBFM audio            job_start kind=fm_demod, deviation_hz 5000
//   wfm_demod  Decode › FM broadcast audio    job_start kind=wfm_demod, deviation_hz 75000
//   am_demod   Decode › AM                    job_start kind=am_demod, deviation_hz null
//   ssb_usb    Decode › SSB (USB)             job_start kind=ssb_demod, sideband usb, at the
//                                             dial: center_hz = selection centre − 1500,
//                                             bandwidth_hz 6000 (ruling R6)
//   ssb_lsb    Decode › SSB (LSB)             likewise, sideband lsb, centre + 1500
//   measure    Start background job › measure job_start kind=measure, deviation_hz null
//                                             (every job_start sends sideband: null unless SSB)
//   iq_read    Save selection as SigMF        iq_read {selection}; the transcript shows the path
//   ask        Send to agent                  /ask "Look at <c> Hz, <bw> Hz wide, from <start>
//                                             to <end>: what is it?"
// The jobs use the selection's centre and bandwidth on the source the waterfall shows (the
// SSB entries their dial frequency and SSB_BANDWIDTH_HZ instead).
// #ctx-footer shows the selection in the vocabulary: center_hz=… bandwidth_hz=… start_ns=…
// end_ns=…, with the span already clamped (see below); while an SSB entry is focused or
// hovered it shows that entry's dial instead: center_hz=<dial> bandwidth_hz=6000
// sideband=usb|lsb start_ns=… end_ns=….
//
// Span: the node reads the selection from the source ring, which keeps RING_HISTORY_S. The
// span is clamped to the newest SPAN_LIMIT_NS before the newest painted column (on opening
// and again when an entry is chosen); the footer says so when it was.
//
// Closing: Escape, a pointer press outside the menu, or choosing an entry. Up/Down move the
// focus between entries; Enter or Space chooses one.

export const SPAN_LIMIT_NS = 8_000_000_000;
export const LABEL_STEP_PX = 18;
// The label spacing in Hz while the waterfall cannot map Hz to pixels (band not known yet).
export const LABEL_FALLBACK_STEP_HZ = 1000;

// The SSB entries tune the carrier this far below (USB) or above (LSB) the selection's centre
// (ruling R6: a voice signal's energy sits about 1.5 kHz from its carrier) ...
export const SSB_DIAL_OFFSET_HZ = 1500;
// ... and ask for this bandwidth.
export const SSB_BANDWIDTH_HZ = 6000;

const ENTRIES = [
  ["classify", "What is this?"],
  ["aprs_decode", "Decode › APRS"],
  ["fm_demod", "Decode › NBFM audio"],
  ["wfm_demod", "Decode › FM broadcast audio"],
  ["am_demod", "Decode › AM"],
  ["ssb_usb", "Decode › SSB (USB)"],
  ["ssb_lsb", "Decode › SSB (LSB)"],
  ["measure", "Start background job › measure"],
  ["iq_read", "Save selection as SigMF"],
  ["ask", "Send to agent"],
];
// The job each decode entry starts: kind, deviation_hz and sideband.
const JOBS = {
  aprs_decode: { kind: "aprs_decode", deviation: null, sideband: null },
  fm_demod: { kind: "fm_demod", deviation: 5000, sideband: null },
  wfm_demod: { kind: "wfm_demod", deviation: 75000, sideband: null },
  am_demod: { kind: "am_demod", deviation: null, sideband: null },
  ssb_usb: { kind: "ssb_demod", deviation: null, sideband: "usb" },
  ssb_lsb: { kind: "ssb_demod", deviation: null, sideband: "lsb" },
  measure: { kind: "measure", deviation: null, sideband: null },
};

// The tuning a job entry uses for a selection: {centerHz, bandwidthHz}. SSB tunes the dial.
export function jobTuning(action, s) {
  const sb = JOBS[action] ? JOBS[action].sideband : null;
  if (sb === "usb") return { centerHz: s.centerHz - SSB_DIAL_OFFSET_HZ, bandwidthHz: SSB_BANDWIDTH_HZ };
  if (sb === "lsb") return { centerHz: s.centerHz + SSB_DIAL_OFFSET_HZ, bandwidthHz: SSB_BANDWIDTH_HZ };
  return { centerHz: s.centerHz, bandwidthHz: s.bandwidthHz };
}

export function mountMenu(wf, opts) {
  return new Menu(wf, opts);
}

// End time of the newest column the waterfall painted (null before any).
function newestNs(wf) {
  if (!wf.columns) return null;
  const slot = (wf.columns - 1) % wf.ringEnd.length;
  return wf.ringEnd[slot] || null;
}

// The selection's span limited to the last SPAN_LIMIT_NS: {startNs, endNs, clamped}.
export function clampSpan(startNs, endNs, newest) {
  if (newest === null) return { startNs, endNs, clamped: false };
  let s = startNs;
  let e = Math.min(endNs, newest);
  const lo = newest - SPAN_LIMIT_NS;
  if (s < lo) s = lo;
  if (e <= s) e = newest;
  s = Math.round(s);
  e = Math.round(e);
  return { startNs: s, endNs: e, clamped: s !== startNs || e !== endNs };
}

class Menu {
  constructor(wf, { applyOp, ask, transcript, ov }) {
    this.wf = wf;
    this.applyOp = applyOp;
    this.ask = ask;
    this.t = transcript;
    this.ov = ov;
    this.sel = null;
    this.labelKeys = [];
    this.pointer = { x: 0, y: 0 };

    this.el = document.createElement("div");
    this.el.id = "ctx";
    this.el.setAttribute("role", "menu");
    this.el.hidden = true;
    for (const [action, text] of ENTRIES) {
      const item = document.createElement("button");
      item.className = "ctx-item";
      item.dataset.action = action;
      item.setAttribute("role", "menuitem");
      item.textContent = text;
      item.addEventListener("click", () => this.choose(action));
      if (JOBS[action] && JOBS[action].sideband) {
        const show = () => this.sel && this.showFooter(this.current(), action);
        const hide = () => this.sel && this.showFooter(this.current());
        item.addEventListener("focus", show);
        item.addEventListener("pointerenter", show);
        item.addEventListener("blur", hide);
        item.addEventListener("pointerleave", hide);
      }
      this.el.append(item);
    }
    this.footer = document.createElement("div");
    this.footer.id = "ctx-footer";
    this.el.append(this.footer);
    document.body.append(this.el);

    // The pointer position of the drag's end: pointerup reaches the document (capture) before
    // the waterfall's own handler calls onSelect.
    this.onPointerUp = (ev) => {
      this.pointer = { x: ev.clientX, y: ev.clientY };
    };
    this.onPointerDown = (ev) => {
      if (!this.el.hidden && !this.el.contains(ev.target)) this.close();
    };
    this.onKey = (ev) => {
      if (this.el.hidden) return;
      if (ev.key === "Escape") {
        ev.preventDefault();
        this.close();
      } else if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
        ev.preventDefault();
        const items = [...this.el.querySelectorAll(".ctx-item")];
        const at = items.indexOf(document.activeElement);
        const step = ev.key === "ArrowDown" ? 1 : -1;
        items[(at + step + items.length) % items.length].focus();
      }
    };
    document.addEventListener("pointerup", this.onPointerUp, true);
    document.addEventListener("pointerdown", this.onPointerDown, true);
    document.addEventListener("keydown", this.onKey);
    wf.onSelect((sel) => this.open(sel, this.pointer.x, this.pointer.y));
  }

  // The selection with its span clamped, as the footer shows it.
  current() {
    const s = this.sel;
    return { ...s, ...clampSpan(s.startNs, s.endNs, newestNs(this.wf)) };
  }

  // The selection, or with an SSB `action` the dial that entry tunes.
  showFooter(s, action = null) {
    const t = jobTuning(action, s);
    const sb = action && JOBS[action] ? JOBS[action].sideband : null;
    this.footer.textContent =
      `center_hz=${t.centerHz} bandwidth_hz=${t.bandwidthHz} ` +
      (sb ? `sideband=${sb} ` : "") +
      `start_ns=${s.startNs} end_ns=${s.endNs}` +
      (s.clamped ? " (span clamped to the last 8 s)" : "");
  }

  open(sel, x, y) {
    if (!this.wf.sourceId) return;
    this.sel = { ...sel, sourceId: this.wf.sourceId };
    this.showFooter(this.current());
    this.el.hidden = false;
    // Keep it inside the viewport.
    const w = this.el.offsetWidth;
    const h = this.el.offsetHeight;
    this.el.style.left = `${Math.max(0, Math.min(x, window.innerWidth - w))}px`;
    this.el.style.top = `${Math.max(0, Math.min(y, window.innerHeight - h))}px`;
    this.el.querySelector(".ctx-item").focus({ preventScroll: true });
  }

  close() {
    this.el.hidden = true;
    this.wf.clearSelection();
  }

  async choose(action) {
    if (!this.sel) return;
    const s = this.current();
    this.showFooter(s, action);
    this.close();
    const selection = {
      source: s.sourceId,
      span: { start_ns: s.startNs, end_ns: s.endNs },
      center_hz: s.centerHz,
      bandwidth_hz: s.bandwidthHz,
    };
    if (action === "classify") {
      this.t.note("classify: running…");
      const r = await this.applyOp({ op: "classify", selection });
      if (r && r.result === "labels") this.showLabels(s, r.value || []);
    } else if (action === "iq_read") {
      const r = await this.applyOp({ op: "iq_read", selection });
      if (r && r.result === "iq" && r.value) {
        this.t.note(`saved ${r.value.samples} samples to ${r.value.path}`);
      }
    } else if (action === "ask") {
      await this.ask(
        `Look at ${s.centerHz} Hz, ${s.bandwidthHz} Hz wide, ` +
          `from ${s.startNs} to ${s.endNs}: what is it?`,
      );
    } else if (action in JOBS) {
      const job = JOBS[action];
      const t = jobTuning(action, s);
      await this.applyOp({
        op: "job_start",
        kind: job.kind,
        source: s.sourceId,
        center_hz: t.centerHz,
        bandwidth_hz: t.bandwidthHz,
        deviation_hz: job.deviation,
        sideband: job.sideband,
      });
    }
  }

  // Draws the layer-1 labels at the selection's centre, stacked downwards, and the top
  // layer-2 label one line under the last of them (overlays.js offsets layer 2), replacing
  // the previous classification's; lists every label in the transcript.
  showLabels(s, labels) {
    for (const k of this.labelKeys) this.ov.unlabel(k);
    this.labelKeys = [];
    if (labels.length === 0) {
      this.t.note(`classify: no labels for ${s.centerHz} Hz`);
      return;
    }
    const y0 = this.wf.yForHz(s.centerHz);
    const y1 = this.wf.yForHz(s.centerHz + 1000);
    const stepHz =
      y0 !== null && y1 !== null && y0 !== y1
        ? (LABEL_STEP_PX * 1000) / (y0 - y1)
        : LABEL_FALLBACK_STEP_HZ;
    // Only the top layer-2 label is drawn; the others appear in the transcript.
    const l1 = labels.filter((l) => l.layer !== 2);
    const top2 = labels
      .filter((l) => l.layer === 2)
      .reduce((a, b) => (a && (a.confidence ?? 0) >= (b.confidence ?? 0) ? a : b), null);
    l1.forEach((l, i) => {
      const key = `classify:${i}`;
      this.ov.label(s.centerHz - i * stepHz, l, key);
      this.labelKeys.push(key);
    });
    if (top2) {
      this.ov.label(s.centerHz - Math.max(0, l1.length - 1) * stepHz, top2, "classify:l2");
      this.labelKeys.push("classify:l2");
    }
    labels.forEach((l) => {
      const conf = typeof l.confidence === "number" ? ` ${l.confidence.toFixed(2)}` : "";
      this.t.note(`L${l.layer} ${l.name}${conf}${l.evidence ? ` (${l.evidence})` : ""}`);
    });
  }

  destroy() {
    document.removeEventListener("pointerup", this.onPointerUp, true);
    document.removeEventListener("pointerdown", this.onPointerDown, true);
    document.removeEventListener("keydown", this.onKey);
    this.el.remove();
  }
}
