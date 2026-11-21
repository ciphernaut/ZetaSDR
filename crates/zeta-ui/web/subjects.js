// subjects.js: what a decoder's events are about (plan 23 spec 4.2, 4.3). A mode row with a
// `subject` names the event field identifying the aircraft, station or sonde; this module
// merges each subject's events into one current state and lists them in the Subjects pane.
// Nothing is kept on the node: a refresh starts empty and refills from live traffic.
//
// Exports:
//   mountSubjects(api, view, selection = new Selection()) -> SubjectStore; sets
//       window.zetaSubjects (null on destroy). `selection` is the page's one selection
//       (selection.js, app.js's), which the store reads and writes for its subjects.
//   SubjectStore:
//     store.lookup(kinds) -> Promise<bool>   the mode rows of `kinds`: `modes_list` once for
//         the ids, one `modes_show` per kind, both cached. A kind that is no row caches null; a
//         failure stays unknown, for the next poll to try. True when anything was learned; one
//         lookup at a time (a call during another answers false, and whoever acts on the
//         other's true asks again).
//     store.rowOf(kind) -> row | null | undefined (undefined: not yet known); the row is
//         `modes_show`'s `mode.row` with the mode's `outputs` (the stream kinds its jobs emit,
//         the union over its ready handlers), `voices` (the roles of its voices' audio
//         streams, empty for one voice) and `handlers` (each with its own `outputs`) beside
//         its fields, so actions.js jobOutputs(job, rowOf(kind)) answers whether a job plays
//         audio
//     store.event(job, payload) -> subject | null   merges the event: non-null, non-blank fields
//         into `state` and, the envelope's aside (`protocol`, `handler`, `summary`, `t_ns`,
//         `position`, `access`), into `by` under the event's source key (sourceKey), the fix
//         into `position`, `count` and `last_seen`. Null when the job's row has
//         no subject, the job has an `owner` (a watch's run or its trial) or is a running identify's
//         current trial (`context.identify.current`, as of the last `sync`), or the key is not a
//         string, number or boolean, or is blank: such an event is unkeyed. The key is trimmed.
//     store.subjectOf(job, payload) -> subject | null   the same test and lookup, no merge
//     Subjects are kept per group (plan 31 spec 4.7): a watch's kept decoders (agent.js groupOf,
//     as of the last `sync`) share their watch's, keyed (watch id, key); siblings (plan 43 call
//     14: jobs whose records carry one `group`, the id of the job first sent from, a bare key
//     that may name a stopped job) share that group's; any other job is its own group, keyed
//     (job id, key). Every call below taking a `jobId` takes a group id or a job id (resolved
//     to its group).
//     store.groupOf(job, jobs = the last sync's) -> {key, kind, title} | null   agent.js groupOf
//     store.groupId(jobId) -> the group id of a job id (a replay's, before any other rule:
//         `replay:<capture id>` (REPLAY_GROUP), the first capture of the set its record
//         replays (`context.replay.captures[0]`, else its `replay`), so every replay of one
//         recording shares it whichever capture of the set it names, plan 44 call 19 and
//         ruling R17; else the watch's for a kept
//         decoder, else the record's `group`, else itself; a group sent from a kept decoder is
//         the watch's for as long as the watch runs, the decoder dropped or not),
//         as of the last `sync`. An event of a job no sync has seen yet goes by its record's
//         `replay`, else its `group`.
//     store.sync(jobs)   regroups (a decoder heard before its watch named it moves its subjects
//         under the watch, the newer of two on one key standing; a sibling heard before its
//         record showed its `group` moves its subjects into the group, two on one key becoming
//         one with what both said, the sibling's copy announced dropped), drops the subjects of
//         groups none of whose jobs runs (the job; a watch's, the watch; a sibling group's, any
//         member; never a replay's group, which stays until closed); notes the running
//         identifies' current trials
//     store.replayGroups() -> the replay groups holding subjects, in the order first heard
//     store.closeGroup(id)   drops every subject of group `id` (each announced to onDrop) and
//         its sources; the Subjects pane's `×` on a replay's section calls it
//     store.sources(jobId) -> [{key, mode, handler, name, slot}]   the group's sources in the
//         order they first supplied a field: `name` the handler's from the row's `handlers` (the
//         id when unknown), `slot` the theme's pin for the handler id (setHandlerSlots), else the
//         next free of 1 to 4, the fifth and later sharing 4
//     store.setHandlerSlots(map)   the theme's `handlers` (handler id -> slot 1..4); re-slots and
//         fires onChange
//     store.heardBy(jobId) -> number   the subjects of the job's group with a `by` entry for that
//         job's source key (its last event's, else its record's kind and `handler`)
//     store.isTrial(jobId) -> bool   a running identify's current trial
//     store.list(jobId) -> subject[]   least recently heard first, a copy (never per message)
//     store.size(jobId) -> number
//     store.lastHeard(jobId) -> subject | null   the last subject that job itself was heard on
//     store.held(jobId, key | null) -> bool   the subject (with null, any of the group's) is not dim
//     store.select(jobId, key)   the subject becomes the page's selection ({kind: "subject",
//         job, group, key}); a key the group does not hold clears the selection
//     store.selected() -> {job, group, key} | null (`job` the subject's): the selection when it
//         is a subject, null when it is none or a register site
//     store.setSelected({job, group, key} | null)   sets the selection to that subject, or none
//     store.selection   the shared Selection, for whoever draws a site selection
//     store.onChange(cb)   any merge, dim or drop, and a row learned
//     store.onLearn(cb)   a row learned, after onChange's
//     store.onTick(cb)   each second, after dim and drop are checked
//     store.onDrop(cb(jobId, key)) (`jobId` the subject's `job`),
//     store.onSelect(cb(sel))   each change of the selection, `sel` as selected() gives it
//     store.destroy()   its callbacks go; the selection itself is the page's and stays
//     A subject past view `subject_dim_s` of silence is dim, past `subject_drop_s` dropped
//     (checked each second, from this page's clock, so a longer `subject_dim_s` un-dims it at
//     the next check); a selection goes with its subject. A replay group's subjects take
//     their `first_seen`/`last_seen` (and each source field's time) from the events' `t_ns`
//     (ms; the recording's times) and neither dim nor drop.
//   A subject: {job, group, heard, center_hz, key, state, by, position, first_seen, last_seen,
//     count, dim, row}. `job` is the id of the job first heard on, `group` its group's id, `heard`
//     the job last heard on and `center_hz` its tuning's centre then (when it had one), `state` a
//     null-prototype object of every field heard, whose last non-null, non-blank value stands,
//     `by` Map<source key, {mode, handler, name, fields: {[field]: {v, t}}}> (what each source
//     said: each field's latest value and when, by this page's clock), `position` {lat, lon,
//     alt_m} or null, the altitude surviving a fix without one.
//   sourceKey(job, payload, row) -> "<mode>@<handler>"   the event's `handler`, else (an older
//     node) the row's own, the first of its `handlers`
//   marksOf(subject, slots) -> Map<field, {mark: null | 1..4 | "differs", title: string | null}>
//     how each field of `state` and `by` is drawn (plan 43 call 15); `slots` maps a source key
//     to its slot (store.sources'). With fewer than two sources in `by` every field is plain
//     (`{mark: null, title: null}`). A string field two or more sources hold different values
//     for (trimmed, case kept) is `differs`, titled `<name>: <value> · <name>: <value>` in slot
//     order; else a field every source holds is plain, one only one holds is that source's
//     slot titled `only <name>`, and one some hold is plain titled `from <name>, <name>`. The
//     envelope's fields and the key (each member's, in a merged group) are always plain.
//   subjectLabel(subject) -> state[row.subject.label] when present, else the key
//   subjectSummary(subject) -> the row's summary template filled from the merged state
//   subjectValues(subject, slots = null) -> [{label, text, title, mark, markTitle}]   the row's columns through
//       metricText; `mark`/`markTitle` from marksOf when `slots` (a Map of source key -> slot) is given, else null
//   fillSummary(template, fields) -> string   summary.rs's `fill`: `{f}`, `{f?}` (dropped with
//     the separators before it), `{a?b}`, each with an optional `:.N` or `:+.N`; trimmed at
//     both ends
//   matchesPairs(obj, pairs) -> bool   every field = value pair of `pairs` (a row's
//     `merge_skip`, `station.when`) holds on `obj`: present, non-null and
//     `String(have) === String(v)`; false for an empty or missing `pairs` (overlays.js tests
//     events with it, so the page has one comparison)
//   mountSubjectsPane(el, store, view, {jobs, watching, footprint, selection, siteChannels})
//       -> pane
//     A watch's kept decoders draw one `section.subject-job[data-job=<watch id>]` at the first
//     one's place (plan 31 spec 4.7), headed `<watch kind> <id tail> · <subject name>`, its
//     columns the key, `Channel` (`th[data-col="channel"]`: the centre, in MHz to 4 places, of
//     the decoder the subject was last heard on, its last known centre once that decoder is
//     gone), the first decoder row's columns (a subject's row when no decoder runs) and the
//     usual tail. It stays while its watch runs and holds subjects, every decoder lost or not,
//     and goes when the watch stops.
//     One `section.subject-job[data-job]` per other running job with subjects or whose row has table
//     columns (a sibling group's members one section, `data-job` the group's id, headed by its
//     first listed member), so a job heard nothing from yet says `0 aircraft` (an `h3` and a table of
//     `tr.subject[data-job][data-key]`, classes `dim`, `selected` and `station`: a fixed
//     station's own row, its row's `subject.station.key` non-null and its state matching
//     `station.when`, plan 35), and one for a subject job
//     `watching(id)` denies, with `.not-watched`, after the watched ones. The default order is
//     last seen in SORT_STEP_MS steps, newest step first, then the key, so a busy table does not
//     churn. `th[data-col]` sorts exactly: a first click ascending, a second reversed, nulls last
//     either way, kept for as long as the job is listed. While the pointer is over a job's table
//     its rows keep their order (a header click sorts once, a new row goes last), resuming on
//     leave. A press (`pointerdown`) on a row selects it, so a row moving cannot lose a click, and
//     a selection (from here or the map) scrolls its row into view.
//     A replay group (store.replayGroups) draws, after every live section, one
//     `section.subject-replay[data-capture=<the set's first capture id>]` (plan 44 call 19,
//     ruling R17), its subjects merged
//     and marked across handlers as a sibling group's are, one `table[data-mode]` per mode its
//     subjects were first heard from (in the order first heard; captioned `<mode>, <n> <subject
//     name>`, its columns that mode's row's, sorted apart), `Last seen` the recording's
//     `HH:MM:SS` (local time), headed `replay of <HH:MM:SS>
//     <centre> MHz` (the capture's `start_ns` and `center_hz` from one `capture_list` per new
//     capture, mhzText; `replay of <capture id>` while unknown or once the list no longer holds
//     it) in a `span.title` beside a `button.close` (`×`) that closes the group. A replay is
//     no subject job: it has no `subject-job` section and takes no part in a merge across rows.
//     A register site selected on the map (selection.js, `opts.selection`, else the store's)
//     draws, first in the pane, one `section.subject-site` headed `Site` with one
//     `tr.site-selected`: the site's name, its holders (`, `-joined) and what the register
//     holds there, `opts.siteChannels(sel)` -> `{channels: [...]}` (`1 channel`, `<n>
//     channels`), `{pending: true}` (`asking the register`) or null (`no channel list`: a
//     site without an id, or a lookup that failed), then a `button.site-clear` (aria-label
//     `Clear site selection`) that clears the selection: a site stays selected after the lit
//     set it was picked from fades (the operator selects on the map, then reads the
//     waterfall's marks), so nothing clears it but the operator. The pane is eligible while a
//     site is selected. Escape pressed while the pane has focus (it takes focus on a press)
//     clears the selection.
//     One table across rows (plan 30 spec 4.8), a view over the per-job subjects: when two or
//     more running jobs' rows declare `subject.merge` and share `subject.name`, one
//     `section.subject-merged[data-name]` (MERGE_SECTION_CLASS) stands at the first one's place
//     instead of their per-job sections, headed `<name>, N (<kind> <source>, ...)`. Its rows are
//     groups of subjects (`mergeGroups`): two join when a field in either row's `merge`, the key
//     included, has equal non-blank values in both; a subject whose state matches every pair of
//     its row's `subject.merge_skip` (`skipsMerge`: a VDL2 ground station) joins none and stands
//     alone, its key column showing its own key. A group's state is its members' merged,
//     newest last seen winning, its `by` the union of theirs, its position the newest member's
//     with one, `count` the sum, `dim` when all are. A sibling group is one job here, its
//     subjects members like any other's. Its key column (`data-col` the first `merge` field) shows the first
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
//         a running watch holds subjects, or a replay group does (app.js shows the pane)
//     pane.groupIds() -> the ids of the groups the pane lists: the running subject jobs' groups,
//         the running watches holding subjects and the replay groups (app.js counts their
//         subjects)
//     pane.destroy()

import { groupOf as watchGroupOf } from "./agent.js";
import { distanceBearing, positionOf } from "./map.js";
import { Selection, isSite } from "./selection.js";
import { metricText, mhzText } from "./units.js";
import { homeOf } from "./view.js";

const TICK_MS = 1000;
const RENDER_MS = 500;
const SORT_STEP_MS = 10_000;
// A failed `capture_list` (a replay section's title) is not asked again sooner than this.
const CAPTURE_RETRY_MS = 5_000;
const MERGE_SECTION_CLASS = "subject-merged";
const REPLAY_SECTION_CLASS = "subject-replay";
// A replay's group id is this and the id of the capture it names (plan 44 call 19).
export const REPLAY_GROUP = "replay:";

const isReplayGroup = (id) => typeof id === "string" && id.startsWith(REPLAY_GROUP);

// The capture a replay record's group is keyed on: the first of the set it replays
// (`context.replay.captures[0]`, whichever capture of the set it names: one recording, ruling
// R17), else the capture its `replay` names (a record with no set, as a job attached to a
// replay); null for a record that is no replay.
function replayCaptureOf(job) {
  if (typeof job?.replay !== "string" || job.replay === "") return null;
  const first = job.context?.replay?.captures?.[0];
  return typeof first === "string" && first !== "" ? first : job.replay;
}

// The group of a replay record (REPLAY_GROUP and replayCaptureOf's capture), else null.
function replayGroupOf(job) {
  const capture = replayCaptureOf(job);
  return capture ? REPLAY_GROUP + capture : null;
}

// The mode a subject was first heard from (its first source's), the key of its replay table.
function subjectMode(s) {
  return s.by?.values().next().value?.mode ?? "?";
}

// `HH:MM:SS` of `ms` (ms since the epoch) on this page's clock (local time).
function clockText(ms) {
  const d = new Date(ms);
  return [d.getHours(), d.getMinutes(), d.getSeconds()].map((n) => String(n).padStart(2, "0")).join(":");
}

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

// The row's columns as text. With `slots` (a Map of source key -> slot 1..4, complete for the
// subject's sources) each also says its `mark` (null, 1..4 or "differs") and `markTitle`
// (names the handler(s), never colour alone); without, both are null.
export function subjectValues(s, slots = null) {
  const marks = slots ? marksOf(s, slots) : null;
  return s.row.subject.columns.map((c) => {
    const text = metricText(s.state[c.field], c.unit);
    const m = marks?.get(c.field);
    return { label: c.label, text, title: text, mark: m?.mark ?? null, markTitle: m?.title ?? null };
  });
}

// A source list ([{key, slot}]) as the Map `marksOf` takes.
export function slotMap(sources) {
  return new Map((sources ?? []).map((x) => [x.key, x.slot]));
}

// ---- what each source said (plan 43 calls 13-16) ------------------------------------------

// The fields the node's envelope sets on every event (zeta-core's ENVELOPE_FIELDS and the
// handler file's extras): never a source's own say, never marked.
const ENVELOPE = new Set(["protocol", "handler", "summary", "t_ns", "position", "access"]);
const SLOTS = 4;

// The handler an event came from: its `handler`, else (an older node) the row's own, the
// first of the row's `handlers`.
function handlerOf(p, row) {
  const named = p && typeof p === "object" ? p.handler : null;
  if (typeof named === "string" && named !== "") return named;
  return row?.handlers?.[0]?.id ?? "builtin";
}

export function sourceKey(job, p, row) {
  return `${job.kind}@${handlerOf(p, row)}`;
}

// A handler's `name` from the row's `handlers`, its id when the row does not list it.
function handlerName(row, id) {
  return (row?.handlers ?? []).find((h) => h.id === id)?.name ?? id;
}

// A source's slot from `slots` (a Map, or an object), else its place in `s.by`.
function slotIn(slots, key, at) {
  const v = slots instanceof Map ? slots.get(key) : slots?.[key];
  return Number.isInteger(v) && v >= 1 && v <= SLOTS ? v : Math.min(at + 1, SLOTS);
}

const PLAIN = Object.freeze({ mark: null, title: null });

export function marksOf(s, slots) {
  const sources = [...(s.by ?? new Map()).entries()]
    .map(([key, src], i) => ({ key, src, slot: slotIn(slots, key, i), at: i }))
    .sort((a, b) => a.slot - b.slot || a.at - b.at);
  const fields = new Set(Object.keys(s.state ?? {}));
  for (const { src } of sources) for (const f of Object.keys(src.fields)) fields.add(f);
  // The key field: the subject's row's, and each member's in a merged group.
  const keys = new Set([s.row?.subject?.key, ...(s.members ?? []).map((m) => m.row?.subject?.key)]);
  const out = new Map();
  for (const f of fields) {
    if (sources.length < 2 || ENVELOPE.has(f) || keys.has(f)) {
      out.set(f, PLAIN);
      continue;
    }
    const holders = sources.filter(({ src }) => Object.hasOwn(src.fields, f));
    const texts = holders
      .filter(({ src }) => typeof src.fields[f].v === "string")
      .map((h) => ({ h, text: h.src.fields[f].v.trim() }));
    if (texts.length >= 2 && new Set(texts.map((x) => x.text)).size > 1) {
      const title = texts.map(({ h, text }) => `${h.src.name}: ${text}`).join(" · ");
      out.set(f, { mark: "differs", title });
    } else if (holders.length === sources.length || holders.length === 0) {
      out.set(f, PLAIN);
    } else if (holders.length === 1) {
      out.set(f, { mark: holders[0].slot, title: `only ${holders[0].src.name}` });
    } else {
      out.set(f, { mark: null, title: `from ${holders.map((h) => h.src.name).join(", ")}` });
    }
  }
  return out;
}

// `into` gains what `from` said: each source's fields, the newer `t` standing.
function mergeBy(into, from) {
  for (const [key, src] of from) {
    const have = into.get(key);
    if (!have) {
      into.set(key, { ...src, fields: Object.assign(Object.create(null), src.fields) });
      continue;
    }
    for (const [f, x] of Object.entries(src.fields)) {
      if (!Object.hasOwn(have.fields, f) || have.fields[f].t <= x.t) have.fields[f] = x;
    }
  }
}

// Two copies of one subject become `into`: its state the two merged (the later heard's
// values standing), every source's say, the later fix, the counts summed.
function foldSubject(into, from) {
  const later = from.last_seen > into.last_seen ? from : into;
  const earlier = later === from ? into : from;
  into.state = Object.assign(Object.create(null), earlier.state, later.state);
  mergeBy(into.by, from.by);
  into.position = later.position ?? earlier.position;
  if (later.center_hz !== undefined) into.center_hz = later.center_hz;
  into.heard = later.heard;
  into.first_seen = Math.min(into.first_seen, from.first_seen);
  into.last_seen = later.last_seen;
  into.count += from.count;
  into.dim = into.dim && from.dim;
}

// ---- the store --------------------------------------------------------------------------

export function mountSubjects(api, view, selection = new Selection()) {
  const store = new SubjectStore(api, view, selection);
  window.zetaSubjects = store;
  return store;
}

// The store's form of a selection: a subject's {job, group, key}, else null.
function subjectForm(sel) {
  return sel && sel.kind === "subject" ? { job: sel.job, group: sel.group, key: sel.key } : null;
}

class SubjectStore {
  constructor(api, view, selection) {
    this.api = api;
    this.view = view;
    this.selection = selection;
    this.rows = new Map(); // job kind -> row | null
    this.rowIds = null; // the mode-row ids (`modes_list`), once looked up
    this.lookingUp = false;
    // group id -> key -> subject. Each merge moves its subject to the end, so the last entry
    // is the last heard even when two events share a millisecond.
    this.jobs = new Map();
    this.groups = new Map(); // a kept decoder's id -> its watch's id, as of the last sync
    this.siblings = new Map(); // a job's id -> its record's `group`, as of the last sync
    // A sibling group's key that was a watch's kept decoder -> that watch, kept while the
    // watch runs (the decoder dropped or not), so the group stays the watch's
    this.keptGroups = new Map();
    // group id -> source key -> {key, mode, handler}, in the order the sources first
    // supplied a field to one of the group's subjects
    this.sourceOrder = new Map();
    this.jobSources = new Map(); // job id -> the source key of its last keyed event
    // A replay's id -> the capture id it names: its group is REPLAY_GROUP + that, kept after
    // the job is gone, as its group is (plan 44)
    this.replays = new Map();
    this.handlerSlots = {}; // the theme's `handlers`: handler id -> slot
    this.lastJobs = [];
    this.trials = new Set(); // the running identifies' current trials
    this.changeCbs = [];
    this.learnCbs = [];
    this.dropCbs = [];
    this.selectCbs = [];
    this.tickCbs = [];
    this.destroyed = false;
    selection.onChange((sel) => {
      if (this.destroyed) return;
      const form = subjectForm(sel);
      for (const cb of this.selectCbs) cb(form);
    });
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
          // `outputs`, `voices` and `handlers` sit beside `row` on the mode; the jobs list's
          // audio rule, its stream picker and its handler tag read them.
          row =
            mode && mode.row
              ? {
                  ...mode.row,
                  outputs: mode.outputs ?? [],
                  voices: mode.voices ?? [],
                  handlers: mode.handlers ?? [],
                }
              : null;
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

  // The watch's group, else the record's `group` (a bare key: it may name a stopped job,
  // never resolved to a record), else the job itself; a group sent from a watch's kept
  // decoder is the watch's while the watch runs.
  groupId(jobId) {
    const capture = this.replays.get(jobId);
    if (capture !== undefined) return REPLAY_GROUP + capture;
    const g = this.groups.get(jobId) ?? this.siblings.get(jobId) ?? jobId;
    return this.groups.get(g) ?? this.keptGroups.get(g) ?? g;
  }

  // `groupId` for a job record, its own `group` standing in until a sync has seen it (a
  // sibling's first events can come before the next poll).
  groupFor(job) {
    const replay = replayGroupOf(job);
    if (replay) {
      this.replays.set(job.id, replayCaptureOf(job));
      return replay;
    }
    if (this.groups.has(job.id) || this.siblings.has(job.id)) return this.groupId(job.id);
    const g = typeof job.group === "string" && job.group !== "" ? job.group : job.id;
    return this.groups.get(g) ?? this.keptGroups.get(g) ?? g;
  }

  subjectOf(job, p) {
    const k = this.keyed(job, p);
    return k ? (this.jobs.get(this.groupFor(job))?.get(k.key) ?? null) : null;
  }

  // The group's sources, `order` (`sourceOrder`'s) noting `key` when new.
  noteSource(group, key, src) {
    let order = this.sourceOrder.get(group);
    if (!order) {
      order = new Map();
      this.sourceOrder.set(group, order);
    }
    if (!order.has(key)) order.set(key, { key, mode: src.mode, handler: src.handler });
  }

  sources(jobId) {
    const order = [...(this.sourceOrder.get(this.groupId(jobId))?.values() ?? [])];
    const pinned = (h) => {
      const v = Object.hasOwn(this.handlerSlots, h) ? this.handlerSlots[h] : null;
      return Number.isInteger(v) && v >= 1 && v <= SLOTS ? v : null;
    };
    const out = order.map((o) => ({
      key: o.key,
      mode: o.mode,
      handler: o.handler,
      name: handlerName(this.rows.get(o.mode), o.handler),
      slot: null,
    }));
    // A pinned slot goes to the first source in order pinned to it; a later one pinned
    // to the same slot takes the next free slot, as an unpinned source does.
    const taken = new Set();
    for (const o of out) {
      const pin = pinned(o.handler);
      if (pin === null || taken.has(pin)) continue;
      o.slot = pin;
      taken.add(pin);
    }
    for (const o of out) {
      if (o.slot !== null) continue;
      let n = 1;
      while (n < SLOTS && taken.has(n)) n++;
      o.slot = n;
      taken.add(n);
    }
    return out;
  }

  setHandlerSlots(map) {
    this.handlerSlots = map && typeof map === "object" ? { ...map } : {};
    this.changed();
  }

  heardBy(jobId) {
    let key = this.jobSources.get(jobId);
    if (!key) {
      const job = this.lastJobs.find((j) => j.id === jobId);
      const row = job && this.rows.get(job.kind);
      if (!row) return 0;
      key = `${job.kind}@${job.handler ?? handlerOf(null, row)}`;
    }
    let n = 0;
    for (const s of this.jobs.get(this.groupId(jobId))?.values() ?? []) if (s.by.has(key)) n++;
    return n;
  }

  event(job, p) {
    if (this.destroyed) return null;
    const k = this.keyed(job, p);
    if (!k) return null;
    const group = this.groupFor(job);
    let subjects = this.jobs.get(group);
    if (!subjects) {
      subjects = new Map();
      this.jobs.set(group, subjects);
    }
    // A replay's times are the recording's (the event's `t_ns`), not this page's clock.
    const now = isReplayGroup(group) && Number.isFinite(p.t_ns) ? p.t_ns / 1e6 : Date.now();
    let s = subjects.get(k.key);
    if (s) subjects.delete(k.key);
    else {
      s = {
        job: job.id,
        group,
        heard: job.id,
        key: k.key,
        state: Object.create(null),
        by: new Map(),
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
    const key = sourceKey(job, p, k.row);
    this.jobSources.set(job.id, key);
    let src = s.by.get(key);
    if (!src) {
      const handler = handlerOf(p, k.row);
      src = { mode: job.kind, handler, name: handlerName(k.row, handler), fields: Object.create(null) };
      s.by.set(key, src);
    }
    this.noteSource(group, key, src);
    for (const [field, v] of Object.entries(p)) {
      if (v === null || v === undefined) continue;
      if (typeof v === "string" && v.trim() === "") continue;
      s.state[field] = v;
      if (!ENVELOPE.has(field)) src.fields[field] = { v, t: now };
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
    for (const j of this.lastJobs) {
      const capture = replayCaptureOf(j);
      if (capture) this.replays.set(j.id, capture);
    }
    this.groups = new Map();
    for (const j of live) {
      const g = watchGroupOf(j, live);
      if (g) this.groups.set(j.id, g.key);
    }
    this.siblings = new Map();
    for (const j of this.lastJobs) {
      if (typeof j.group === "string" && j.group !== "") this.siblings.set(j.id, j.group);
    }
    // A sibling group's key kept by a watch now, remembered; one whose watch stopped, forgotten.
    // Only group keys: a lone decoder the watch drops is its own group again, as before.
    const groupKeys = new Set(this.siblings.values());
    for (const [decoder, watch] of this.groups) {
      if (groupKeys.has(decoder)) this.keptGroups.set(decoder, watch);
    }
    for (const [key, watch] of [...this.keptGroups]) {
      if (!running.has(watch)) this.keptGroups.delete(key);
    }
    const known = new Set(this.lastJobs.map((j) => j.id));
    for (const id of [...this.jobSources.keys()]) if (!known.has(id)) this.jobSources.delete(id);
    let dropped = false;
    // A decoder heard before its watch named it, or a sibling before its record showed its
    // `group`: its subjects move under the group.
    for (const jobId of new Set([...this.groups.keys(), ...this.siblings.keys()])) {
      const group = this.groupId(jobId);
      const own = this.jobs.get(jobId);
      if (group === jobId || !own) continue;
      this.moveInto(jobId, own, group, !this.groups.has(jobId));
      dropped = true;
    }
    // A group lives while its job runs, or (a watch's, a sibling group's) any member does; a
    // replay's until it is closed (store.closeGroup).
    const alive = new Set([...running, ...live.map((j) => this.groupId(j.id))]);
    for (const [group, subjects] of [...this.jobs]) {
      if (alive.has(group) || isReplayGroup(group)) continue;
      for (const key of [...subjects.keys()]) this.drop(group, key);
      dropped = true;
    }
    for (const group of [...this.sourceOrder.keys()]) {
      if (!alive.has(group) && !isReplayGroup(group)) this.sourceOrder.delete(group);
    }
    if (dropped) this.changed();
  }

  // The subjects `own` heard under `jobId` move into `group`. A key the group already holds:
  // for a sibling (`fold`) the two become one, the group's standing with what both said; for
  // a watch's decoder the newer stands. The one that goes is announced dropped.
  moveInto(jobId, own, group, fold) {
    this.jobs.delete(jobId);
    let into = this.jobs.get(group);
    if (!into) {
      into = new Map();
      this.jobs.set(group, into);
    }
    for (const src of this.sourceOrder.get(jobId)?.values() ?? []) this.noteSource(group, src.key, src);
    this.sourceOrder.delete(jobId);
    for (const s of own.values()) {
      const other = into.get(s.key);
      if (other && fold) {
        foldSubject(other, s);
        const sel = this.selected();
        if (sel && sel.job === s.job && sel.key === s.key) {
          this.setSelected({ job: other.job, group, key: s.key });
        }
        for (const cb of this.dropCbs) cb(s.job, s.key);
        continue;
      }
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
    return subjectForm(this.selection.get());
  }

  setSelected(sel) {
    this.selection.set(sel ? { kind: "subject", job: sel.job, group: sel.group, key: sel.key } : null);
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

  // The replays' groups holding subjects (REPLAY_GROUP ids), in the order first heard.
  replayGroups() {
    return [...this.jobs.keys()].filter(isReplayGroup);
  }

  // Every subject of group `id` dropped (announced as any drop) and its sources forgotten.
  closeGroup(id) {
    const subjects = this.jobs.get(id);
    for (const key of [...(subjects?.keys() ?? [])]) this.drop(id, key);
    this.sourceOrder.delete(id);
    this.changed();
  }

  drop(jobId, key) {
    const group = this.groupId(jobId);
    const subjects = this.jobs.get(group);
    const s = subjects?.get(key);
    if (!s) return;
    subjects.delete(key);
    if (subjects.size === 0) this.jobs.delete(group);
    const sel = this.selected();
    if (sel && sel.group === group && sel.key === key) this.setSelected(null);
    for (const cb of this.dropCbs) cb(s.job, key);
  }

  tick() {
    const now = Date.now();
    const dimMs = this.view.get("subject_dim_s") * 1000;
    const dropMs = this.view.get("subject_drop_s") * 1000;
    let changed = false;
    for (const [jobId, subjects] of [...this.jobs]) {
      // A replay's times are the recording's: its rows neither dim nor drop on this clock.
      if (isReplayGroup(jobId)) continue;
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
    this.siblings.clear();
    this.keptGroups.clear();
    this.sourceOrder.clear();
    this.jobSources.clear();
    this.replays.clear();
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

// `el` wears the class of `mark` (null, 1..4 or "differs") and none of the others.
export function setMark(el, mark) {
  for (let n = 1; n <= SLOTS; n++) el.classList.toggle(`mark-${n}`, mark === n);
  el.classList.toggle("mark-differs", mark === "differs");
}

// A section's sources for its legend and marks, from the store's `sources(jobId)`: `label` the
// chip's text, `title` what it says on hover.
function legendSources(list, labelOf) {
  return list.map((x) => ({ key: x.key, slot: x.slot, label: labelOf(x), title: `${x.mode} by ${x.name}` }));
}

// Several groups' sources as one list (the merged section's): a source once, its slot kept
// unless another took it, then the next free (the fifth and later sharing 4).
function unionSources(lists) {
  const out = [];
  const taken = new Set();
  for (const x of lists.flat()) {
    if (out.some((o) => o.key === x.key)) continue;
    let slot = x.slot;
    if (taken.has(slot)) {
      slot = 1;
      while (slot < SLOTS && taken.has(slot)) slot++;
    }
    taken.add(slot);
    out.push({ ...x, slot });
  }
  return out;
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

// `obj` holds every pair of `pairs` (a row's `merge_skip` or `station.when`, field = value):
// each field present, non-null and equal as text (`String(have) === String(v)`, so a number
// or boolean pair matches its printed form). An empty or missing map never matches.
export function matchesPairs(obj, pairs) {
  const entries = Object.entries(pairs ?? {});
  return (
    !!obj &&
    entries.length > 0 &&
    entries.every(([f, v]) => {
      const have = obj[f];
      return have !== null && have !== undefined && String(have) === String(v);
    })
  );
}

// The subject takes no part in `merge`: its state matches every pair of its row's
// `subject.merge_skip` (an empty map never matches).
function skipsMerge(s) {
  return matchesPairs(s.state, s.row.subject.merge_skip);
}

// A fixed station's own row (plan 35 spec 4.5): its row names stations by a key
// (`subject.station.key` non-null) and its state matches `station.when`. A group is one when
// every member is.
function isStation(s) {
  return (s.members ?? [s]).every((m) => {
    const st = m.row?.subject?.station;
    return !!st && st.key !== null && st.key !== undefined && matchesPairs(m.state, st.when);
  });
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
  // Every member's sources (plan 43 call 29), so marksOf reads a group as a subject.
  const by = new Map();
  for (const m of members) mergeBy(by, m.by ?? new Map());
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
    by,
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

// What the site row says the register holds there.
function siteChannelsText(answer) {
  if (answer && answer.pending) return "asking the register";
  if (!answer || !Array.isArray(answer.channels)) return "no channel list";
  const n = answer.channels.length;
  return n === 1 ? "1 channel" : `${n} channels`;
}

class SubjectsPane {
  constructor(
    el,
    store,
    view,
    {
      jobs = () => [],
      watching = () => true,
      footprint = () => "wide",
      selection = store.selection,
      siteChannels = () => null,
    } = {},
  ) {
    this.el = el;
    this.store = store;
    this.view = view;
    this.jobs = jobs;
    this.watching = watching;
    this.footprint = footprint;
    this.selection = selection;
    this.siteChannels = siteChannels;
    this.site = null; // the selected site's section: {el, cells}, while one is selected
    // section id (a job id, or `merged:<name>`) -> {el, h3, note, table, head, body, sig,
    // rows: row id -> tr, hover, order: the row ids as last sorted}
    this.sections = new Map();
    this.sorts = new Map(); // section id -> {col, dir}
    this.pending = null; // the throttled redraw's timer
    this.lastRender = 0;
    // capture id -> its `capture_list` record, or null when the list no longer holds it: what
    // titles a replay's section, asked once per capture (again after a failed list)
    this.captures = new Map();
    this.capturesAsked = null; // the `capture_list` in flight, or null
    this.capturesFailed = -Infinity; // Date.now() of the last failed one
    this.destroyed = false;
    this.onClick = (ev) => this.click(ev);
    this.onPress = (ev) => this.press(ev);
    this.onKey = (ev) => {
      if (ev.key === "Escape" && this.selection.get()) this.selection.set(null);
    };
    el.addEventListener("click", this.onClick);
    el.addEventListener("pointerdown", this.onPress);
    el.addEventListener("keydown", this.onKey);
    // Focusable without a tab stop, so a press in it takes the focus Escape needs.
    if (!el.hasAttribute("tabindex")) el.tabIndex = -1;
    store.onChange(() => this.schedule());
    selection.onChange((sel) => {
      this.render();
      if (!sel) return;
      const row = sel.kind === "site" ? "tr.site-selected" : "tr.subject.selected";
      this.el.querySelector(row)?.scrollIntoView({ block: "nearest" });
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
      // A replay's subjects are its capture's group (drawReplaySection), never a live view's.
      return isRunning(j) && !j.owner && !j.replay && !this.store.isTrial(j.id) && row && row.subject;
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
      this.heldWatches().length > 0 ||
      this.store.replayGroups().length > 0 ||
      isSite(this.selection.get())
    );
  }

  groupIds() {
    return [
      ...new Set([
        ...this.subjectJobs().map((j) => this.store.groupId(j.id)),
        ...this.heldWatches().map((j) => j.id),
        ...this.store.replayGroups(),
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
    if (ev.target.closest("button.site-clear")) {
      this.selection.set(null);
      return;
    }
    const close = ev.target.closest(`section.${REPLAY_SECTION_CLASS} button.close`);
    if (close) {
      this.store.closeGroup(REPLAY_GROUP + close.closest("section").dataset.capture);
      this.render();
      return;
    }
    const th = ev.target.closest("th[data-col]");
    if (!th) return;
    const el = th.closest(`section.subject-job, section.${MERGE_SECTION_CLASS}, section.${REPLAY_SECTION_CLASS}`);
    if (!el) return;
    const replay = el.classList.contains(REPLAY_SECTION_CLASS);
    const mode = replay ? th.closest("table")?.dataset.mode : null;
    const secId = el.classList.contains(MERGE_SECTION_CLASS)
      ? mergedId(el.dataset.name)
      : replay
        ? REPLAY_GROUP + el.dataset.capture
        : el.dataset.job;
    // A replay section's tables sort apart, each by `<group id>\0<mode>`.
    const id = replay ? `${secId}\u0000${mode}` : secId;
    const col = th.dataset.col;
    const cur = this.sorts.get(id);
    this.sorts.set(id, { col, dir: cur && cur.col === col ? -cur.dir : 1 });
    // Asked for under the pointer: sorted once, then held as any other order.
    const sec = this.sections.get(secId);
    const t = replay ? sec?.tables?.get(mode) : sec;
    if (t) t.order = null;
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
  // A replay's (`recorded`) Last seen is the recording's clock time, not an age.
  columns(row, { key = null, extra = [], afterKey = [], recorded = false } = {}) {
    const sub = row.subject;
    key ??= { col: sub.key, label: sub.key, text: (s) => s.key, value: (s) => s.key };
    const lastSeen = {
      col: "last_seen",
      label: "Last seen",
      text: recorded
        ? (s) => clockText(s.last_seen)
        : (s) => `${Math.max(0, Math.round((Date.now() - s.last_seen) / 1000))} s`,
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
      legendSources(this.store.sources(id), (x) => x.name),
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

  // The section `id`, made on first use: a job's (`subject-job[data-job]`), with `name` the
  // merged one (`subject-merged[data-name]`), with `capture` a replay group's
  // (`subject-replay[data-capture]`, its heading's text in a span beside a `button.close`).
  section(id, name = null, capture = null) {
    let sec = this.sections.get(id);
    if (sec) return sec;
    const el = document.createElement("section");
    if (capture !== null) {
      el.className = REPLAY_SECTION_CLASS;
      el.dataset.capture = capture;
    } else if (name === null) {
      el.className = "subject-job";
      el.dataset.job = id;
    } else {
      el.className = MERGE_SECTION_CLASS;
      el.dataset.name = name;
    }
    const h3 = document.createElement("h3");
    let title = null;
    if (capture !== null) {
      title = document.createElement("span");
      title.className = "title";
      const close = document.createElement("button");
      close.type = "button";
      close.className = "close";
      close.setAttribute("aria-label", "Close this replay's subjects");
      close.title = "Close this replay's subjects";
      close.textContent = "×";
      h3.append(title, close);
    }
    const note = document.createElement("div");
    note.className = "not-watched";
    note.textContent = "not watched (its source has no event feed)";
    if (capture !== null) {
      // A replay's subjects may be of several modes: a table per mode (drawReplaySection).
      const tablesEl = document.createElement("div");
      tablesEl.className = "replay-tables";
      el.append(h3, tablesEl);
      sec = { el, h3, title, note, tablesEl, tables: new Map() };
    } else {
      const t = this.makeTable();
      el.append(h3, t.table);
      sec = { el, h3, title, note, ...t };
      // The table's hover is the section's own (its fields are the section's).
      t.table.addEventListener("pointerenter", () => {
        sec.hover = true;
      });
      t.table.addEventListener("pointerleave", () => {
        sec.hover = false;
        this.render();
      });
    }
    this.sections.set(id, sec);
    return sec;
  }

  // A subjects table with no rows: {table, head, body, sig, rows, hover, order}, the shape
  // drawTable and held take; whoever makes it wires its hover.
  makeTable() {
    const table = document.createElement("table");
    const thead = document.createElement("thead");
    const head = document.createElement("tr");
    thead.append(head);
    const body = document.createElement("tbody");
    table.append(thead, body);
    return { table, head, body, sig: "", rows: new Map(), hover: false, order: null };
  }

  // The legend under the heading: a chip per source in its mark, then `differs`; only with two
  // or more `sources` ([{key, slot, label, title}]).
  drawLegend(sec, sources) {
    if (sources.length < 2) {
      sec.legend?.remove();
      sec.legendSig = "";
      return;
    }
    if (!sec.legend) {
      sec.legend = document.createElement("p");
      sec.legend.className = "subject-legend";
    }
    const sig = JSON.stringify(sources.map((x) => [x.key, x.slot, x.label, x.title]));
    if (sig !== sec.legendSig) {
      sec.legendSig = sig;
      const chips = sources.map((x) => {
        const chip = document.createElement("span");
        chip.className = `chip mark-${x.slot}`;
        chip.dataset.source = x.key;
        chip.textContent = x.label;
        chip.title = x.title;
        return chip;
      });
      const differs = document.createElement("span");
      differs.className = "chip mark-differs";
      differs.textContent = "differs";
      differs.title = "the handlers gave different values";
      sec.legend.replaceChildren(...chips, differs);
    }
    if (sec.legend.previousElementSibling !== sec.h3) sec.h3.after(sec.legend);
  }

  // The section's heading, its not-watched note and its legend.
  drawHead(sec, heading, watched, sources) {
    setText(sec.title ?? sec.h3, heading);
    if (watched) sec.note.remove();
    else if (!sec.note.isConnected) sec.h3.after(sec.note);
    this.drawLegend(sec, sources);
  }

  drawSection(sec, heading, subjects, cols, watched, sources = []) {
    this.drawHead(sec, heading, watched, sources);
    this.drawTable(sec, subjects, cols, sources);
  }

  // The rows of table `t` (makeTable's shape): `subjects` under `cols`, marked by `sources`.
  drawTable(t, subjects, cols, sources) {
    const slots = slotMap(sources);
    t.table.hidden = subjects.length === 0;
    // A table hidden under the pointer sends no leave.
    if (t.table.hidden) t.hover = false;
    const sig = JSON.stringify(cols.map((c) => c.col));
    if (sig !== t.sig) {
      t.sig = sig;
      t.head.replaceChildren(
        ...cols.map((c) => {
          const th = document.createElement("th");
          th.dataset.col = c.col;
          th.textContent = c.label;
          return th;
        }),
      );
      t.body.replaceChildren();
      t.rows.clear();
    }
    const keep = new Set(subjects.map(idOf));
    for (const [id, tr] of [...t.rows]) {
      if (keep.has(id)) continue;
      tr.remove();
      t.rows.delete(id);
    }
    const sel = this.store.selected();
    subjects.forEach((s, i) => {
      let tr = t.rows.get(idOf(s));
      if (!tr) {
        tr = document.createElement("tr");
        tr.className = "subject";
        for (const c of cols) {
          const td = document.createElement("td");
          td.dataset.col = c.col;
          tr.append(td);
        }
        t.rows.set(idOf(s), tr);
      }
      // A group's newest member can change under the same row.
      if (tr.dataset.job !== s.job) tr.dataset.job = s.job;
      if (tr.dataset.key !== s.key) tr.dataset.key = s.key;
      tr.classList.toggle("dim", s.dim);
      tr.classList.toggle("selected", holds(s, sel));
      tr.classList.toggle("station", isStation(s));
      const values = subjectValues(s, sources.length >= 2 ? slots : null);
      const byField = new Map(s.row.subject.columns.map((c, i) => [c.field, values[i]]));
      cols.forEach((c, n) => {
        const text = c.text(s, values);
        const v = byField.get(c.col);
        const mark = text !== "" && v ? v.mark : null;
        const why = text !== "" && v?.markTitle ? v.markTitle : null;
        setText(tr.children[n], text, why ? `${text} (${why})` : text);
        setMark(tr.children[n], mark);
      });
      if (t.body.children[i] !== tr) t.body.insertBefore(tr, t.body.children[i] || null);
    });
  }

  // A job's section, or a sibling group's (`id` the group's, `job` its first member).
  drawJobSection(sec, { id, job, watched }) {
    const row = this.store.rowOf(job.kind);
    const cols = this.columns(row);
    const subjects = this.held(sec, this.sorted(id, this.store.list(job.id).reverse(), cols));
    const sources = legendSources(this.store.sources(id), (x) => x.name);
    this.drawSection(
      sec,
      `${job.kind} ${job.source}, ${subjects.length} ${row.subject.name}`,
      subjects,
      cols,
      watched,
      sources,
    );
  }

  // A replay group's section (plan 44 call 19): every replay of one capture, merged and
  // marked as siblings are, titled from the capture's record; drawn while it holds subjects.
  // A table per mode (ruling R12), in the order the modes were first heard, each its own
  // row's columns under a caption `<mode>, <n> <subject name>`, sorted apart
  // (`<group id>\0<mode>`).
  drawReplaySection(sec, { id, capture }) {
    const rec = this.captureOf(capture);
    const heading =
      rec && Number.isFinite(rec.start_ns) && Number.isFinite(rec.center_hz)
        ? `replay of ${clockText(rec.start_ns / 1e6)} ${mhzText(rec.center_hz)} MHz`
        : `replay of ${capture}`;
    const sources = legendSources(this.store.sources(id), (x) => x.name);
    this.drawHead(sec, heading, true, sources);
    const byMode = new Map(); // mode -> its subjects, least recently heard first
    for (const s of this.store.list(id)) {
      const mode = subjectMode(s);
      if (!byMode.has(mode)) byMode.set(mode, []);
      byMode.get(mode).push(s);
    }
    for (const [mode, t] of [...sec.tables]) {
      if (byMode.has(mode)) continue;
      t.table.remove();
      sec.tables.delete(mode);
    }
    [...byMode].forEach(([mode, list], i) => {
      let t = sec.tables.get(mode);
      if (!t) {
        t = this.makeTable();
        t.table.dataset.mode = mode;
        t.caption = document.createElement("caption");
        t.table.prepend(t.caption);
        t.table.addEventListener("pointerenter", () => {
          t.hover = true;
        });
        t.table.addEventListener("pointerleave", () => {
          t.hover = false;
          this.render();
        });
        sec.tables.set(mode, t);
      }
      const row = list[list.length - 1].row;
      const cols = this.columns(row, { recorded: true });
      const subjects = this.held(t, this.sorted(`${id}\u0000${mode}`, list.reverse(), cols));
      setText(t.caption, `${mode}, ${subjects.length} ${row.subject.name}`);
      this.drawTable(t, subjects, cols, sources);
      if (sec.tablesEl.children[i] !== t.table) {
        sec.tablesEl.insertBefore(t.table, sec.tablesEl.children[i] || null);
      }
    });
  }

  // The capture's `capture_list` record: null when the list no longer holds it, undefined
  // while not known (one list asked for, its answer redrawing the pane; after a failed one,
  // a draw CAPTURE_RETRY_MS later asks again).
  captureOf(id) {
    if (this.captures.has(id)) return this.captures.get(id);
    if (this.capturesAsked || Date.now() - this.capturesFailed < CAPTURE_RETRY_MS) return undefined;
    this.capturesAsked = (async () => {
      let r = null;
      try {
        r = await this.store.api.op({ op: "capture_list", signal: null });
      } catch {
        r = null;
      }
      this.capturesAsked = null;
      if (this.destroyed) return;
      if (!r || r.result !== "captures" || !Array.isArray(r.value)) {
        this.capturesFailed = Date.now();
        return;
      }
      const held = new Map(r.value.filter((c) => c && typeof c.id === "string").map((c) => [c.id, c]));
      for (const g of this.store.replayGroups()) {
        const c = g.slice(REPLAY_GROUP.length);
        this.captures.set(c, held.get(c) ?? null);
      }
      this.render();
    })();
    return undefined;
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
    // Every member group's sources, a chip reading `<protocol> <name>`.
    const sources = unionSources(
      members.map((x) =>
        legendSources(this.store.sources(x.job.id), (src) => `${this.store.rowOf(src.mode)?.protocol ?? src.mode} ${src.name}`),
      ),
    );
    this.drawSection(sec, `${name}, ${subjects.length} (${via})`, subjects, cols, members.every((x) => x.watched), sources);
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
      const w = id !== x.job.id ? all.get(id) : null;
      return w && w.context?.watch ? w : null;
    };
    // A sibling group (plan 43 call 14) is one entry, its first member's, under the group's
    // id: one section, and one member of a merge across rows (call 29).
    const seen = new Set();
    wanted = wanted.flatMap((x) => {
      if (watchOf(x)) return [x];
      const groupId = this.store.groupId(x.job.id);
      if (seen.has(groupId)) return [];
      seen.add(groupId);
      return [{ ...x, groupId }];
    });
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
      if (!members || members.length < 2 || !members.includes(x)) parts.push({ ...x, id: x.groupId });
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

  // The selected site's section (`section.subject-site`), drawn or updated; null, and gone,
  // while no site is selected.
  drawSite() {
    const sel = this.selection.get();
    if (!isSite(sel)) {
      this.site?.el.remove();
      this.site = null;
      return null;
    }
    if (!this.site) {
      const el = document.createElement("section");
      el.className = "subject-site";
      const h3 = document.createElement("h3");
      h3.textContent = "Site";
      const table = document.createElement("table");
      const body = document.createElement("tbody");
      const row = document.createElement("tr");
      row.className = "site-selected";
      const cells = ["name", "holders", "channels"].map((col) => {
        const td = document.createElement("td");
        td.dataset.col = col;
        row.append(td);
        return td;
      });
      // The selection outlives the lit set it was made from (no marker may be left to click
      // again), so the row clears it itself.
      const clear = document.createElement("button");
      clear.type = "button";
      clear.className = "site-clear";
      clear.setAttribute("aria-label", "Clear site selection");
      clear.title = "Clear site selection";
      clear.textContent = "×";
      const last = document.createElement("td");
      last.append(clear);
      row.append(last);
      body.append(row);
      table.append(body);
      el.append(h3, table);
      this.site = { el, cells };
    }
    const texts = [
      sel.name || sel.siteId || "",
      (sel.holders || []).join(", "),
      siteChannelsText(this.siteChannels(sel)),
    ];
    this.site.cells.forEach((td, i) => setText(td, texts[i], texts[i]));
    return this.site.el;
  }

  render() {
    if (this.destroyed || this.isHidden()) return;
    this.lastRender = Date.now();
    const site = this.drawSite();
    if (site && this.el.firstElementChild !== site) this.el.prepend(site);
    const first = site ? 1 : 0;
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
    // The replays' groups, after the live views: a capture's subjects stay until closed.
    for (const id of this.store.replayGroups()) parts.push({ id, capture: id.slice(REPLAY_GROUP.length) });
    const ids = new Set(parts.map((p) => p.id));
    for (const [id, sec] of [...this.sections]) {
      if (ids.has(id)) continue;
      sec.el.remove();
      this.sections.delete(id);
    }
    const listed = new Set([...(this.jobs() || []).map((j) => j.id), ...ids]);
    for (const id of [...this.sorts.keys()]) {
      // A replay table's sort is `<group id>\0<mode>`: kept while its section is listed.
      if (!listed.has(id.split("\u0000")[0])) this.sorts.delete(id);
    }
    parts.forEach((part, i) => {
      const sec = this.section(part.id, part.name ?? null, part.capture ?? null);
      if (part.capture !== undefined) this.drawReplaySection(sec, part);
      else if (part.group) this.drawWatchSection(sec, part);
      else if (part.members) this.drawMergedSection(sec, part);
      else this.drawJobSection(sec, part);
      const at = first + i;
      if (this.el.children[at] !== sec.el) this.el.insertBefore(sec.el, this.el.children[at] || null);
    });
  }

  destroy() {
    this.destroyed = true;
    clearInterval(this.timer);
    if (this.pending !== null) clearTimeout(this.pending);
    this.el.removeEventListener("click", this.onClick);
    this.el.removeEventListener("pointerdown", this.onPress);
    this.el.removeEventListener("keydown", this.onKey);
    this.el.replaceChildren();
    this.sections.clear();
    this.site = null;
  }
}
