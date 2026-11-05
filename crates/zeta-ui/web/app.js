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
// Wide, each side pane's header bar (`.pane-head`) folds it (view policy `side`, applySide);
// both folded narrow the column to --side-collapsed-w. Narrow, the bars are disabled and
// read expanded (the drawer shows both panes). The Jobs header carries the running
// count (`.count`, set by agent.js startJobs).
//
// Source tabs come from `source_list` (id and tuning range; in narrow, id and a compact
// centre in MHz such as `144.8`); the selection is kept in
// `sessionStorage["zeta.source"]`. The command line (#cmd, focused by Ctrl-K / ⌘K): text
// that parses as a JSON object with an `op` key is applied with `api.op` (its outcome reaches
// the transcript through the `ops` stream); `view` alone or followed by `set`, `show`,
// `reset` or `clear` is a view command (view.js: `view set <key> <value>`, `view show`,
// `view reset`, `view clear`), handled here before both, its answer a transcript line;
// `source pin [<center_hz> <bandwidth_hz>]`, `source unpin` and `source gain <db>|auto` act
// on the shown source the same way (runSourceCommand: `source_pin` at its `current` capture or
// the band given, `source_unpin`, `source_set_gain`), their reply or error a transcript line;
// anything else (other text starting "view " or "source " included) goes to `api.ask`.
//
// Conflicts (spec 4.5): a `conflict` reply to an op this page applied (command line, menu,
// a dragged outline's `job_retune`), or one the ops stream shows for another caller, adds a
// line naming each holder by kind and tuning from the last job list (`conflict: source S
// held by passthrough <id> @ 100.000 MHz`) with one button per holder, `stop <kind> <short
// id> and retry`, which applies the same op again with `replace: [<id>]` (conflictActions).
// A pin is never a holder: a conflict with none on a pinned source names the node's `unpin
// source S (source_unpin)` option and offers `unpin S and retry` (`source_unpin`, then the
// same op).
//
// Tabs: a pinned source's tab carries a `pin` tag after its range and the title `pin
// <lo>–<hi> MHz; gain <db> dB` (`gain automatic`; no gain part without adjustable gain),
// kept up to date by each `source_list` poll.
//
// The coarse layer (coarse.js) is mounted on the waterfall at login: a view past the fine
// capture starts the page's own scan on a free scanner source (view policies zoom_out,
// auto_scan_stop, scanner); its notes become transcript lines, and each frame of the scan it
// follows is handed to the waterfall's coarse layer (wf.addCoarseEvent).
//
// Following the capture: the waterfall's `follow` policy is the view's (wf.follow), and each
// jobs poll is followed by a `source_list` (state.sources, then wf.syncSources): a shown
// source whose `current` moved eases the view there under `follow target`.
//
// The waterfall (waterfall.js) is mounted in #waterfall at login and follows the selected
// source (selecting the source it already shows does nothing); it is destroyed when the node
// rejects the secret. Each jobs poll is handed to it too (wf.syncJobs: a scan starting or
// ending on the shown source), and its notices (a refused retune) become transcript lines.
// The overlays (overlays.js) sit on it: each jobs poll hands them the job
// list (they watch the selected source's running jobs and outline its rigid holders' bands;
// in the wide footprint a retunable job's outline is dragged to retune it through applyOp),
// and in the narrow footprint every
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
// Listen to all (#listen-all, and the typed `mix` or `mix ID[,ID]` with every id a source):
// `mix_start` (null: every source) and plays the mix it answers, a single stream of every
// channel's audio (listen()). The AudioContext is made in the click, before the op is awaited
// (browsers tie autoplay to the gesture), and given to audio.js. Pressed while a mix plays it
// stops (aria-pressed and its text follow the audio). The mix is a job with no band, so its
// row has no outline: a MixWatch (mix.js) keeps one event socket on the playing (else the
// newest) mix, its row's `.mix-now` cell says who is talking or was last, and
// ov.setSpeaking lights the jobs it hears. A demodulator's row and outline menu offer
// `Mute in mix` while a running mix covers its source (muteFor). Other text starting `mix` goes to the agent.
//
// The map (map.js) is mounted in #map at login: the overlays hand it every event of the jobs
// they watch and of the positioned jobs on the other sources (ov.onEvent), each jobs poll drops the tracks of jobs that ended (map.sync), and
// a job row with a fix shows its distance and bearing from the view's `home` (map.rowText).
// With view `map on` <html data-map="on"> is set: in the wide footprint the map is a pane
// beside the waterfall; in the narrow one #map-toggle (`map`, beside the source tabs) shows
// it in the waterfall's place (<html data-pane="map">) and back.
//
// The subjects (subjects.js): the store is mounted before the overlays, which hand it every
// event first and ask it for mode rows; each jobs poll drops the subjects of jobs that ended.
// Its pane, #subjects, is the Subjects tab of the pane strip (panes.js, #panes beside the
// source tabs): the tab shows while a running job's row has table columns or while it is
// selected, and the pane shows in #pane under the waterfall while the view's `pane` is
// `subjects` (`view set subjects on|off` is an alias). In the narrow footprint the pane is a
// tab in the waterfall's place (<html data-pane="subjects">) that the strip's tab opens and closes.
// The Bits pane (bits.js, #bits) is the strip's `bits` tab, eligible while a running job has a
// slicer stream; Explore's Work the bits and a job row's Bits button select it on that job. A
// `bits` job runs on source `node`, so it draws no outline (overlays.js outlines only the
// selected source's jobs).
//
// The mic button (#mic, voice.js) is mounted at login: its transcript is put in the command
// line and submitted as if typed (so free text goes to /ask); #mic-status beside it shows
// failures such as a node without a speech daemon.
//
// Exports `state` ({api, transcript, jobs, wf, coarse, subjects, subjectsPane, panes, ov, map, menu,
// audio, mic, sources, selected, view, lastJobs, allowStops, allowDeletes}), `onSourceChange(fn)` for the later views (waterfall, audio,
// voice), `conflictActions(err, retry)`, `runSourceCommand(state, words)` and
// `runPlanCommand(state, words)`.
//
// Typed commands beside `view` and `source`: `allow stops on|off` (state.allowStops) and
// `allow deletes on|off` (state.allowDeletes), both off at load; every following /ask carries
// `allow: {stop_jobs, replace, delete_captures}` of them; an answer from a run that may stop
// jobs ends " [may stop jobs]" (" [may delete captures]" likewise), each turn of its `turn_log`
// (a failed run's too) is a collapsed `details.turn` with that turn's tool calls, and a refused
// call (`not_allowed`) adds a note naming what was refused with the action `allow … and ask
// again`, which re-sends that one request with the refused flags true and leaves the switches
// as they were; the note's buttons disable once clicked), `stop all` (a `jobs_stop` of every job but the
// page's spectrum) and `plan start <plan> <kind> [<attach>] [ch N,N-M]` (a `plan_start` op on
// the shown source through applyOp: conflicts get retry buttons, refused channels and
// warnings a note each).

import { OpError, connect, Unauthorized } from "./api.js";
import { QUIET_OPS, Transcript, errorText, startJobs, startOpsFeed } from "./agent.js";
import { mountWaterfall } from "./waterfall.js";
import { mountOverlays } from "./overlays.js";
import { mountMenu } from "./menu.js";
import { playJob } from "./audio.js";
import { closeGraph, redrawOpenGraph, showGraph } from "./graph.js";
import { mountMic } from "./voice.js";
import {
  AGENT_VIEW_KEYS,
  homeOf,
  mountView,
  runViewCommand,
  setStationHome,
  setThemeIds,
  themeIds,
} from "./view.js";
import { channelLabel, loadPlans, resolvePlan } from "./plans.js";
import { MixWatch, isMuted, mixCovering, muteOp } from "./mix.js";
import { mountCoarse } from "./coarse.js";
import { mountMap } from "./map.js";
import { mountSubjects, mountSubjectsPane } from "./subjects.js";
import { mountPanes } from "./panes.js";
import { mountExplorePane } from "./explore.js";
import { mountBitsPane } from "./bits.js";

export const NARROW_MAX_PX = 700;
const SECRET_KEY = "zeta.secret";
const SOURCE_KEY = "zeta.source";

export const state = {
  api: null,
  transcript: null,
  jobs: null,
  ops: null,
  wf: null,
  subjects: null,
  subjectsPane: null,
  panes: null,
  explore: null,
  bits: null, // bits.js: the Bits pane
  ov: null,
  map: null, // map.js: the positions of decoded events
  menu: null,
  audio: null, // {jobId, mix, seen (a job list has shown it), player}
  mix: null, // mix.js: who the watched mix hears now
  mic: null,
  sources: [],
  selected: null,
  view: null, // view.js policies
  coarse: null, // coarse.js: the automatic scans of a view past the capture
  lastJobs: [], // the last job_list result
  allowStops: false, // `allow stops on|off`: /ask may stop and replace jobs (not persisted)
  allowDeletes: false, // `allow deletes on|off`: /ask may delete captures (not persisted)
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
    applyPanes();
    applySide();
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

// A pinned source's tab title (spec 4.1): `pin <lo>–<hi> MHz`, then `; gain <db> dB` (or
// `gain automatic`) when the source has adjustable gain.
function pinTitle(src) {
  const p = src.pin;
  const half = p.bandwidth_hz / 2;
  let text = `pin ${mhz(p.center_hz - half)}–${mhz(p.center_hz + half)} MHz`;
  const g = src.gain;
  if (g) text += g.automatic || g.db === null ? "; gain automatic" : `; gain ${g.db} dB`;
  return text;
}

// The parts of a tab that follow the source's state: the fault, the `pin` tag and the title.
function decorateTab(tab, s) {
  tab.classList.toggle("fault", !!s.fault);
  let tag = tab.querySelector(".tag.pin");
  if (s.pin && !tag) {
    tag = document.createElement("span");
    tag.className = "tag pin";
    tag.textContent = "pin";
    tab.querySelector(".range").after(tag);
  } else if (!s.pin && tag) {
    tag.remove();
  }
  let title;
  if (s.fault) title = `fault: ${JSON.stringify(s.fault)}`;
  else if (s.pin) title = pinTitle(s);
  else title = rangeText(s);
  const pairs = healthPairs(s);
  tab.title = `${title}; ${pairs.map(([k, v]) => `${k} ${v}`).join(" ")}`;
  let health = tab.querySelector(".health");
  if (!health) {
    health = document.createElement("span");
    health.className = "health";
    tab.append(health);
  }
  health.replaceChildren(
    ...pairs.flatMap(([k, v]) => {
      const key = document.createElement("span");
      key.className = "k";
      key.textContent = k;
      const val = document.createElement("span");
      val.className = "v";
      val.dataset.field = k;
      val.textContent = v;
      return [key, val];
    }),
  );
}

// The numbers a source's bar carries: [field, text] pairs from its `source_list` record.
function healthPairs(s) {
  const rate = s.stats?.measured_rate_hz;
  const g = s.gain;
  const pairs = [
    ["pin", s.pin ? (s.pin.center_hz / 1e6).toFixed(3) : "—"],
    ["rate", rate == null ? "—" : String(rate).replace(/\B(?=(\d{3})+(?!\d))/g, " ")],
    ["ovf", s.stats?.overflows ?? "—"],
    ["gain", g ? (g.db == null ? "auto" : `${g.db} dB`) : "—"],
    ["hold", (s.holders || []).length],
    ["bank", s.bank ? s.bank.bin_hz : "direct"],
  ];
  if (s.fault) pairs.push(["fault", "fault"]);
  return pairs.map(([k, v]) => [k, String(v)]);
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
      decorateTab(tab, s);
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

// A `plan_start` result's refused channels and warnings, one note each.
function noteRefusals(v) {
  if (!v) return;
  for (const r of v.refused || []) {
    state.transcript.note(`ch${r.channel}: ${errorText(r.error)}`, true);
  }
  for (const w of v.warnings || []) state.transcript.note(w);
}

// Applies `op` as typed by the user (command line or context menu); its outcome reaches the
// transcript through the ops stream, except a successful quiet op (job_list, stream_list,
// source_list, modes_list, modes_show), whose "ui" record is hidden with the polls (ruling R6): that one is shown
// from the /op reply. A `conflict` also gets its offer line (offerReplace). The result
// JSON, or null when it failed; with `{errors: true}` an error the node applied (not a
// transport or validation failure) comes back as its JSON (`error`, ...) so the caller can
// act on it (classify's `too_old` retry).
async function applyOp(op, { errors = false } = {}) {
  const t = state.transcript;
  if (!state.api) return null;
  let result = null;
  try {
    result = await state.api.op(op);
    if (QUIET_OPS.has(op.op)) t.reply(op, result);
    if (op.op === "plan_start") noteRefusals(result && result.value);
  } catch (e) {
    if (notApplied(e)) t.note(`${op.op}: ${e.message}`, true);
    else if (e.json && e.json.error === "conflict") await offerReplace(op, e.json);
    if (errors && !notApplied(e)) result = e.json;
  }
  state.jobs?.refresh();
  return result;
}

// ---- conflicts -------------------------------------------------------------------------

// Ops that take `replace` (a conflict on them can be retried with the holders named).
const REPLACEABLE = new Set(["job_start", "job_retune", "source_ensure", "source_pin", "plan_start"]);

// `<kind> <id> @ <centre MHz>` for holder `id`, from the last job list (the conflict's
// `current` centre when the job is not in it).
function holderText(id, err) {
  const j = state.lastJobs.find((x) => x.id === id);
  const t = (j && j.tuning) || err.current;
  const at = t && typeof t.center_hz === "number" ? ` @ ${mhz(t.center_hz)} MHz` : "";
  return `${j ? j.kind : "job"} ${id}${at}`;
}

// `conflict: source S held by …`, with the node's `unpin source S (source_unpin)` option
// after it when the source is pinned (a pin is never among the holders).
function conflictText(err) {
  const holders = Array.isArray(err.holders) ? err.holders : [];
  const who = holders.length ? holders.map((h) => holderText(h, err)).join(", ") : "nobody";
  const options = Array.isArray(err.options) ? err.options : [];
  const unpin = options.find((o) => typeof o === "string" && o.startsWith("unpin source "));
  return `conflict: source ${err.source ?? "?"} held by ${who}${unpin ? `; ${unpin}` : ""}`;
}

// Whether the last `source_list` shows a pin on source `id`.
function pinned(id) {
  const s = state.sources.find((x) => x.id === id);
  return !!(s && s.pin);
}

// One action per holder of a `conflict` error: `stop <kind> <short id> and retry`, whose
// run calls retry([id]) (the original op again with `replace: [id]`). With no holders on a
// pinned source (spec 4.1) one action `unpin <source> and retry`: `source_unpin`, then
// retry([]).
export function conflictActions(err, retry) {
  const holders = err && Array.isArray(err.holders) ? err.holders : [];
  const actions = holders.map((id) => {
    const j = state.lastJobs.find((x) => x.id === id);
    return {
      label: `stop ${j ? j.kind : "job"} ${id.slice(-6)} and retry`,
      run: () => retry([id]),
    };
  });
  if (err && holders.length === 0 && typeof err.source === "string" && pinned(err.source)) {
    const source = err.source;
    actions.push({
      label: `unpin ${source} and retry`,
      run: async () => {
        await applyOp({ op: "source_unpin", source });
        return retry([]);
      },
    });
  }
  return actions;
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

// ---- source commands (spec 4.2) -------------------------------------------------------

const INT_RE = /^\d+$/;

// The shown source's `source_list` record, fetched now (null when it is not listed).
async function shownSource() {
  const sources = (await state.api.op({ op: "source_list" })).value || [];
  state.sources = sources;
  return sources.find((s) => s.id === state.selected) || null;
}

// The op a source command asks for, or {error} when the words do not parse.
function sourceOp(src, words) {
  const [verb, ...args] = words;
  if (verb === "pin") {
    if (args.length === 0) {
      const c = src.current;
      if (!c || typeof c.center_hz !== "number" || typeof c.bandwidth_hz !== "number") {
        return { error: `${src.id} has no capture to pin` };
      }
      const { center_hz, bandwidth_hz } = c;
      return { op: { op: "source_pin", source: src.id, center_hz, bandwidth_hz, replace: [] } };
    }
    if (args.length !== 2 || !args.every((a) => INT_RE.test(a))) {
      return { error: "source pin: expected <center_hz> <bandwidth_hz>" };
    }
    const [center_hz, bandwidth_hz] = args.map(Number);
    return { op: { op: "source_pin", source: src.id, center_hz, bandwidth_hz, replace: [] } };
  }
  if (verb === "unpin") {
    if (args.length) return { error: "source unpin: expected no arguments" };
    return { op: { op: "source_unpin", source: src.id } };
  }
  // gain
  if (args.length === 1 && args[0] === "auto") {
    return { op: { op: "source_set_gain", source: src.id, gain_db: null } };
  }
  if (args.length !== 1 || !/^-?\d+(\.\d+)?$/.test(args[0])) {
    return { error: "source gain: expected <db> or auto" };
  }
  return { op: { op: "source_set_gain", source: src.id, gain_db: Number(args[0]) } };
}

// The reply line for a successful source op.
function sourceReply(op, result) {
  if (op.op === "source_pin") {
    return `pinned ${op.source} at ${op.center_hz} Hz, ${op.bandwidth_hz} Hz`;
  }
  if (op.op === "source_unpin") return `unpinned ${op.source}`;
  const g = (result && result.value) || {};
  if (g.automatic || g.db === null || g.db === undefined) return `gain ${op.source}: automatic`;
  return `gain ${op.source}: ${g.db} dB (${g.min_db}..=${g.max_db})`;
}

// `source pin [<center_hz> <bandwidth_hz>]`, `source unpin`, `source gain <db>|auto` on the
// shown source: {text, error} for the transcript, or {text: null} when a conflict was
// offered its retry line instead (offerReplace).
export async function runSourceCommand(st, words) {
  let src;
  try {
    src = await shownSource();
  } catch (e) {
    return { text: `source_list: ${e.message}`, error: true };
  }
  if (!src) return { text: `no source shown`, error: true };
  const parsed = sourceOp(src, words);
  if (parsed.error) return { text: parsed.error, error: true };
  const op = parsed.op;
  try {
    const result = await st.api.op(op);
    return { text: sourceReply(op, result), error: false };
  } catch (e) {
    if (e.json && e.json.error === "conflict") {
      await offerReplace(op, e.json);
      return { text: null, error: true };
    }
    return { text: e.message, error: true };
  } finally {
    st.jobs?.refresh();
  }
}

// `plan start <plan> <kind> [<attach>] [ch N,N-M]`: the `plan_start` op for the shown source,
// or {error} when the words do not parse. No `ch` means every channel of the plan.
function planOp(src, words) {
  const [verb, plan, kind, ...rest] = words;
  const usage = "plan start: expected <plan> <kind> [<attach>] [ch N,N-M]";
  if (verb !== "start" || !plan || !kind) return { error: usage };
  let channels = null;
  const at = rest.indexOf("ch");
  if (at >= 0) {
    const list = rest.slice(at + 1);
    rest.length = at;
    if (list.length !== 1 || !/^\d+(-\d+)?(,\d+(-\d+)?)*$/.test(list[0])) {
      return { error: "plan start: ch expects numbers like 1,2,5-8" };
    }
    const out = new Set();
    for (const item of list[0].split(",")) {
      const [lo, hi] = item.split("-").map(Number);
      if (hi !== undefined && (hi < lo || hi - lo >= 1000)) {
        return { error: `plan start: range ${item} is reversed or wider than 1000` };
      }
      for (let n = lo; n <= (hi ?? lo); n += 1) out.add(n);
    }
    channels = [...out].sort((a, b) => a - b);
  }
  if (rest.length > 1) return { error: usage };
  return {
    op: {
      op: "plan_start",
      plan,
      channels,
      kind,
      source: src.id,
      deviation_hz: null,
      sideband: null,
      attach: rest[0] ?? null,
      language: null,
      replace: [],
    },
  };
}

// `plan start …` on the shown source, applied through applyOp (a conflict gets its retry
// buttons, refusals and warnings their notes). {text: null}: applyOp wrote the transcript.
export async function runPlanCommand(st, words) {
  let src;
  try {
    src = await shownSource();
  } catch (e) {
    return { text: `source_list: ${e.message}`, error: true };
  }
  if (!src) return { text: `no source shown`, error: true };
  const parsed = planOp(src, words);
  if (parsed.error) return { text: parsed.error, error: true };
  await applyOp(parsed.op);
  return { text: null, error: false };
}

// Sends `text` to /ask and shows the answer (or the error, e.g. no LLM configured). The
// allowance is the session's `allow stops` switch unless `allowOverride` gives one for this
// request only (the unlock button's re-run); the switch itself is never changed here.
async function askAgent(text, allowOverride = null) {
  const t = state.transcript;
  if (!state.api) return;
  t.user(text);
  try {
    const stops = state.allowStops;
    const allow = allowOverride ?? {
      stop_jobs: stops,
      replace: stops,
      delete_captures: state.allowDeletes,
    };
    const r = await state.api.ask(text, 32, allow);
    let may = "";
    if (r.allow && (r.allow.stop_jobs || r.allow.replace)) may += " [may stop jobs]";
    if (r.allow && r.allow.delete_captures) may += " [may delete captures]";
    t.assistant((r.answer ?? "(no answer)") + may, false, r.turn_log || [], r.calls || []);
    // The agent's page-only `view_set` calls are this tab's to apply, as if typed.
    for (const c of r.calls || []) {
      if (c.tool !== "view_set" || !c.ok || !c.arguments) continue;
      const { key, value } = c.arguments;
      if (!AGENT_VIEW_KEYS.includes(key)) {
        const allowed = AGENT_VIEW_KEYS.join(", ");
        t.note(`the agent may not set view ${key} (allowed: ${allowed})`, true);
        continue;
      }
      const v = runViewCommand(state.view, ["set", String(key), String(value)]);
      t.note(v.text, v.error);
    }
    const refused = (r.calls || []).filter((c) => c.result && c.result.error === "not_allowed");
    if (refused.length) {
      // A refused `capture_delete` needs `delete_captures`; any other refusal is a stop or a
      // replace. The retry re-sends this one request with the refused flags true on top of
      // the switches; the session stays as it was (`allow stops|deletes on` are the switches).
      const deletes = refused.some((c) => c.tool === "capture_delete");
      const stopsRefused = refused.some((c) => c.tool !== "capture_delete");
      const what = [stopsRefused && "stop or replace jobs", deletes && "delete captures"]
        .filter(Boolean)
        .join(" or ");
      const label = [stopsRefused && "stops", deletes && "deletes"].filter(Boolean).join(" and ");
      t.note(`the agent was not allowed to ${what}`, false, [
        {
          label: `allow ${label} and ask again`,
          // The note's buttons disable on click.
          run: () =>
            askAgent(text, {
              stop_jobs: stopsRefused || allow.stop_jobs,
              replace: stopsRefused || allow.replace,
              delete_captures: deletes || !!allow.delete_captures,
            }),
        },
      ]);
    }
  } catch (e) {
    // A failed run (502) still carries the transcript so far: show its turns.
    const tr = e instanceof OpError && e.json && e.json.transcript;
    t.assistant(e.message, true, (tr && tr.turn_log) || [], (tr && tr.calls) || []);
  }
  state.jobs?.refresh();
}

async function runCommand(text) {
  if (!state.api) return;
  // Only `view` and `view set|show|reset|clear …` are view commands (ruling R24): other text
  // starting "view " ("view the strongest signal") is a question for the agent.
  if (/^view(\s+(set|show|reset|clear)\b.*)?$/.test(text)) {
    state.transcript.user(text);
    const r = runViewCommand(state.view, text.split(/\s+/).slice(1));
    state.transcript.note(r.text, r.error);
    return;
  }
  if (/^source\s+(pin|unpin|gain)\b/.test(text)) {
    state.transcript.user(text);
    const r = await runSourceCommand(state, text.split(/\s+/).slice(1));
    if (r.text !== null) state.transcript.note(r.text, r.error);
    return;
  }
  // `mix` or `mix ID[,ID]` with every id a source; "mix the channels please" is the agent's.
  const mix = /^mix(?:\s+([\w.-]+(?:,[\w.-]+)*))?$/.exec(text);
  if (mix) {
    const ids = mix[1] ? mix[1].split(",") : null;
    if (!ids || ids.every((id) => state.sources.some((s) => s.id === id))) {
      state.transcript.user(text);
      await listen(ids, { toggle: false });
      return;
    }
  }
  const allow = /^allow\s+(stops|deletes)\s+(on|off)$/.exec(text);
  if (allow) {
    state.transcript.user(text);
    const on = allow[2] === "on";
    if (allow[1] === "stops") state.allowStops = on;
    else state.allowDeletes = on;
    state.transcript.note(`allow ${allow[1]}: ${allow[2]}`);
    return;
  }
  // Exactly these words: "stop all jobs" and the like are requests for the agent.
  if (/^stop\s+all$/.test(text)) {
    state.transcript.user(text);
    await applyOp({ op: "jobs_stop", source: null, kind: null, plan: null });
    return;
  }
  if (/^plan\s+start\b/.test(text)) {
    state.transcript.user(text);
    const r = await runPlanCommand(state, text.split(/\s+/).slice(1));
    if (r.text !== null) state.transcript.note(r.text, r.error);
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
  // The channel plans load once; the rows, outlines and axis are drawn again when they are in.
  loadPlans(api).then(() => {
    if (state.api !== api) return;
    redrawPlan();
    state.jobs?.redraw();
  });
  state.ops = startOpsFeed(api, state.transcript, {
    onConflict: (p) => offerReplace(p.op, p.error),
  });
  state.wf = mountWaterfall($("#waterfall"), api);
  state.wf.onNotice = (text) => state.transcript?.note(text, true);
  state.wf.follow = () => state.view.get("follow");
  state.wf.plan = () => state.view.get("plan");
  state.coarse = mountCoarse(state.wf, api, state.view, {
    transcript: state.transcript,
    sources: () => state.sources,
    jobs: () => state.lastJobs,
  });
  state.coarse.onRow((msg) => state.wf?.addCoarseEvent(msg));
  state.subjects = mountSubjects(api, state.view);
  state.ov = mountOverlays($("#waterfall"), state.wf, api, {
    subjects: state.subjects,
    jobsEl: $("#jobs"),
    plan: () => state.view.get("plan"),
    applyOp,
    footprint: () => document.documentElement.dataset.footprint || "wide",
    onOutlineMenu: (job, x, y) => state.menu?.openOutline(job, x, y),
    notice: (text) => state.transcript?.note(text, true),
  });
  state.map = mountMap($("#map"), api, state.view, {
    jobs: () => state.lastJobs,
    subjects: state.subjects,
  });
  state.ov.onEvent((job, p) => {
    state.subjects?.event(job, p);
    state.map?.event(job, p);
  });
  state.subjectsPane = mountSubjectsPane($("#subjects"), state.subjects, state.view, {
    jobs: () => state.lastJobs,
    watching: (id) => (state.ov ? state.ov.watching(id) : true),
    footprint,
  });
  state.panes = mountPanes($("#panes"), state.view, { footprint, host: $("#pane") });
  state.panes.register("subjects", "subjects", {
    eligible: () => !!state.subjectsPane && state.subjectsPane.eligible(),
    el: $("#subjects"),
    onShow: () => state.subjectsPane?.render(),
    // A last draw going out, so a hidden pane holds no ended job's section.
    onHide: () => state.subjectsPane?.render(),
  });
  state.explore = mountExplorePane($("#explore"), api, state.view, state.transcript, {
    panes: state.panes,
    ov: () => state.ov,
    wf: () => state.wf,
    onBits: (id) => state.bits?.setJob(id),
  });
  state.bits = mountBitsPane($("#bits"), api, state.view, state.transcript, {
    panes: state.panes,
    jobs: () => state.lastJobs,
  });
  state.subjects.onLearn(applyPanes);
  loadStationHome(api);
  loadThemes(api);
  applyTheme();
  applySide();
  mountSideHeads();
  applyRail();
  applyMapPane();
  state.view.onChange((key) => {
    if (key === "plan") redrawPlan();
    if (key === "theme") applyTheme();
    if (key === "side") applySide();
    if (key === "map") applyMapPane();
    if (key === "pane") applyPanes();
  });
  state.ov.setSource(state.selected);
  state.menu = mountMenu(state.wf, {
    applyOp,
    attachOp,
    muteFor,
    ask: askAgent,
    transcript: state.transcript,
    ov: state.ov,
    ringSeconds: (id) => state.sources.find((x) => x.id === id)?.ring?.seconds ?? null,
    plan: () => resolvePlan(state.view.get("plan")),
    command: (text) => {
      const cmd = $("#cmd");
      cmd.value = text;
      cmd.focus();
    },
  });
  state.transcript.onAssistant((text, error) => {
    if (!error && footprint() === "narrow") answerRibbon(text);
  });
  state.nodeCheck = startNodeCheck(api);
  state.mix = new MixWatch(api, {
    onChange: () => {
      state.ov?.setSpeaking(state.mix ? state.mix.activeIds() : []);
      paintMix();
    },
  });
  state.jobs = startJobs(api, $("#jobs"), state.transcript, {
    view: state.view,
    countEl: $(".pane.jobs h2 .count"),
    onJobs: (jobs) => {
      state.lastJobs = jobs;
      state.explore?.noteJobs(jobs);
      state.bits?.noteJobs(jobs);
      if (state.map) state.map.sync();
      state.subjects?.sync(jobs);
      if (state.ov) state.ov.sync(jobs, shown());
      applyPanes();
      if (state.wf) state.wf.syncJobs(jobs);
      pollSources(api);
      stopAudioUnlessRunning(jobs);
      state.mix?.sync(jobs, state.audio?.jobId);
      state.ov?.setSpeaking(state.mix ? state.mix.activeIds() : []);
      paintMix();
      syncListen();
    },
    onPlay: toggleAudio,
    onTranscribe: (job) => applyOp(attachOp(job.id)),
    onGraph: (job) => showGraph(api, job.id),
    playing: (id) => !!state.audio && state.audio.jobId === id,
    onHot: (id, on) => state.ov?.setHot(id, on),
    squelch: (id) => (state.ov ? state.ov.squelch(id) : null),
    lastEvent: (id) => (state.ov ? state.ov.lastEvent(id) : null),
    activity: (id) =>
      state.ov ? state.ov.activity(id) : { watched: false, speaking: false, hot: false },
    watchedMix: () => (state.mix ? state.mix.jobId : null),
    position: (id) => (state.map ? state.map.rowText(id) : null),
    mixText: (job) => mixRowText(job.id),
    mute: muteFor,
    onBits: (job) => {
      state.bits?.setJob(job.id);
      state.panes?.select("bits");
    },
  });
  state.mic = mountMic($("#mic"), api, (text) => {
    $("#cmd").value = text;
    submitCommand();
  });
  loadSources();
}

// The channel names drawn again: the outlines and the axis (the job rows redraw themselves
// on the view's change).
function redrawPlan() {
  if (state.ov) state.ov.syncOutlines(state.ov.lastJobs, shown());
  if (state.wf) state.wf.drawAxis();
}

// <html data-map="on"> while the view's `map` policy is on (style.css lays the pane out);
// off also leaves the narrow footprint's map tab.
// The node's station as the home while the view's `home` is none (view.js homeOf): read
// once per page load; `view set home` keeps precedence. A node without one, or an older
// node without the op, leaves no home.
async function loadStationHome(api) {
  try {
    const r = await api.op({ op: "station_get" });
    if (!r || r.result !== "station" || !r.value) return;
    setStationHome(r.value);
    state.map?.drawHome();
    state.jobs?.refresh();
  } catch {
    // No station: the view's home only.
  }
}

// The ready themes from `themes_list` become what `view set theme` offers (view.js
// themeIds); a stored theme that is not among them falls back to console. A node without the
// op leaves the built-ins.
async function loadThemes(api) {
  let ids;
  try {
    const r = await api.op({ op: "themes_list" });
    if (!r || r.result !== "themes" || !Array.isArray(r.value?.themes)) return;
    ids = r.value.themes.filter((t) => t.state === "ready").map((t) => t.id);
  } catch {
    return; // The built-in themes only.
  }
  if (state.api !== api) return;
  setThemeIds(ids);
  const theme = state.view.get("theme");
  if (!themeIds().includes(theme)) themeFallback(theme);
}

// The theme in force: the view's `theme` policy applied from its files under
// /ui/themes/<id>/ (theme.json, then style.css, which the node wraps in
// [data-theme="<id>"]). <html data-theme> changes only once the new stylesheet has loaded,
// so the page is never unstyled, and with it <html data-rail> (layout.rail), the waterfall's
// ramp and canvas colours, the rail and an open graph view. A later switch supersedes one in
// flight. A theme whose files fail falls back to console (themeFallback).
let themeSwitch = 0;
async function applyTheme() {
  const id = state.view ? state.view.get("theme") : "console";
  const seq = ++themeSwitch;
  // A stylesheet still loading for an older switch is superseded now, not once this one has
  // fetched its theme.json: it drops itself on load and leaves the sheet in force alone.
  themeCssLatest = null;
  const root = document.documentElement;
  let theme;
  try {
    const r = await fetch(`themes/${encodeURIComponent(id)}/theme.json`, { cache: "no-cache" });
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    theme = await r.json();
    if (seq !== themeSwitch) return;
    await loadThemeCss(id);
  } catch {
    if (seq === themeSwitch) themeFallback(id);
    return;
  }
  if (seq !== themeSwitch) return;
  root.dataset.theme = id;
  root.dataset.rail = theme.layout?.rail === false ? "off" : "on";
  applyRail();
  if (state.wf) {
    state.wf.refreshThemeColours();
    state.wf.setRamp(theme.ramp);
  }
  redrawOpenGraph();
}

// Loads themes/<id>/style.css into a new <link id="theme-css">, replacing the old one once it
// has loaded (the old rules stay until then); rejects when it fails to load. A stylesheet a
// later call superseded is dropped when it loads, leaving the newer one.
let themeCssLatest = null;
function loadThemeCss(id) {
  const old = document.getElementById("theme-css");
  const href = `themes/${encodeURIComponent(id)}/style.css`;
  themeCssLatest = null;
  if (old && old.getAttribute("href") === href && old.sheet) return Promise.resolve();
  return new Promise((resolve, reject) => {
    const link = document.createElement("link");
    themeCssLatest = link;
    link.rel = "stylesheet";
    link.href = href;
    link.onload = () => {
      if (link !== themeCssLatest) {
        link.remove();
        resolve();
        return;
      }
      if (old !== link) old?.remove();
      link.id = "theme-css";
      resolve();
    };
    link.onerror = () => {
      link.remove();
      reject(new Error(`${href} failed to load`));
    };
    (old ?? document.head.lastElementChild).after(link);
  });
}

// A theme that is not available (gone from the node, broken, its files failing): the
// transcript says so once and the view's `theme` goes back to console, which applies it.
function themeFallback(id) {
  if (id === "console") {
    document.documentElement.dataset.theme = "console";
    return;
  }
  state.transcript?.note(`theme ${id} is not available; using console`, true);
  state.view?.set("theme", "console");
}

// The side column's panes (view policy `side`): the ones left out of the value are folded to
// their header bars (`section.pane[data-collapsed]`, the button's aria-expanded), and
// <html data-side="…"> lets style.css narrow the column when both are.
const SIDE_PANES = ["transcript", "jobs"];

function sideOpen(value) {
  if (value === "both") return SIDE_PANES;
  return SIDE_PANES.includes(value) ? [value] : [];
}

// Narrow, the drawer shows both panes whatever `side` says, so the header bars are inert:
// disabled (out of the tab order, no Enter), each announced expanded. Applied again on a
// footprint change (applyFootprint).
function applySide() {
  const value = state.view ? state.view.get("side") : "both";
  const open = sideOpen(value);
  const narrow = footprint() === "narrow";
  for (const name of SIDE_PANES) {
    const section = document.querySelector(`#side .pane.${name}`);
    if (!section) continue;
    const on = open.includes(name);
    if (on) delete section.dataset.collapsed;
    else section.dataset.collapsed = "";
    const head = section.querySelector(".pane-head");
    if (!head) continue;
    head.disabled = narrow;
    head.setAttribute("aria-expanded", String(narrow || on));
  }
  document.documentElement.dataset.side = value;
}

// A header bar folds or unfolds its own pane, the other kept as it is. Mounted once per
// document (the handlers read the view of the session in force).
let sideHeadsMounted = false;
function mountSideHeads() {
  if (sideHeadsMounted) return;
  sideHeadsMounted = true;
  for (const name of SIDE_PANES) {
    const head = document.querySelector(`#side .pane.${name} .pane-head`);
    if (!head) continue;
    head.addEventListener("click", () => {
      if (!state.view) return;
      const open = new Set(sideOpen(state.view.get("side")));
      if (open.has(name)) open.delete(name);
      else open.add(name);
      const next = open.size === 2 ? "both" : open.size === 1 ? [...open][0] : "none";
      state.view.set("side", next);
    });
  }
}

// The job tags live in the waterfall's rail when the theme's layout has one (<html
// data-rail="on">), on their outlines otherwise.
function applyRail() {
  if (!state.ov || !state.wf) return;
  state.ov.setRail(document.documentElement.dataset.rail === "on" ? state.wf.rail : null);
}

function applyMapPane() {
  const root = document.documentElement;
  const on = !!state.view && state.view.get("map") === "on";
  if (on) root.dataset.map = "on";
  else {
    delete root.dataset.map;
    if (root.dataset.pane === "map") delete root.dataset.pane;
  }
  $("#map-toggle").setAttribute("aria-pressed", String(root.dataset.pane === "map"));
  state.panes?.render();
}

// The pane strip and #pane from the view's `pane` and the panes' eligibility (panes.js), and
// the map's narrow tab, which a pane's tab replaces. Run on a view change, a row learned, a jobs
// poll and a footprint change, never on a message.
function applyPanes() {
  state.panes?.render();
  $("#map-toggle").setAttribute(
    "aria-pressed",
    String(document.documentElement.dataset.pane === "map"),
  );
}

// The shown source's last `source_list` record (null when unknown).
function shown() {
  return state.sources.find((s) => s.id === state.selected) || null;
}

// The tabs follow each `source_list` poll (pin tag, title, fault) without re-selecting.
function syncTabs(sources) {
  for (const tab of document.querySelectorAll(".src-tab")) {
    const s = sources.find((x) => x.id === tab.dataset.id);
    if (s) decorateTab(tab, s);
  }
}

// After each jobs poll: `source_list` into state.sources and the waterfall (a moved capture),
// one at a time; a failure waits for the next poll.
let sourcesPolling = false;

async function pollSources(api) {
  if (sourcesPolling) return;
  sourcesPolling = true;
  try {
    const sources = (await api.op({ op: "source_list" })).value || [];
    if (state.api !== api) return;
    state.sources = sources;
    if (state.wf) state.wf.syncSources(sources);
    syncTabs(sources);
    if (state.ov) state.ov.syncOutlines(state.lastJobs, shown());
  } catch {
    /* the next poll retries */
  } finally {
    sourcesPolling = false;
  }
}

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

// The node rejected the secret after login: stop everything using it and ask again.
function logout(api) {
  if (state.api !== api) return;
  state.api = null;
  api.onUnauthorized = null;
  api.onBusy = null;
  if (state.nodeCheck) state.nodeCheck.close();
  state.nodeCheck = null;
  stopAudio();
  if (state.mix) state.mix.close();
  state.mix = null;
  closeGraph();
  if (state.mic) state.mic.destroy();
  if (state.ops) state.ops.close();
  if (state.jobs) state.jobs.close();
  if (state.menu) state.menu.destroy();
  if (state.ov) state.ov.destroy();
  if (state.map) state.map.destroy();
  if (state.explore) state.explore.destroy();
  if (state.bits) state.bits.destroy();
  if (state.panes) state.panes.destroy();
  if (state.subjectsPane) state.subjectsPane.destroy();
  if (state.subjects) state.subjects.destroy();
  if (state.coarse) state.coarse.destroy();
  if (state.wf) state.wf.destroy();
  state.ops = null;
  state.jobs = null;
  state.ov = null;
  state.map = null;
  state.panes = null;
  state.explore = null;
  state.bits = null;
  state.subjectsPane = null;
  state.subjects = null;
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
  syncListen();
}

// ▶ on a job row: plays that job (stopping any other); ■ on the playing row stops it.
// `ctx`: an AudioContext made earlier in the gesture (audio.js playJob).
function toggleAudio(job, { ctx = null } = {}) {
  const same = state.audio && state.audio.jobId === job.id;
  stopAudio();
  if (!same && state.api) {
    const entry = { jobId: job.id, mix: job.kind === "mix", seen: !ctx, player: null };
    state.audio = entry;
    entry.player = playJob(state.api, job, {
      ctx,
      onError: (message) => state.transcript.note(message, true),
      onEnd: () => {
        if (state.audio === entry) state.audio = null;
        state.jobs?.redraw();
        syncListen();
      },
    });
  } else if (ctx) {
    ctx.close().catch(() => {});
  }
  state.jobs?.redraw();
  syncListen();
}

// #listen-all: pressed while a mix plays it stops; else `mix_start` for `sources` (null:
// every source) and plays the mix it answers. The context is made first, inside the click.
let listening = false; // a mix_start of listen() is in flight
async function listen(sources, { toggle }) {
  if (!state.api) return;
  if (toggle && state.audio && state.audio.mix) {
    stopAudio();
    state.jobs?.redraw();
    return;
  }
  if (listening) return; // a second press while mix_start is in flight
  listening = true;
  const ctx = new AudioContext();
  ctx.resume().catch(() => {});
  let id = null;
  try {
    const r = await state.api.op({ op: "mix_start", sources });
    id = r && r.value;
  } catch (e) {
    ctx.close().catch(() => {});
    state.transcript?.note(`mix_start: ${e.json ? errorText(e.json) : e.message}`, true);
    return;
  } finally {
    listening = false;
  }
  if (!state.api || typeof id !== "string") {
    ctx.close().catch(() => {});
    return;
  }
  stopAudio();
  toggleAudio({ id, kind: "mix" }, { ctx });
  state.jobs?.refresh();
}

// #listen-all follows the audio: pressed (and `Stop listening`) while a mix plays.
function syncListen() {
  const b = $("#listen-all");
  if (!b) return;
  const on = !!(state.audio && state.audio.mix);
  b.setAttribute("aria-pressed", String(on));
  b.textContent = on ? "Stop listening" : "Listen to all";
}

// The mute a demodulator's row or outline offers: the running mix that hears it (mix.js
// mixCovering, the playing one first) and whether the job list says the channel is muted there,
// for the label; null when no mix covers the job. `run()` flips what the mix's graph says then
// and answers `{muted}`, or null when nothing was applied.
function muteFor(job) {
  const mix = mixCovering(job, state.lastJobs, state.audio?.jobId);
  if (!mix) return null;
  return {
    muted: isMuted(job, mix),
    title: `graph_put ${mix.id}`,
    run: () => {
      const turn = muteQueue.then(() => flipMute(job));
      muteQueue = turn.catch(() => {});
      return turn;
    },
  };
}

// The page's mutes, one at a time: each reads the graph the one before it wrote.
let muteQueue = Promise.resolve();

// The job list is up to 2 s old and a graph_put replaces the whole list of mutes, so the edit is
// made on the graph as it stands now.
async function flipMute(rendered) {
  const job = state.lastJobs.find((j) => j.id === rendered.id) ?? rendered;
  const mix = mixCovering(job, state.lastJobs, state.audio?.jobId);
  if (!mix || !state.api) return null;
  let graph;
  try {
    graph = (await state.api.op({ op: "graph_get", job: mix.id })).value;
  } catch (e) {
    state.transcript.note(`graph_get: ${e.message}`, true);
    return null;
  }
  const muted = !isMuted(job, { graph });
  return (await applyOp(muteOp(job, mix.id, graph, muted))) ? { muted } : null;
}

// What a mix's row says (mix.js rowText) with the plan's channel names.
function mixRowText(id) {
  if (!state.mix) return "";
  const plan = state.view ? state.view.get("plan") : "auto";
  const label = (hz) => {
    const ch = channelLabel(hz, plan);
    return ch ? ch.text : `${(hz / 1e6).toFixed(3)} MHz`;
  };
  return state.mix.rowText(id, label, Date.now());
}

// Rewrites every mix row's `.mix-now` cell in place (no redraw: the events come often).
function paintMix() {
  for (const cell of document.querySelectorAll("#jobs .job .mix-now")) {
    const row = cell.closest(".job");
    cell.textContent = row ? mixRowText(row.dataset.id) : "";
  }
}

function stopAudioUnlessRunning(jobs) {
  const a = state.audio;
  if (!a) return;
  const j = jobs.find((x) => x.id === a.jobId);
  // A mix started by the click is not in a list polled before it: wait until one has shown it.
  if (j) a.seen = true;
  if (!j ? a.seen : !j.state || j.state.status !== "running") stopAudio();
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
// draw the narrow-footprint answer ribbon for `text` without an LLM (ruling R3) or, given
// `hz` first, a ribbon at hz (keyed by `key` when given, of job id `job` when given; the
// element), and draw a probe label
// at a frequency.
window.zetaTest = {
  get lastRowFormat() {
    return state.wf ? state.wf.lastRowFormat : null;
  },
  get waterfallMode() {
    return state.wf ? state.wf.mode : null;
  },
  ribbon(...args) {
    if (args.length <= 1) return answerRibbon(args[0]);
    const [hz, text, cls, key, job = null] = args;
    if (!state.ov) return null;
    return key
      ? state.ov.keyedRibbon(key, hz, text, cls, 1, () => false, job)
      : state.ov.ribbon(hz, text, cls, job);
  },
  // A layer-1 overlay label `name` at `hz` (key "probe"); the element, or null without overlays.
  label(hz, name) {
    return state.ov ? state.ov.label(hz, { layer: 1, name }, "probe") : null;
  },
  // A keyed ribbon (overlays.js keyedRibbon) for a test; the element, or null without overlays.
  keyedRibbon(key, hz, text, cls, count) {
    return state.ov ? state.ov.keyedRibbon(key, hz, text, cls, count, () => true) : null;
  },
  setSecret(s) {
    if (state.api) state.api.secret = s;
  },
  // The ids `view set theme` accepts (view.js themeIds: the ready ones once themes_list
  // answered).
  themeIds() {
    return themeIds();
  },
  // The home in force (view.js homeOf: view `home`, else the station read at load), or null.
  home() {
    return state.view ? homeOf(state.view) : null;
  },
  poll() {
    return state.jobs ? state.jobs.refresh() : Promise.resolve();
  },
  // Resolve the shown source's feed as if the node listed none (an older node), or again
  // as it is; takes effect at the next feed lookup (a source switch, a sync without a feed).
  hideFeeds(on) {
    if (state.ov) state.ov.hideFeeds = !!on;
  },
  // The idle age (ms) after which plan channels fold into a group (agent.js JOBS_FOLD_IDLE_MS),
  // for the page's lifetime; the rows are drawn again at once.
  foldIdle(ms) {
    if (state.jobs) state.jobs.foldIdle(ms);
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
  // The waterfall: {sourceId, jobId, painted, badge, coarsePainted, coarseMasked, easing, view
  // (the band shown, {lo, hi} or null), band (the capture)}, or null without one.
  wf() {
    const wf = state.wf;
    if (!wf) return null;
    const box = (b) => (b ? { lo: b.lo, hi: b.hi } : null);
    return {
      sourceId: wf.sourceId,
      jobId: wf.jobId,
      painted: wf.painted,
      badge: !wf.badge.hidden,
      coarsePainted: wf.coarsePainted,
      coarseMasked: wf.coarseMasked,
      easing: wf.easing,
      view: box(wf.shown()),
      band: box(wf.band),
    };
  },
  // The Waterfall instance itself, or null without one: test-only, for e2e probes that call
  // its methods inside the page (fillColumn, redraw); never used by the page.
  waterfall() {
    return state.wf || null;
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
  $("#listen-all").addEventListener("click", () => listen(null, { toggle: true }));
  $("#map-toggle").addEventListener("click", () => {
    const root = document.documentElement;
    if (root.dataset.pane === "map") delete root.dataset.pane;
    else root.dataset.pane = "map";
    applyMapPane();
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
