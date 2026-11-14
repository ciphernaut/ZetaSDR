// units.js: frequencies as the page writes them in prose (transcript notes); the vocabulary
// lines (center_hz=…) keep integer Hz.
//
// Exports:
//   mhzText(hz) -> "<MHz to 4 decimals>"        e.g. 100001000 -> "100.0010"
//   khzText(hz) -> "<kHz to 1 decimal>"         e.g. 5000 -> "5.0"
//   bandText(centerHz, bandwidthHz) -> "<mhzText(centerHz)> MHz, <khzText(bandwidthHz)> kHz"
//   metricText(value, unit) -> a mode row's value in metres and km/h, rounded: `ft`, `m` ->
//       "6401 m", `kt`, `km_h` -> "780 km/h", `m_s` -> "+5.0 m/s" (a climb rate stays a climb
//       rate, one decimal and its sign), `deg` -> "245°" (359.6 -> "0°"); no unit or a
//       non-number as sent, an object as JSON; null or undefined -> ""
//   registerText(allocations) -> "register says <EMISSION> <licensee>, <site>, <d> km, <p> W"
//       for the nearest `tx` assignment of a classification's or verdict's allocations
//       (parts it lacks left out), or null when there is none (an older node's line: a node
//       that sends `register` gets registerLines instead)
//   emissionWords(decoded) -> an emission designator's words from its decoded parts
//       (`emission_decoded`): "Frequency modulation, data"; "" when it has none
//   registerVerdictLine(register) -> a classification's `register` answer's verdict line, or
//       null (no register, or no verdict on a register that answered): the node's
//       `verdict.summary` (`register agrees: …`, `register disagrees: …`, `nothing licensed in
//       range; …`, `register unreachable: …`, `register could not list licences: …`; with no
//       verdict, an unreachable answer reads `register unreachable: <reason>` and one whose
//       `candidates` query failed `register could not list licences: <reason>`), then
//       ` (register data as of <YYYY-MM-DD>)` when the answer has an `as_of`
//   registerLines(register, {measured}) -> the transcript lines for a `register` answer
//       ([] when it is null), in this order:
//         - the verdict line (registerVerdictLine);
//         - for a `partial` status, `register partly answered: <query>: <reason>` per failed
//           query;
//         - unless unreachable: `band: <primary services>` with `; secondary <others>` (each
//           service's qualifier in brackets after it; every service when none is primary; no
//           line for an empty plan);
//         - the first REGISTER_TOP candidates that are not receivers (only a transmitter can be
//           the signal), each `<holder>, <site>, <d> km <bearing>°, <emission words>, expected
//           about <n> dBm`: the holder `licence <no>` when unnamed, whole kilometres and
//           degrees, the designator when it has no words, `, measured <m> dBm` after the
//           expected level when `measured` (dBm, a calibrated source's) is a number, `, pointed
//           away` when its geometry's off_beam_deg exceeds half the device's beamwidth_deg;
//           parts a candidate lacks left out;
//         - what the first of them belongs to, when its `related` names anything:
//           `belongs to: licence <no>, paired with <mhz> MHz (<role>), …, <n> more channels of
//           the holder at the site, link to <far site> <d> km`.
//   originSummary(answer) -> one line for an `origin_lookup` answer, as the node's
//       origin::summary words it: "<n> sites on <f> MHz: <licensee>, <site> <d> km <agree>
//       <score>; ..." naming the first ORIGIN_SUMMARY_SITES (the distance left out when
//       unknown, the service when the row names neither licensee nor site); with no origins,
//       the register's status first (`register_status`: "register unreachable: <reason>", or
//       "register could not list licences: <reason>; partial answer: <query>, … failed" when
//       its `candidates` query failed), else "no register provider" (`register_rows` null),
//       "register gave no rows" (0), else "no licensed transmitter on <f> MHz". <f> is MHz
//       without trailing zeros. Keep it in step with origin.rs `summary`.

// The sites originSummary names (origin.rs ORIGIN_SUMMARY_SITES).
export const ORIGIN_SUMMARY_SITES = 3;

export function originSummary(a) {
  const hz = a && a.band ? Number(a.band.center_hz) : NaN;
  const f = Number.isFinite(hz) ? (hz / 1e6).toFixed(6).replace(/\.?0+$/, "") : "?";
  const origins = (a && Array.isArray(a.origins)) ? a.origins : [];
  if (origins.length === 0) {
    const status = a?.register_status;
    if (status?.state === "unreachable") {
      return `register unreachable: ${status.reason || "no reason given"}`;
    }
    const failed = Array.isArray(status?.failed) ? status.failed : [];
    const listing = failed.find((x) => x && x.query === "candidates");
    if (listing) {
      const names = failed.map((x) => x.query).join(", ");
      const tail = status.state === "partial" ? `; partial answer: ${names} failed` : "";
      return `register could not list licences: ${listing.reason}${tail}`;
    }
    if (a?.register_rows === null || a?.register_rows === undefined) return "no register provider";
    if (a.register_rows === 0) return "register gave no rows";
    return `no licensed transmitter on ${f} MHz`;
  }
  const sites = origins.slice(0, ORIGIN_SUMMARY_SITES).map((o) => {
    const al = o.allocation || {};
    let s = [al.licensee, al.site].filter(Boolean).join(", ") || al.service || "";
    if (typeof al.distance_km === "number") s += ` ${al.distance_km.toFixed(0)} km`;
    return `${s} ${o.agree} ${Number(o.score).toFixed(1)}`;
  });
  const n = origins.length;
  return `${n} ${n === 1 ? "site" : "sites"} on ${f} MHz: ${sites.join("; ")}`;
}

export function registerText(allocations) {
  const tx = (allocations || [])
    .filter((a) => a && a.role === "tx")
    .sort((a, b) => (a.distance_km ?? Infinity) - (b.distance_km ?? Infinity))[0];
  if (!tx) return null;
  const where = [tx.licensee, tx.site].filter((x) => typeof x === "string" && x);
  if (typeof tx.distance_km === "number") where.push(`${tx.distance_km.toFixed(0)} km`);
  if (typeof tx.power_w === "number") where.push(`${tx.power_w} W`);
  const code = typeof tx.emission === "string" ? ` ${tx.emission}` : "";
  return `register says${code} ${where.join(", ")}`.trim();
}

// The candidates registerLines names.
export const REGISTER_TOP = 3;

export function emissionWords(e) {
  if (!e) return "";
  return [e.modulation_name, e.info_name].filter((x) => typeof x === "string" && x).join(", ");
}

export function registerVerdictLine(register) {
  if (!register) return null;
  const v = register.verdict;
  let line = v && typeof v.summary === "string" && v.summary ? v.summary : null;
  if (!line && register.status?.state === "unreachable") {
    line = `register unreachable: ${register.status.reason || "no reason given"}`;
  }
  const listing = (register.status?.failed || []).find((f) => f && f.query === "candidates");
  if (!line && listing) line = `register could not list licences: ${listing.reason}`;
  if (!line) return null;
  const asOf = typeof register.as_of === "string" ? register.as_of : "";
  const date = asOf.match(/^\d{4}-\d{2}-\d{2}/)?.[0] || asOf;
  return date ? `${line} (register data as of ${date})` : line;
}

// The `band:` line of a register answer's plan segments, or null for an empty plan.
function bandLine(plan) {
  const services = (plan || []).flatMap((p) => (p && Array.isArray(p.services) ? p.services : []));
  const names = (keep) => {
    const out = [];
    for (const s of services) {
      if (!s || typeof s.name !== "string" || !keep(s)) continue;
      const name = s.qualifier ? `${s.name} (${s.qualifier})` : s.name;
      if (!out.includes(name)) out.push(name);
    }
    return out;
  };
  const primary = names((s) => s.primary);
  const secondary = names((s) => !s.primary).filter((n) => !primary.includes(n));
  if (primary.length === 0) return secondary.length ? `band: ${secondary.join(", ")}` : null;
  const tail = secondary.length ? `; secondary ${secondary.join(", ")}` : "";
  return `band: ${primary.join(", ")}${tail}`;
}

// One candidate's line (see registerLines).
function candidateLine(c, measured) {
  const d = c.device || {};
  const g = c.geometry;
  const parts = [];
  const holder = c.holder?.name || (c.licence?.number ? `licence ${c.licence.number}` : "");
  if (holder) parts.push(holder);
  if (c.site?.name) parts.push(c.site.name);
  if (g && Number.isFinite(g.distance_km)) {
    const b = Number.isFinite(g.bearing_deg) ? ` ${Math.round(g.bearing_deg) % 360}°` : "";
    parts.push(`${Math.round(g.distance_km)} km${b}`);
  }
  const words = emissionWords(d.emission_decoded) || d.emission;
  if (words) parts.push(words);
  const level = c.expected?.level_dbm;
  if (Number.isFinite(level)) {
    parts.push(`expected about ${Math.round(level)} dBm`);
    if (Number.isFinite(measured)) parts.push(`measured ${Math.round(measured)} dBm`);
  }
  if (Number.isFinite(g?.off_beam_deg) && Number.isFinite(d.beamwidth_deg)
      && g.off_beam_deg > d.beamwidth_deg / 2) {
    parts.push("pointed away");
  }
  return parts.join(", ");
}

// What candidate `c` belongs to (see registerLines), or null when its `related` names nothing.
function belongsLine(c) {
  const r = c.related;
  if (!r) return null;
  const paired = (r.paired || []).map(
    (ch) => `${mhzText(ch.center_hz)} MHz${ch.role ? ` (${ch.role})` : ""}`,
  );
  const more = Number.isFinite(r.holder_channels) ? r.holder_channels : 0;
  const far = r.far_end && r.far_end.site;
  if (paired.length === 0 && more === 0 && !far) return null;
  const parts = c.licence?.number ? [`licence ${c.licence.number}`] : [];
  if (paired.length) parts.push(`paired with ${paired.join(", ")}`);
  if (more) {
    parts.push(`${more} more ${more === 1 ? "channel" : "channels"} of the holder at the site`);
  }
  if (far) {
    const d = r.far_end.distance_km;
    parts.push(`link to ${far.name || far.id}${Number.isFinite(d) ? ` ${Math.round(d)} km` : ""}`);
  }
  return `belongs to: ${parts.join(", ")}`;
}

export function registerLines(register, { measured } = {}) {
  if (!register) return [];
  const lines = [];
  const verdict = registerVerdictLine(register);
  if (verdict) lines.push(verdict);
  const status = register.status || {};
  if (status.state === "partial") {
    for (const f of status.failed || []) {
      lines.push(`register partly answered: ${f.query}: ${f.reason}`);
    }
  }
  if (status.state === "unreachable") return lines;
  const band = bandLine(register.plan);
  if (band) lines.push(band);
  const top = (register.candidates || [])
    .filter((c) => c && c.device?.role !== "rx")
    .slice(0, REGISTER_TOP);
  for (const c of top) lines.push(candidateLine(c, measured));
  const belongs = top[0] ? belongsLine(top[0]) : null;
  if (belongs) lines.push(belongs);
  return lines;
}

export function mhzText(hz) {
  return (hz / 1e6).toFixed(4);
}

export function khzText(hz) {
  return (hz / 1e3).toFixed(1);
}

export function bandText(centerHz, bandwidthHz) {
  return `${mhzText(centerHz)} MHz, ${khzText(bandwidthHz)} kHz`;
}

export function metricText(value, unit) {
  if (value === null || value === undefined) return "";
  if (typeof value === "object") return JSON.stringify(value);
  if (typeof value !== "number" || !Number.isFinite(value)) return String(value);
  switch (unit) {
    case "ft":
      return `${Math.round(value * 0.3048)} m`;
    case "m":
      return `${Math.round(value)} m`;
    case "kt":
      return `${Math.round(value * 1.852)} km/h`;
    case "km_h":
      return `${Math.round(value)} km/h`;
    case "m_s": {
      // Number() so -0.04, which rounds to "-0.0", reads as no climb.
      const fixed = Number(value.toFixed(1));
      return `${fixed < 0 ? "" : "+"}${(fixed === 0 ? 0 : fixed).toFixed(1)} m/s`;
    }
    case "deg":
      return `${((Math.round(value) % 360) + 360) % 360}°`;
    default:
      return String(value);
  }
}
