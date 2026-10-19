// agent.js: the transcript pane and the jobs list.
//
// Exports:
//   formatOp(payload) -> {text, error}
//       One `ops` stream payload ({op, outcome, result|error, elapsed_ms, transport}) as a
//       transcript line: `source_list → ok (2 sources)`,
//       `job_start kind=measure source=tone center_hz=100000000 … → job 01…`,
//       `job_start … → out_of_range field=center_hz … suggested=…` (error: true; the
//       corrective `suggested` value when the node offers one).
//   class Transcript(el)
//       .op(payload)            append formatOp(payload); a line equal to the last one is
//                               folded into it with a ×N count. Ok outcomes of the read-only
//                               polls (QUIET_OPS: job_list, stream_list, source_list) whose
//                               transport is "ui" (a UI's own calls) are hidden; errors and
//                               other callers' records always show (ruling R6). Records from
//                               another caller are prefixed `[agent]`, `[http]`, `[local]`.
//       .reply(op, result)      a line for an op this page typed, from its /op reply (a
//                               quiet op's ops-stream record is hidden)
//       .user(text)             a request typed by the user
//       .assistant(text, error) an answer from /ask (error: an assistant-error line)
//       .note(text, error)      a UI line (connection state, local errors)
//       .onAssistant(fn)        fn(text, error) is also called for every assistant line
//                               (Task 8 hangs the narrow-footprint ribbons here)
//   startOpsFeed(api, transcript) -> {close}
//       Finds the `ops` stream (the GET /streams entry whose provenance.source is "node")
//       and appends one line per frame; reconnects and re-resolves the id when it drops.
//   startJobs(api, el, transcript, {onJobs, onPlay, onGraph, playing}) -> {close, refresh,
//                                                                         redraw}
//       Polls `job_list` every 2 s and renders the jobs that are not stopped: kind, source,
//       state, id, an `.events` cell (filled by overlays.js), on `fm_demod` rows a
//       `button.play` (▶, or ■ while playing(id) is true) calling onPlay(job), a
//       `button.graph` calling onGraph(job), and a Stop button applying `job_stop`.
//       onJobs(jobs) is called with the whole list after each render; redraw() renders the
//       last list again (after the playing state changed).

export const JOBS_POLL_MS = 2000;
export const QUIET_OPS = new Set(["job_list", "stream_list", "source_list"]);
const MAX_LINES = 500;

function scalar(v) {
  return v === null || ["string", "number", "boolean"].includes(typeof v);
}

// The node serialises op fields alphabetically; these lead, in this order, then the rest.
const LEADING = ["kind", "source", "job", "stream", "center_hz", "bandwidth_hz", "deviation_hz"];

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
    parts.push(scalar(v) ? `${k}=${v}` : `${k}=…`);
  }
  return parts.join(" ");
}

function plural(n, word) {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

function resultText(r) {
  if (!r || typeof r !== "object") return "ok";
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
    case "labels":
      return `ok (${plural(Array.isArray(v) ? v.length : 0, "label")})`;
    case "iq":
      return `ok (${plural(v && v.samples ? v.samples : 0, "sample")})`;
    default:
      return `ok (${r.result})`;
  }
}

function errorText(e) {
  if (!e || typeof e !== "object") return "error";
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

  #append(text, classes, title) {
    const stick = this.el.scrollTop + this.el.clientHeight >= this.el.scrollHeight - 4;
    const last = this.last;
    if (last && last.text === text && last.classes === classes) {
      last.count += 1;
      last.countEl.textContent = ` ×${last.count}`;
      if (title) last.line.title = title;
    } else {
      const line = document.createElement("div");
      line.className = "line " + classes;
      const body = document.createElement("span");
      body.textContent = text;
      const countEl = document.createElement("span");
      countEl.className = "count";
      line.append(body, countEl);
      if (title) line.title = title;
      this.el.append(line);
      this.last = { text, classes, count: 1, line, countEl };
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

  assistant(text, error = false) {
    this.#append(text, error ? "assistant error" : "assistant");
    for (const fn of this.listeners) fn(text, error);
  }

  note(text, error = false) {
    this.#append(text, error ? "note error" : "note");
  }

  onAssistant(fn) {
    this.listeners.push(fn);
  }
}

async function opsStreamId(api) {
  const list = await api.streams();
  const s = list.find((i) => i.provenance && i.provenance.source === "node");
  return s ? s.id : null;
}

export function startOpsFeed(api, transcript) {
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
        if (msg.type === "frame") transcript.op(msg.payload);
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

function jobRow(job, onStop, { onPlay, onGraph, playing }) {
  const row = document.createElement("div");
  row.className = "job";
  row.dataset.id = job.id;
  const status = job.state && job.state.status ? job.state.status : "?";
  row.dataset.state = status;
  const cells = [
    ["kind", job.kind],
    ["source", job.source],
    ["state", status],
    ["id", job.id.slice(-6)],
    ["events", ""],
  ];
  for (const [cls, text] of cells) {
    const span = document.createElement("span");
    span.className = cls;
    span.textContent = text;
    row.append(span);
  }
  if (job.state && job.state.reason) row.title = job.state.reason;
  const actions = document.createElement("span");
  actions.className = "actions";
  if (job.kind === "fm_demod" && onPlay) {
    const on = playing ? playing(job.id) : false;
    const play = document.createElement("button");
    play.className = "play";
    play.textContent = on ? "■" : "▶";
    play.title = on ? "stop the audio" : "play the audio";
    play.setAttribute("aria-pressed", String(on));
    play.onclick = () => onPlay(job);
    actions.append(play);
  }
  if (onGraph) {
    const graph = document.createElement("button");
    graph.className = "graph";
    graph.textContent = "graph";
    graph.title = `graph_get ${job.id}`;
    graph.onclick = () => onGraph(job);
    actions.append(graph);
  }
  const stop = document.createElement("button");
  stop.className = "stop";
  stop.textContent = "Stop";
  stop.title = `job_stop ${job.id}`;
  stop.onclick = () => onStop(job.id, stop);
  actions.append(stop);
  row.append(actions);
  return row;
}

export function startJobs(
  api,
  el,
  transcript,
  { onJobs = null, onPlay = null, onGraph = null, playing = null } = {},
) {
  let timer = null;
  let stopped = false;
  let last = null;
  const buttons = { onPlay, onGraph, playing };
  const render = (jobs) => {
    last = jobs;
    const shown = jobs.filter((j) => !(j.state && j.state.status === "stopped"));
    el.replaceChildren(
      ...shown.map((j) =>
        jobRow(j, async (id, button) => {
          button.disabled = true;
          try {
            await api.op({ op: "job_stop", job: id });
          } catch (e) {
            if (!(e.json && e.status === 422)) transcript.note(`job_stop: ${e.message}`, true);
          }
          refresh();
        }, buttons),
      ),
    );
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
    redraw() {
      if (last && !stopped) render(last);
    },
    close() {
      stopped = true;
      clearTimeout(timer);
    },
  };
}
