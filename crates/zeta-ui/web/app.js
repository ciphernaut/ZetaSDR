// app.js: the web app's entry point.
//
// Boot: without `sessionStorage["zeta.secret"]` the login box shows (secret field, Connect).
// Connect calls GET /health with the bearer: a wrong secret shows "secret rejected", stores
// nothing and opens no websocket. A stored secret that the node now rejects is dropped and
// the login box shows.
//
// Footprint: `wide` or `narrow` on <html data-footprint>, narrow below NARROW_MAX_PX of
// viewport width; `?footprint=wide|narrow` forces it. Re-evaluated on resize. In narrow the
// side pane is a drawer opened by #drawer-toggle (<html data-drawer="open">).
//
// Source tabs come from `source_list` (id and tuning range; in narrow, id and a compact
// centre in MHz such as `144.8`); the selection is kept in
// `sessionStorage["zeta.source"]`. The command line (#cmd, focused by Ctrl-K / ⌘K): text
// that parses as a JSON object with an `op` key is applied with `api.op` (its outcome reaches
// the transcript through the `ops` stream); text starting with `view ` is a view command
// (view.js: `view set <key> <value>`, `view show`, `view reset`, `view clear`), handled
// here before both, its answer a transcript line; anything else goes to `api.ask`.
//
// Conflicts (spec 4.5): a `conflict` reply to an op this page applied (command line, menu),
// or one the ops stream shows for another caller, adds a line naming each holder by kind
// and tuning from the last job list (`conflict: source S held by passthrough <id> @ 100.000
// MHz`) with one button per holder, `stop <kind> <short id> and retry`, which applies the
// same op again with `replace: [<id>]` (conflictActions).
//
// The coarse layer (coarse.js) is mounted on the waterfall at login: a view past the fine
// capture starts the page's own scan on a free scanner source (view policies zoom_out,
// auto_scan_stop, scanner); its notes become transcript lines.
//
// The waterfall (waterfall.js) is mounted in #waterfall at login and follows the selected
// source (selecting the source it already shows does nothing); it is destroyed when the node
// rejects the secret. Each jobs poll is handed to it too (wf.syncJobs: a scan starting or
// ending on the shown source), and its notices (a refused retune) become transcript lines.
// The overlays (overlays.js) sit on it: each jobs poll hands them the job
// list (they watch the selected source's running jobs), and in the narrow footprint every
// /ask answer also becomes an `agent` ribbon at the centre of the band shown.
//
// A drag on the waterfall opens the context menu (menu.js), whose entries go through the same
// op and /ask paths as the command line.
//
// Node check: every NODE_CHECK_MS the page polls GET /health and reloads itself once when the
// node's `build` is not the one it connected to, or its `uptime_s` went backwards (a restart):
// this document would otherwise keep running old code against the new node (its stored secret
// logs the new document in again).
//
// Job rows: ▶ on a row of an audio kind (agent.js AUDIO_KINDS) plays its audio (audio.js; one
// job at a time, ■ stops it, and it stops when the job leaves the running list or the secret
// is rejected); Transcribe on a running one applies `job_attach {kind: "transcribe", job,
// language: null}` through the command-line op path (its outcome, an error too, reaches the
// transcript); `graph` opens the job's graph view (graph.js).
//
// The mic button (#mic, voice.js) is mounted at login: its transcript is put in the command
// line and submitted as if typed (so free text goes to /ask); #mic-status beside it shows
// failures such as a node without a speech daemon.
//
// Exports `state` ({api, transcript, jobs, wf, coarse, ov, menu, audio, mic, sources,
// selected, view, lastJobs}), `onSourceChange(fn)` for the later views (waterfall, audio,
// voice), and `conflictActions(err, retry)`.

import { connect, Unauthorized } from "./api.js";
import { QUIET_OPS, Transcript, startJobs, startOpsFeed } from "./agent.js";
import { mountWaterfall } from "./waterfall.js";
import { mountOverlays } from "./overlays.js";
import { mountMenu } from "./menu.js";
import { playJob } from "./audio.js";
import { closeGraph, showGraph } from "./graph.js";
import { mountMic } from "./voice.js";
import { mountView, runViewCommand } from "./view.js";
import { mountCoarse } from "./coarse.js";

export const NARROW_MAX_PX = 700;
const SECRET_KEY = "zeta.secret";
const SOURCE_KEY = "zeta.source";

export const state = {
  api: null,
  transcript: null,
  jobs: null,
  ops: null,
  wf: null,
  ov: null,
  menu: null,
  audio: null, // {jobId, player}
  mic: null,
  sources: [],
  selected: null,
  view: null, // view.js policies
  coarse: null, // coarse.js: the automatic scans of a view past the capture
  lastJobs: [], // the last job_list result
};
const sourceListeners = [];

export function onSourceChange(fn) {
  sourceListeners.push(fn);
}

const $ = (sel) => document.querySelector(sel);

function store(key, value) {
  try {
    if (value === null) sessionStorage.removeItem(key);
    else sessionStorage.setItem(key, value);
  } catch {
    /* storage unavailable: the app still works for this page load */
  }
}

function load(key) {
  try {
    return sessionStorage.getItem(key);
  } catch {
    return null;
  }
}

// ---- footprint -------------------------------------------------------------------------

function footprint() {
  const forced = new URLSearchParams(location.search).get("footprint");
  if (forced === "wide" || forced === "narrow") return forced;
  return window.innerWidth < NARROW_MAX_PX ? "narrow" : "wide";
}

function applyFootprint() {
  const root = document.documentElement;
  const fp = footprint();
  if (root.dataset.footprint !== fp) {
    root.dataset.footprint = fp;
    delete root.dataset.drawer;
  }
}

// ---- login -----------------------------------------------------------------------------

function showLogin(message = "") {
  $("#app").hidden = true;
  $("#login").hidden = false;
  $("#login-error").textContent = message;
  $("#secret").focus();
}

let connecting = false;

async function login(secret, { remembered = false } = {}) {
  if (connecting || state.api) return;
  connecting = true;
  $("#connect").disabled = true;
  let api;
  try {
    api = await connect(secret);
  } catch (e) {
    if (e instanceof Unauthorized) {
      store(SECRET_KEY, null);
      showLogin(remembered ? "stored secret rejected; enter it again" : "secret rejected");
    } else {
      showLogin(`cannot reach the node: ${e.message}`);
    }
    return;
  } finally {
    connecting = false;
    $("#connect").disabled = false;
  }
  store(SECRET_KEY, secret);
  $("#secret").value = "";
  start(api);
}

// ---- sources ---------------------------------------------------------------------------

function hz(cap) {
  if (typeof cap === "number") return cap;
  if (cap && typeof cap === "object") {
    const v = Object.values(cap).find((x) => typeof x === "number");
    return v === undefined ? null : v;
  }
  return null;
}

function mhz(v) {
  return (v / 1e6).toFixed(3);
}

// The centre shown on a narrow tab, in MHz without trailing zeros (`144.8`): the source's
// current tuning, else the middle of its tune range; "" when neither is known.
function centreText(src) {
  const c = src.capabilities || {};
  let v = src.current && typeof src.current.center_hz === "number" ? src.current.center_hz : null;
  if (v === null) {
    const lo = hz(c.tune_min_hz);
    const hi = hz(c.tune_max_hz);
    if (lo !== null && hi !== null) v = (lo + hi) / 2;
  }
  return v === null ? "" : mhz(v).replace(/\.?0+$/, "");
}

function rangeText(src) {
  const c = src.capabilities || {};
  const lo = hz(c.tune_min_hz);
  const hi = hz(c.tune_max_hz);
  return lo === null || hi === null ? "" : `${mhz(lo)}–${mhz(hi)} MHz`;
}

function select(id) {
  state.selected = id;
  store(SOURCE_KEY, id);
  for (const tab of document.querySelectorAll(".src-tab")) {
    tab.setAttribute("aria-selected", String(tab.dataset.id === id));
  }
  const src = state.sources.find((s) => s.id === id) || null;
  for (const fn of sourceListeners) fn(src);
}

function renderTabs(sources) {
  const tabs = $("#tabs");
  tabs.replaceChildren(
    ...sources.map((s) => {
      const tab = document.createElement("button");
      tab.className = "src-tab";
      tab.setAttribute("role", "tab");
      tab.dataset.id = s.id;
      const name = document.createElement("span");
      name.className = "name";
      name.textContent = s.id;
      const range = document.createElement("span");
      range.className = "range";
      range.textContent = rangeText(s);
      const centre = document.createElement("span");
      centre.className = "centre";
      centre.textContent = centreText(s);
      tab.append(name, range, centre);
      if (s.fault) tab.classList.add("fault");
      tab.title = s.fault ? `fault: ${JSON.stringify(s.fault)}` : rangeText(s);
      tab.onclick = () => select(s.id);
      return tab;
    }),
  );
  const saved = load(SOURCE_KEY);
  const pick = sources.find((s) => s.id === saved) || sources[0];
  if (pick) select(pick.id);
}

async function loadSources() {
  try {
    const r = await state.api.op({ op: "source_list" });
    state.sources = r.value || [];
    renderTabs(state.sources);
  } catch (e) {
    state.transcript.note(`source_list: ${e.message}`, true);
  }
}

// ---- command line ----------------------------------------------------------------------

const history = [];
let historyAt = 0;

function parseOp(text) {
  if (!text.startsWith("{")) return null;
  try {
    const v = JSON.parse(text);
    return v && typeof v === "object" && !Array.isArray(v) && "op" in v ? v : null;
  } catch {
    return null;
  }
}

// Whether a failed /op never reached the node's apply (and so is not on the ops stream).
function notApplied(e) {
  if (!e.json || e.status !== 422) return true;
  return e.json.error === "other" && /^invalid operation/.test(e.json.message || "");
}

// Applies `op` as typed by the user (command line or context menu); its outcome reaches the
// transcript through the ops stream, except a successful quiet op (job_list, stream_list,
// source_list), whose "ui" record is hidden with the polls (ruling R6): that one is shown
// from the /op reply. A `conflict` also gets its offer line (offerReplace). The result
// JSON, or null when it failed.
async function applyOp(op) {
  const t = state.transcript;
  if (!state.api) return null;
  let result = null;
  try {
    result = await state.api.op(op);
    if (QUIET_OPS.has(op.op)) t.reply(op, result);
  } catch (e) {
    if (notApplied(e)) t.note(`${op.op}: ${e.message}`, true);
    else if (e.json && e.json.error === "conflict") await offerReplace(op, e.json);
  }
  state.jobs?.refresh();
  return result;
}

// ---- conflicts -------------------------------------------------------------------------

// Ops that take `replace` (a conflict on them can be retried with the holders named).
const REPLACEABLE = new Set(["job_start", "source_ensure"]);

// `<kind> <id> @ <centre MHz>` for holder `id`, from the last job list (the conflict's
// `current` centre when the job is not in it).
function holderText(id, err) {
  const j = state.lastJobs.find((x) => x.id === id);
  const t = (j && j.tuning) || err.current;
  const at = t && typeof t.center_hz === "number" ? ` @ ${mhz(t.center_hz)} MHz` : "";
  return `${j ? j.kind : "job"} ${id}${at}`;
}

function conflictText(err) {
  const holders = Array.isArray(err.holders) ? err.holders : [];
  const who = holders.length ? holders.map((h) => holderText(h, err)).join(", ") : "nobody";
  return `conflict: source ${err.source ?? "?"} held by ${who}`;
}

// One action per holder of a `conflict` error: `stop <kind> <short id> and retry`, whose
// run calls retry([id]) (the original op again with `replace: [id]`).
export function conflictActions(err, retry) {
  const holders = err && Array.isArray(err.holders) ? err.holders : [];
  return holders.map((id) => {
    const j = state.lastJobs.find((x) => x.id === id);
    return {
      label: `stop ${j ? j.kind : "job"} ${id.slice(-6)} and retry`,
      run: () => retry([id]),
    };
  });
}

// The conflict line for `op` (refused with `err`), with its retry buttons when the op
// takes `replace`. The job list is refreshed first so the holders' kinds are known.
async function offerReplace(op, err) {
  if (state.jobs) await state.jobs.refresh();
  const actions = REPLACEABLE.has(op.op)
    ? conflictActions(err, (replace) => applyOp({ ...op, replace }))
    : [];
  state.transcript?.note(conflictText(err), true, actions);
}

// The op attaching a transcription to job `id` (the language left to the backend).
function attachOp(id) {
  return { op: "job_attach", kind: "transcribe", job: id, language: null };
}

// Sends `text` to /ask and shows the answer (or the error, e.g. no LLM configured).
async function askAgent(text) {
  const t = state.transcript;
  if (!state.api) return;
  t.user(text);
  try {
    const r = await state.api.ask(text);
    t.assistant(r.answer ?? "(no answer)");
  } catch (e) {
    t.assistant(e.message, true);
  }
  state.jobs?.refresh();
}

async function runCommand(text) {
  if (!state.api) return;
  if (text === "view" || text.startsWith("view ")) {
    state.transcript.user(text);
    const r = runViewCommand(state.view, text.split(/\s+/).slice(1));
    state.transcript.note(r.text, r.error);
    return;
  }
  const op = parseOp(text);
  if (op) {
    state.transcript.user(text);
    await applyOp(op);
  } else {
    await askAgent(text);
  }
}

// Submits what the command line holds (Enter, or a voice transcript put there).
function submitCommand() {
  const cmd = $("#cmd");
  const text = cmd.value.trim();
  if (!text) return;
  history.push(text);
  historyAt = history.length;
  cmd.value = "";
  runCommand(text);
}

function wireCommandLine() {
  const cmd = $("#cmd");
  cmd.addEventListener("keydown", (ev) => {
    if (ev.key === "Enter") {
      submitCommand();
    } else if (ev.key === "ArrowUp" && historyAt > 0) {
      historyAt -= 1;
      cmd.value = history[historyAt];
      ev.preventDefault();
    } else if (ev.key === "ArrowDown" && historyAt < history.length) {
      historyAt += 1;
      cmd.value = history[historyAt] ?? "";
      ev.preventDefault();
    }
  });
  document.addEventListener("keydown", (ev) => {
    if ((ev.ctrlKey || ev.metaKey) && ev.key.toLowerCase() === "k") {
      ev.preventDefault();
      cmd.focus();
    }
  });
}

// ---- start -----------------------------------------------------------------------------

function start(api) {
  if (state.api) return;
  state.api = api;
  api.onUnauthorized = () => logout(api);
  $("#login").hidden = true;
  $("#app").hidden = false;
  $("#transcript").replaceChildren();
  state.transcript = new Transcript($("#transcript"));
  api.onBusy = (message) =>
    state.transcript?.note(`stream refused: ${message} (node out of stream slots)`, true);
  state.view = mountView();
  state.ops = startOpsFeed(api, state.transcript, {
    onConflict: (p) => offerReplace(p.op, p.error),
  });
  state.wf = mountWaterfall($("#waterfall"), api);
  state.wf.onNotice = (text) => state.transcript?.note(text, true);
  state.coarse = mountCoarse(state.wf, api, state.view, {
    transcript: state.transcript,
    sources: () => state.sources,
    jobs: () => state.lastJobs,
  });
  state.ov = mountOverlays($("#waterfall"), state.wf, api, { jobsEl: $("#jobs") });
  state.ov.setSource(state.selected);
  state.menu = mountMenu(state.wf, {
    applyOp,
    attachOp,
    ask: askAgent,
    transcript: state.transcript,
    ov: state.ov,
  });
  state.transcript.onAssistant((text, error) => {
    if (!error && footprint() === "narrow") answerRibbon(text);
  });
  state.nodeCheck = startNodeCheck(api);
  state.jobs = startJobs(api, $("#jobs"), state.transcript, {
    view: state.view,
    onJobs: (jobs) => {
      state.lastJobs = jobs;
      if (state.ov) state.ov.sync(jobs);
      if (state.wf) state.wf.syncJobs(jobs);
      stopAudioUnlessRunning(jobs);
    },
    onPlay: toggleAudio,
    onTranscribe: (job) => applyOp(attachOp(job.id)),
    onGraph: (job) => showGraph(api, job.id),
    playing: (id) => !!state.audio && state.audio.jobId === id,
  });
  state.mic = mountMic($("#mic"), api, (text) => {
    $("#cmd").value = text;
    submitCommand();
  });
  loadSources();
}

// The node rejected the secret after login: stop everything using it and ask again.
// ---- node check ------------------------------------------------------------------------

export const NODE_CHECK_MS = 5000;

// Every NODE_CHECK_MS, GET /health: when the node's `build` differs from the one connect()
// saw, or its `uptime_s` went backwards (a restart), this document is stale code talking to
// a new node, so it reloads itself once (the stored secret logs the new document in again).
// A node that cannot be reached is the waterfall badge's business, not this check's; an
// older node without `build` is only checked for a restart. Returns {check, close}: check()
// runs one check now (the e2e hook) and resolves true when a reload was started.
function startNodeCheck(api) {
  let timer = null;
  let reloading = false;
  async function check() {
    if (reloading || state.api !== api) return false;
    let h;
    try {
      h = await api.health();
    } catch {
      return false;
    }
    if (state.api !== api) return false;
    const upgraded = api.build !== null && typeof h.build === "string" && h.build !== api.build;
    const restarted =
      api.uptimeS !== null && typeof h.uptime_s === "number" && h.uptime_s < api.uptimeS;
    if (typeof h.uptime_s === "number") api.uptimeS = h.uptime_s;
    if (!upgraded && !restarted) return false;
    reloading = true;
    state.transcript?.note(
      upgraded ? `node upgraded (build ${h.build}): reloading` : "node restarted: reloading",
      true,
    );
    location.reload();
    return true;
  }
  timer = setInterval(check, NODE_CHECK_MS);
  return {
    check,
    close() {
      clearInterval(timer);
      timer = null;
    },
  };
}

function logout(api) {
  if (state.api !== api) return;
  state.api = null;
  api.onUnauthorized = null;
  api.onBusy = null;
  if (state.nodeCheck) state.nodeCheck.close();
  state.nodeCheck = null;
  stopAudio();
  closeGraph();
  if (state.mic) state.mic.destroy();
  if (state.ops) state.ops.close();
  if (state.jobs) state.jobs.close();
  if (state.menu) state.menu.destroy();
  if (state.ov) state.ov.destroy();
  if (state.coarse) state.coarse.destroy();
  if (state.wf) state.wf.destroy();
  state.ops = null;
  state.jobs = null;
  state.ov = null;
  state.coarse = null;
  state.menu = null;
  state.wf = null;
  state.mic = null;
  store(SECRET_KEY, null);
  showLogin("secret rejected");
}

// ---- audio -----------------------------------------------------------------------------

function stopAudio() {
  const a = state.audio;
  state.audio = null;
  if (a) a.player.stop();
}

// ▶ on a job row: plays that job (stopping any other); ■ on the playing row stops it.
function toggleAudio(job) {
  const same = state.audio && state.audio.jobId === job.id;
  stopAudio();
  if (!same && state.api) {
    const entry = { jobId: job.id, player: null };
    state.audio = entry;
    entry.player = playJob(state.api, job, {
      onError: (message) => state.transcript.note(message, true),
      onEnd: () => {
        if (state.audio === entry) state.audio = null;
        state.jobs?.redraw();
      },
    });
  }
  state.jobs?.redraw();
}

function stopAudioUnlessRunning(jobs) {
  const a = state.audio;
  if (!a) return;
  const j = jobs.find((x) => x.id === a.jobId);
  if (!j || !j.state || j.state.status !== "running") stopAudio();
}

// An `agent` ribbon at the centre of the band the waterfall shows (the middle when unknown).
function answerRibbon(text) {
  if (!state.ov) return null;
  return state.ov.ribbon(state.wf ? state.wf.centerHz() : null, text, "agent");
}

// Hooks for the e2e scenarios: simulate a changed secret, force a jobs poll, and read what
// the waterfall follows ({sourceId, jobId, painted, badge}, or null without a waterfall),
// what the coarse layer follows (`coarse()`: {scanJobId, followed, source, rows}), the
// format of the last painted spectrum row (`lastRowFormat`: "q" or "db", null before one), its
// mode (`waterfallMode`: "spectrum" or "scan", null without a waterfall),
// draw the narrow-footprint answer ribbon for `text` without an LLM (ruling R3), and draw a
// probe label at a frequency.
window.zetaTest = {
  get lastRowFormat() {
    return state.wf ? state.wf.lastRowFormat : null;
  },
  get waterfallMode() {
    return state.wf ? state.wf.mode : null;
  },
  ribbon(text) {
    answerRibbon(text);
  },
  // A layer-1 overlay label `name` at `hz` (key "probe"); the element, or null without overlays.
  label(hz, name) {
    return state.ov ? state.ov.label(hz, { layer: 1, name }, "probe") : null;
  },
  setSecret(s) {
    if (state.api) state.api.secret = s;
  },
  poll() {
    return state.jobs ? state.jobs.refresh() : Promise.resolve();
  },
  // The node build this document connected to (null before login or on an older node).
  build() {
    return state.api ? state.api.build : null;
  },
  // Run one node check now; resolves true when it started a reload.
  nodeCheck() {
    return state.nodeCheck ? state.nodeCheck.check() : Promise.resolve(false);
  },
  // The coarse layer: {scanJobId, followed, source, rows}, or null without one.
  coarse() {
    const c = state.coarse;
    if (!c) return null;
    return { scanJobId: c.scanJobId, followed: c.followed, source: c.source, rows: c.rows };
  },
  wf() {
    const wf = state.wf;
    if (!wf) return null;
    return { sourceId: wf.sourceId, jobId: wf.jobId, painted: wf.painted, badge: !wf.badge.hidden };
  },
};

function boot() {
  applyFootprint();
  onSourceChange((src) => {
    // Re-selecting the shown source keeps its job and columns.
    if (state.wf && state.wf.sourceId !== (src ? src.id : null)) state.wf.setSource(src);
    if (state.ov) {
      state.ov.setSource(src ? src.id : null);
      state.jobs?.refresh();
    }
  });
  wireCommandLine();
  window.addEventListener("resize", applyFootprint);
  $("#drawer-toggle").addEventListener("click", () => {
    const root = document.documentElement;
    if (root.dataset.drawer === "open") delete root.dataset.drawer;
    else root.dataset.drawer = "open";
  });
  $("#login-form").addEventListener("submit", (ev) => {
    ev.preventDefault();
    const secret = $("#secret").value;
    if (secret) login(secret);
  });
  const saved = load(SECRET_KEY);
  if (saved) login(saved, { remembered: true });
  else showLogin();
}

boot();
