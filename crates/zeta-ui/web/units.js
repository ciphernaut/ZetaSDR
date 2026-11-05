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
//       (parts it lacks left out), or null when there is none

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
