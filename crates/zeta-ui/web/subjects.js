// subjects.js: what a decoder's events are about (plan 23 spec 4.2, 4.3). A mode row with a
// `subject` names the event field identifying the aircraft, station or sonde; this module
// merges each subject's events into one current state and lists them in the Subjects pane.
// Nothing is kept on the node: a refresh starts empty and refills from live traffic.
//
// Exports:
//   mountSubjects(api, view) -> SubjectStore; sets window.zetaSubjects (null on destroy)
//   SubjectStore:
//     store.lookup(kinds) -> Promise<bool>   the mode rows of `kinds`: `modes_list` once for
//         the ids, one `modes_show` per kind, both cached. A kind that is no row caches null; a
//         failure stays unknown, for the next poll to try. True when anything was learned; one
//         lookup at a time (a call during another answers false, and whoever acts on the
//         other's true asks again).
//     store.rowOf(kind) -> row | null | undefined (undefined: not yet known)
//     store.event(job, payload) -> subject | null   merges the event: non-null fields into
//         `state`, the fix into `position`, `count` and `last_seen`. Null when the job's row has
//         no subject, the job has an `owner` (a watch's or identify's trial) or the key is null,
//         absent or blank: such an event is unkeyed.
//     store.subjectOf(job, payload) -> subject | null   the same test and lookup, no merge
//     store.sync(jobs)   drops the subjects of jobs not `running`
//     store.list(jobId) -> subject[]   least recently heard first
//     store.lastHeard(jobId) -> subject | null
//     store.held(jobId, key | null) -> bool   the subject (with null, any of the job's) is not dim
//     store.select(jobId, key), store.selected() -> {job, key} | null
//     store.onChange(cb)   any merge, dim or drop, and a row learned
//     store.onDrop(cb(jobId, key)), store.onSelect(cb(sel)), store.destroy()
//     A subject past view `subject_dim_s` of silence is dim, past `subject_drop_s` dropped
//     (checked each second, from this page's clock); a selection goes with its subject.
//   A subject: {job, key, state, position, first_seen, last_seen, count, dim, row}. `job` is the
//     job id, `state` a null-prototype object of every field heard, whose last non-null value
//     stands, `position` {lat, lon, alt_m} or null, the altitude surviving a fix without one.
//   subjectLabel(subject) -> state[row.subject.label] when present, else the key
//   subjectSummary(subject) -> the row's summary template filled from the merged state
//   subjectValues(subject) -> [{label, text, title}]   the row's columns through metricText
//   fillSummary(template, fields) -> string   summary.rs's `fill`: `{f}`, `{f?}` (dropped with
//     the separators before it), `{a?b}`, each with an optional `:.N` or `:+.N`
//   mountSubjectsPane(el, store, view, {jobs, watching, footprint}) -> pane
//     One `section.subject-job[data-job]` per running job with subjects (an `h3` and a table of
//     `tr.subject[data-job][data-key]`, classes `dim` and `selected`), and one for a subject job
//     `watching(id)` denies, with `.not-watched`. `th[data-col]` sorts: a first click ascending,
//     a second reversed, nulls last either way (default last seen, newest first). A click on a
//     row selects it. Redrawn at most every RENDER_MS on a change, and each second. Cells are
//     written only when they differ, so a tooltip outlives a redraw. All text by textContent.
//     pane.eligible() -> bool   a running job's row has table columns (app.js shows the pane)
//     pane.destroy()

import { MAX_WATCHED } from "./overlays.js";
import { distanceBearing, positionOf } from "./map.js";
import { metricText } from "./units.js";
import { homePoint } from "./view.js";

const TICK_MS = 1000;
const RENDER_MS = 500;

const isRunning = (j) => !!(j && j.state && j.state.status === "running");

// ---- the summary template --------------------------------------------------------------

const SEPARATORS_AT_END = /[\s·:>,]+$/u;
const NUMBER_FORMAT = /^(\+?)\.([0-6])$/;

// A placeholder's inside as [names, format]: format null unless it ends in `:.N` or `:+.N`.
function splitFormat(inner) {
  const at = inner.lastIndexOf(":");
  if (at < 0) return [inner, null];
  const m = NUMBER_FORMAT.exec(inner.slice(at + 1));
  if (!m) return [inner, null];
  return [inner.slice(0, at), { plus: m[1] === "+", decimals: Number(m[2]) }];
}

function fieldOf(fields, name) {
  return Object.hasOwn(fields, name) ? fields[name] : undefined;
}

function fieldText(v, format) {
  if (v === null || v === undefined) return "null";
  if (typeof v === "string") return v;
  if (typeof v === "number" && format) {
    const fixed = v.toFixed(format.decimals);
    return format.plus && v >= 0 ? `+${fixed}` : fixed;
  }
  return typeof v === "object" ? JSON.stringify(v) : String(v);
}

export function fillSummary(template, fields) {
  let out = "";
  // Where the literal text after the last value begins: only that is separator material.
  let literalFrom = 0;
  let rest = template;
  for (;;) {
    const open = rest.indexOf("{");
    if (open < 0) break;
    const len = rest.slice(open).indexOf("}");
    if (len < 0) break;
    out += rest.slice(0, open);
    const [names, format] = splitFormat(rest.slice(open + 1, open + len));
    rest = rest.slice(open + len + 1);
    const q = names.indexOf("?");
    const a = q < 0 ? names : names.slice(0, q);
    const b = q < 0 ? "" : names.slice(q + 1);
    const v = fieldOf(fields, a);
    const there = v !== null && v !== undefined;
    if (q < 0) out += fieldText(v, format);
    else if (there) out += fieldText(v, format);
    else if (b === "") out = out.slice(0, literalFrom) + out.slice(literalFrom).replace(SEPARATORS_AT_END, "");
    else out += fieldText(fieldOf(fields, b), format);
    literalFrom = out.length;
  }
  return (out + rest).trimEnd();
}

// ---- a subject's text -------------------------------------------------------------------

export function subjectLabel(s) {
  const name = s.row.subject.label;
  const v = name ? s.state[name] : null;
  return v !== null && v !== undefined && String(v).trim() !== "" ? String(v) : s.key;
}

export function subjectSummary(s) {
  return fillSummary(s.row.event.summary, s.state);
}

export function subjectValues(s) {
  return s.row.subject.columns.map((c) => {
    const text = metricText(s.state[c.field], c.unit);
    return { label: c.label, text, title: text };
  });
}

// ---- the store --------------------------------------------------------------------------

export function mountSubjects(api, view) {
  const store = new SubjectStore(api, view);
  window.zetaSubjects = store;
  return store;
}

class SubjectStore {
  constructor(api, view) {
    this.api = api;
    this.view = view;
    this.rows = new Map(); // job kind -> row | null
    this.rowIds = null; // the mode-row ids (`modes_list`), once looked up
    this.lookingUp = false;
    // job id -> key -> subject. Each merge moves its subject to the end, so the last entry
    // is the last heard even when two events share a millisecond.
    this.jobs = new Map();
    this.sel = null;
    this.changeCbs = [];
    this.dropCbs = [];
    this.selectCbs = [];
    this.destroyed = false;
    this.timer = setInterval(() => this.tick(), TICK_MS);
  }

  async lookup(kinds) {
    const unknown = [...new Set(kinds)].filter((k) => !this.rows.has(k));
    if (this.lookingUp || this.destroyed || unknown.length === 0) return false;
    this.lookingUp = true;
    let learned = false;
    try {
      if (!this.rowIds) {
        const r = await this.api.op({ op: "modes_list", family: null, state: null });
        const modes = (r && r.value && r.value.modes) || [];
        this.rowIds = new Set(modes.map((m) => m.id));
      }
      for (const kind of unknown) {
        if (this.destroyed) return false;
        let row = null;
        if (this.rowIds.has(kind)) {
          const r = await this.api.op({ op: "modes_show", id: kind });
          row = (r && r.value && r.value.mode && r.value.mode.row) || null;
        }
        this.rows.set(kind, row);
        learned = true;
      }
    } catch {
      // Tried again on the next poll.
    } finally {
      this.lookingUp = false;
    }
    if (learned) this.changed();
    return learned;
  }

  rowOf(kind) {
    return this.rows.get(kind);
  }

  // The row and the key of a keyed event, or null.
  keyed(job, p) {
    const row = this.rows.get(job.kind);
    if (!row || !row.subject || job.owner) return null;
    if (!p || typeof p !== "object" || Array.isArray(p)) return null;
    const v = fieldOf(p, row.subject.key);
    if (v === null || v === undefined || String(v).trim() === "") return null;
    return { row, key: String(v) };
  }

  subjectOf(job, p) {
    const k = this.keyed(job, p);
    return k ? (this.jobs.get(job.id)?.get(k.key) ?? null) : null;
  }

  event(job, p) {
    if (this.destroyed) return null;
    const k = this.keyed(job, p);
    if (!k) return null;
    let subjects = this.jobs.get(job.id);
    if (!subjects) {
      subjects = new Map();
      this.jobs.set(job.id, subjects);
    }
    const now = Date.now();
    let s = subjects.get(k.key);
    if (s) subjects.delete(k.key);
    else {
      s = {
        job: job.id,
        key: k.key,
        state: Object.create(null),
        position: null,
        first_seen: now,
        last_seen: now,
        count: 0,
        dim: false,
        row: k.row,
      };
    }
    subjects.set(k.key, s);
    for (const [field, v] of Object.entries(p)) {
      if (v !== null && v !== undefined) s.state[field] = v;
    }
    const fix = positionOf(p);
    if (fix) {
      s.position = {
        lat: fix.lat,
        lon: fix.lon,
        alt_m: fix.alt_m ?? (s.position ? s.position.alt_m : null),
      };
    }
    s.last_seen = now;
    s.count += 1;
    s.dim = false;
    this.changed();
    return s;
  }

  sync(jobs) {
    const running = new Set((jobs || []).filter(isRunning).map((j) => j.id));
    let dropped = false;
    for (const [jobId, subjects] of [...this.jobs]) {
      if (running.has(jobId)) continue;
      for (const key of [...subjects.keys()]) this.drop(jobId, key);
      dropped = true;
    }
    if (dropped) this.changed();
  }

  list(jobId) {
    return [...(this.jobs.get(jobId)?.values() ?? [])];
  }

  lastHeard(jobId) {
    const all = this.list(jobId);
    return all.length > 0 ? all[all.length - 1] : null;
  }

  held(jobId, key) {
    const all = this.list(jobId);
    return all.some((s) => !s.dim && (key === null || key === undefined || s.key === key));
  }

  select(jobId, key) {
    const s = this.jobs.get(jobId)?.get(key);
    this.setSelected(s ? { job: jobId, key } : null);
  }

  selected() {
    return this.sel;
  }

  setSelected(sel) {
    this.sel = sel;
    for (const cb of this.selectCbs) cb(sel);
  }

  onChange(cb) {
    this.changeCbs.push(cb);
  }

  onDrop(cb) {
    this.dropCbs.push(cb);
  }

  onSelect(cb) {
    this.selectCbs.push(cb);
  }

  changed() {
    for (const cb of this.changeCbs) cb();
  }

  drop(jobId, key) {
    const subjects = this.jobs.get(jobId);
    if (!subjects || !subjects.delete(key)) return;
    if (subjects.size === 0) this.jobs.delete(jobId);
    if (this.sel && this.sel.job === jobId && this.sel.key === key) this.setSelected(null);
    for (const cb of this.dropCbs) cb(jobId, key);
  }

  tick() {
    const now = Date.now();
    const dimMs = this.view.get("subject_dim_s") * 1000;
    const dropMs = this.view.get("subject_drop_s") * 1000;
    let changed = false;
    for (const [jobId, subjects] of [...this.jobs]) {
      for (const s of [...subjects.values()]) {
        const age = now - s.last_seen;
        if (age >= dropMs) {
          this.drop(jobId, s.key);
          changed = true;
        } else if (age >= dimMs && !s.dim) {
          s.dim = true;
          changed = true;
        }
      }
    }
    if (changed) this.changed();
  }

  destroy() {
    this.destroyed = true;
    clearInterval(this.timer);
    this.jobs.clear();
    this.sel = null;
    this.changeCbs = [];
    this.dropCbs = [];
    this.selectCbs = [];
    if (window.zetaSubjects === this) window.zetaSubjects = null;
  }
}

// ---- the pane ---------------------------------------------------------------------------

const DEFAULT_SORT = { col: "last_seen", dir: -1 };

function compareValues(a, b) {
  if (typeof a === "number" && typeof b === "number") return a - b;
  return String(a).localeCompare(String(b));
}

function setText(el, text, title = null) {
  if (el.textContent !== text) el.textContent = text;
  if (title !== null && el.title !== title) el.title = title;
}

export function mountSubjectsPane(el, store, view, opts = {}) {
  return new SubjectsPane(el, store, view, opts);
}

class SubjectsPane {
  constructor(el, store, view, { jobs = () => [], watching = () => true, footprint = () => "wide" } = {}) {
    this.el = el;
    this.store = store;
    this.view = view;
    this.jobs = jobs;
    this.watching = watching;
    this.footprint = footprint;
    this.sections = new Map(); // job id -> {el, h3, note, table, head, body, sig, rows: key -> tr}
    this.sorts = new Map(); // job id -> {col, dir}
    this.pending = null; // the throttled redraw's timer
    this.lastRender = 0;
    this.destroyed = false;
    this.onClick = (ev) => this.click(ev);
    el.addEventListener("click", this.onClick);
    store.onChange(() => this.schedule());
    store.onSelect(() => this.render());
    view.onChange((key) => {
      if (key === "home" || key === "subjects") this.render();
    });
    this.timer = setInterval(() => this.render(), TICK_MS);
    this.render();
  }

  // The running jobs whose row names a subject (a job's trial never is one).
  subjectJobs() {
    return (this.jobs() || []).filter((j) => {
      const row = this.store.rowOf(j.kind);
      return isRunning(j) && !j.owner && row && row.subject;
    });
  }

  eligible() {
    return this.subjectJobs().some((j) => this.store.rowOf(j.kind).subject.columns.length > 0);
  }

  schedule() {
    if (this.pending !== null || this.destroyed) return;
    const wait = Math.max(0, this.lastRender + RENDER_MS - Date.now());
    this.pending = setTimeout(() => {
      this.pending = null;
      this.render();
    }, wait);
  }

  click(ev) {
    const th = ev.target.closest("th[data-col]");
    if (th) {
      const jobId = th.closest("section.subject-job").dataset.job;
      const col = th.dataset.col;
      const cur = this.sorts.get(jobId) || DEFAULT_SORT;
      this.sorts.set(jobId, { col, dir: cur.col === col ? -cur.dir : 1 });
      this.render();
      return;
    }
    const tr = ev.target.closest("tr.subject");
    if (tr) this.store.select(tr.dataset.job, tr.dataset.key);
  }

  // The table's columns for a job's row: {col, label, text(s, values), value(s)}.
  columns(row) {
    const sub = row.subject;
    const key = { col: sub.key, label: sub.key, text: (s) => s.key, value: (s) => s.key };
    const lastSeen = {
      col: "last_seen",
      label: "Last seen",
      text: (s) => `${Math.max(0, Math.round((Date.now() - s.last_seen) / 1000))} s`,
      value: (s) => s.last_seen,
    };
    const ofColumn = (c, i) => ({
      col: c.field,
      label: c.label,
      text: (s, values) => values[i].text,
      value: (s) => s.state[c.field] ?? null,
    });
    if (this.footprint() === "narrow") {
      const at = sub.columns.findIndex((c) => c.unit === "ft" || c.unit === "m");
      return [key, ...(at < 0 ? [] : [ofColumn(sub.columns[at], at)]), lastSeen];
    }
    const cols = [
      key,
      ...sub.columns.map(ofColumn),
      {
        col: "position",
        label: "Position",
        text: (s) => (s.position ? `${s.position.lat.toFixed(2)}, ${s.position.lon.toFixed(2)}` : ""),
        value: (s) => (s.position ? s.position.lat : null),
      },
    ];
    const home = homePoint(this.view.get("home"));
    if (home) {
      const from = (s) => (s.position ? distanceBearing(home, s.position) : null);
      cols.push({
        col: "distance",
        label: "Distance",
        text: (s) => {
          const d = from(s);
          return d ? `${d.km.toFixed(1)} km, ${Math.round(d.deg) % 360}°` : "";
        },
        value: (s) => from(s)?.km ?? null,
      });
    }
    cols.push(lastSeen, {
      col: "count",
      label: "Count",
      text: (s) => String(s.count),
      value: (s) => s.count,
    });
    return cols;
  }

  // Newest heard first, then by the job's sort; a tie keeps that order.
  sorted(jobId, cols) {
    const subjects = this.store.list(jobId).reverse();
    const wanted = this.sorts.get(jobId) || DEFAULT_SORT;
    const sort = cols.some((c) => c.col === wanted.col) ? wanted : DEFAULT_SORT;
    const col = cols.find((c) => c.col === sort.col);
    return subjects
      .map((s) => ({ s, v: col.value(s) }))
      .sort((a, b) => {
        const an = a.v === null || a.v === undefined;
        const bn = b.v === null || b.v === undefined;
        if (an || bn) return an - bn;
        return sort.dir * compareValues(a.v, b.v);
      })
      .map((x) => x.s);
  }

  section(jobId) {
    let sec = this.sections.get(jobId);
    if (sec) return sec;
    const el = document.createElement("section");
    el.className = "subject-job";
    el.dataset.job = jobId;
    const h3 = document.createElement("h3");
    const note = document.createElement("div");
    note.className = "not-watched";
    note.textContent = `not watched (limit ${MAX_WATCHED})`;
    const table = document.createElement("table");
    const thead = document.createElement("thead");
    const head = document.createElement("tr");
    thead.append(head);
    const body = document.createElement("tbody");
    table.append(thead, body);
    el.append(h3, table);
    sec = { el, h3, note, table, head, body, sig: "", rows: new Map() };
    this.sections.set(jobId, sec);
    return sec;
  }

  drawSection(sec, job, row, subjects, cols, watched) {
    setText(
      sec.h3,
      `${job.kind} ${job.source}, ${subjects.length} ${row.subject.name}`,
    );
    if (watched) sec.note.remove();
    else if (!sec.note.isConnected) sec.h3.after(sec.note);
    sec.table.hidden = subjects.length === 0;
    const sig = JSON.stringify(cols.map((c) => c.col));
    if (sig !== sec.sig) {
      sec.sig = sig;
      sec.head.replaceChildren(
        ...cols.map((c) => {
          const th = document.createElement("th");
          th.dataset.col = c.col;
          th.textContent = c.label;
          return th;
        }),
      );
      sec.body.replaceChildren();
      sec.rows.clear();
    }
    const keep = new Set(subjects.map((s) => s.key));
    for (const [key, tr] of [...sec.rows]) {
      if (keep.has(key)) continue;
      tr.remove();
      sec.rows.delete(key);
    }
    const sel = this.store.selected();
    subjects.forEach((s, i) => {
      let tr = sec.rows.get(s.key);
      if (!tr) {
        tr = document.createElement("tr");
        tr.className = "subject";
        tr.dataset.job = s.job;
        tr.dataset.key = s.key;
        for (const c of cols) {
          const td = document.createElement("td");
          td.dataset.col = c.col;
          tr.append(td);
        }
        sec.rows.set(s.key, tr);
      }
      tr.classList.toggle("dim", s.dim);
      tr.classList.toggle("selected", !!sel && sel.job === s.job && sel.key === s.key);
      const values = subjectValues(s);
      cols.forEach((c, n) => {
        const text = c.text(s, values);
        setText(tr.children[n], text, text);
      });
      if (sec.body.children[i] !== tr) sec.body.insertBefore(tr, sec.body.children[i] || null);
    });
  }

  render() {
    if (this.destroyed) return;
    this.lastRender = Date.now();
    const wanted = this.subjectJobs()
      .map((job) => ({ job, subjects: this.store.list(job.id), watched: this.watching(job.id) }))
      .filter((x) => x.subjects.length > 0 || !x.watched);
    const ids = new Set(wanted.map((x) => x.job.id));
    for (const [id, sec] of [...this.sections]) {
      if (ids.has(id)) continue;
      sec.el.remove();
      this.sections.delete(id);
      this.sorts.delete(id);
    }
    wanted.forEach(({ job, watched }, i) => {
      const row = this.store.rowOf(job.kind);
      const sec = this.section(job.id);
      const cols = this.columns(row);
      const subjects = this.sorted(job.id, cols);
      this.drawSection(sec, job, row, subjects, cols, watched);
      if (this.el.children[i] !== sec.el) this.el.insertBefore(sec.el, this.el.children[i] || null);
    });
  }

  destroy() {
    this.destroyed = true;
    clearInterval(this.timer);
    if (this.pending !== null) clearTimeout(this.pending);
    this.el.removeEventListener("click", this.onClick);
    this.el.replaceChildren();
    this.sections.clear();
  }
}
