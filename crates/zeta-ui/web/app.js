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
// Source tabs come from `source_list` (id and tuning range); the selection is kept in
// `sessionStorage["zeta.source"]`. The command line (#cmd, focused by Ctrl-K / ⌘K): text
// that parses as a JSON object with an `op` key is applied with `api.op` (its outcome reaches
// the transcript through the `ops` stream); anything else goes to `api.ask`.
//
// The waterfall (waterfall.js) is mounted in #waterfall at login and follows the selected
// source (selecting the source it already shows does nothing); it is destroyed when the node
// rejects the secret. The overlays (overlays.js) sit on it: each jobs poll hands them the job
// list (they watch the selected source's running jobs), and in the narrow footprint every
// /ask answer also becomes an `agent` ribbon at the centre of the band shown.
//
// A drag on the waterfall opens the context menu (menu.js), whose entries go through the same
// op and /ask paths as the command line.
//
// Job rows: ▶ on an `fm_demod` row plays its audio (audio.js; one job at a time, ■ stops it,
// and it stops when the job leaves the running list or the secret is rejected); `graph` opens
// the job's graph view (graph.js).
//
// Exports `state` ({api, transcript, jobs, wf, ov, menu, audio, sources, selected}) and `onSourceChange(fn)` for
// the later views (waterfall, audio, voice).

import { connect, Unauthorized } from "./api.js";
import { Transcript, startJobs, startOpsFeed } from "./agent.js";
import { mountWaterfall } from "./waterfall.js";
import { mountOverlays } from "./overlays.js";
import { mountMenu } from "./menu.js";
import { playJob } from "./audio.js";
import { showGraph } from "./graph.js";

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
  sources: [],
  selected: null,
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
      tab.append(name, range);
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
// transcript through the ops stream. The result JSON, or null when it failed.
async function applyOp(op) {
  const t = state.transcript;
  if (!state.api) return null;
  // Its ops-stream record shows even when it is one of the hidden polls (ruling R5).
  t.typed(op.op);
  let result = null;
  try {
    result = await state.api.op(op);
  } catch (e) {
    if (notApplied(e)) {
      t.untyped(op.op);
      t.note(`${op.op}: ${e.message}`, true);
    }
  }
  state.jobs?.refresh();
  return result;
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
  const op = parseOp(text);
  if (op) {
    state.transcript.user(text);
    await applyOp(op);
  } else {
    await askAgent(text);
  }
}

function wireCommandLine() {
  const cmd = $("#cmd");
  cmd.addEventListener("keydown", (ev) => {
    if (ev.key === "Enter") {
      const text = cmd.value.trim();
      if (!text) return;
      history.push(text);
      historyAt = history.length;
      cmd.value = "";
      runCommand(text);
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
  state.ops = startOpsFeed(api, state.transcript);
  state.wf = mountWaterfall($("#waterfall"), api);
  state.ov = mountOverlays($("#waterfall"), state.wf, api, { jobsEl: $("#jobs") });
  state.ov.setSource(state.selected);
  state.menu = mountMenu(state.wf, {
    applyOp,
    ask: askAgent,
    transcript: state.transcript,
    ov: state.ov,
  });
  state.transcript.onAssistant((text, error) => {
    if (!error && footprint() === "narrow") answerRibbon(text);
  });
  state.jobs = startJobs(api, $("#jobs"), state.transcript, {
    onJobs: (jobs) => {
      if (state.ov) state.ov.sync(jobs);
      stopAudioUnlessRunning(jobs);
    },
    onPlay: toggleAudio,
    onGraph: (job) => showGraph(api, job.id),
    playing: (id) => !!state.audio && state.audio.jobId === id,
  });
  loadSources();
}

// The node rejected the secret after login: stop everything using it and ask again.
function logout(api) {
  if (state.api !== api) return;
  state.api = null;
  api.onUnauthorized = null;
  stopAudio();
  if (state.ops) state.ops.close();
  if (state.jobs) state.jobs.close();
  if (state.menu) state.menu.destroy();
  if (state.ov) state.ov.destroy();
  if (state.wf) state.wf.destroy();
  state.ops = null;
  state.jobs = null;
  state.ov = null;
  state.menu = null;
  state.wf = null;
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
// the waterfall follows ({sourceId, jobId, painted, badge}, or null without a waterfall), and
// draw the narrow-footprint answer ribbon for `text` without an LLM (ruling R3).
window.zetaTest = {
  ribbon(text) {
    answerRibbon(text);
  },
  setSecret(s) {
    if (state.api) state.api.secret = s;
  },
  poll() {
    return state.jobs ? state.jobs.refresh() : Promise.resolve();
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
