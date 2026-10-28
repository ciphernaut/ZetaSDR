// view.js: the page's view policies (spec 4.3) and the `view` commands' state.
//
// Exports:
//   POLICIES   the registry, one entry per policy: {key, values | parse, default_, describe}.
//       `values` lists the allowed words; `parse(text)` instead returns the value or
//       undefined when `text` is not one; `describe` names what is allowed (error texts and
//       `view show`). A new behaviour is a new entry: the commands and `view show` are
//       generic over the table.
//         follow           target | free (default target)
//         zoom_out         scan | none (default scan)
//         auto_scan_stop   inside | keep (default inside)
//         finished_rows_s  a non-negative integer of seconds, 0 = never shown (default 60)
//         scanner          auto | a source id (default auto)
//         plan             auto | none | a loaded channel plan id (default auto; plans.js)
//         map              on | off (default off): the map pane (map.js)
//         home             `LAT,LON` (lat -90..90, lon -180..180, decimal degrees) | none
//                          (default none): where the map's home marker sits and the job rows'
//                          distance and bearing count from; kept as `<lat>,<lon>` with the
//                          numbers as JavaScript prints them (`-27.50,153.00` → `-27.5,153`)
//   homePoint(value) -> {lat, lon} | null   the `home` policy's value as numbers (null: none)
//   VIEW_KEY   the sessionStorage key holding the values set ({key: value}, defaults left out)
//   mountView() -> View
//     view.get(key)             the value (the default when not set)
//     view.set(key, text) -> {ok: true, value} | {ok: false, error}
//         error texts: `unknown view key "<key>" (allowed: follow, zoom_out, …)`,
//         `view set <key>: "<text>" is not allowed (allowed: <describe>)`
//     view.show() -> string     one line per key: `<key> <value> (allowed <describe>)`
//     view.reset()              every policy back to its default (fires onChange per key)
//     view.clear()              fires onClear (the jobs list drops its finished rows)
//     view.onChange(cb)         cb(key, value) after a set or reset
//     view.onClear(cb)          cb() on clear()
//   runViewCommand(view, words) -> {text, error}
//       The transcript line for `view <words…>`: `set <key> <value>` → `<key> = <value>`,
//       `show`, `reset` → `view policies reset`, `clear` → `finished rows cleared`;
//       anything else an error naming the commands.

import { planIds } from "./plans.js";

export const VIEW_KEY = "zeta.view";

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

export const POLICIES = [
  { key: "follow", values: ["target", "free"], default_: "target", describe: "target, free" },
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
    parse: (t) => (SOURCE_ID.test(t) ? t : undefined),
    default_: "auto",
    describe: "auto, a source id",
  },
  {
    key: "plan",
    parse: (t) => (t === "auto" || t === "none" || planIds().includes(t) ? t : undefined),
    default_: "auto",
    get describe() {
      return ["auto", "none", ...planIds()].join(", ");
    },
  },
  { key: "map", values: ["on", "off"], default_: "off", describe: "on, off" },
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
];

const byKey = new Map(POLICIES.map((p) => [p.key, p]));

function parseValue(p, text) {
  if (p.values) return p.values.includes(text) ? text : undefined;
  return p.parse(text);
}

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

export function mountView() {
  // Only values that still parse are kept (a stored value from an older page may not).
  const values = {};
  for (const [k, v] of Object.entries(loadStored())) {
    const p = byKey.get(k);
    if (p && parseValue(p, String(v)) !== undefined) values[k] = parseValue(p, String(v));
  }
  const changeCbs = [];
  const clearCbs = [];
  const fire = (key) => {
    for (const cb of changeCbs) cb(key, view.get(key));
  };
  const view = {
    get(key) {
      const p = byKey.get(key);
      if (!p) return undefined;
      return key in values ? values[key] : p.default_;
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
      if (v === p.default_) delete values[key];
      else values[key] = v;
      saveStored(values);
      fire(key);
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
