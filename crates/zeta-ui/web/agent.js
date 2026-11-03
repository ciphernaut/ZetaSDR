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
//                               ask to find positioned jobs, capture_list, the Explore pane's poll, and station_get, the home read at load) whose
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
//   startJobs(api, el, transcript, {onJobs, onPlay, onTranscribe, onGraph, playing, squelch,
//                                   view, position, onHot})
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
//       state, id, an `.events` cell (filled by overlays.js: a count, or `—` over its
//       watch cap), on rows of an AUDIO_KINDS kind a
//       `button.play` (▶, or ■ while playing(id) is true) calling onPlay(job) and, while
//       running, a `button.transcribe` (Transcribe) calling onTranscribe(job) (ruling R14), a
//       `button.graph` calling onGraph(job), and a Stop button applying `job_stop`.
//       An attached job (one with an `input` job) is a `.job.attached` row placed right
//       under its input's row, its kind shown as `↳ <kind>`, with neither of the audio
//       buttons. A job's `state.reason` (a failure) is also the row's tooltip.
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
//       label only, run() decides from the mix's graph at the click.
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
//       A job folds only on evidence (foldPlan): an unwatched squelch kind needs a running mix
//       covering its source (activity(id): {watched, speaking, hot}, overlays.js), and a
//       `speaking` or `hot` row never folds.
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
// The row spans whose full text is their title (line two cuts them with an ellipsis).
const TITLED = ["band", "source", "events"];
export const AUDIO_KINDS = ["fm_demod", "wfm_demod", "am_demod", "ssb_demod", "mix"];
export const SQUELCH_KINDS = ["fm_demod", "am_demod"];
export const QUIET_OPS = new Set([
  "job_list",
  "stream_list",
  "source_list",
  "modes_list",
  "modes_show",
  "capture_list",
  "station_get",
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

  // `turnLog` is /ask's `turn_log`: after the answer line, one collapsed `<details
  // class="turn">` per turn that has reasoning or content.
  assistant(text, error = false, turnLog = []) {
    this.#append(text, error ? "assistant error" : "assistant");
    for (const turn of turnLog) {
      if (!turn || (!turn.reasoning && !turn.content)) continue;
      const d = document.createElement("details");
      d.className = "turn";
      const sum = document.createElement("summary");
      // `calls` is the completion's tool-call count (TurnEntry.calls).
      const k = Number.isInteger(turn.calls) ? turn.calls : 0;
      sum.textContent = `turn ${turn.turn} (${plural(k, "call")})`;
      d.append(sum);
      for (const part of [turn.reasoning, turn.content]) {
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

function jobRow(
  job,
  onStop,
  { onPlay, onTranscribe, onGraph, playing, squelch, plan, position, onHot, mixText, mute },
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
  const audio = !attached && AUDIO_KINDS.includes(job.kind);
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
function jobGroup(plan) {
  const g = document.createElement("details");
  g.className = "job-group";
  g.dataset.plan = plan;
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
  g.append(summary);
  return g;
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
    countEl = null,
    lastEvent = null,
    activity = null,
  } = {},
) {
  let timer = null;
  let foldIdleMs = JOBS_FOLD_IDLE_MS;
  // id -> performance.now() when this page first saw the job running (its idle age starts
  // there when no event has been seen).
  const runningSince = new Map();
  const groupEls = new Map(); // plan -> its kept details.job-group
  let ageTimer = null;
  let stopped = false;
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
    onHot,
    mixText,
    mute,
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
  // (b) it is watched (overlays.js, at most MAX_WATCHED = 4 jobs: TODO.md "UI: the page
  //     watches at most 4 jobs"), its squelch known closed and no event for foldIdleMs;
  // (c) it is an unwatched squelch kind (its squelch and events unknown) on a source a
  //     running mix covers, and the mix does not hear it (not `speaking`).
  // Never a `speaking` or `hot` row; an unwatched squelch kind with no covering mix never
  // folds. Every way out of a group (the job stopped, its squelch opened, an event arrived,
  // the mix heard it, the group under JOBS_FOLD_MIN) is this test failing at the next render,
  // one poll at most.
  const foldPlan = (j, now, jobs) => {
    if (isFinished(j) || j.input || j.elastic || !j.tuning || !view) return null;
    const ch = channelLabel(j.tuning.center_hz, view.get("plan"));
    if (!ch) return null;
    const seen = activity ? activity(j.id) : { watched: true, speaking: false, hot: false };
    if (seen.speaking || seen.hot) return null;
    if (SQUELCH_KINDS.includes(j.kind) && !seen.watched) {
      return mixCovering(j, jobs) ? ch.plan : null;
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
    for (const j of shown) {
      if (isFinished(j)) runningSince.delete(j.id);
      else if (!runningSince.has(j.id)) runningSince.set(j.id, now);
    }
    const planOf = new Map();
    const members = new Map(); // plan -> its foldable jobs (not the attached ones)
    for (const j of shown) {
      const plan = foldPlan(j, now, jobs);
      if (!plan) continue;
      planOf.set(j.id, plan);
      if (!members.has(plan)) members.set(plan, []);
      members.get(plan).push(j);
    }
    for (const [plan, list] of members) if (list.length <= JOBS_FOLD_MIN) members.delete(plan);
    for (const plan of [...groupEls.keys()]) if (!members.has(plan)) groupEls.delete(plan);
    const inside = new Map(); // plan -> its rows, in list order
    const out = [];
    shown.forEach((j, i) => {
      const plan = planOf.get(j.input ?? j.id);
      if (!plan || !members.has(plan)) {
        out.push(rows[i]);
        return;
      }
      if (!inside.has(plan)) {
        if (!groupEls.has(plan)) groupEls.set(plan, jobGroup(plan));
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
    return out;
  };
  const render = (jobs) => {
    last = jobs;
    // A job a controller (a watch) started is shown through its owner, not on its own.
    const shown = underInputs(visible(jobs)).filter((j) => !j.owner);
    const rows = shown.map((j) =>
      jobRow(j, async (id, button) => {
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
      const r = await api.op({ op: "job_list" });
      if (!stopped) render(r.value || []);
    } catch {
      /* the next poll retries; op errors appear through the ops stream */
    }
  };
  const loop = async () => {
    await refresh();
    if (!stopped) timer = setTimeout(loop, JOBS_POLL_MS);
  };
  loop();
  return {
    refresh,
    redraw,
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
