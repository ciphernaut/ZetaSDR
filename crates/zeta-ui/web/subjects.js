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
//     store.rowOf(kind) -> row | null | undefined (undefined: not yet known); the row is
//         `modes_show`'s `mode.row` with the mode's `outputs` (the stream kinds its job emits)
//         beside its fields, so `rowOf(kind).outputs` answers whether the job plays audio
//     store.event(job, payload) -> subject | null   merges the event: non-null, non-blank fields
//         into `state`, the fix into `position`, `count` and `last_seen`. Null when the job's row has
//         no subject, the job has an `owner` (a watch's run or its trial) or is a running identify's
//         current trial (`context.identify.current`, as of the last `sync`), or the key is not a
//         string, number or boolean, or is blank: such an event is unkeyed. The key is trimmed.
//     store.subjectOf(job, payload) -> subject | null   the same test and lookup, no merge
//     Subjects are kept per group (plan 31 spec 4.7): a watch's kept decoders (agent.js groupOf,
//     as of the last `sync`) share their watch's, keyed (watch id, key); any other job is its own
//     group, keyed (job id, key). Every call below taking a `jobId` takes a group id or a job id
//     (resolved to its group).
//     store.groupOf(job, jobs = the last sync's) -> {key, kind, title} | null   agent.js groupOf
//     store.groupId(jobId) -> the group id of a job id (the watch's for a kept decoder, else
//         itself), as of the last `sync`
//     store.sync(jobs)   regroups (a decoder heard before its watch named it moves its subjects
//         under the watch, the newer of two on one key standing), drops the subjects of groups
//         whose job (the watch, for a merged group) is not `running`; notes the running
//         identifies' current trials
//     store.isTrial(jobId) -> bool   a running identify's current trial
//     store.list(jobId) -> subject[]   least recently heard first, a copy (never per message)
//     store.size(jobId) -> number
//     store.lastHeard(jobId) -> subject | null   the last subject that job itself was heard on
//     store.held(jobId, key | null) -> bool   the subject (with null, any of the group's) is not dim
//     store.select(jobId, key), store.selected() -> {job, group, key} | null (`job` the subject's)
//     store.onChange(cb)   any merge, dim or drop, and a row learned
//     store.onLearn(cb)   a row learned, after onChange's
//     store.onTick(cb)   each second, after dim and drop are checked
//     store.onDrop(cb(jobId, key)) (`jobId` the subject's `job`), store.onSelect(cb(sel)),
//     store.destroy()
//     A subject past view `subject_dim_s` of silence is dim, past `subject_drop_s` dropped
//     (checked each second, from this page's clock, so a longer `subject_dim_s` un-dims it at
//     the next check); a selection goes with its subject.
//   A subject: {job, group, heard, center_hz, key, state, position, first_seen, last_seen, count,
//     dim, row}. `job` is the id of the job first heard on, `group` its group's id, `heard` the
//     job last heard on and `center_hz` its tuning's centre then (when it had one), `state` a null-prototype object of every field heard, whose last non-null, non-blank value
//     stands, `position` {lat, lon, alt_m} or null, the altitude surviving a fix without one.
//   subjectLabel(subject) -> state[row.subject.label] when present, else the key
//   subjectSummary(subject) -> the row's summary template filled from the merged state
//   subjectValues(subject) -> [{label, text, title}]   the row's columns through metricText
//   fillSummary(template, fields) -> string   summary.rs's `fill`: `{f}`, `{f?}` (dropped with
//     the separators before it), `{a?b}`, each with an optional `:.N` or `:+.N`; trimmed at
//     both ends
//   mountSubjectsPane(el, store, view, {jobs, watching, footprint}) -> pane
//     A watch's kept decoders draw one `section.subject-job[data-job=<watch id>]` at the first
//     one's place (plan 31 spec 4.7), headed `<watch kind> <id tail> · <subject name>`, its
//     columns the key, `Channel` (`th[data-col="channel"]`: the centre, in MHz to 4 places, of
//     the decoder the subject was last heard on, its last known centre once that decoder is
//     gone), the first decoder row's columns (a subject's row when no decoder runs) and the
//     usual tail. It stays while its watch runs and holds subjects, every decoder lost or not,
//     and goes when the watch stops.
//     One `section.subject-job[data-job]` per other running job with subjects or whose row has table
//     columns, so a job heard nothing from yet says `0 aircraft` (an `h3` and a table of
//     `tr.subject[data-job][data-key]`, classes `dim` and `selected`), and one for a subject job
//     `watching(id)` denies, with `.not-watched`, after the watched ones. The default order is
//     last seen in SORT_STEP_MS steps, newest step first, then the key, so a busy table does not
//     churn. `th[data-col]` sorts exactly: a first click ascending, a second reversed, nulls last
//     either way, kept for as long as the job is listed. While the pointer is over a job's table
//     its rows keep their order (a header click sorts once, a new row goes last), resuming on
//     leave. A press (`pointerdown`) on a row selects it, so a row moving cannot lose a click, and
//     a selection (from here or the map) scrolls its row into view.
//     One table across rows (plan 30 spec 4.8), a view over the per-job subjects: when two or
//     more running jobs' rows declare `subject.merge` and share `subject.name`, one
//     `section.subject-merged[data-name]` (MERGE_SECTION_CLASS) stands at the first one's place
//     instead of their per-job sections, headed `<name>, N (<kind> <source>, ...)`. Its rows are
//     groups of subjects (`mergeGroups`): two join when a field in either row's `merge`, the key
//     included, has equal non-blank values in both; a subject whose state matches every pair of
//     its row's `subject.merge_skip` (`skipsMerge`: a VDL2 ground station) joins none and stands
//     alone, its key column showing its own key. A group's state is its members' merged,
//     newest last seen winning, its position the newest member's with one, `count` the sum,
//     `dim` when all are. Its key column (`data-col` the first `merge` field) shows the first
//     `merge` field the group has; then the union of the rows' columns, `Via` (the members'
//     protocols) and the usual tail. Its `tr.subject[data-job][data-key]` names the newest
//     member, the one a press selects; it is `selected` when any member is. A job alone in its
//     name, or whose row has no `merge`, keeps its own section.
//     Redrawn at most every RENDER_MS on a change, and each second; not at all while hidden (the
//     `hidden` attribute, or the narrow footprint's other tab), so whoever shows it calls render.
//     Cells are written only when they differ, so a tooltip outlives a redraw. All text by
//     textContent.
//     pane.render()   draws now, unless hidden
//     pane.eligible() -> bool   a running job's row has table columns, a trial's not counting,
//         or a running watch holds subjects (app.js shows the pane)
//     pane.groupIds() -> the ids of the groups the pane lists: the running subject jobs' groups
//         and the running watches holding subjects (app.js counts their subjects)
//     pane.destroy()

import { groupOf as watchGroupOf } from "./agent.js";
import { MAX_POSITIONED } from "./overlays.js";
import { distanceBearing, positionOf } from "./map.js";
import { metricText } from "./units.js";
import { homeOf } from "./view.js";

const TICK_MS = 1000;
const RENDER_MS = 500;
const SORT_STEP_MS = 10_000;
const MERGE_SECTION_CLASS = "subject-merged";

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
  return (out + rest).trim();
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
    // group id -> key -> subject. Each merge moves its subject to the end, so the last entry
    // is the last heard even when two events share a millisecond.
    this.jobs = new Map();
    this.groups = new Map(); // a kept decoder's id -> its watch's id, as of the last sync
    this.lastJobs = [];
    this.trials = new Set(); // the running identifies' current trials
    this.sel = null;
    this.changeCbs = [];
    this.learnCbs = [];
    this.dropCbs = [];
    this.selectCbs = [];
    this.tickCbs = [];
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
          const mode = r && r.value && r.value.mode;
          // `outputs` sits beside `row` on the mode; the jobs list's audio rule reads it.
          row = mode && mode.row ? { ...mode.row, outputs: mode.outputs ?? [] } : null;
        }
        this.rows.set(kind, row);
        learned = true;
      }
    } catch {
      // Tried again on the next poll.
    } finally {
      this.lookingUp = false;
    }
    if (learned) {
      this.changed();
      for (const cb of this.learnCbs) cb();
    }
    return learned;
  }

  rowOf(kind) {
    return this.rows.get(kind);
  }

  // The row and the key of a keyed event, or null.
  keyed(job, p) {
    const row = this.rows.get(job.kind);
    if (!row || !row.subject || job.owner || this.trials.has(job.id)) return null;
    if (!p || typeof p !== "object" || Array.isArray(p)) return null;
    const v = fieldOf(p, row.subject.key);
    if (!["string", "number", "boolean"].includes(typeof v)) return null;
    const key = String(v).trim();
    return key === "" ? null : { row, key };
  }

  groupOf(job, jobs = this.lastJobs) {
    return watchGroupOf(job, jobs);
  }

  groupId(jobId) {
    return this.groups.get(jobId) ?? jobId;
  }

  subjectOf(job, p) {
    const k = this.keyed(job, p);
    return k ? (this.jobs.get(this.groupId(job.id))?.get(k.key) ?? null) : null;
  }

  event(job, p) {
    if (this.destroyed) return null;
    const k = this.keyed(job, p);
    if (!k) return null;
    const group = this.groupId(job.id);
    let subjects = this.jobs.get(group);
    if (!subjects) {
      subjects = new Map();
      this.jobs.set(group, subjects);
    }
    const now = Date.now();
    let s = subjects.get(k.key);
    if (s) subjects.delete(k.key);
    else {
      s = {
        job: job.id,
        group,
        heard: job.id,
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
    s.heard = job.id;
    if (typeof job.tuning?.center_hz === "number") s.center_hz = job.tuning.center_hz;
    for (const [field, v] of Object.entries(p)) {
      if (v === null || v === undefined) continue;
      if (typeof v === "string" && v.trim() === "") continue;
      s.state[field] = v;
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
    const live = (jobs || []).filter(isRunning);
    const running = new Set(live.map((j) => j.id));
    this.trials = new Set(
      live
        .filter((j) => j.kind === "identify" && j.context?.identify?.current)
        .map((j) => j.context.identify.current),
    );
    this.lastJobs = jobs || [];
    this.groups = new Map();
    for (const j of live) {
      const g = watchGroupOf(j, live);
      if (g) this.groups.set(j.id, g.key);
    }
    let dropped = false;
    // A decoder heard before its watch named it: its subjects move under the watch.
    for (const [jobId, group] of this.groups) {
      const own = this.jobs.get(jobId);
      if (!own) continue;
      this.jobs.delete(jobId);
      let into = this.jobs.get(group);
      if (!into) {
        into = new Map();
        this.jobs.set(group, into);
      }
      for (const s of own.values()) {
        const other = into.get(s.key);
        if (other && other.last_seen > s.last_seen) {
          for (const cb of this.dropCbs) cb(s.job, s.key);
          continue;
        }
        if (other) for (const cb of this.dropCbs) cb(other.job, other.key);
        into.delete(s.key);
        s.group = group;
        into.set(s.key, s);
      }
      // The last heard stays last.
      const order = [...into.values()].sort((a, b) => a.last_seen - b.last_seen);
      into.clear();
      for (const s of order) into.set(s.key, s);
      dropped = true;
    }
    for (const [group, subjects] of [...this.jobs]) {
      if (running.has(group)) continue;
      for (const key of [...subjects.keys()]) this.drop(group, key);
      dropped = true;
    }
    if (dropped) this.changed();
  }

  list(jobId) {
    return [...(this.jobs.get(this.groupId(jobId))?.values() ?? [])];
  }

  size(jobId) {
    return this.jobs.get(this.groupId(jobId))?.size ?? 0;
  }

  isTrial(jobId) {
    return this.trials.has(jobId);
  }

  lastHeard(jobId) {
    const subjects = [...(this.jobs.get(this.groupId(jobId))?.values() ?? [])];
    for (let i = subjects.length - 1; i >= 0; i--) {
      if (subjects[i].heard === jobId) return subjects[i];
    }
    return null;
  }

  held(jobId, key) {
    const subjects = this.jobs.get(this.groupId(jobId));
    if (!subjects) return false;
    if (key !== null && key !== undefined) {
      const s = subjects.get(key);
      return !!s && !s.dim;
    }
    for (const s of subjects.values()) {
      if (!s.dim) return true;
    }
    return false;
  }

  select(jobId, key) {
    const group = this.groupId(jobId);
    const s = this.jobs.get(group)?.get(key);
    this.setSelected(s ? { job: s.job, group, key } : null);
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

  onLearn(cb) {
    this.learnCbs.push(cb);
  }

  onDrop(cb) {
    this.dropCbs.push(cb);
  }

  onSelect(cb) {
    this.selectCbs.push(cb);
  }

  onTick(cb) {
    this.tickCbs.push(cb);
  }

  changed() {
    for (const cb of this.changeCbs) cb();
  }

  drop(jobId, key) {
    const group = this.groupId(jobId);
    const subjects = this.jobs.get(group);
    const s = subjects?.get(key);
    if (!s) return;
    subjects.delete(key);
    if (subjects.size === 0) this.jobs.delete(group);
    if (this.sel && this.sel.group === group && this.sel.key === key) this.setSelected(null);
    for (const cb of this.dropCbs) cb(s.job, key);
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
        } else if (age >= dimMs !== s.dim) {
          s.dim = !s.dim;
          changed = true;
        }
      }
    }
    if (changed) this.changed();
    for (const cb of this.tickCbs) cb();
  }

  destroy() {
    this.destroyed = true;
    clearInterval(this.timer);
    this.jobs.clear();
    this.groups.clear();
    this.sel = null;
    this.changeCbs = [];
    this.learnCbs = [];
    this.dropCbs = [];
    this.selectCbs = [];
    this.tickCbs = [];
    if (window.zetaSubjects === this) window.zetaSubjects = null;
  }
}

// ---- the pane ---------------------------------------------------------------------------

function compareValues(a, b) {
  if (typeof a === "number" && typeof b === "number") return a - b;
  return String(a).localeCompare(String(b));
}

function setText(el, text, title = null) {
  if (el.textContent !== text) el.textContent = text;
  if (title !== null && el.title !== title) el.title = title;
}

// ---- one table across rows (spec 4.8) ---------------------------------------------------

const mergedId = (name) => `merged:${name}`;

// A drawn row's identity in its section: a group's own id, else the subject's key.
const idOf = (s) => s.id ?? s.key;

// A subject's value of `field` as text, the key counting as its key field; null when absent,
// blank or not a scalar.
function fieldValue(s, field) {
  const v = field === s.row.subject.key ? s.key : s.state[field];
  if (!["string", "number", "boolean"].includes(typeof v)) return null;
  const text = String(v).trim();
  return text === "" ? null : text;
}

// The subject takes no part in `merge`: its state matches every pair of its row's
// `subject.merge_skip` (an empty map never matches).
function skipsMerge(s) {
  const pairs = Object.entries(s.row.subject.merge_skip ?? {});
  return (
    pairs.length > 0 &&
    pairs.every(([f, v]) => {
      const have = s.state[f];
      return have !== null && have !== undefined && String(have) === String(v);
    })
  );
}

// The union of the rows' `merge` fields, in order.
function mergeFields(rows) {
  return [...new Set(rows.flatMap((row) => row.subject.merge ?? []))];
}

// The merged section's row: the union of the rows' columns by field, the first `merge` field's
// left out (the key column shows it), so `subjectValues` fills a group's cells.
function mergedRow(name, rows) {
  const merge = mergeFields(rows);
  const seen = new Set([merge[0]]);
  const columns = [];
  for (const c of rows.flatMap((row) => row.subject.columns)) {
    if (seen.has(c.field)) continue;
    seen.add(c.field);
    columns.push(c);
  }
  return { subject: { key: merge[0], name, columns, merge, label: null } };
}

// A group of subjects as one: shaped as a subject, naming its newest member.
function groupOf(members, fields, row) {
  members.sort((a, b) => a.last_seen - b.last_seen);
  const newest = members[members.length - 1];
  const state = Object.create(null);
  for (const m of members) Object.assign(state, m.state);
  const fixed = members.filter((m) => m.position);
  const newestFirst = [...members].reverse();
  let keyText = newest.key;
  for (const f of fields) {
    const v = newestFirst.map((m) => fieldValue(m, f)).find((x) => x !== null);
    if (v !== undefined) {
      keyText = v;
      break;
    }
  }
  if (members.length === 1 && skipsMerge(newest)) keyText = newest.key;
  return {
    id: members.map((m) => `${m.job}\u0001${m.key}`).sort()[0],
    members,
    job: newest.job,
    key: newest.key,
    keyText,
    state,
    position: fixed.length > 0 ? fixed[fixed.length - 1].position : null,
    first_seen: Math.min(...members.map((m) => m.first_seen)),
    last_seen: newest.last_seen,
    count: members.reduce((n, m) => n + m.count, 0),
    dim: members.every((m) => m.dim),
    via: [...new Set(members.map((m) => m.row.protocol))].sort().join(", "),
    row,
  };
}

// A drawn row holds the selection: the subject, or any member of a group.
function holds(s, sel) {
  return !!sel && (s.members ?? [s]).some((m) => m.job === sel.job && m.key === sel.key);
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
    // section id (a job id, or `merged:<name>`) -> {el, h3, note, table, head, body, sig,
    // rows: row id -> tr, hover, order: the row ids as last sorted}
    this.sections = new Map();
    this.sorts = new Map(); // section id -> {col, dir}
    this.pending = null; // the throttled redraw's timer
    this.lastRender = 0;
    this.destroyed = false;
    this.onClick = (ev) => this.click(ev);
    this.onPress = (ev) => this.press(ev);
    el.addEventListener("click", this.onClick);
    el.addEventListener("pointerdown", this.onPress);
    store.onChange(() => this.schedule());
    store.onSelect((sel) => {
      this.render();
      if (sel) this.el.querySelector("tr.subject.selected")?.scrollIntoView({ block: "nearest" });
    });
    view.onChange((key) => {
      if (key === "home" || key === "subjects") this.render();
    });
    this.timer = setInterval(() => this.render(), TICK_MS);
    this.render();
  }

  // The running jobs whose row names a subject (a job's trial, or a running identify's, never is
  // one).
  subjectJobs() {
    return (this.jobs() || []).filter((j) => {
      const row = this.store.rowOf(j.kind);
      return isRunning(j) && !j.owner && !this.store.isTrial(j.id) && row && row.subject;
    });
  }

  // The running watches holding subjects (a merged section stays while its watch runs, its
  // decoders lost or not: spec 4.7).
  heldWatches() {
    return (this.jobs() || []).filter(
      (j) => isRunning(j) && j.context?.watch && this.store.groupId(j.id) === j.id && this.store.size(j.id) > 0,
    );
  }

  eligible() {
    return (
      this.subjectJobs().some((j) => this.store.rowOf(j.kind).subject.columns.length > 0) ||
      this.heldWatches().length > 0
    );
  }

  groupIds() {
    return [
      ...new Set([
        ...this.subjectJobs().map((j) => this.store.groupId(j.id)),
        ...this.heldWatches().map((j) => j.id),
      ]),
    ];
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
    if (!th) return;
    const el = th.closest(`section.subject-job, section.${MERGE_SECTION_CLASS}`);
    if (!el) return;
    const id = el.classList.contains(MERGE_SECTION_CLASS) ? mergedId(el.dataset.name) : el.dataset.job;
    const col = th.dataset.col;
    const cur = this.sorts.get(id);
    this.sorts.set(id, { col, dir: cur && cur.col === col ? -cur.dir : 1 });
    // Asked for under the pointer: sorted once, then held as any other order.
    const sec = this.sections.get(id);
    if (sec) sec.order = null;
    this.render();
  }

  press(ev) {
    if (ev.button !== 0) return;
    const tr = ev.target.closest("tr.subject");
    if (tr) this.store.select(tr.dataset.job, tr.dataset.key);
  }

  // Not on screen: nothing to draw, whoever shows it renders.
  isHidden() {
    if (this.el.hidden) return true;
    return this.footprint() === "narrow" && document.documentElement.dataset.pane !== "subjects";
  }

  // The table's columns for a row: {col, label, text(s, values), value(s)}. `key` replaces the
  // key column and `extra` goes after the row's own columns (the merged section's).
  columns(row, { key = null, extra = [], afterKey = [] } = {}) {
    const sub = row.subject;
    key ??= { col: sub.key, label: sub.key, text: (s) => s.key, value: (s) => s.key };
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
      return [key, ...afterKey, ...(at < 0 ? [] : [ofColumn(sub.columns[at], at)]), lastSeen];
    }
    const cols = [
      key,
      ...afterKey,
      ...sub.columns.map(ofColumn),
      ...extra,
      {
        col: "position",
        label: "Position",
        text: (s) => (s.position ? `${s.position.lat.toFixed(2)}, ${s.position.lon.toFixed(2)}` : ""),
        value: (s) => (s.position ? s.position.lat : null),
      },
    ];
    const home = homeOf(this.view);
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

  // The merged section's columns for `row` (`mergedRow`): the key column showing the first
  // `merge` field a group has, the row's, then `Via`.
  mergedColumns(row) {
    const merge = row.subject.merge;
    const key = { col: merge[0], label: merge.join(" / "), text: (g) => g.keyText, value: (g) => g.keyText };
    const via = { col: "via", label: "Via", text: (g) => g.via, value: (g) => g.via };
    return this.columns(row, { key, extra: [via] });
  }

  // A watch's merged section (spec 4.7): its decoders' subjects keyed (watch, key), with the
  // `Channel` column after the key (the decoder the subject was last heard on).
  drawWatchSection(sec, { id, watch, members }) {
    const list = this.store.list(id);
    // No decoder running (every one lost): a subject's own row.
    const row = members.length > 0 ? this.store.rowOf(members[0].job.kind) : list[list.length - 1].row;
    const byId = new Map((this.jobs() || []).map((j) => [j.id, j]));
    const centre = (s) => {
      const live = byId.get(s.heard ?? s.job);
      const hz = isRunning(live) ? live.tuning?.center_hz : undefined;
      return typeof hz === "number" ? hz : (s.center_hz ?? null);
    };
    const channel = {
      col: "channel",
      label: "Channel",
      text: (s) => {
        const hz = centre(s);
        return typeof hz === "number" ? (hz / 1e6).toFixed(4) : "";
      },
      value: centre,
    };
    const cols = this.columns(row, { afterKey: [channel] });
    const subjects = this.held(sec, this.sorted(id, list.reverse(), cols));
    this.drawSection(
      sec,
      `${watch.kind} ${id.slice(-6)} · ${row.subject.name}`,
      subjects,
      cols,
      members.every((x) => x.watched),
    );
  }

  // The rows of the merged section: the subjects of `jobs` (rows with a `merge`, one
  // `subject.name`) joined by union-find, two subjects one group when a field in either row's
  // `merge` has equal non-blank values in both; each group as `groupOf`, drawn by `row`.
  mergeGroups(jobs, row) {
    const subjects = jobs.flatMap((j) => this.store.list(j.id));
    const fields = row.subject.merge;
    const parent = subjects.map((_, i) => i);
    const find = (i) => {
      while (parent[i] !== i) i = parent[i] = parent[parent[i]];
      return i;
    };
    const union = (a, b) => {
      parent[find(a)] = find(b);
    };
    // `field value` -> the first subject whose row merges on `field` and has `value`: every
    // later one with that value, merging on it or not, joins it.
    const owners = new Map();
    const at = (f, v) => `${f}\u0000${v}`;
    // A subject that `skipsMerge` neither owns a value nor joins one: a group of its own.
    const skip = subjects.map(skipsMerge);
    subjects.forEach((s, i) => {
      if (skip[i]) return;
      for (const f of s.row.subject.merge ?? []) {
        const v = fieldValue(s, f);
        if (v === null) continue;
        if (owners.has(at(f, v))) union(i, owners.get(at(f, v)));
        else owners.set(at(f, v), i);
      }
    });
    subjects.forEach((s, i) => {
      if (skip[i]) return;
      for (const f of fields) {
        const v = fieldValue(s, f);
        if (v !== null && owners.has(at(f, v))) union(i, owners.get(at(f, v)));
      }
    });
    const groups = new Map(); // root -> members
    subjects.forEach((s, i) => {
      const root = find(i);
      if (!groups.has(root)) groups.set(root, []);
      groups.get(root).push(s);
    });
    return [...groups.values()].map((members) => groupOf(members, fields, row));
  }

  // The default order, or the section's sort over `items` (newest heard first), a tie keeping
  // that order.
  sorted(id, items, cols) {
    const wanted = this.sorts.get(id);
    const col = wanted && cols.find((c) => c.col === wanted.col);
    if (!col) {
      const step = (s) => Math.floor(s.last_seen / SORT_STEP_MS);
      const name = (s) => s.keyText ?? s.key;
      return items.sort((a, b) => step(b) - step(a) || name(a).localeCompare(name(b)));
    }
    const sort = wanted;
    return items
      .map((s) => ({ s, v: col.value(s) }))
      .sort((a, b) => {
        const an = a.v === null || a.v === undefined;
        const bn = b.v === null || b.v === undefined;
        if (an || bn) return an - bn;
        return sort.dir * compareValues(a.v, b.v);
      })
      .map((x) => x.s);
  }

  // The sorted rows in the order the pointer is holding, a new row last; else as sorted, which
  // then is the order to hold.
  held(sec, subjects) {
    if (!sec.hover || !sec.order) {
      sec.order = subjects.map(idOf);
      return subjects;
    }
    const at = new Map(sec.order.map((id, i) => [id, i]));
    const place = (s) => at.get(idOf(s)) ?? at.size;
    const kept = subjects.sort((a, b) => place(a) - place(b));
    sec.order = kept.map(idOf);
    return kept;
  }

  // The section `id`, made on first use: a job's (`subject-job[data-job]`), or with `name` the
  // merged one (`subject-merged[data-name]`).
  section(id, name = null) {
    let sec = this.sections.get(id);
    if (sec) return sec;
    const el = document.createElement("section");
    if (name === null) {
      el.className = "subject-job";
      el.dataset.job = id;
    } else {
      el.className = MERGE_SECTION_CLASS;
      el.dataset.name = name;
    }
    const h3 = document.createElement("h3");
    const note = document.createElement("div");
    note.className = "not-watched";
    note.textContent = `not watched (limit ${MAX_POSITIONED})`;
    const table = document.createElement("table");
    const thead = document.createElement("thead");
    const head = document.createElement("tr");
    thead.append(head);
    const body = document.createElement("tbody");
    table.append(thead, body);
    el.append(h3, table);
    sec = { el, h3, note, table, head, body, sig: "", rows: new Map(), hover: false, order: null };
    table.addEventListener("pointerenter", () => {
      sec.hover = true;
    });
    table.addEventListener("pointerleave", () => {
      sec.hover = false;
      this.render();
    });
    this.sections.set(id, sec);
    return sec;
  }

  drawSection(sec, heading, subjects, cols, watched) {
    setText(sec.h3, heading);
    if (watched) sec.note.remove();
    else if (!sec.note.isConnected) sec.h3.after(sec.note);
    sec.table.hidden = subjects.length === 0;
    // A table hidden under the pointer sends no leave.
    if (sec.table.hidden) sec.hover = false;
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
    const keep = new Set(subjects.map(idOf));
    for (const [id, tr] of [...sec.rows]) {
      if (keep.has(id)) continue;
      tr.remove();
      sec.rows.delete(id);
    }
    const sel = this.store.selected();
    subjects.forEach((s, i) => {
      let tr = sec.rows.get(idOf(s));
      if (!tr) {
        tr = document.createElement("tr");
        tr.className = "subject";
        for (const c of cols) {
          const td = document.createElement("td");
          td.dataset.col = c.col;
          tr.append(td);
        }
        sec.rows.set(idOf(s), tr);
      }
      // A group's newest member can change under the same row.
      if (tr.dataset.job !== s.job) tr.dataset.job = s.job;
      if (tr.dataset.key !== s.key) tr.dataset.key = s.key;
      tr.classList.toggle("dim", s.dim);
      tr.classList.toggle("selected", holds(s, sel));
      const values = subjectValues(s);
      cols.forEach((c, n) => {
        const text = c.text(s, values);
        setText(tr.children[n], text, text);
      });
      if (sec.body.children[i] !== tr) sec.body.insertBefore(tr, sec.body.children[i] || null);
    });
  }

  drawJobSection(sec, { job, watched }) {
    const row = this.store.rowOf(job.kind);
    const cols = this.columns(row);
    const subjects = this.held(sec, this.sorted(job.id, this.store.list(job.id).reverse(), cols));
    this.drawSection(sec, `${job.kind} ${job.source}, ${subjects.length} ${row.subject.name}`, subjects, cols, watched);
  }

  drawMergedSection(sec, { id, name, members }) {
    const row = mergedRow(
      name,
      members.map((x) => this.store.rowOf(x.job.kind)),
    );
    const cols = this.mergedColumns(row);
    const groups = this.mergeGroups(
      members.map((x) => x.job),
      row,
    ).sort((a, b) => b.last_seen - a.last_seen);
    const subjects = this.held(sec, this.sorted(id, groups, cols));
    const via = members.map((x) => `${x.job.kind} ${x.job.source}`).join(", ");
    this.drawSection(sec, `${name}, ${subjects.length} (${via})`, subjects, cols, members.every((x) => x.watched));
  }

  // The sections to draw, in order: a job's own, or for the jobs whose rows declare `merge` and
  // share a `subject.name`, two or more of them, one merged section at the first one's place.
  // `wanted` is in the order of `subjectJobs()` save the not-watched last; a merged section's
  // members keep `subjectJobs()`'s order.
  parts(wanted, jobs) {
    const order = new Map(jobs.map((j, i) => [j.id, i]));
    // A watch's kept decoders (the watch listed): one section per watch, at the first one's
    // place, never in a merge across rows.
    const all = new Map((this.jobs() || []).map((j) => [j.id, j]));
    const watchOf = (x) => {
      const id = this.store.groupId(x.job.id);
      return id !== x.job.id && all.has(id) ? all.get(id) : null;
    };
    const byName = new Map(); // subject.name -> the merging jobs' entries
    for (const x of wanted) {
      if (watchOf(x)) continue;
      const sub = this.store.rowOf(x.job.kind).subject;
      if ((sub.merge ?? []).length === 0) continue;
      if (!byName.has(sub.name)) byName.set(sub.name, []);
      byName.get(sub.name).push(x);
    }
    const parts = [];
    const watches = new Map(); // watch id -> its part
    for (const x of wanted) {
      const watch = watchOf(x);
      if (watch) {
        if (!watches.has(watch.id)) {
          const part = { id: watch.id, watch, members: [], group: true };
          watches.set(watch.id, part);
          parts.push(part);
        }
        watches.get(watch.id).members.push(x);
        continue;
      }
      const name = this.store.rowOf(x.job.kind).subject.name;
      const members = byName.get(name);
      if (!members || members.length < 2 || !members.includes(x)) parts.push({ id: x.job.id, ...x });
      else if (members[0] === x) {
        members.sort((a, b) => order.get(a.job.id) - order.get(b.job.id));
        parts.push({ id: mergedId(name), name, members });
      }
    }
    // A running watch whose decoders are all gone keeps its section while it holds subjects.
    for (const watch of this.heldWatches()) {
      if (!watches.has(watch.id)) parts.push({ id: watch.id, watch, members: [], group: true });
    }
    return parts;
  }

  render() {
    if (this.destroyed || this.isHidden()) return;
    this.lastRender = Date.now();
    const jobs = this.subjectJobs();
    const wanted = jobs
      .map((job) => ({ job, watched: this.watching(job.id) }))
      .filter(
        (x) =>
          this.store.size(x.job.id) > 0 ||
          !x.watched ||
          this.store.rowOf(x.job.kind).subject.columns.length > 0,
      )
      .sort((a, b) => b.watched - a.watched);
    const parts = this.parts(wanted, jobs);
    const ids = new Set(parts.map((p) => p.id));
    for (const [id, sec] of [...this.sections]) {
      if (ids.has(id)) continue;
      sec.el.remove();
      this.sections.delete(id);
    }
    const listed = new Set([...(this.jobs() || []).map((j) => j.id), ...ids]);
    for (const id of [...this.sorts.keys()]) {
      if (!listed.has(id)) this.sorts.delete(id);
    }
    parts.forEach((part, i) => {
      const sec = this.section(part.id, part.name ?? null);
      if (part.group) this.drawWatchSection(sec, part);
      else if (part.members) this.drawMergedSection(sec, part);
      else this.drawJobSection(sec, part);
      if (this.el.children[i] !== sec.el) this.el.insertBefore(sec.el, this.el.children[i] || null);
    });
  }

  destroy() {
    this.destroyed = true;
    clearInterval(this.timer);
    if (this.pending !== null) clearTimeout(this.pending);
    this.el.removeEventListener("click", this.onClick);
    this.el.removeEventListener("pointerdown", this.onPress);
    this.el.replaceChildren();
    this.sections.clear();
  }
}
