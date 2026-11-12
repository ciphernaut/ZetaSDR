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
// `source pin [<center_hz> <bandwidth_hz>]`, `source unpin`, `source gain <db>|auto`,
// `source capture fit|widest` and `source measure` act on the shown source the same way (runSourceCommand: `source_pin` at its
// `current` capture or the band given, `source_unpin`, `source_set_gain`, `source_measure`),
// their reply or error a transcript line;
// anything else (other text starting "view " or "source " included) goes to `api.ask`.
// Every ask carries askContext() (the shown radio, `main`, `radio_policy`, `radio_prefer`);
// after the answer, selectionNotes(selection) adds a note per source a radio op ran on off
// the default radio, and one when the shown radio was dropped as the focus.
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
// The default radio (spec 4.9, paintDefault): the tab of the radio a request naming none runs
// on carries class `default` and a `.default-mark` dot, and says so in its title.
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
// The map is the Map tab of the sources strip (renderTabs appends it after the source tabs):
// the view's `main` (rf | map; `map on|off` is its alias) says what #main shows, and
// <html data-main> follows it (applyMain; style.css hides the waterfall, which keeps its job,
// zoom and outlines, and shows the map in its place, in both footprints). The Map tab sets
// `main map` (a second click `main rf`), a source tab `main rf` after selecting its source;
// the shown source is unchanged meanwhile and its tab carries class `kept`.
// Lit sites (plan 32 spec 4.4): an `origin` event of a job the overlays follow lights the
// job's set (map.origin keyed by the job id, faded from the job's last event, ov.lastEvent;
// on a row with `subject.station`, from its last station frame once one came, plan 35),
// and is a transcript line as a watch's verdict is (verdictLine); What is this on a box and
// on a capture light one-shot sets from their classification's `origins` (`classify:<t_ns>`,
// `capture:<id>`); each jobs poll seeds the running jobs' `context.origins` (map.seed). A
// job row's or the Bits header's Origins lights the job's set from the op's answer at once,
// and notes it (units.js originSummary) only for a job the overlays do not follow
// (ov.follows). The
// Map tab's `.count` is the number of lit sites (map.litCount; empty at zero), kept over
// renderTabs; the page never switches to the map by itself.
//
// The subjects (subjects.js): the store is mounted before the overlays, which hand it every
// event first and ask it for mode rows; each jobs poll drops the subjects of jobs that ended.
// Its pane, #subjects, is the Subjects tab of the pane strip (panes.js, #panes beside the
// source tabs): the tab shows while a running job's row has table columns or while it is
// selected, and the pane shows in #pane under the waterfall while the view's `pane` is
// `subjects` (`view set subjects on|off` is an alias). In the narrow footprint the pane is a
// tab in the waterfall's place (<html data-pane="subjects">) that the strip's tab opens and closes.
// The view's `dock` (auto | bottom | right; auto the theme's `layout.dock`) moves #pane, whichever
// pane it shows, into the side column as a third folding section in the wide footprint (applyDock).
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
// audio, mic, sources, selected, view, lastJobs, allowStops, allowDeletes, allowSignatures}),
// `onSourceChange(fn)` for the later views (waterfall, audio,
// voice), `conflictActions(err, retry)`, `runSourceCommand(state, words)` and
// `runPlanCommand(state, words)`.
//
// Typed commands beside `view` and `source`: `allow stops on|off` (state.allowStops),
// `allow deletes on|off` (state.allowDeletes) and `allow signatures on|off`
// (state.allowSignatures), all off at load; every following /ask carries
// `allow: {stop_jobs, replace, delete_captures, accept_signatures}` of them; an answer from a
// run that may stop jobs ends " [may stop jobs]" (" [may delete captures]" and
// " [may accept signatures]" likewise), each turn of its `turn_log`
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
import { mountMap, resolveMapTiles } from "./map.js";
import { mountSubjects, mountSubjectsPane } from "./subjects.js";
import { mountPanes } from "./panes.js";
import { mountExplorePane } from "./explore.js";
import { mountBitsPane } from "./bits.js";
import { mountVideo } from "./video.js";
import { originSummary } from "./units.js";

export const NARROW_MAX_PX = 700;
const SECRET_KEY = "zeta.secret";
const SOURCE_KEY = "zeta.source";
// The agent calls `allow signatures` unlocks: answering a proposal is the operator's.
const SIGNATURE_DECISIONS = ["signature_accept", "signature_reject"];

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
  video: null, // video.js: the Video pane and the tags' thumbnails
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
  // `allow signatures on|off`: /ask may accept and reject signatures (not persisted)
  allowSignatures: false,
  dockCollapsed: false, // the docked pane's section folded (applyDock; not persisted)
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
  syncMainTabs();
  paintDefault();
  const src = state.sources.find((s) => s.id === id) || null;
  for (const fn of sourceListeners) fn(src);
}

// The strip's selection from the view's `main` and the shown source: the Map tab is the
// selected one while `main` is map, the shown source's tab then keeps class `kept`.
function syncMainTabs() {
  const map = !!state.view && state.view.get("main") === "map";
  for (const tab of document.querySelectorAll("#tabs .src-tab")) {
    const isMap = tab.classList.contains("map-tab");
    const shownTab = !isMap && tab.dataset.id === state.selected;
    tab.setAttribute("aria-selected", String(isMap ? map : shownTab && !map));
    if (!isMap) tab.classList.toggle("kept", shownTab && map);
  }
  paintDefault();
}

// The default radio's mark (spec 4.9): class `default` and a title suffix on the tab of the
// radio the agent would take for a request that names none (defaultRadio), on the RF view and
// the Map alike (the shown source's tab is the `kept` one there). decorateTab calls paintTab
// too, since each poll rewrites the title.
const DEFAULT_TITLE = "default radio for the agent";

function paintTab(tab) {
  if (tab.classList.contains("map-tab") || !state.view) return;
  const on = tab.dataset.id === defaultRadio();
  tab.classList.toggle("default", on);
  const base = tab.dataset.baseTitle ?? tab.title;
  tab.title = on ? `${base}; ${DEFAULT_TITLE} (policy ${state.view.get("radio_policy")})` : base;
}

function paintDefault() {
  for (const tab of document.querySelectorAll("#tabs .src-tab")) paintTab(tab);
}

// The Map tab: not a source (source_list never lists it, syncTabs never touches it).
function mapTab() {
  const tab = document.createElement("button");
  tab.className = "src-tab map-tab";
  tab.setAttribute("role", "tab");
  tab.dataset.view = "map";
  const name = document.createElement("span");
  name.className = "name";
  name.textContent = "Map";
  const count = document.createElement("span");
  count.className = "count";
  tab.append(name, count);
  tab.onclick = () => {
    if (state.view) state.view.set("main", state.view.get("main") === "map" ? "rf" : "map");
  };
  return tab;
}

// The Map tab's badge: the number of lit sites, empty at zero.
function paintMapCount() {
  const el = document.querySelector("#tabs .map-tab .count");
  if (!el) return;
  const n = state.map ? state.map.litCount() : 0;
  el.textContent = n > 0 ? String(n) : "";
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
  const starting = startingText(s);
  let note = tab.querySelector(".starting");
  if (starting && !note) {
    note = document.createElement("span");
    note.className = "starting";
    tab.querySelector(".range").after(note);
  } else if (!starting && note) {
    note.remove();
  }
  if (note && starting) note.textContent = starting;
  let title;
  if (s.fault) title = `fault: ${JSON.stringify(s.fault)}`;
  else if (s.pin) title = pinTitle(s);
  else title = rangeText(s);
  const pairs = healthPairs(s);
  tab.dataset.baseTitle = `${title}; ${pairs.map(([k, v]) => `${k} ${v}`).join(" ")}`;
  if (starting) tab.dataset.baseTitle += `; ${starting}`;
  tab.title = tab.dataset.baseTitle;
  paintTab(tab);
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

// While a source's worker reports its start (`worker.starting`: a respawn measuring what its
// link sustains, the host busy meanwhile), the notice its tab shows as its own part (`.starting`
// after `.range`, on every tab and in every theme) and in its title: not a fault. Null otherwise.
function startingText(s) {
  const st = s.worker?.starting;
  if (!st) return null;
  return `measuring the link: ${st.detail}, ${st.step} of ${st.of}, up to ${Math.ceil(st.eta_ms / 1000)} s`;
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

// ---- link diagnostics (spec 4.6a) ------------------------------------------------------

// A source tab is a <button> and holds no control, so a source's own actions sit in one hover
// box beside the strip, `div#src-menu`: shown under a tab while the pointer is on a tab whose
// source has a `worker` record, or on the box, and hidden SRC_MENU_HIDE_MS after it left both.
// Its one entry, "Run link diagnostics", applies `source_measure` (applyOp: a conflict gets the
// usual stop-and-retry); when the respawn's worker says hello the transcript gets the result
// (noteDiagnostics).
const SRC_MENU_HIDE_MS = 300;
const LINK_DIAG_TITLE = "measures the rates this radio's link sustains: about a minute, the host will be busy";
const srcMenu = { el: null, source: null, tab: null, over: null, hide: null };

function srcMenuEl() {
  if (srcMenu.el) return srcMenu.el;
  const el = document.createElement("div");
  el.id = "src-menu";
  el.hidden = true;
  const diag = document.createElement("button");
  diag.className = "link-diag";
  diag.textContent = "Run link diagnostics";
  diag.title = LINK_DIAG_TITLE;
  diag.onclick = () => {
    const source = srcMenu.source;
    hideSrcMenu();
    if (source) applyOp({ op: "source_measure", source, replace: [] });
  };
  el.append(diag);
  el.onmouseenter = () => {
    srcMenu.over = "menu";
    clearTimeout(srcMenu.hide);
  };
  el.onmouseleave = () => leaveSrcMenu("menu");
  document.body.append(el);
  srcMenu.el = el;
  return el;
}

// The pointer came onto `tab`: the box shows under it when its source has a worker record.
function enterSrcTab(tab) {
  srcMenu.over = "tab";
  srcMenu.tab = tab;
  clearTimeout(srcMenu.hide);
  refreshSrcMenu();
}

function leaveSrcMenu(what) {
  if (srcMenu.over === what) srcMenu.over = null;
  clearTimeout(srcMenu.hide);
  srcMenu.hide = setTimeout(() => {
    if (!srcMenu.over) hideSrcMenu();
  }, SRC_MENU_HIDE_MS);
}

function hideSrcMenu() {
  clearTimeout(srcMenu.hide);
  srcMenu.over = null;
  srcMenu.tab = null;
  srcMenu.source = null;
  if (srcMenu.el) srcMenu.el.hidden = true;
}

// Show the box under the tab the pointer is on, or hide it when that tab's source has no
// worker record (each poll calls this too, so a record that arrives later shows it).
function refreshSrcMenu() {
  const tab = srcMenu.tab;
  if (!tab || srcMenu.over !== "tab") return;
  const s = state.sources.find((x) => x.id === tab.dataset.id);
  if (!s || !s.worker) {
    if (srcMenu.el) srcMenu.el.hidden = true;
    srcMenu.source = null;
    return;
  }
  const el = srcMenuEl();
  const r = tab.getBoundingClientRect();
  el.style.left = `${Math.max(0, Math.round(r.left))}px`;
  el.style.top = `${Math.round(r.bottom)}px`;
  srcMenu.source = s.id;
  el.hidden = false;
}

// Megasamples a second for `hz`, to at most two decimals, trailing zeros dropped (38.4).
function msps(hz) {
  return String(Number((hz / 1e6).toFixed(2)));
}

// What a source's link was measured at: [{bits, hz}] per depth (`bits` null for a source with
// one), from its worker's latest hello, else its record.
function linkFigures(s) {
  const latest = s?.worker?.link_measure;
  const depths = latest?.depths?.length ? latest.depths : s?.capabilities?.depths || [];
  if (depths.length) return depths.map((d) => ({ bits: d.bits, hz: hz(d.max_bandwidth_hz) }));
  const top = hz(latest?.max_bandwidth_hz ?? s?.capabilities?.max_bandwidth_hz);
  return top === null ? [] : [{ bits: null, hz: top }];
}

// The result line of a measure: `link diagnostics <source>: <items>; in force now, the record
// is set once a run`, an item per depth (`12-bit 30.72 → 38.4 Msps`, or `8-bit unchanged`), or
// `unchanged` alone when nothing moved. The figures govern tunes at once through the depth in
// force; the source's record takes only the first differing hello of a run.
export function diagnosticsLine(source, before, after) {
  const items = after.map(({ bits, hz: now }) => {
    const was = before.find((b) => b.bits === bits);
    const what = bits === null ? "" : `${bits}-bit `;
    if (was && was.hz === now) return null;
    return was ? `${what}${msps(was.hz)} → ${msps(now)} Msps` : `${what}→ ${msps(now)} Msps`;
  });
  const dropped = before.filter((b) => !after.some((a) => a.bits === b.bits));
  const moved = items.some((i) => i !== null) || dropped.length > 0;
  const text = moved
    ? [
        ...after.map(({ bits }, i) => items[i] ?? `${bits === null ? "" : `${bits}-bit `}unchanged`),
        ...dropped.map(({ bits }) => `${bits}-bit dropped`),
      ].join(", ")
    : "unchanged";
  return `link diagnostics ${source}: ${text}; in force now, the record is set once a run`;
}

// The measures waiting for their worker's hello, by source: the figures and the worker's
// `since_ns` before the call.
const diagnostics = new Map();

// Wait for `source`'s measure (just applied) to end, from `kept`, what diagKept(s) gave before.
function awaitDiagnostics(source, kept) {
  diagnostics.set(source, kept);
}

function diagKept(s) {
  return { before: linkFigures(s), since: s?.worker?.since_ns ?? null };
}

// On a `source_list` whose worker of a waiting source said hello after the call (a later
// `since_ns`) and is no longer starting, one transcript line with the result.
function noteDiagnostics(sources) {
  for (const [source, kept] of diagnostics) {
    const s = sources.find((x) => x.id === source);
    const w = s?.worker;
    if (!w || w.starting || typeof w.since_ns !== "number") continue;
    if (kept.since !== null && !(w.since_ns > kept.since)) continue;
    diagnostics.delete(source);
    state.transcript?.note(diagnosticsLine(source, kept.before, linkFigures(s)));
  }
}

function renderTabs(sources) {
  const tabs = $("#tabs");
  tabs.replaceChildren(
    ...sources.map((s) => {
      const tab = document.createElement("button");
      tab.className = "src-tab";
      tab.setAttribute("role", "tab");
      tab.dataset.id = s.id;
      const mark = document.createElement("span");
      mark.className = "default-mark";
      mark.setAttribute("aria-hidden", "true");
      const name = document.createElement("span");
      name.className = "name";
      name.textContent = s.id;
      const range = document.createElement("span");
      range.className = "range";
      range.textContent = rangeText(s);
      const centre = document.createElement("span");
      centre.className = "centre";
      centre.textContent = centreText(s);
      tab.append(mark, name, range, centre);
      decorateTab(tab, s);
      tab.addEventListener("mouseenter", () => enterSrcTab(tab));
      tab.addEventListener("mouseleave", () => leaveSrcMenu("tab"));
      tab.onclick = () => {
        select(s.id);
        if (state.view && state.view.get("main") !== "rf") state.view.set("main", "rf");
      };
      return tab;
    }),
    mapTab(),
  );
  paintMapCount();
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
    // A set_start's refusal names its member (and the channel of a plan member).
    const what = r.member === undefined
      ? `ch${r.channel}`
      : `${r.member}${r.channel === null || r.channel === undefined ? "" : ` ch${r.channel}`}`;
    state.transcript.note(`${what}: ${errorText(r.error)}`, true);
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
  // A measure's result is noted when the new worker says hello (noteDiagnostics).
  const kept = op.op === "source_measure" ? diagKept(state.sources.find((s) => s.id === op.source)) : null;
  try {
    result = await state.api.op(op);
    if (QUIET_OPS.has(op.op)) t.reply(op, result);
    if (kept) {
      awaitDiagnostics(op.source, kept);
      t.note(sourceReply(op, result));
    }
    if (op.op === "plan_start" || op.op === "set_start") noteRefusals(result && result.value);
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
const REPLACEABLE = new Set([
  "job_start",
  "job_retune",
  "source_ensure",
  "source_pin",
  "source_measure",
  "plan_start",
]);

// `<kind> <id> @ <centre MHz>` for holder `id`, from the last job list (the conflict's
// `current` centre when the job is not in it).
function holderText(id, err) {
  const j = state.lastJobs.find((x) => x.id === id);
  const t = (j && j.tuning) || err.current;
  const at = t && typeof t.center_hz === "number" ? ` @ ${mhz(t.center_hz)} MHz` : "";
  return `${j ? j.kind : "job"} ${id}${at}`;
}

// `conflict: source S held by …`, with the node's `unpin source S (source_unpin)` option
// after it when the source is pinned (a pin is never among the holders). A set_start's
// conflict leads its options with the role's own line (`radios.<role> …`), which already
// names each source and its holders: that line is shown instead of `source S held by …`.
function conflictText(err) {
  const options = Array.isArray(err.options) ? err.options : [];
  const unpin = options.find((o) => typeof o === "string" && o.startsWith("unpin source "));
  const tail = unpin ? `; ${unpin}` : "";
  if (typeof options[0] === "string" && options[0].startsWith("radios.")) {
    return `conflict: ${options[0]}${tail}`;
  }
  const holders = Array.isArray(err.holders) ? err.holders : [];
  const who = holders.length ? holders.map((h) => holderText(h, err)).join(", ") : "nobody";
  return `conflict: source ${err.source ?? "?"} held by ${who}${tail}`;
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

// The job form of origin_lookup (the node reads the job's band and modulation).
function originJobOp(job) {
  return { op: "origin_lookup", job, source: null, center_hz: null, bandwidth_hz: null, modulation: null };
}

// Origins by job `id`. The answer lights the map at once (an `origin` event for the same job,
// arriving on the feed as well, replaces it under the same key). It is noted in the
// transcript only for a job the overlays do not follow (a `bits` job on source `node`, a job
// on another source that is not positioned), whose event the page never sees; a followed
// job's event writes its own line.
async function askOrigins(id) {
  const r = await applyOp(originJobOp(id));
  const a = r && r.value;
  if (!a || !Array.isArray(a.origins)) return;
  state.map?.origin(id, a);
  if (state.ov?.follows(id)) return;
  const job = (state.lastJobs || []).find((j) => j.id === id);
  state.transcript?.note(`${job ? job.kind : "job"} ${String(id).slice(-6)}: ${originSummary(a)}`);
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
  if (verb === "measure") {
    if (args.length) return { error: "source measure: expected no arguments" };
    return { op: { op: "source_measure", source: src.id, replace: [] } };
  }
  if (verb === "capture") {
    if (args.length !== 1 || !["fit", "widest"].includes(args[0])) {
      return { error: "source capture: expected fit or widest" };
    }
    return { op: { op: "source_set_capture", source: src.id, policy: args[0] } };
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
  if (op.op === "source_measure") {
    return `measuring ${op.source} again: about a minute, the host will be busy`;
  }
  if (op.op === "source_set_capture") {
    return op.policy === "widest"
      ? `capture ${op.source}: widest (its full width, from the next time a job tunes it free)`
      : `capture ${op.source}: fit (the narrowest rate that holds the jobs, from the next time a job tunes it free)`;
  }
  const g = (result && result.value) || {};
  if (g.automatic || g.db === null || g.db === undefined) return `gain ${op.source}: automatic`;
  return `gain ${op.source}: ${g.db} dB (${g.min_db}..=${g.max_db})`;
}

// `source pin [<center_hz> <bandwidth_hz>]`, `source unpin`, `source gain <db>|auto`,
// `source measure`, `source capture fit|widest` on the shown source: {text, error} for the
// transcript, or {text: null}
// when a conflict was offered its retry line instead (offerReplace).
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
    const kept = diagKept(src);
    const result = await st.api.op(op);
    if (op.op === "source_measure") awaitDiagnostics(src.id, kept);
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

// The radio context every ask carries (spec 4.4): the shown radio (kept while the Map tab
// is selected), what #main shows, and the view's radio_policy and radio_prefer (the list
// split into ids; `none` is no preference).
export function askContext() {
  const prefer = state.view.get("radio_prefer");
  return {
    focus: state.selected ?? null,
    main: state.view.get("main"),
    radio_policy: state.view.get("radio_policy"),
    radio_prefer: prefer === "none" ? [] : prefer.split(","),
  };
}

// The radio the agent takes as its default for a request that names none: the shown radio
// under `focus` and `focus-fallback`, none under `free-first` and `ask`.
export function defaultRadio() {
  const policy = state.view.get("radio_policy");
  return policy === "focus" || policy === "focus-fallback" ? (state.selected ?? null) : null;
}

// The note lines an answer's `selection` adds after it: one per source a radio op ran on
// off the default radio (`ran on <src> (<tools>), not the default radio <radio> (<policy>)`,
// the tools distinct and in call order), and `<note>; no default radio` when the context's
// focus was dropped.
export function selectionNotes(selection) {
  if (!selection) return [];
  const lines = [];
  const bySource = new Map();
  for (const o of selection.off_default || []) {
    if (!bySource.has(o.source)) bySource.set(o.source, []);
    const tools = bySource.get(o.source);
    if (!tools.includes(o.tool)) tools.push(o.tool);
  }
  for (const [src, tools] of bySource) {
    lines.push(
      `ran on ${src} (${tools.join(", ")}), not the default radio ${selection.radio} ` +
        `(${selection.policy})`,
    );
  }
  if (selection.note && selection.radio == null && selection.note.endsWith("is not a source"))
    lines.push(`${selection.note}; no default radio`);
  return lines;
}

// Sends `text` to /ask and shows the answer (or the error, e.g. no LLM configured). The
// allowance is the session's `allow` switches unless `allowOverride` gives one for this
// request only (the unlock button's re-run); the switches are never changed here.
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
      accept_signatures: state.allowSignatures,
    };
    const r = await state.api.ask(text, 32, allow, askContext());
    let may = "";
    if (r.allow && (r.allow.stop_jobs || r.allow.replace)) may += " [may stop jobs]";
    if (r.allow && r.allow.delete_captures) may += " [may delete captures]";
    if (r.allow && r.allow.accept_signatures) may += " [may accept signatures]";
    t.assistant((r.answer ?? "(no answer)") + may, false, r.turn_log || [], r.calls || []);
    for (const line of selectionNotes(r.selection)) t.note(line);
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
      // A refused `capture_delete` needs `delete_captures`, a refused `signature_accept` or
      // `signature_reject` needs `accept_signatures`; any other refusal is a stop or a
      // replace. The retry re-sends this one request with the refused flags true on top of
      // the switches; the session stays as it was (`allow stops|deletes|signatures on` are
      // the switches).
      const isSignature = (c) => SIGNATURE_DECISIONS.includes(c.tool);
      const deletes = refused.some((c) => c.tool === "capture_delete");
      const signatures = refused.some(isSignature);
      const stopsRefused = refused.some((c) => c.tool !== "capture_delete" && !isSignature(c));
      const what = [
        stopsRefused && "stop or replace jobs",
        deletes && "delete captures",
        signatures && "accept or reject signatures",
      ]
        .filter(Boolean)
        .join(" or ");
      const label = [stopsRefused && "stops", deletes && "deletes", signatures && "signatures"]
        .filter(Boolean)
        .join(" and ");
      t.note(`the agent was not allowed to ${what}`, false, [
        {
          label: `allow ${label} and ask again`,
          // The note's buttons disable on click.
          run: () =>
            askAgent(text, {
              stop_jobs: stopsRefused || allow.stop_jobs,
              replace: stopsRefused || allow.replace,
              delete_captures: deletes || !!allow.delete_captures,
              accept_signatures: signatures || !!allow.accept_signatures,
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
  if (/^source\s+(pin|unpin|gain|measure|capture)\b/.test(text)) {
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
  const allow = /^allow\s+(stops|deletes|signatures)\s+(on|off)$/.exec(text);
  if (allow) {
    state.transcript.user(text);
    const on = allow[2] === "on";
    if (allow[1] === "stops") state.allowStops = on;
    else if (allow[1] === "deletes") state.allowDeletes = on;
    else state.allowSignatures = on;
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
  state.wf.onNotice = (text, actions = []) => state.transcript?.note(text, true, actions);
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
    requestPoll: () => (pollPaused || !state.jobs ? Promise.resolve() : state.jobs.refresh()),
  });
  state.map = mountMap($("#map"), api, state.view, {
    jobs: () => state.lastJobs,
    subjects: state.subjects,
    // A row with fixed stations fades its lit sites from the last station frame (plan 35),
    // any other from the job's last event.
    lastEvent: (id) => state.ov?.lastStationEvent(id) ?? state.ov?.lastEvent(id) ?? null,
    tiles: resolveTiles,
  });
  state.map.onChange(paintMapCount);
  state.ov.onEvent((job, p) => {
    state.subjects?.event(job, p);
    // A watch starting or dropping a decoder moves its row between polls (plan 31 spec 4.7).
    if (p && p.protocol === "watch" && p.job && (p.state === "decoding" || p.state === "lost")) {
      if (!pollPaused && state.jobs) state.jobs.refresh();
    }
    state.map?.event(job, p);
    if (p && p.protocol === "origin") state.map?.origin(job.id, p);
    const line = verdictLine(job, p);
    if (line) state.transcript.note(line);
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
    onOrigins: (key, answer) => state.map?.origin(key, answer),
    onBits: (id) => state.bits?.setJob(id),
  });
  state.bits = mountBitsPane($("#bits"), api, state.view, state.transcript, {
    panes: state.panes,
    jobs: () => state.lastJobs,
    onOrigins: (id) => askOrigins(id),
  });
  // The Video pane (video.js): the whole of #main while shown; the tags' live thumbnails.
  const video = mountVideo(api, {
    panes: state.panes,
    view: state.view,
    el: $("#video"),
    jobs: () => state.lastJobs,
    shownSource: () => state.selected ?? null,
  });
  state.video = video;
  state.panes.register("video", "video", {
    eligible: video.eligible,
    el: $("#video"),
    onShow: video.show,
    onHide: video.hide,
    full: true,
  });
  state.ov.onTagMedia = (id, canvas, on) => (on ? video.openThumb(id, canvas) : video.closeThumb(id));
  state.ov.onVideoLock = video.onLock;
  state.ov.onThumbClick = (id) => {
    const on = document.documentElement.dataset.full === "video";
    video.select(id);
    if (!on) state.panes?.click("video");
  };
  state.subjects.onLearn(applyPanes);
  loadStationHome(api);
  loadThemes(api);
  applyTheme();
  applySide();
  mountSideHeads();
  applyRail();
  applyMain();
  applyDock();
  applyMapTiles();
  state.subjects.onChange(paintDockHead);
  state.view.onChange((key) => {
    if (key === "plan") redrawPlan();
    if (key === "theme") applyTheme();
    if (key === "side") applySide();
    if (key === "main") applyMain();
    if (key === "pane") applyPanes();
    if (key === "dock") applyDock();
    if (key === "map_tiles") applyMapTiles();
    if (key === "radio_policy") paintDefault();
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
    sets: () => state.jobs?.sets() ?? [],
    sources: () => state.sources,
    onOrigins: (key, answer) => state.map?.origin(key, answer),
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
    audioKind: (kind) => state.subjects?.rowOf(kind) ?? null,
    view: state.view,
    shownSource: () => state.selected ?? null,
    applyOp,
    depths: (id) => state.sources.find((x) => x.id === id)?.capabilities?.depths ?? [],
    countEl: $(".pane.jobs h2 .count"),
    onJobs: (jobs) => {
      state.lastJobs = jobs;
      state.explore?.noteJobs(jobs);
      state.bits?.noteJobs(jobs);
      if (state.map) {
        state.map.sync();
        state.map.seed(jobs);
      }
      state.subjects?.sync(jobs);
      if (state.ov) state.ov.sync(jobs, shown());
      applyPanes();
      state.video?.sync();
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
    onOrigins: (job) => askOrigins(job.id),
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
  armClearAll($("#jobs-clear"));
  state.mic = mountMic($("#mic"), api, (text) => {
    $("#cmd").value = text;
    submitCommand();
  });
  loadSources();
}

// The transcript line of a watched job's verdict, or null: a watch's `unknown`, `decoding`
// and `lost` events, an identify's `verdict` event and any job's `origin` event (plan 32
// spec 4.3), as `<kind> <id tail>: <summary>` (an unknown's summary carries the register's
// nearest transmitter). Its ribbon still
// shows on the waterfall; the line keeps it.
export const VERDICT_WATCH_STATES = new Set(["unknown", "decoding", "lost"]);
function verdictLine(job, p) {
  if (!p || typeof p !== "object" || typeof p.summary !== "string") return null;
  const watch = p.protocol === "watch" && VERDICT_WATCH_STATES.has(p.state);
  if (!watch && p.protocol !== "verdict" && p.protocol !== "origin") return null;
  return `${job.kind} ${String(job.id).slice(-6)}: ${p.summary}`;
}

// The Jobs pane's Clear all: a first click arms it (`sure?` for CLEAR_CONFIRM_MS), a second
// applies the typed `stop all` (one `jobs_stop`, every job but the page's elastic spectrum)
// and then drops the finished rows as `view clear` does.
export const CLEAR_CONFIRM_MS = 3000;
function armClearAll(btn) {
  if (!btn) return;
  let armed = null;
  const label = btn.textContent;
  btn.addEventListener("click", async () => {
    if (armed === null) {
      btn.textContent = "sure?";
      armed = setTimeout(() => {
        armed = null;
        btn.textContent = label;
      }, CLEAR_CONFIRM_MS);
      return;
    }
    clearTimeout(armed);
    armed = null;
    btn.textContent = label;
    btn.disabled = true;
    try {
      state.transcript.user("stop all");
      await applyOp({ op: "jobs_stop", source: null, kind: null, plan: null });
      await state.jobs?.refresh(); // the stopped jobs are finished rows before the clear
      state.view?.clear();
    } finally {
      btn.disabled = false;
    }
  });
}

// The channel names drawn again: the outlines and the axis (the job rows redraw themselves
// on the view's change).
function redrawPlan() {
  if (state.ov) state.ov.syncOutlines(state.ov.lastJobs, shown());
  if (state.wf) state.wf.drawAxis();
}

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
  themeDock = theme.layout?.dock ?? null;
  themeTiles = theme;
  applyDock();
  applyMapTiles();
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
  // The docked pane's header bar (applyDock builds the section on demand): folds it in the
  // page's memory, the `side` policy untouched.
  document.getElementById("side")?.addEventListener("click", (ev) => {
    const head = ev.target.closest("#pane-dock .pane-head");
    if (!head || footprint() === "narrow") return;
    state.dockCollapsed = !state.dockCollapsed;
    applyDock();
  });
}

// The job tags live in the waterfall's rail when the theme's layout has one (<html
// data-rail="on">), on their outlines otherwise.
function applyRail() {
  if (!state.ov || !state.wf) return;
  state.ov.setRail(document.documentElement.dataset.rail === "on" ? state.wf.rail : null);
}

// <html data-main="rf|map"> from the view's `main` (style.css shows the waterfall or the map
// in #main), the strip's selection with it; the box menu's selection goes, as a source switch
// drops it.
function applyMain() {
  const main = state.view ? state.view.get("main") : "rf";
  const root = document.documentElement;
  if (root.dataset.main !== main) {
    root.dataset.main = main;
    if (main === "map") state.menu?.close();
  }
  syncMainTabs();
  state.panes?.render();
}

// The pane strip and #pane from the view's `pane` and the panes' eligibility (panes.js), then
// where #pane docks. Run on a view change, a row learned, a jobs poll and a footprint change,
// never on a message.
function applyPanes() {
  state.panes?.render();
  applyDock();
}

// ---- the dock ---------------------------------------------------------------------------
// The view's `dock` (auto | bottom | right) places the pane host #pane: `bottom` as the last
// child of #main (style.css gives it the bottom of the main area), `right` in the side column
// as a third section, `section.pane.dock#pane-dock`, between the Transcript and the Jobs pane,
// with a header bar naming the shown pane (and, for Subjects, a count) that folds it. `auto`
// is the applied theme's `layout.dock`, else bottom. <html data-dock> carries the resolved
// value in both footprints; the narrow footprint leaves #pane in #main (panes take the main
// area's place there). The move is a DOM move of the same element, so the panes see nothing.
// The fold is page memory (state.dockCollapsed), not a policy, forgotten when no pane is shown
// or the dock leaves `right`.
let themeDock = null; // the applied theme's layout.dock ("bottom" | "right" | null)

// ---- the map's tiles ----------------------------------------------------------------------
// The view's `map_tiles` resolved through the applied theme (map.js resolveMapTiles: its
// `layout.map_tiles`, else by its `color_scheme`) onto <html data-map-tiles> and the map's
// one tile layer. Run at load, on a `map_tiles` change and when a theme applies.
let themeTiles = null; // the applied theme.json (its color_scheme and layout.map_tiles)

function resolveTiles() {
  return resolveMapTiles(state.view ? state.view.get("map_tiles") : "auto", themeTiles);
}

function applyMapTiles() {
  const id = resolveTiles();
  const root = document.documentElement;
  if (root.dataset.mapTiles !== id) root.dataset.mapTiles = id;
  state.map?.applyTiles();
}

function resolveDock() {
  const v = state.view ? state.view.get("dock") : "auto";
  if (v === "bottom" || v === "right") return v;
  return themeDock === "right" ? "right" : "bottom";
}

function applyDock() {
  const dock = resolveDock();
  const root = document.documentElement;
  if (root.dataset.dock !== dock) root.dataset.dock = dock;
  const host = document.getElementById("pane");
  const main = document.getElementById("main");
  if (!host || !main) return;
  const pane = state.panes ? state.panes.current() : null;
  // The fold is forgotten when no pane is shown or the dock leaves `right`, so a return to
  // `right` comes back unfolded; a trip through the narrow footprint keeps it.
  if (!pane || dock !== "right") state.dockCollapsed = false;
  let sec = document.getElementById("pane-dock");
  const app = document.getElementById("app");
  // A full pane (the Video pane) takes #main whatever the dock.
  if (dock !== "right" || !pane || footprint() === "narrow" || root.dataset.full) {
    if (host.parentElement !== main) main.append(host);
    sec?.remove();
    delete app?.dataset.dockOpen;
    return;
  }
  if (!sec) {
    sec = document.createElement("section");
    sec.className = "pane dock";
    sec.id = "pane-dock";
    const h2 = document.createElement("h2");
    const head = document.createElement("button");
    head.type = "button";
    head.className = "pane-head";
    head.setAttribute("aria-controls", "pane-dock");
    const label = document.createElement("span");
    label.className = "label";
    const count = document.createElement("span");
    count.className = "count";
    head.append(label, " ", count);
    h2.append(head);
    sec.append(h2);
    const side = document.getElementById("side");
    side.insertBefore(sec, side.querySelector(".pane.jobs"));
  }
  if (host.parentElement !== sec) sec.append(host);
  // An open docked pane keeps the column wide under `side none` (style.css).
  if (state.dockCollapsed) {
    sec.dataset.collapsed = "";
    delete app?.dataset.dockOpen;
  } else {
    delete sec.dataset.collapsed;
    if (app) app.dataset.dockOpen = "";
  }
  sec.querySelector(".pane-head").setAttribute("aria-expanded", String(!state.dockCollapsed));
  paintDockHead();
}

// The docked section's header: the shown pane's tab label, capitalised (`Subjects`), and for
// Subjects the number of subjects the running jobs have, counted before merged groups fold (so
// it can exceed the table's rows; docs/themes.md says so); the count is empty for the others.
function paintDockHead() {
  const head = document.querySelector("#pane-dock .pane-head");
  const pane = state.panes ? state.panes.current() : null;
  if (!head || !pane) return;
  const tab = document.querySelector(`#panes .pane-tab[data-pane="${CSS.escape(pane)}"]`);
  const text = (tab ? tab.textContent : pane).trim();
  const label = text.charAt(0).toUpperCase() + text.slice(1);
  const labelEl = head.querySelector(".label");
  if (labelEl.textContent !== label) labelEl.textContent = label;
  let count = "";
  if (pane === "subjects" && state.subjects && state.subjectsPane) {
    // Per group: a watch's decoders share one set of subjects.
    const n = state.subjectsPane.groupIds().reduce((sum, g) => sum + state.subjects.size(g), 0);
    count = String(n);
  }
  const countEl = head.querySelector(".count");
  if (countEl.textContent !== count) countEl.textContent = count;
}

// The shown source's last `source_list` record (null when unknown).
function shown() {
  return state.sources.find((s) => s.id === state.selected) || null;
}

// The tabs follow each `source_list` poll (pin tag, title, fault) without re-selecting, and so
// do the hover box and the measures waiting for their result.
function syncTabs(sources) {
  for (const tab of document.querySelectorAll(".src-tab")) {
    const s = sources.find((x) => x.id === tab.dataset.id);
    if (s) decorateTab(tab, s);
  }
  refreshSrcMenu();
  noteDiagnostics(sources);
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
  if (state.video) state.video.destroy();
  state.video = null;
  if (state.subjectsPane) state.subjectsPane.destroy();
  if (state.subjects) state.subjects.destroy();
  if (state.coarse) state.coarse.destroy();
  if (state.wf) state.wf.destroy();
  state.ops = null;
  state.jobs = null;
  state.ov = null;
  state.map = null;
  state.panes = null;
  applyDock(); // #pane back in #main, the docked section gone
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
// mode (`waterfallMode`: "spectrum", "scan" or "sweep", null without a waterfall),
// draw the narrow-footprint answer ribbon for `text` without an LLM (ruling R3) or, given
// `hz` first, a ribbon at hz (keyed by `key` when given, of job id `job` when given; the
// element), and draw a probe label
// at a frequency.
let pollPaused = false;
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
  // The map's tile layer in force (map.js tileLayer: {id, url, attribution}), or null before
  // the map is made.
  tileLayer() {
    return state.map ? state.map.tileLayer() : null;
  },
  // Lights `answer` (an origin answer) under the one-shot `key` (`band:…`).
  lightOrigin(key, answer) {
    state.map?.origin(key, answer);
  },
  // The keys of the map's lit sets (map.js litKeys: job ids and one-shot keys).
  litKeys() {
    return state.map ? state.map.litKeys() : [];
  },
  // The home in force (view.js homeOf: view `home`, else the station read at load), or null.
  home() {
    return state.view ? homeOf(state.view) : null;
  },
  // The ids of the running jobs on the shown source whose squelch gate the page knows.
  squelchJobs() {
    return state.ov ? [...state.ov.squelches.keys()] : [];
  },
  // performance.now() of the job's last station frame (ov.lastStationEvent), or null.
  lastStationEvent(jobId) {
    return state.ov ? state.ov.lastStationEvent(jobId) : null;
  },
  poll() {
    return state.jobs ? state.jobs.refresh() : Promise.resolve();
  },
  // The jobs poll timer and the feed's poll request do nothing while on; poll() still works.
  pausePolling(on) {
    pollPaused = !!on;
    state.jobs?.pause(pollPaused);
  },
  // Runs ov.sync with the current (stale) job list as a fresh poll: a poll that started
  // before a held frame's job did.
  staleSync() {
    if (state.ov) state.ov.sync([...state.lastJobs], shown());
  },
  // How many feed frames the overlays hold for a job no poll has listed yet.
  pendingFrames() {
    return state.ov ? state.ov.pending.length : 0;
  },
  // Resolve the shown source's feed as if the node listed none (an older node), or again
  // as it is; takes effect at the next feed lookup (a source switch, a sync without a feed).
  hideFeeds(on) {
    if (state.ov) state.ov.hideFeeds = !!on;
  },
  // Tell the shown source's feed it lagged (a `lagged` message), or drop its socket so it
  // reconnects.
  feedLag() {
    const f = state.ov && state.ov.feed;
    if (f) state.ov.onFeedMessage(f, { type: "lagged", missed: 1 });
  },
  // Seed reads (job_context for a squelch gate) wait, once answered, while on.
  holdSeeds(on) {
    const ov = state.ov;
    if (!ov) return;
    if (on && !ov.seedHold) {
      ov.seedHold = new Promise((r) => (ov.seedRelease = r));
    } else if (!on && ov.seedHold) {
      const r = ov.seedRelease;
      ov.seedHold = null;
      r();
    }
  },
  // How many times reseedAll ran, and how many seed reads were discarded as stale.
  reseeds() {
    return state.ov ? state.ov.reseeds : 0;
  },
  staleSeedsDiscarded() {
    return state.ov ? state.ov.staleSeeds : 0;
  },
  feedReconnect() {
    const f = state.ov && state.ov.feed;
    if (f) f.sub.drop();
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
  // The shown source's id (state.selected; unchanged while the Map tab is selected).
  shown() {
    return state.selected ?? null;
  },
  // The note lines an answer's `selection` adds (selectionNotes).
  selectionNotes(selection) {
    return selectionNotes(selection);
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
    // The video pane's feeds: another radio's lock events come from its own event feed.
    state.video?.refresh();
  });
  wireCommandLine();
  window.addEventListener("resize", applyFootprint);
  $("#drawer-toggle").addEventListener("click", () => {
    const root = document.documentElement;
    if (root.dataset.drawer === "open") delete root.dataset.drawer;
    else root.dataset.drawer = "open";
  });
  $("#listen-all").addEventListener("click", () => listen(null, { toggle: true }));
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
