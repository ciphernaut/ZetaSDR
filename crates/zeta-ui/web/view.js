// view.js: the page's view policies (spec 4.3) and the `view` commands' state.
//
// Exports:
//   POLICIES   the registry, one entry per policy: {key, values | parse, default_, describe,
//       tab?} (`tab: true`: per tab; every other policy is shared by the browser, see below).
//       `values` lists the allowed words; `parse(text)` instead returns the value or
//       undefined when `text` is not one; `describe` names what is allowed (error texts and
//       `view show`). A new behaviour is a new entry: the commands and `view show` are
//       generic over the table.
//         follow           target | free (default target)
//         zoom_out         scan | none (default scan)
//         auto_scan_stop   inside | keep (default inside)
//         finished_rows_s  a non-negative integer of seconds, 0 = never shown (default 60)
//         scanner          auto (the shown source) | a source id (default auto; coarse.js)
//         plan             auto | none | a loaded channel plan id (default auto; plans.js)
//         main             rf | map (default rf): what #main shows, the waterfall or the map
//                          (app.js applyMain; the Map tab of the sources strip sets it); a
//                          `main` set that contradicts `map on` or `off` puts it back to auto
//         map              auto | on | off (default auto): an alias of `main`: `on` sets
//                          `main map`, `off` sets `main rf`, `auto` changes nothing
//         home             `LAT,LON` (lat -90..90, lon -180..180, decimal degrees) | none
//                          (default none): where the map's home marker sits and the job rows'
//                          distance and bearing count from (none: the node's station, when
//                          one is set; see homeOf); kept as `<lat>,<lon>` with the
//                          numbers as JavaScript prints them (`-27.50,153.00` → `-27.5,153`)
//         subjects         auto | on | off (default auto): an alias of `pane` for the
//                          Subjects pane (subjects.js): `on` sets `pane subjects`, `off` sets
//                          `pane none` when Subjects is the pane shown, `auto` changes no pane
//                          (the tab shows while a running job's row has table columns); a
//                          `pane` set that contradicts `on` or `off` puts it back to auto
//         subject_dim_s    a positive integer of seconds (default 60): a silent subject dims
//         subject_drop_s   a positive integer of seconds (default 300): and then is dropped
//         origin_fade_s    a positive integer of seconds (default ORIGIN_FADE_S_DEFAULT, 300):
//                          a lit site's brightness falls to nothing over this long after its
//                          signal was last active, then the site goes (map.js tick)
//         pane             none | a registered pane's id (default none; panes.js): the pane
//                          shown under the waterfall. A stored id is kept over a reload before
//                          its pane registers (the strip shows none until it does)
//         theme            a ready theme's id (default console; themeIds): the theme the
//                          page applies (app.js applyTheme). A stored id that is a valid id
//                          is kept over a reload before themes_list answers; app.js falls
//                          back to console when it is not ready
//         side             both | transcript | jobs | none (default both): which of the side
//                          column's panes are open, the others folded to their header bars
//                          (app.js applySide)
//         dock             auto | bottom | right (default auto): where #pane shows, under the
//                          main view or as a third folding section of the side column
//                          (app.js applyDock); auto is the theme's `layout.dock`, else bottom
//         map_tiles        auto | osm | osm-dark | opentopo | esri-street | esri-imagery
//                          (default auto): the map's tile provider (map.js TILE_PROVIDERS;
//                          app.js applyMapTiles); auto is the theme's `layout.map_tiles`, else
//                          osm-dark under a dark theme and osm under a light one
//         radio_policy     focus | focus-fallback | free-first | ask (default focus): how the
//                          agent picks a radio for a request that names none (app.js
//                          askContext sends it with every ask)
//         radio_prefer     none | a comma-separated list of source ids, no repeats (default
//                          none): the agent's preference order among the free radios; the ids
//                          are not checked against source_list (a radio absent today may be
//                          attached tomorrow)
//         dir              auto | ltr | rtl (default auto): the document's text direction
//                          (app.js applyDir sets <html dir>; auto leaves the attribute off, so
//                          the browser's own applies). It mirrors the frame: the side column,
//                          the rail and axis, the tab strips, text alignment and the menus.
//                          Frequency is vertical on the waterfall (highest at the top), so
//                          `dir` never turns it round; which side time runs from is
//                          `time_flow`'s alone, independent of `dir`
//         time_flow        right | left (default right): the side the newest column is on.
//                          `right`: the newest columns at the right edge, the waterfall
//                          scrolling and ribbons drifting left; `left`: the mirror. Independent
//                          of `dir`. The waterfall's newestSide() reads it (waterfall.js) and
//                          every part with a time direction asks that, never this policy
//         video            smooth | sharp (default smooth): the full video view's picture
//                          (video.js): every field at 360 wide, or full width 5 a second
//       Where a policy lives. A policy that is about what this tab is looking at (a
//       navigation state) is per tab (`tab: true`, in sessionStorage under VIEW_KEY): follow,
//       scanner, plan, main, map, pane, subjects. Every other policy (a look-and-feel, layout
//       or behaviour preference: theme, side, dock, map_tiles, video, the seconds, home,
//       zoom_out, auto_scan_stop, radio_policy, radio_prefer) is shared by the browser, in
//       `localStorage` under VIEW_PREFS_KEY, so a new tab starts from what was set and a change
//       in one tab applies in the others at once (the `storage` event fires onChange for each
//       key that differs, once the page has mounted: listen). A value an older page left under
//       RADIO_PREFS_KEY is carried over once. An older page kept every policy per tab: the first
//       tab of this build to load, finding no shared store, copies the shared-kind values of its
//       own VIEW_KEY into it (and leaves them there; this build reads VIEW_KEY for the per-tab
//       policies only); every later tab finds the store and keeps it.
//   homePoint(value) -> {lat, lon} | null   the `home` policy's value as numbers (null: none)
//   setStationHome(point|null)  the node's station position ({lat, lon}; app.js reads it once
//       with `station_get` at page load), the home used while the view's `home` is none
//   homeOf(view) -> {lat, lon} | null   the home in force: the view's `home` when set, else
//       the station's (`view set home` takes precedence; `none` falls back to the station)
//   themeIds() -> [id]   the ids `view set theme` accepts: the built-ins until
//       setThemeIds(ids) gives the ready ids `themes_list` answered (app.js loadThemes)
//   AGENT_VIEW_KEYS   the keys an agent's `view_set` may set (presentation only; app.js
//       refuses any other before applying)
//   VIEW_KEY   the sessionStorage key holding the per-tab values set ({key: value}, defaults
//       left out)
//   VIEW_PREFS_KEY   the localStorage key holding the shared values set (the same shape),
//       written even with none set (`{}`: this build has run in the browser); with storage
//       unavailable the defaults apply and a value set holds for the page load
//   RADIO_PREFS_KEY   where an older page kept radio_policy and radio_prefer (migrated)
//   mountView() -> View
//     view.get(key)             the value (the default when not set)
//     view.set(key, text) -> {ok: true, value} | {ok: false, error}
//         error texts: `unknown view key "<key>" (allowed: follow, zoom_out, …)`,
//         `view set <key>: "<text>" is not allowed (allowed: <describe>)`
//     view.show() -> string     one line per key: `<key> <value> (allowed <describe>)`
//     view.reset()              every policy back to its default (fires onChange per key),
//                               the shared store (localStorage) cleared too, so every tab follows
//     view.clear()              fires onClear (the jobs list drops its finished rows)
//     view.onChange(cb)         cb(key, value) after a set or reset
//     view.onClear(cb)          cb() on clear()
//     view.listen()             follows the other tabs from now (the `storage` event: before,
//                               it is ignored), reading the shared store anew first (app.js
//                               calls it once the page has mounted every listener)
//     view.destroy()            stops following them and removes the listener (logout)
//   runViewCommand(view, words) -> {text, error}
//       The transcript line for `view <words…>`: `set <key> <value>` → `<key> = <value>`,
//       `show`, `reset` → `view policies reset`, `clear` → `finished rows cleared`;
//       anything else an error naming the commands.

import { planIds } from "./plans.js";
import { paneIds } from "./panes.js";

export const VIEW_KEY = "zeta.view";
// The localStorage key of the shared policies; RADIO_PREFS_KEY is where an older page kept
// radio_policy and radio_prefer (read once and moved).
export const VIEW_PREFS_KEY = "zeta.view_policies";
export const RADIO_PREFS_KEY = "zeta.radio_prefs";
// The `origin_fade_s` default: a lit site fades over the subjects' drop time.
export const ORIGIN_FADE_S_DEFAULT = 300;

// The keys the agent's page-only `view_set` may set: presentation only (zeta-agent's
// VIEW_KEYS). `zoom_out`, `scanner` and `auto_scan_stop` drive scans and `home` moves where
// distances count from: those are the operator's to type.
export const AGENT_VIEW_KEYS = [
  "theme",
  "side",
  "dock",
  "main",
  "map",
  "pane",
  "subjects",
  "finished_rows_s",
  "subject_dim_s",
  "subject_drop_s",
  "plan",
  "follow",
  "origin_fade_s",
  "map_tiles",
  "video",
];

const SOURCE_ID = /^[A-Za-z0-9_.:-]+$/;
const DECIMAL = /^[+-]?(\d+(\.\d*)?|\.\d+)$/;

// `LAT,LON` as two numbers in range, or null.
function latLon(text) {
  const parts = String(text).split(",");
  if (parts.length !== 2 || !parts.every((x) => DECIMAL.test(x))) return null;
  const [lat, lon] = parts.map(Number);
  if (!Number.isFinite(lat) || !Number.isFinite(lon)) return null;
  if (Math.abs(lat) > 90 || Math.abs(lon) > 180) return null;
  return { lat, lon };
}

export function homePoint(value) {
  return value === "none" ? null : latLon(value);
}

let stationHome = null;

export function setStationHome(point) {
  const ok = point && Number.isFinite(point.lat) && Number.isFinite(point.lon);
  stationHome = ok ? { lat: point.lat, lon: point.lon } : null;
}

export function homeOf(view) {
  return homePoint(view.get("home")) ?? stationHome;
}

const PANE_ID = /^[a-z][a-z0-9_]*(:[A-Za-z0-9_.:-]+)?$/;
const THEME_ID = /^[a-z][a-z0-9_-]{0,31}$/;

// The built-in themes, offered until `themes_list` answers.
const BUILTIN_THEMES = ["console", "plain", "cyberpunk", "terminal", "psytrance", "vibecoder"];
let readyThemes = BUILTIN_THEMES;

export function themeIds() {
  return [...readyThemes];
}

export function setThemeIds(ids) {
  readyThemes = ids.filter((id) => typeof id === "string" && THEME_ID.test(id));
}

const positiveSeconds = (t) =>
  /^[1-9]\d*$/.test(t) && Number.isSafeInteger(Number(t)) ? Number(t) : undefined;

export const POLICIES = [
  {
    key: "follow",
    tab: true,
    values: ["target", "free"],
    default_: "target",
    describe: "target, free",
  },
  { key: "zoom_out", values: ["scan", "none"], default_: "scan", describe: "scan, none" },
  {
    key: "auto_scan_stop",
    values: ["inside", "keep"],
    default_: "inside",
    describe: "inside, keep",
  },
  {
    key: "finished_rows_s",
    parse: (t) => (/^\d+$/.test(t) && Number.isSafeInteger(Number(t)) ? Number(t) : undefined),
    default_: 60,
    describe: "a non-negative integer of seconds, 0 for never shown",
  },
  {
    key: "scanner",
    tab: true,
    parse: (t) => (SOURCE_ID.test(t) ? t : undefined),
    default_: "auto",
    describe: "auto (the shown source), a source id",
  },
  {
    key: "plan",
    tab: true,
    parse: (t) => (t === "auto" || t === "none" || planIds().includes(t) ? t : undefined),
    default_: "auto",
    get describe() {
      return ["auto", "none", ...planIds()].join(", ");
    },
  },
  {
    key: "main",
    tab: true,
    values: ["rf", "map"],
    default_: "rf",
    describe: "rf, map",
    then(v, view, put) {
      const m = view.get("map");
      if ((m === "on" && v !== "map") || (m === "off" && v !== "rf")) put("map", "auto");
    },
  },
  {
    key: "map",
    tab: true,
    values: ["auto", "on", "off"],
    default_: "auto",
    describe: "auto, on, off",
    // The alias: `on` and `off` choose what #main shows.
    then(v, view, put) {
      if (v === "on") put("main", "map");
      else if (v === "off") put("main", "rf");
    },
  },
  {
    key: "home",
    parse: (t) => {
      if (t === "none") return t;
      const p = latLon(t);
      return p ? `${p.lat},${p.lon}` : undefined;
    },
    default_: "none",
    describe: "LAT,LON in decimal degrees (lat -90..90, lon -180..180), none",
  },
  {
    key: "subjects",
    tab: true,
    values: ["auto", "on", "off"],
    default_: "auto",
    describe: "auto, on, off",
    // The alias: `on` and `off` choose the pane.
    then(v, view, put) {
      if (v === "on") put("pane", "subjects");
      else if (v === "off" && view.get("pane") === "subjects") put("pane", "none");
    },
  },
  {
    key: "subject_dim_s",
    parse: positiveSeconds,
    default_: 60,
    describe: "a positive integer of seconds",
  },
  {
    key: "subject_drop_s",
    parse: positiveSeconds,
    default_: 300,
    describe: "a positive integer of seconds",
  },
  {
    key: "origin_fade_s",
    parse: positiveSeconds,
    default_: ORIGIN_FADE_S_DEFAULT,
    describe: "a positive integer of seconds",
  },
  {
    key: "pane",
    tab: true,
    parse: (t) => (t === "none" || paneIds().includes(t) ? t : undefined),
    restore: (t) => (t === "none" || PANE_ID.test(t) ? t : undefined),
    default_: "none",
    get describe() {
      return ["none", ...paneIds()].join(", ");
    },
    then(v, view, put) {
      const s = view.get("subjects");
      if ((s === "on" && v !== "subjects") || (s === "off" && v === "subjects")) {
        put("subjects", "auto");
      }
    },
  },
  {
    key: "theme",
    parse: (t) => (themeIds().includes(t) ? t : undefined),
    restore: (t) => (THEME_ID.test(t) ? t : undefined),
    default_: "console",
    get describe() {
      return themeIds().join(", ");
    },
  },
  {
    key: "side",
    values: ["both", "transcript", "jobs", "none"],
    default_: "both",
    describe: "both, transcript, jobs, none",
  },
  {
    key: "dock",
    values: ["auto", "bottom", "right"],
    default_: "auto",
    describe: "auto, bottom, right",
  },
  {
    key: "map_tiles",
    values: ["auto", "osm", "osm-dark", "opentopo", "esri-street", "esri-imagery"],
    default_: "auto",
    describe: "auto, osm, osm-dark, opentopo, esri-street, esri-imagery",
  },
  {
    key: "radio_policy",
    values: ["focus", "focus-fallback", "free-first", "ask"],
    default_: "focus",
    describe: "focus, focus-fallback, free-first, ask",
  },
  {
    key: "radio_prefer",
    parse: (t) => {
      if (t === "none") return t;
      const ids = t.split(",");
      const ok = ids.every((id) => SOURCE_ID.test(id)) && new Set(ids).size === ids.length;
      return ok ? t : undefined;
    },
    default_: "none",
    describe: "none, a comma-separated list of source ids",
  },
  {
    key: "dir",
    values: ["auto", "ltr", "rtl"],
    default_: "auto",
    describe: "auto (the browser's own), ltr, rtl",
  },
  {
    key: "time_flow",
    values: ["right", "left"],
    default_: "right",
    describe: "right (the newest column on the right, time running left), left",
  },
  {
    key: "video",
    values: ["smooth", "sharp"],
    default_: "smooth",
    describe: "video picture: smooth (every field, 360 wide) or sharp (full width, 5 a second)",
  },
];

const byKey = new Map(POLICIES.map((p) => [p.key, p]));

function parseValue(p, text) {
  if (p.values) return p.values.includes(text) ? text : undefined;
  return p.parse(text);
}

// The per-tab values (sessionStorage[VIEW_KEY]): an object of the keys set.
function loadStored() {
  try {
    const v = JSON.parse(sessionStorage.getItem(VIEW_KEY) || "{}");
    return v && typeof v === "object" && !Array.isArray(v) ? v : {};
  } catch {
    return {};
  }
}

function saveStored(values) {
  try {
    if (Object.keys(values).length) sessionStorage.setItem(VIEW_KEY, JSON.stringify(values));
    else sessionStorage.removeItem(VIEW_KEY);
  } catch {
    /* storage unavailable: the values hold for this page load */
  }
}

// The shared policies in localStorage (a JSON object of the keys set), or null when
// storage is unavailable.
function loadShared() {
  try {
    const v = JSON.parse(localStorage.getItem(VIEW_PREFS_KEY) || "{}");
    return v && typeof v === "object" && !Array.isArray(v) ? v : {};
  } catch {
    return null;
  }
}

// True when the write took; false when storage refused it (the values then hold for this
// page load, in the caller's held copy). The key is written even with no value set (`{}`): its
// presence says this build has kept the shared policies here (see the carry-over in mountView).
function saveShared(values) {
  try {
    localStorage.setItem(VIEW_PREFS_KEY, JSON.stringify(values));
    return true;
  } catch {
    return false;
  }
}

// Whether the shared store exists (null: storage unavailable).
function sharedExists() {
  try {
    return localStorage.getItem(VIEW_PREFS_KEY) !== null;
  } catch {
    return null;
  }
}

// The value a stored `text` of policy `p` stands for, or undefined when it no longer parses
// (a stored value from an older page may not).
function restoreValue(p, text) {
  return p.restore ? p.restore(text) : parseValue(p, text);
}

// The valid values of a stored object, for the policies that are `tab` or not.
function valuesFrom(stored, tab) {
  const out = {};
  for (const [k, v] of Object.entries(stored)) {
    const p = byKey.get(k);
    if (!p || !!p.tab !== tab) continue;
    const kept = restoreValue(p, String(v));
    if (kept !== undefined) out[k] = kept;
  }
  return out;
}

export function mountView() {
  // The per-tab values (policies marked `tab`), and the shared ones (every other policy).
  const tabStored = loadStored();
  const values = valuesFrom(tabStored, true);
  let shared = valuesFrom(loadShared() ?? {}, false);
  // Once a write has failed the held copy is the truth for the rest of the page load (a
  // read that still works would give back the value from before).
  let storageFailed = false;
  // An older page kept every policy per tab: the first tab of this build to load, finding no
  // shared store, carries its own shared-kind values into one (they stay in the tab's store,
  // which this build reads for the per-tab ones only); later tabs find the store and keep it.
  if (sharedExists() === false) {
    shared = valuesFrom(tabStored, false);
    if (!saveShared(shared)) storageFailed = true;
  }
  // The radio preferences an older page left under RADIO_PREFS_KEY are carried over once
  // (a value already shared wins), then the old key goes.
  try {
    const old = JSON.parse(localStorage.getItem(RADIO_PREFS_KEY) || "null");
    if (old && typeof old === "object" && !Array.isArray(old)) {
      const present = {};
      for (const k of ["radio_policy", "radio_prefer"]) if (k in old) present[k] = old[k];
      const moved = valuesFrom(present, false);
      shared = { ...moved, ...shared };
      if (!saveShared(shared)) storageFailed = true;
    }
    if (!storageFailed) localStorage.removeItem(RADIO_PREFS_KEY);
  } catch {
    /* storage unavailable, or the old value unreadable */
  }
  // A stored `map on` from a page before `main` (spec 4.4) still shows the map (`off` is
  // `main rf`, the default).
  if (!("main" in values) && values.map === "on") values.main = "map";
  const changeCbs = [];
  const clearCbs = [];
  const fire = (key) => {
    for (const cb of changeCbs) cb(key, view.get(key));
  };
  // Another tab changed the shared policies: read them anew and announce what differs.
  const refresh = () => {
    if (storageFailed) return;
    const stored = loadShared();
    if (!stored) return;
    const next = valuesFrom(stored, false);
    const before = shared;
    shared = next;
    for (const p of POLICIES) {
      if (!p.tab && before[p.key] !== next[p.key]) fire(p.key);
    }
  };
  // A value already checked, stored and announced (an alias's `then` sets another key so).
  const put = (key, v) => {
    const p = byKey.get(key);
    if (p.tab) {
      if (v === p.default_) delete values[key];
      else values[key] = v;
      saveStored(values);
    } else {
      refresh();
      const next = { ...shared };
      if (v === p.default_) delete next[key];
      else next[key] = v;
      shared = next;
      if (!saveShared(next)) storageFailed = true;
    }
    fire(key);
  };
  // Another tab's change, once the page has mounted (listen) and until destroy.
  let listening = false;
  const onStorage = (e) => {
    if (listening && (e.key === null || e.key === VIEW_PREFS_KEY)) refresh();
  };
  window.addEventListener("storage", onStorage);
  const view = {
    listen() {
      if (listening) return;
      listening = true;
      refresh();
    },
    destroy() {
      listening = false;
      window.removeEventListener("storage", onStorage);
    },
    get(key) {
      const p = byKey.get(key);
      if (!p) return undefined;
      const src = p.tab ? values : shared;
      return key in src ? src[key] : p.default_;
    },
    set(key, text) {
      const p = byKey.get(key);
      if (!p) {
        const keys = POLICIES.map((x) => x.key).join(", ");
        return { ok: false, error: `unknown view key "${key}" (allowed: ${keys})` };
      }
      const v = parseValue(p, String(text));
      if (v === undefined) {
        return { ok: false, error: `view set ${key}: "${text}" is not allowed (allowed: ${p.describe})` };
      }
      put(key, v);
      if (p.then) p.then(v, view, put);
      return { ok: true, value: v };
    },
    show() {
      return POLICIES.map((p) => `${p.key} ${view.get(p.key)} (allowed ${p.describe})`).join(
        "\n",
      );
    },
    reset() {
      for (const k of Object.keys(values)) delete values[k];
      saveStored(values);
      shared = {};
      if (!saveShared(shared)) storageFailed = true;
      for (const p of POLICIES) fire(p.key);
    },
    clear() {
      for (const cb of clearCbs) cb();
    },
    onChange(cb) {
      changeCbs.push(cb);
    },
    onClear(cb) {
      clearCbs.push(cb);
    },
  };
  return view;
}

export function runViewCommand(view, words) {
  const [cmd, ...rest] = words;
  if (cmd === "set" && rest.length === 2) {
    const r = view.set(rest[0], rest[1]);
    return r.ok ? { text: `${rest[0]} = ${r.value}`, error: false } : { text: r.error, error: true };
  }
  if (cmd === "show" && rest.length === 0) return { text: view.show(), error: false };
  if (cmd === "reset" && rest.length === 0) {
    view.reset();
    return { text: "view policies reset", error: false };
  }
  if (cmd === "clear" && rest.length === 0) {
    view.clear();
    return { text: "finished rows cleared", error: false };
  }
  return {
    text: `view: expected "set <key> <value>", "show", "reset" or "clear", got "${words.join(" ")}"`,
    error: true,
  };
}
