// units.js: frequencies as the page writes them in prose (transcript notes); the vocabulary
// lines (center_hz=…) keep integer Hz.
//
// Exports:
//   mhzText(hz) -> "<MHz to 4 decimals>"        e.g. 100001000 -> "100.0010"
//   khzText(hz) -> "<kHz to 1 decimal>"         e.g. 5000 -> "5.0"
//   bandText(centerHz, bandwidthHz) -> "<mhzText(centerHz)> MHz, <khzText(bandwidthHz)> kHz"
//   metricText(value, unit) -> a mode row's value in metres and km/h, rounded: `ft`, `m` ->
//       "6401 m", `kt`, `km_h` -> "780 km/h", `m_s` -> "5 m/s" (a climb rate stays a climb
//       rate), `deg` -> "245°"; no unit or a non-number as sent; null or undefined -> ""

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
    case "m_s":
      return `${Math.round(value)} m/s`;
    case "deg":
      return `${Math.round(value)}°`;
    default:
      return String(value);
  }
}
