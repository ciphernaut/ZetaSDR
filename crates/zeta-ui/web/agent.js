// agent.js: the transcript pane and the jobs list.
//
// Exports:
//   AUDIO_KINDS                 job kinds with an audio stream (their rows get ▶ and Transcribe;
//                               a `mix` is one, and its row has no band)
//   AUTO_SCAN_KEY               the sessionStorage key holding the page's automatic scan id
//   errorText(error) -> the text of an op error JSON as formatOp shows it after `→`
//   formatOp(payload) -> {text, error}
//       One `ops` stream payload ({op, outcome, result|error, elapsed_ms, transport}) as a
//       transcript line: `source_list → ok (2 sources)`,
//       `job_start kind=measure source=tone center_hz=100000000 … → job 01…`,
//       `job_start … → out_of_range field=center_hz … suggested=…` (error: true; the
//       corrective `suggested` value when the node offers one),
//       `classify … → ok (2 labels: L1 wfm 0.90, L2 FM Broadcast Radio 1.00)` (every label,
//       layer 1 and layer 2, as `L<layer> <name> <confidence, 2 dp>`).
//   class Transcript(el)
//       .op(payload)            append formatOp(payload); a line equal to the last one is
//                               folded into it with a ×N count. Ok outcomes of the read-only
//                               polls and lookups (QUIET_OPS: job_list, stream_list,
//                               source_list, and modes_list and modes_show, which the overlays
//                               ask to find positioned jobs, capture_list and signature_list, the Explore pane's polls, station_get, the home read at load, and set_list, read with job_list) whose
//                               transport is "ui" (a UI's own calls) are hidden; errors and
//                               other callers' records always show (ruling R6). Records from
//                               another caller are prefixed `[agent]`, `[http]`, `[local]`.
//       .reply(op, result)      a line for an op this page typed, from its /op reply (a
//                               quiet op's ops-stream record is hidden)
//       .user(text)             a request typed by the user
//       .assistant(text, error) an answer from /ask (error: an assistant-error line)
//       .note(text, error = false, actions = [])
//                               a UI line (connection state, local errors); each action
//                               {label, run} is a button after the text (`button.action`):
//                               a click disables the line's buttons and calls run(). A line
//                               with actions is never folded into the one before.
//       .onAssistant(fn)        fn(text, error) is also called for every assistant line
//                               (Task 8 hangs the narrow-footprint ribbons here)
//   startOpsFeed(api, transcript, {onConflict}) -> {close}
//       Finds the `ops` stream (the GET /streams entry whose provenance.source is "node" and
//       that belongs to no job, as a mix's streams do)
//       and appends one line per frame; reconnects and re-resolves the id when it drops.
//       onConflict(payload) is called for a `conflict` error of another caller (transport
//       not "ui"; this page's own typed ops are handled where it applies them).
//   SQUELCH_KINDS               job kinds whose demodulator carries the automatic squelch
//   setSquelchTag(row, event)   adds the `.tag.squelch` tag to a job row's `.kind` cell while
//                               `event` (the job's last `squelch` event) has `open: false`
//                               (tooltip: its power and floor, the neighbour's `adjacent_db`
//                               when measured, and whether the guard holds it), removes it
//                               otherwise (null: none seen)
//   isAudioJob(job, rowOf)      AUDIO_KINDS, or a mode row whose `outputs` lists `audio` (startJobs' `audioKind`: kind -> row)
//   startJobs(api, el, transcript, {onJobs, onPlay, onTranscribe, onGraph, playing, squelch,
//                                   view, position, onHot, shownSource})
//                                                       -> {close, refresh, redraw}
//       Each row carries `data-job` (the id, beside `data-id`), `data-colour` (overlays.js
//       channelColour, the colour its outline and tag share) and, when the tuning is
//       usable, a `.band` cell (bandText) after the short id; with `onHot(id, on)` given,
//       pointerenter/pointerleave on a row call it (overlays.js setHot puts `hot` on the
//       job's outline and tag, and on the row).
//       Polls `job_list` every 2 s and renders the running jobs, and the finished ones
//       (stopped, failed) for view.get("finished_rows_s") seconds after this page first saw
//       them finished (0: never; without `view`, 60 s); view.onClear drops the finished rows
//       at once. A finished row shows `.reason` (the state's reason, or `stopped`) and no
//       Stop, ▶ or Transcribe button. A row of an elastic job (the page's own spectrum job)
//       reads `<kind> elastic` and has only Stop. The row of the page's own automatic scan
//       (the id in sessionStorage[AUTO_SCAN_KEY], coarse.js) reads `scan auto`; a scan the
//       page did not start reads `scan`. Each row shows kind, source,
//       state, id, an `.events` cell (filled by overlays.js: a count of the job's events seen on its source's feed), on rows of an AUDIO_KINDS kind a
//       `button.play` (▶, or ■ while playing(id) is true) calling onPlay(job) and, while
//       running, a `button.transcribe` (Transcribe) calling onTranscribe(job) (ruling R14), a
//       `button.graph` calling onGraph(job), and a Stop button applying `job_stop`.
//       An attached job (one with an `input` job) is a `.job.attached` row placed right
//       under its input's row, its kind shown as `↳ <kind>`, with neither of the audio
//       buttons. A job's `state.reason` (a failure) is also the row's tooltip.
//       A running scan row (one `job_context` per poll, a `graph_get` once per job; both are
//       QUIET_OPS) shows `.sweep-ms` (the
//       last sweep, `<n> ms` or `<n.n> s`) and a `select.scan-look` over SCAN_LOOKS (fast,
//       averaged, long look; coarse and fast when `depths(source)` lists a lossy depth) or
//       `custom`; a choice sets dwell_ms, average and bits on the stored fft node and applies
//       `graph_put` through applyOp (a conflict shows its line naming the holders; graph_put
//       takes no `replace`, so there is no stop-and-retry). The row whose select has the focus
//       is left as it is across renders (an open dropdown stays open).
//       A running row of a SQUELCH_KINDS kind is tagged `squelch` (setSquelchTag) while
//       squelch(id) (overlays.js: the last `squelch` event seen on the job's event stream)
//       has `open: false`.
//       A running row whose position(id) (map.js rowText: the job's last fix from the view's
//       `home`, `14.8 km, 47°`) is not null shows it in a `.pos` line under the row.
//       A running `mix` row (job_start refuses the kind: it comes from mix_start) has a
//       `.mix-now` cell with mixText(job) (mix.js rowText: `ch9 now`, `last ch9 12 s ago`);
//       app.js rewrites it in place as the mix's events arrive. A running demodulator's row has
//       a `.mute` button (`Mute in mix` / `Unmute in mix`, aria-pressed) when mute(job) gives
//       {muted, title, run} (app.js: a running mix covers the job's source); `muted` is the
//       label only, run() decides from the mix's graph at the click. A running row whose
//       job's `context.slicer.stream` exists (a `bits` or `graph` job with a slicer) has a
//       `button.bits` (Bits) calling onBits(job) (app.js: the Bits pane on that job).
//       `button.origins` (Origins, a running job with a band: not mix, spectrum or scan) calling
//       onOrigins(job) (app.js: origin_lookup by the job).
//       onJobs(jobs) is called with the whole list after each render; redraw() renders the
//       last list again (after the playing state changed).
//       Each render writes the running rows' count into countEl (the Jobs header's `.count`).
//       Rows sit on two lines (style.css), every text span `dir="auto"`, the ones line two may
//       cut (TITLED, `.reason`, `.id`) carrying their full text as title. Idle plan channels
//       fold (foldPlan): when more than JOBS_FOLD_MIN running jobs sit on one plan's channels
//       (channelLabel under view `plan`) with their squelch closed (squelch(id) not open; a
//       kind without a squelch counts as closed) and no event (lastEvent(id), overlays.js) for
//       JOBS_FOLD_IDLE_MS since this page first saw them running, their rows and the rows
//       attached to them are moved into one `details.job-group[data-plan]` whose summary reads
//       `<n> idle <kinds> on <plan>[, <m> <attached kind>]`; it is open unless the operator
//       folded it (sessionStorage[JOB_FOLD_KEY + plan]). foldIdle(ms) changes the idle age.
//       sets() gives set_list's entries as last read.
//       A job folds only on evidence (foldPlan): an unwatched squelch kind needs the watched
//       mix (watchedMix(): its id) running and covering its source, and that mix not to have
//       heard it for JOBS_FOLD_IDLE_MS (activity(id): {watched, speaking, hot}, overlays.js);
//       a `speaking` row never folds, nor a `hot` one unless the hot is its own row's hover.
//       A watch's decoders fold under the watch first (groupOf, plan 31 spec 4.7, 9.22): a
//       membership fold, drawn from one member, whatever their activity. The watch's own row
//       becomes the summary of a `details.job-group[data-plan=<watch id>][data-kind="watch"]`
//       standing where the watch's row stood, its `.group-text` reading `<watch kind> <id
//       tail> · <mode> × <n>[, <mode> × <n>] decoding, <m> waiting` (m: the watch's
//       context.watch.cap.waiting), its decoders' rows (and the rows attached to them) under
//       it. Plan groups carry data-kind "plan". A decoder's Stop is its own job_stop.
//       A set's jobs fold under the set (groupOf with `set_list`, read with each `job_list`
//       poll; plan 31 spec 5.5, 9.19): a membership fold into
//       `details.job-group[data-plan=<set id>][data-kind="set"]` standing where the first
//       of them stood, its summary a `.group-text` reading `<set name> · <n> on <shown
//       source>, +<m> on <other source>` (shownSource(), the source the page shows; null:
//       `<n> on <source>` for each) and a `button.stop` that reads `Stop set?` on a first
//       click and sends `set_stop` on a second within SET_STOP_CONFIRM_MS. Only the rows on
//       the shown source are listed under it; a plan member's channels sit there directly,
//       a watch member's group (the watch's row and its decoders) nests inside it: two
//       levels at most. A watch row's Stop is the watch's job_stop, a member row's its own.
//   groupOf(job, jobs, sets = []) -> {key, kind: "watch" | "set", title} | null
//       the watch `job` decodes for: a running, unowned job named in some running watch's
//       context.watch.decoding[].job (`key` the watch id, `title` `<watch kind> <id tail>`);
//       else the set whose running instance lists it in `running.members[].job` (`sets`:
//       set_list's entries; `key` the set id, `title` its name)
//   SET_STOP_CONFIRM_MS         how long a set group's Stop stays armed (3000)
import { channelColour } from "./overlays.js";
import { mixCovering } from "./mix.js";
import { channelLabel } from "./plans.js";
import { bandText } from "./units.js";


export const JOBS_POLL_MS = 2000;
// Idle plan channels fold into one group row (spec 4.7): more than JOBS_FOLD_MIN running
// jobs on one plan's channels, each with its squelch closed (or no squelch) and no event
// for JOBS_FOLD_IDLE_MS.
export const JOBS_FOLD_MIN = 8;
export const JOBS_FOLD_IDLE_MS = 60_000;
export const JOB_FOLD_KEY = "zeta.jobfold."; // + plan id: "open" | "closed", the operator's fold
// The group a job folds under, the nearest first (plan 31 spec 4.7, 5.5, 9.22): a watch's
// kept decoder (a running, unowned job that a running watch's `context.watch.decoding`
// names) is in the watch's group; an unowned job a running set instance lists (`sets`,
// `set_list`'s entries: `running.members[].job`) is in the set's group. Null otherwise.
export function groupOf(j, jobs, sets = []) {
  if (!j || j.owner) return null;
  if (j.state?.status === "running") {
    for (const w of jobs || []) {
      if (w.id === j.id || w.state?.status !== "running") continue;
      const decoding = w.context?.watch?.decoding;
      if (Array.isArray(decoding) && decoding.some((d) => d && d.job === j.id)) {
        return { key: w.id, kind: "watch", title: `${w.kind} ${w.id.slice(-6)}` };
      }
    }
  }
  for (const s of sets || []) {
    const members = s?.running?.members;
    if (Array.isArray(members) && members.some((m) => m && m.job === j.id)) {
      return { key: s.id, kind: "set", title: s.name };
    }
  }
  return null;
}
// How long a set group's Stop stays armed ("Stop set?") for the second click that sends
// `set_stop` (spec 9.7: the confirm the page's other destructive buttons use).
export const SET_STOP_CONFIRM_MS = 3000;

// The row spans whose full text is their title (line two cuts them with an ellipsis).
const TITLED = ["band", "source", "events"];
export const AUDIO_KINDS = ["fm_demod", "wfm_demod", "am_demod", "ssb_demod", "mix"];
// Whether a job's row gets ▶ and Transcribe: its kind is a voice demodulator, or its mode
// row's `outputs` (modes_list) lists `audio`. `rowOf(kind)` gives the row, or null/undefined.
export function isAudioJob(job, rowOf) {
  return AUDIO_KINDS.includes(job.kind) || !!rowOf?.(job.kind)?.outputs?.includes("audio");
}
export const SQUELCH_KINDS = ["fm_demod", "am_demod"];
export const QUIET_OPS = new Set([
  "job_list",
  "stream_list",
  "source_list",
  "modes_list",
  "modes_show",
  "themes_list",
  "capture_list",
  "signature_list",
  "station_get",
  "set_list",
  "job_context",
  "graph_get",
]);
export const AUTO_SCAN_KEY = "zeta.autoScan";
const MAX_LINES = 500;

function scalar(v) {
  return v === null || ["string", "number", "boolean"].includes(typeof v);
}

// The node serialises op fields alphabetically; these lead, in this order, then the rest.
const LEADING = ["kind", "source", "job", "stream", "center_hz", "bandwidth_hz", "deviation_hz", "sideband"];

function opText(op) {
  if (!op || typeof op !== "object") return String(op);
  const parts = [op.op ?? "?"];
  const keys = Object.keys(op);
  const ordered = LEADING.filter((k) => keys.includes(k)).concat(
    keys.filter((k) => !LEADING.includes(k)),
  );
  for (const k of ordered) {
    const v = op[k];
    if (k === "op" || v === null || v === undefined) continue;
    // A rigid job_start (the usual case) reads as before; only `elastic=true` shows.
    if (k === "elastic" && v === false) continue;
    // Likewise a request that replaces nothing; named ids show.
    if (k === "replace" && Array.isArray(v) && v.length === 0) continue;
    parts.push(scalar(v) ? `${k}=${v}` : `${k}=…`);
  }
  return parts.join(" ");
}

function plural(n, word) {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

// One /ask call record (`{tool, arguments, ok, result}`) as a turn shows it.
function callText(c) {
  if (!c) return "";
  const args = c.arguments === undefined ? "" : ` ${JSON.stringify(c.arguments)}`;
  const r = c.result;
  const outcome = c.ok
    ? "ok"
    : `error: ${(r && (r.message || r.error)) || JSON.stringify(r)}`;
  return `→ ${c.tool}${args}\n  ${outcome}`;
}

// `L<layer> <name> <confidence, 2 dp>`, as the overlay labels read.
function labelText(l) {
  if (!l || typeof l !== "object") return String(l);
  const conf = typeof l.confidence === "number" ? ` ${l.confidence.toFixed(2)}` : "";
  return `L${l.layer ?? "?"} ${l.name ?? "?"}${conf}`;
}

function resultText(r) {
  if (!r || typeof r !== "object") return "ok";
  if (r.truncated && r.counts && r.result === "plan_start") {
    const c = r.counts;
    return `ok (${c.started} started, ${c.existing} existing, ${c.refused} refused)`;
  }
  if (r.truncated && r.counts && r.result === "set_start") {
    const c = r.counts;
    const w = Array.isArray(r.warnings) ? r.warnings.length : 0;
    return `ok (${c.started} started, ${c.existing} existing, ${c.refused} refused, ${w} warnings)`;
  }
  if (r.truncated && r.counts && r.result === "set_stopped") {
    const c = r.counts;
    return `ok (${c.stopped} stopped, ${c.ended} ended, ${c.failed} failed)`;
  }
  if (r.truncated && r.counts && r.result === "jobs_stopped") {
    return `ok (${r.counts.stopped} stopped, ${r.counts.failed} failed)`;
  }
  if (r.truncated) return `ok (${r.result}, ${r.bytes} bytes)`;
  const v = r.value;
  switch (r.result) {
    case "ok":
      return "ok";
    case "job":
      return `job ${v}`;
    case "sources":
      return `ok (${plural(v.length, "source")})`;
    case "jobs":
      return `ok (${plural(v.length, "job")})`;
    case "streams":
      return `ok (${plural(v.length, "stream")})`;
    case "frames":
      return `ok (${plural((v && v.frames ? v.frames.length : 0), "frame")})`;
    case "classification": {
      const labels = v && Array.isArray(v.labels) ? v.labels : [];
      const names = labels.map(labelText).join(", ");
      return `ok (${plural(labels.length, "label")}${names ? `: ${names}` : ""})`;
    }
    case "iq":
      return `ok (${plural(v && v.samples ? v.samples : 0, "sample")})`;
    case "plan_start": {
      const n = (k) => (v && Array.isArray(v[k]) ? v[k].length : 0);
      return `ok (${n("started")} started, ${n("existing")} existing, ${n("refused")} refused)`;
    }
    case "set_start": {
      const n = (k) => (v && Array.isArray(v[k]) ? v[k].length : 0);
      return (
        `ok (${n("started")} started, ${n("existing")} existing, ${n("refused")} refused, ` +
        `${n("warnings")} warnings)`
      );
    }
    case "set_stopped": {
      const n = (k) => (v && Array.isArray(v[k]) ? v[k].length : 0);
      return `ok (${n("stopped")} stopped, ${n("ended")} ended, ${n("failed")} failed)`;
    }
    case "jobs_stopped": {
      const n = (k) => (v && Array.isArray(v[k]) ? v[k].length : 0);
      return `ok (${n("stopped")} stopped, ${n("failed")} failed)`;
    }
    default:
      return `ok (${r.result})`;
  }
}

export function errorText(e) {
  if (!e || typeof e !== "object") return "error";
  if (e.error === "duplicate") {
    return `duplicate of job ${String(e.job ?? "").slice(-6)} ${e.message ?? ""}`.trimEnd();
  }
  if (e.error === "not_allowed") return `not allowed: ${e.message ?? ""}`.trimEnd();
  const parts = [e.error ?? "error"];
  if (e.field !== undefined) parts.push(`field=${e.field}`);
  if (e.value !== undefined && e.min !== undefined && e.max !== undefined) {
    parts.push(`${e.value} not in ${e.min}..${e.max}`);
  } else if (e.value !== undefined) {
    parts.push(`value=${e.value}`);
  }
  if (e.suggested !== undefined && e.suggested !== null) parts.push(`suggested=${e.suggested}`);
  if (e.why) parts.push(e.why);
  if (e.message) parts.push(e.message);
  if (e.detail !== undefined) parts.push(typeof e.detail === "string" ? e.detail : JSON.stringify(e.detail));
  return parts.join(" ");
}

export function formatOp(p) {
  const head = opText(p.op);
  if (p.outcome === "error") return { text: `${head} → ${errorText(p.error)}`, error: true };
  return { text: `${head} → ${resultText(p.result)}`, error: false };
}

export class Transcript {
  constructor(el) {
    this.el = el;
    this.last = null;
    this.listeners = [];
  }

  // Whether an ops-stream payload is shown: everything but a UI's own successful polls.
  #shows(payload) {
    const name = payload && payload.op && payload.op.op;
    return !(QUIET_OPS.has(name) && payload.transport === "ui" && payload.outcome !== "error");
  }

  #append(text, classes, title, actions = []) {
    const stick = this.el.scrollTop + this.el.clientHeight >= this.el.scrollHeight - 4;
    const last = this.last;
    if (!actions.length && last && !last.actions && last.text === text && last.classes === classes) {
      last.count += 1;
      last.countEl.textContent = ` ×${last.count}`;
      if (title) last.line.title = title;
    } else {
      const line = document.createElement("div");
      line.className = "line " + classes;
      const body = document.createElement("span");
      body.dir = "auto";
      body.textContent = text;
      const countEl = document.createElement("span");
      countEl.className = "count";
      line.append(body, countEl);
      const buttons = actions.map(({ label, run }) => {
        const b = document.createElement("button");
        b.className = "action";
        b.textContent = label;
        b.onclick = () => {
          for (const x of buttons) x.disabled = true;
          run();
        };
        return b;
      });
      line.append(...buttons);
      if (title) line.title = title;
      this.el.append(line);
      this.last = { text, classes, count: 1, line, countEl, actions: buttons.length > 0 };
      while (this.el.childElementCount > MAX_LINES) this.el.firstElementChild.remove();
    }
    if (stick) this.el.scrollTop = this.el.scrollHeight;
  }

  op(payload) {
    if (!this.#shows(payload)) return;
    const { text, error } = formatOp(payload);
    const who = payload.transport;
    const prefix = who && who !== "ui" ? `[${who}] ` : "";
    const ms = payload.elapsed_ms;
    this.#append(prefix + text, error ? "op error" : "op", ms === undefined ? "" : `${ms} ms`);
  }

  reply(op, result) {
    this.#append(formatOp({ op, outcome: "ok", result }).text, "op");
  }

  user(text) {
    this.#append(text, "user");
  }

  // `turnLog` is /ask's `turn_log` and `calls` its `calls`, in order: after the answer line,
  // one collapsed `<details class="turn">` per turn that has reasoning, content or tool
  // calls, holding them (each call as `tool {arguments}` and its outcome). A model run
  // without thinking has no reasoning, so its calls are what a turn shows.
  assistant(text, error = false, turnLog = [], calls = []) {
    this.#append(text, error ? "assistant error" : "assistant");
    let next = 0;
    for (const turn of turnLog) {
      if (!turn) continue;
      // `calls` is the completion's tool-call count (TurnEntry.calls).
      const k = Number.isInteger(turn.calls) ? turn.calls : 0;
      const mine = calls.slice(next, next + k);
      next += k;
      if (!turn.reasoning?.trim() && !turn.content && !mine.length) continue;
      const d = document.createElement("details");
      d.className = "turn";
      const sum = document.createElement("summary");
      sum.textContent = `turn ${turn.turn} (${plural(k, "call")})`;
      d.append(sum);
      const parts = [turn.reasoning?.trim(), turn.content, ...mine.map(callText)];
      for (const part of parts) {
        if (!part) continue;
        const pre = document.createElement("pre");
        pre.textContent = part;
        d.append(pre);
      }
      this.el.append(d);
      this.last = null;
    }
    for (const fn of this.listeners) fn(text, error);
  }

  note(text, error = false, actions = []) {
    this.#append(text, error ? "note error" : "note", "", actions);
  }

  onAssistant(fn) {
    this.listeners.push(fn);
  }
}

async function opsStreamId(api) {
  const list = await api.streams();
  // A mix's streams are the node's too (source "node") but belong to a job: not the ops feed.
  const s = list.find((i) => i.provenance && i.provenance.source === "node" && !i.provenance.job);
  return s ? s.id : null;
}

export function startOpsFeed(api, transcript, { onConflict = null } = {}) {
  let sub = null;
  let stopped = false;
  let retry = null;
  const begin = async () => {
    let id = null;
    try {
      id = await opsStreamId(api);
    } catch {
      id = null;
    }
    if (stopped) return;
    if (!id) {
      transcript.note("no ops stream on this node; retrying", true);
      retry = setTimeout(begin, 2000);
      return;
    }
    sub = api.streamText(
      id,
      (msg) => {
        if (msg.type === "frame") {
          transcript.op(msg.payload);
          const p = msg.payload;
          if (
            onConflict &&
            p &&
            p.outcome === "error" &&
            p.error &&
            p.error.error === "conflict" &&
            p.transport !== "ui"
          ) {
            onConflict(p);
          }
        }
        else if (msg.type === "lagged") transcript.note(`ops: ${msg.missed} missed`, true);
      },
      ({ reconnecting }) => {
        if (reconnecting) transcript.note("ops stream closed; reconnecting", true);
      },
      { onReconnect: () => opsStreamId(api) },
    );
  };
  begin();
  return {
    close() {
      stopped = true;
      clearTimeout(retry);
      if (sub) sub.close();
    },
  };
}

function autoScanId() {
  try {
    return sessionStorage.getItem(AUTO_SCAN_KEY);
  } catch {
    return null;
  }
}

const isFinished = (j) => !!(j.state && (j.state.status === "stopped" || j.state.status === "failed"));

export function setSquelchTag(row, event) {
  const cell = row && row.querySelector(".kind");
  if (!cell) return;
  let tag = cell.querySelector(".tag.squelch");
  if (!event || event.open !== false) {
    if (tag) {
      tag.previousSibling?.remove(); // the space before it
      tag.remove();
    }
    return;
  }
  if (!tag) {
    tag = document.createElement("span");
    tag.className = "tag squelch";
    tag.dir = "auto";
    tag.textContent = "squelch";
    cell.append(" ", tag);
  }
  const db = (v) => (typeof v === "number" ? `${v.toFixed(1)} dBFS` : "?");
  let title = `squelch closed: no audio (power ${db(event.power_db)}, floor ${db(event.floor_db)})`;
  if (typeof event.adjacent_db === "number") title += `; neighbour ${db(event.adjacent_db)}`;
  if (event.guarded) title += "; held by the adjacent-channel guard";
  tag.title = title;
}

// Job kinds with no band of their own: `origin_lookup` refuses them.
const NO_BAND_KINDS = ["mix", "spectrum", "scan"];

// The looks a scan row offers (spec 4.5, 4.6a): dwell per step, sweeps averaged, depth. `bits:
// null` keeps the source's own depth; `coarse` asks for its lossy depth and is offered only where
// the source lists one.
export const SCAN_LOOKS = [
  { id: "fast", label: "fast", dwell_ms: 20, average: 1, bits: null },
  { id: "averaged", label: "averaged", dwell_ms: 20, average: 16, bits: null },
  { id: "long", label: "long look", dwell_ms: 250, average: 1, bits: null },
  {
    id: "coarse",
    label: "coarse and fast",
    dwell_ms: 20,
    average: 1,
    bits: "lossy",
    title:
      "8-bit samples: a weak signal beside a strong one in the same capture is lost. For a coarse survey of occupancy, never for faint signals.",
  },
];

// The lossy depth's bits of a source's `capabilities.depths`, or null.
export const lossyBits = (depths) => (depths || []).find((d) => d && d.lossless === false)?.bits ?? null;

// The look a stored `fft` node's params match, else "custom".
export function lookOf(params, lossy) {
  const p = params || {};
  const bits = p.bits ?? null;
  for (const l of SCAN_LOOKS) {
    const want = l.bits === "lossy" ? lossy : l.bits;
    if (l.bits === "lossy" && want === null) continue;
    if ((p.dwell_ms ?? 20) === l.dwell_ms && (p.average ?? 1) === l.average && bits === want) return l.id;
  }
  return "custom";
}

// A sweep time as `<n> ms` under a second, else `<n.n> s`.
export const sweepText = (ms) => (ms < 1000 ? `${Math.round(ms)} ms` : `${(ms / 1000).toFixed(1)} s`);

function lookSelect(job, scan, depths, onLook) {
  const lossy = lossyBits(depths);
  const sel = document.createElement("select");
  sel.className = "scan-look";
  sel.title = "the scan's look: dwell per step and sweeps averaged (graph_put on its fft node)";
  const looks = SCAN_LOOKS.filter((l) => l.bits !== "lossy" || lossy !== null);
  const current = lookOf(scan.params, lossy);
  const ids = current === "custom" ? [...looks, { id: "custom", label: "custom" }] : looks;
  for (const l of ids) {
    const o = document.createElement("option");
    o.value = l.id;
    o.textContent = l.label;
    if (l.title) o.title = l.title;
    sel.append(o);
  }
  sel.value = current;
  sel.onchange = () => {
    const l = SCAN_LOOKS.find((x) => x.id === sel.value);
    if (l) onLook(job, { dwell_ms: l.dwell_ms, average: l.average, bits: l.bits === "lossy" ? lossy : l.bits });
  };
  return sel;
}

function jobRow(
  job,
  onStop,
  { onPlay, onTranscribe, onGraph, playing, squelch, plan, position, onHot, mixText, mute, onBits, onOrigins, audioKind, scanInfo, onLook },
) {
  const row = document.createElement("div");
  const attached = !!job.input;
  const finished = isFinished(job);
  row.className = ["job", attached && "attached", finished && "finished"].filter(Boolean).join(" ");
  row.dataset.id = job.id;
  row.dataset.job = job.id;
  row.dataset.colour = String(channelColour(job.id));
  if (onHot) {
    row.addEventListener("pointerenter", () => onHot(job.id, true));
    row.addEventListener("pointerleave", () => onHot(job.id, false));
  }
  const status = job.state && job.state.status ? job.state.status : "?";
  row.dataset.state = status;
  const cells = [
    ["kind", attached ? `↳ ${job.kind}` : job.kind],
    ["source", job.source],
    ["state", status],
    ["id", job.id.slice(-6)],
    ["events", ""],
  ];
  const t = job.tuning;
  if (t && Number.isFinite(t.center_hz) && Number.isFinite(t.bandwidth_hz) && t.bandwidth_hz > 0) {
    cells.splice(4, 0, ["band", bandText(t.center_hz, t.bandwidth_hz)]);
  }
  if (job.kind === "mix") cells.splice(4, 0, ["mix-now", mixText ? mixText(job) : ""]);
  for (const [cls, text] of cells) {
    const span = document.createElement("span");
    span.className = cls;
    span.textContent = text;
    span.dir = "auto";
    if (TITLED.includes(cls)) span.title = text;
    if (cls === "id") span.title = job.id;
    if (cls === "kind" && job.elastic) {
      const tag = document.createElement("span");
      tag.className = "tag elastic";
      tag.dir = "auto";
      tag.textContent = "elastic";
      tag.title = "follows the capture; holds no lease";
      span.append(" ", tag);
    }
    if (cls === "kind" && plan && job.tuning) {
      const ch = channelLabel(job.tuning.center_hz, plan());
      if (ch) {
        const tag = document.createElement("span");
        tag.className = "tag ch";
        tag.dir = "auto";
        tag.textContent = ch.text;
        tag.title = ch.title;
        span.append(" ", tag);
      }
    }
    if (cls === "kind" && job.kind === "scan" && job.id === autoScanId()) {
      const tag = document.createElement("span");
      tag.className = "tag auto";
      tag.dir = "auto";
      tag.textContent = "auto";
      tag.title = "the page's automatic scan of the view outside the capture";
      span.append(" ", tag);
    }
    row.append(span);
  }
  if (squelch && status === "running" && SQUELCH_KINDS.includes(job.kind)) {
    setSquelchTag(row, squelch(job.id));
  }
  const pos = position && status === "running" ? position(job.id) : null;
  if (pos) {
    const span = document.createElement("span");
    span.className = "pos";
    span.textContent = pos;
    span.dir = "auto";
    span.title = "the last position decoded, from the view's home";
    row.append(span);
  }
  const scan = job.kind === "scan" && status === "running" && scanInfo ? scanInfo(job) : null;
  if (scan && Number.isFinite(scan.sweep_ms)) {
    const span = document.createElement("span");
    span.className = "sweep-ms";
    span.textContent = sweepText(scan.sweep_ms);
    span.title = "the last sweep's time";
    row.append(span);
  }
  if (job.state && job.state.reason) row.title = job.state.reason;
  if (finished) {
    const reason = document.createElement("span");
    reason.className = "reason";
    reason.textContent = (job.state && job.state.reason) || "stopped";
    reason.dir = "auto";
    reason.title = reason.textContent;
    row.append(reason);
  }
  const actions = document.createElement("span");
  actions.className = "actions";
  if (finished) {
    if (onGraph) actions.append(graphButton(job, onGraph));
    row.append(actions);
    return row;
  }
  if (job.elastic) {
    actions.append(stopButton(job, onStop));
    row.append(actions);
    return row;
  }
  const audio = !attached && isAudioJob(job, audioKind);
  if (audio && onPlay) {
    const on = playing ? playing(job.id) : false;
    const play = document.createElement("button");
    play.className = "play";
    play.textContent = on ? "■" : "▶";
    play.title = on ? "stop the audio" : "play the audio";
    play.setAttribute("aria-pressed", String(on));
    play.onclick = () => onPlay(job);
    actions.append(play);
  }
  if (audio && onTranscribe && status === "running") {
    const tr = document.createElement("button");
    tr.className = "transcribe";
    tr.textContent = "Transcribe";
    tr.title = `job_attach kind=transcribe job=${job.id}`;
    tr.onclick = () => onTranscribe(job);
    actions.append(tr);
  }
  const m = mute && status === "running" ? mute(job) : null;
  if (m) {
    const b = document.createElement("button");
    b.className = "mute";
    b.textContent = m.muted ? "Unmute in mix" : "Mute in mix";
    b.title = m.title;
    b.setAttribute("aria-pressed", String(m.muted));
    b.onclick = () => m.run();
    actions.append(b);
  }
  if (onBits && status === "running" && job.context?.slicer?.stream) {
    const b = document.createElement("button");
    b.className = "bits";
    b.textContent = "Bits";
    b.title = "show its bits in the Bits pane";
    b.onclick = () => onBits(job);
    actions.append(b);
  }
  if (onOrigins && status === "running" && job.tuning && !NO_BAND_KINDS.includes(job.kind)) {
    const b = document.createElement("button");
    b.className = "origins";
    b.textContent = "Origins";
    b.title = `origin_lookup ${job.id}`;
    b.onclick = () => onOrigins(job);
    actions.append(b);
  }
  if (scan && scan.params && onLook) actions.append(lookSelect(job, scan, scan.depths, onLook));
  if (onGraph) actions.append(graphButton(job, onGraph));
  actions.append(stopButton(job, onStop));
  row.append(actions);
  return row;
}

function graphButton(job, onGraph) {
  const graph = document.createElement("button");
  graph.className = "graph";
  graph.textContent = "graph";
  graph.title = `graph_get ${job.id}`;
  graph.onclick = () => onGraph(job);
  return graph;
}

function stopButton(job, onStop) {
  const stop = document.createElement("button");
  stop.className = "stop";
  stop.textContent = "Stop";
  stop.title = `job_stop ${job.id}`;
  stop.onclick = () => onStop(job.id, stop);
  return stop;
}

// `jobs` with each attached job moved right after its input (in list order); one whose input
// is not in `jobs` keeps its place.
function underInputs(jobs) {
  const ids = new Set(jobs.map((j) => j.id));
  const out = [];
  for (const j of jobs) {
    if (j.input && ids.has(j.input)) continue;
    out.push(j, ...jobs.filter((a) => a.input === j.id));
  }
  return out;
}

// The fold of the operator's choice: "closed" when folded this session, else open.
function storedOpen(plan) {
  try {
    return sessionStorage.getItem(JOB_FOLD_KEY + plan) !== "closed";
  } catch {
    return true;
  }
}

// The group row of a plan's idle channels, made once per plan and kept across renders (so a
// focused summary stays focused); open unless the operator folded it this session. Only a
// change of the fold is written (opening a fresh group queues a `toggle` too).
function jobGroup(plan, kind = "plan") {
  const g = document.createElement("details");
  g.className = "job-group";
  g.dataset.plan = plan;
  g.dataset.kind = kind;
  g.open = storedOpen(plan);
  g.addEventListener("toggle", () => {
    if (g.open === storedOpen(plan)) return;
    try {
      sessionStorage.setItem(JOB_FOLD_KEY + plan, g.open ? "open" : "closed");
    } catch {
      /* the fold is not remembered */
    }
  });
  const summary = document.createElement("summary");
  summary.dir = "auto";
  if (kind === "watch" || kind === "set") {
    const text = document.createElement("span");
    text.className = "group-text";
    text.dir = "auto";
    summary.append(text);
  }
  g.append(summary);
  return g;
}

// A watch group's text: `<watch kind> <id tail> · <mode> × <n> decoding, <m> waiting`, the
// modes those of `list` (its decoders) by the watch's `decoding` entries.
function watchGroupText(watch, list) {
  const w = watch.context?.watch || {};
  const modeOf = new Map((w.decoding || []).map((d) => [d.job, d.mode]));
  const modes = new Map();
  for (const j of list) {
    const m = modeOf.get(j.id) ?? j.kind;
    modes.set(m, (modes.get(m) || 0) + 1);
  }
  const waiting = w.cap?.waiting ?? 0;
  return (
    `${watch.kind} ${watch.id.slice(-6)} · ` +
    [...modes].map(([m, n]) => `${m} × ${n}`).join(", ") +
    ` decoding, ${waiting} waiting`
  );
}

// A set group's text: `<set name> · <n> on <shown source>, +<m> on <other source>` over
// `list` (the instance's jobs on a radio, attached ones not counted), the other sources in
// list order; with no shown source (`here` null), `<n> on <source>` for each.
function setGroupText(set, list, here) {
  const by = new Map();
  for (const j of list) by.set(j.source, (by.get(j.source) || 0) + 1);
  const parts = here === null ? [] : [`${by.get(here) || 0} on ${here}`];
  for (const [src, n] of by) {
    if (src !== here) parts.push(here === null ? `${n} on ${src}` : `+${n} on ${src}`);
  }
  return `${set.name} · ${parts.join(", ")}`;
}

// `<n> idle <kinds> on <plan>`, plus `, <m> <kind>` for each kind attached to them.
function groupText(plan, list, shown) {
  const kinds = [...new Set(list.map((j) => j.kind))].join(", ");
  const inside = new Set(list.map((j) => j.id));
  const attached = new Map();
  for (const j of shown) {
    if (j.input && inside.has(j.input)) attached.set(j.kind, (attached.get(j.kind) || 0) + 1);
  }
  return (
    `${list.length} idle ${kinds} on ${plan}` +
    [...attached].map(([kind, m]) => `, ${m} ${kind}`).join("")
  );
}

// `parent`'s children made `nodes`, in order, without detaching a node that stays (a kept
// group keeps its focus): the stale ones go first, then each node is put in its place.
function placeChildren(parent, nodes) {
  const keep = new Set(nodes);
  for (const c of [...parent.childNodes]) if (!keep.has(c)) c.remove();
  let ref = parent.firstChild;
  for (const n of nodes) {
    if (n === ref) ref = ref.nextSibling;
    else parent.insertBefore(n, ref);
  }
}

export function startJobs(
  api,
  el,
  transcript,
  {
    onJobs = null,
    onPlay = null,
    onTranscribe = null,
    onGraph = null,
    playing = null,
    squelch = null,
    view = null,
    position = null,
    onHot = null,
    mixText = null,
    mute = null,
    onBits = null,
    onOrigins = null,
    audioKind = null,
    countEl = null,
    lastEvent = null,
    activity = null,
    watchedMix = null,
    shownSource = null,
    applyOp = null,
    depths = null,
  } = {},
) {
  let timer = null;
  // id -> {sweep_ms, params}: a running scan's last sweep time (job_context, read with each
  // job_list) and its stored fft node's params (graph_get, read once per job and again after
  // this page's own graph_put or when the context's asked look no longer matches the node's:
  // a restart by someone else).
  const scans = new Map();
  const readScan = async (id) => {
    const c = await api.op({ op: "job_context", job: id }).catch(() => null);
    const ctx = c?.value?.scan ?? {};
    let params = scans.get(id)?.params ?? null;
    const stale =
      params &&
      ((params.dwell_ms ?? 20) !== (ctx.dwell_ms_asked ?? ctx.dwell_ms ?? 20) ||
        (params.average ?? 1) !== (ctx.average_asked ?? ctx.average ?? 1) ||
        (params.bits ?? null) !== (ctx.bits ?? null));
    if (!params || stale) {
      const g = await api.op({ op: "graph_get", job: id }).catch(() => null);
      params = g?.value?.nodes?.find((n) => n.kind === "fft")?.params ?? null;
    }
    scans.set(id, { sweep_ms: ctx.timing?.sweep_ms ?? null, params });
  };
  // A look is the stored fft node's dwell, average and depth, put back (the scan restarts).
  const setLook = async (job, look) => {
    try {
      const r = await api.op({ op: "graph_get", job: job.id });
      const graph = r && r.value;
      const node = graph?.nodes?.find((n) => n.kind === "fft");
      if (!node) throw new Error("no fft node");
      node.params = { ...node.params, ...look };
      const put = { op: "graph_put", job: job.id, graph };
      if (applyOp) await applyOp(put);
      else await api.op(put);
    } catch (e) {
      transcript.note(`graph_put: ${e.message}`, true);
    }
    scans.delete(job.id);
    refresh();
  };
  let sets = []; // set_list's entries, read with each job_list
  // A set's group: its text and a Stop that arms on the first click ("Stop set?") and sends
  // `set_stop` on a second one within SET_STOP_CONFIRM_MS (spec 5.5, 9.7). The group is kept
  // across renders, so the armed state is too.
  const setGroup = (id) => {
    const g = jobGroup(id, "set");
    const stop = document.createElement("button");
    stop.className = "stop";
    stop.textContent = "Stop";
    stop.title = `set_stop ${id}`;
    let armed = null;
    stop.onclick = async (ev) => {
      ev.preventDefault(); // a button in the summary does not toggle the fold
      if (armed === null) {
        stop.textContent = "Stop set?";
        armed = setTimeout(() => {
          armed = null;
          stop.textContent = "Stop";
        }, SET_STOP_CONFIRM_MS);
        return;
      }
      clearTimeout(armed);
      armed = null;
      stop.textContent = "Stop";
      stop.disabled = true;
      try {
        await api.op({ op: "set_stop", set: id });
      } catch (e) {
        if (!(e.json && e.status === 422)) transcript.note(`set_stop: ${e.message}`, true);
      } finally {
        stop.disabled = false;
      }
      refresh();
    };
    g.querySelector("summary").append(stop);
    return g;
  };
  let foldIdleMs = JOBS_FOLD_IDLE_MS;
  // id -> performance.now() when this page first saw the job running (its idle age starts
  // there when no event has been seen).
  const runningSince = new Map();
  // id -> performance.now() when the watched mix last heard the job (`speaking`): the idle
  // age of path (c) below starts there.
  const lastSpeaking = new Map();
  const groupEls = new Map(); // plan -> its kept details.job-group
  // The job whose own row the pointer is on (set here from the row's pointerenter/leave): its
  // `hot` comes from the list itself and does not keep it out of a group.
  let rowHot = null;
  let ageTimer = null;
  let stopped = false;
  let paused = false; // zetaTest.pausePolling: the timer's polls do nothing (refresh still works)
  let last = null;
  // id -> performance.now() when this page first saw the job finished (-Infinity: cleared).
  const finishedAt = new Map();
  const buttons = {
    onPlay,
    onTranscribe,
    onGraph,
    playing,
    squelch,
    plan: view ? () => view.get("plan") : null,
    position,
    onHot: onHot
      ? (id, on) => {
          rowHot = on ? id : rowHot === id ? null : rowHot;
          onHot(id, on);
        }
      : null,
    mixText,
    mute,
    onBits,
    onOrigins,
    audioKind,
    scanInfo: (job) => {
      const s = scans.get(job.id);
      return s ? { ...s, depths: depths ? depths(job.source) : [] } : null;
    },
    onLook: setLook,
  };
  const keepMs = () => {
    const s = view ? Number(view.get("finished_rows_s")) : 60;
    return Number.isFinite(s) && s > 0 ? s * 1000 : 0;
  };
  // The jobs listed now: running ones, and finished ones still inside finished_rows_s;
  // schedules a redraw for when the next finished row ages out.
  const visible = (jobs) => {
    const now = performance.now();
    const ids = new Set(jobs.map((j) => j.id));
    for (const id of finishedAt.keys()) if (!ids.has(id)) finishedAt.delete(id);
    const keep = keepMs();
    let next = Infinity;
    const out = jobs.filter((j) => {
      if (!isFinished(j)) {
        finishedAt.delete(j.id);
        return true;
      }
      if (!finishedAt.has(j.id)) finishedAt.set(j.id, now);
      const left = finishedAt.get(j.id) + keep - now;
      if (left > 0) next = Math.min(next, left);
      return left > 0;
    });
    clearTimeout(ageTimer);
    ageTimer = next < Infinity && !stopped ? setTimeout(() => redraw(), next + 20) : null;
    return out;
  };
  const redraw = () => {
    if (last && !stopped) render(last);
  };
  if (view) {
    view.onClear(() => {
      for (const j of last || []) if (isFinished(j)) finishedAt.set(j.id, -Infinity);
      redraw();
    });
    view.onChange((key) => {
      if (key === "finished_rows_s" || key === "plan" || key === "home") redraw();
    });
  }
  // The plan id of a running job on a plan channel that folds, on evidence the page has:
  // (a) its kind has no squelch (counted closed) and it sent no event for foldIdleMs;
  // (b) it is watched (overlays.js: every running job on the shown source, through the
  //     source's feed), its squelch known closed and no event for foldIdleMs;
  // (c) it is an unwatched squelch kind (its squelch and events unknown) on a source the
  //     mix the page watches (watchedMix(), mix.js MixWatch: the one `speaking` comes from)
  //     covers, and that mix has not heard it (`speaking`) for foldIdleMs (lastSpeaking);
  //     another mix is no evidence.
  // Never a `speaking` row, nor a `hot` one when the hot comes from outside the list (a rail
  // tag, an outline): a row hovered inside its group stays there (rowHot). An unwatched
  // squelch kind with no watched covering mix never folds. Every way out of a group (the job
  // stopped, its squelch opened, an event arrived, the mix heard it, the group under
  // JOBS_FOLD_MIN) is this test failing at the next render, one poll at most.
  const foldPlan = (j, now, jobs) => {
    if (isFinished(j) || j.input || j.elastic || !j.tuning || !view) return null;
    const ch = channelLabel(j.tuning.center_hz, view.get("plan"));
    if (!ch) return null;
    const seen = activity ? activity(j.id) : { watched: true, speaking: false, hot: false };
    if (seen.speaking || (seen.hot && j.id !== rowHot)) return null;
    if (SQUELCH_KINDS.includes(j.kind) && !seen.watched) {
      const id = watchedMix ? watchedMix() : null;
      const mix = id ? jobs.filter((m) => m.id === id) : [];
      if (!mixCovering(j, mix)) return null;
      const since = Math.max(runningSince.get(j.id) ?? now, lastSpeaking.get(j.id) ?? -Infinity);
      return now - since >= foldIdleMs ? ch.plan : null;
    }
    if (SQUELCH_KINDS.includes(j.kind)) {
      const sq = squelch ? squelch(j.id) : null;
      if (!sq || sq.open !== false) return null;
    }
    const heard = lastEvent ? lastEvent(j.id) : null;
    const since = Math.max(runningSince.get(j.id) ?? now, heard ?? -Infinity);
    return now - since >= foldIdleMs ? ch.plan : null;
  };
  // The rows in list order, a plan's foldable jobs (and the jobs attached to them) wrapped in
  // one `details.job-group[data-plan]` where the first of them stood.
  const fold = (shown, rows, jobs) => {
    const now = performance.now();
    const ids = new Set(shown.map((j) => j.id));
    for (const id of runningSince.keys()) if (!ids.has(id)) runningSince.delete(id);
    for (const id of lastSpeaking.keys()) if (!ids.has(id)) lastSpeaking.delete(id);
    for (const j of shown) {
      if (isFinished(j)) {
        runningSince.delete(j.id);
        lastSpeaking.delete(j.id);
        continue;
      }
      if (!runningSince.has(j.id)) runningSince.set(j.id, now);
      if (activity && activity(j.id).speaking) lastSpeaking.set(j.id, now);
    }
    // A watch's decoders first (groupOf), under a watch whose own row is shown.
    const shownIds = new Set(shown.map((j) => j.id));
    const watchOf = new Map(); // decoder id -> watch id
    const decoders = new Map(); // watch id -> its decoders (not the attached ones)
    for (const j of shown) {
      const g = groupOf(j, jobs);
      if (!g || !shownIds.has(g.key)) continue;
      watchOf.set(j.id, g.key);
      if (!decoders.has(g.key)) decoders.set(g.key, []);
      decoders.get(g.key).push(j);
    }
    // Then a set's jobs (groupOf with the sets): the jobs a running instance lists. A watch
    // member's group nests inside the set's (two levels at most, spec 9.19); a plan member's
    // channels sit in the set's group directly.
    const setOf = new Map(); // job id -> set id (the instance's jobs, not attached ones)
    const setInfo = new Map(); // set id -> its set_list entry
    for (const j of shown) {
      if (watchOf.has(j.id) || j.input) continue;
      const g = groupOf(j, jobs, sets);
      if (!g || g.kind !== "set") continue;
      setOf.set(j.id, g.key);
      setInfo.set(g.key, sets.find((s) => s.id === g.key));
    }
    const planOf = new Map();
    const members = new Map(); // plan -> its foldable jobs (not the attached ones)
    for (const j of shown) {
      if (watchOf.has(j.id) || setOf.has(j.id)) continue;
      const plan = foldPlan(j, now, jobs);
      if (!plan) continue;
      planOf.set(j.id, plan);
      if (!members.has(plan)) members.set(plan, []);
      members.get(plan).push(j);
    }
    for (const [plan, list] of members) if (list.length <= JOBS_FOLD_MIN) members.delete(plan);
    const setKey = (id) => `set:${id}`; // a set's key in groupEls (apart from plan ids)
    const live = new Set([...members.keys(), ...decoders.keys(), ...[...setInfo.keys()].map(setKey)]);
    for (const key of [...groupEls.keys()]) if (!live.has(key)) groupEls.delete(key);
    const inside = new Map(); // plan -> its rows, in list order
    const watchRows = new Map(); // watch id -> {watch, row}: its own row
    const decoderRows = new Map(); // watch id -> its decoders' rows, in list order
    const setRows = new Map(); // set id -> its rows on the shown source, in list order
    const here = shownSource ? shownSource() ?? null : null;
    const out = [];
    // `node` (j's row, or a watch member's group) under j's set when j (or its input) is one
    // of a set's jobs: listed when on the shown source, else only counted. Whether it was.
    const intoSet = (j, node) => {
      const set = setOf.get(j.input ?? j.id);
      if (!set) return false;
      const key = setKey(set);
      if (!setRows.has(set)) {
        if (groupEls.get(key)?.dataset.kind !== "set") groupEls.set(key, setGroup(set));
        setRows.set(set, []);
        out.push(groupEls.get(key));
      }
      if (here === null || j.source === here) setRows.get(set).push(node);
      return true;
    };
    shown.forEach((j, i) => {
      if (decoders.has(j.id)) {
        if (groupEls.get(j.id)?.dataset.kind !== "watch") groupEls.set(j.id, jobGroup(j.id, "watch"));
        watchRows.set(j.id, { watch: j, row: rows[i] });
        if (!intoSet(j, groupEls.get(j.id))) out.push(groupEls.get(j.id));
        return;
      }
      const watch = watchOf.get(j.input ?? j.id);
      if (watch) {
        if (!decoderRows.has(watch)) decoderRows.set(watch, []);
        decoderRows.get(watch).push(rows[i]);
        return;
      }
      if (intoSet(j, rows[i])) return;
      const plan = planOf.get(j.input ?? j.id);
      if (!plan || !members.has(plan)) {
        out.push(rows[i]);
        return;
      }
      if (!inside.has(plan)) {
        if (groupEls.get(plan)?.dataset.kind !== "plan") groupEls.set(plan, jobGroup(plan));
        inside.set(plan, []);
        out.push(groupEls.get(plan));
      }
      inside.get(plan).push(rows[i]);
    });
    for (const [plan, list] of inside) {
      const g = groupEls.get(plan);
      const summary = g.querySelector("summary");
      const text = groupText(plan, members.get(plan), shown);
      if (summary.textContent !== text) summary.textContent = text;
      placeChildren(g, [summary, ...list]);
    }
    // A watch's group: the watch's row as its summary, its decoders under it.
    for (const [id, { watch, row }] of watchRows) {
      const list = decoderRows.get(id) || [];
      const g = groupEls.get(id);
      const summary = g.querySelector("summary");
      const text = summary.querySelector(".group-text");
      const t = watchGroupText(watch, decoders.get(id));
      if (text.textContent !== t) text.textContent = t;
      text.title = t;
      placeChildren(summary, [row, text]);
      placeChildren(g, [summary, ...list]);
    }
    // A set's group: its name and the count per source as its summary, its rows on the shown
    // source (a watch member's group among them) under it.
    for (const [id, list] of setRows) {
      const g = groupEls.get(setKey(id));
      const text = g.querySelector("summary > .group-text");
      const t = setGroupText(setInfo.get(id), shown.filter((j) => setOf.get(j.id) === id), here);
      if (text.textContent !== t) text.textContent = t;
      text.title = t;
      placeChildren(g, [g.querySelector("summary"), ...list]);
    }
    return out;
  };
  const render = (jobs) => {
    last = jobs;
    // A job a controller (a watch) started is shown through its owner, not on its own.
    const shown = underInputs(visible(jobs)).filter((j) => !j.owner);
    // A row whose look select has the focus stays as it is: a rebuilt one would close the
    // open dropdown under the pointer.
    const focused = document.activeElement?.matches?.("select.scan-look")
      ? document.activeElement.closest(".job")
      : null;
    const rows = shown.map((j) =>
      focused && focused.dataset.job === j.id && focused.parentNode
        ? focused
        : jobRow(j, async (id, button) => {
        button.disabled = true;
        try {
          await api.op({ op: "job_stop", job: id });
        } catch (e) {
          if (!(e.json && e.status === 422)) transcript.note(`job_stop: ${e.message}`, true);
        }
        refresh();
      }, buttons),
    );
    placeChildren(el, fold(shown, rows, jobs));
    if (countEl) {
      const n = shown.filter((j) => !isFinished(j)).length;
      countEl.textContent = String(n);
      countEl.title = `${n} running`;
    }
    if (shown.length === 0) {
      const empty = document.createElement("div");
      empty.className = "empty";
      empty.textContent = "no jobs";
      el.append(empty);
    }
    if (onJobs) onJobs(jobs);
  };
  const refresh = async () => {
    try {
      const [r, s] = await Promise.all([
        api.op({ op: "job_list" }),
        api.op({ op: "set_list" }).catch(() => null),
      ]);
      if (s) sets = s.value || [];
      const running = (r.value || []).filter((j) => j.kind === "scan" && !isFinished(j));
      await Promise.all(running.map((j) => readScan(j.id)));
      for (const id of [...scans.keys()]) if (!running.some((j) => j.id === id)) scans.delete(id);
      if (!stopped) render(r.value || []);
    } catch {
      /* the next poll retries; op errors appear through the ops stream */
    }
  };
  const loop = async () => {
    if (!paused) await refresh();
    if (!stopped) timer = setTimeout(loop, JOBS_POLL_MS);
  };
  loop();
  return {
    refresh,
    redraw,
    // set_list's entries as last read with the jobs.
    sets: () => sets,
    pause(on) {
      paused = !!on;
    },
    // The idle age at which plan channels fold (JOBS_FOLD_IDLE_MS), drawn again at once.
    foldIdle(ms) {
      foldIdleMs = Number(ms);
      redraw();
    },
    close() {
      stopped = true;
      clearTimeout(timer);
      clearTimeout(ageTimer);
    },
  };
}
