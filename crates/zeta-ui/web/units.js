// units.js: frequencies as the page writes them in prose (transcript notes); the vocabulary
// lines (center_hz=…) keep integer Hz.
//
// Exports:
//   mhzText(hz) -> "<MHz to 4 decimals>"        e.g. 100001000 -> "100.0010"
//   khzText(hz) -> "<kHz to 1 decimal>"         e.g. 5000 -> "5.0"
//   bandText(centerHz, bandwidthHz) -> "<mhzText(centerHz)> MHz, <khzText(bandwidthHz)> kHz"

export function mhzText(hz) {
  return (hz / 1e6).toFixed(4);
}

export function khzText(hz) {
  return (hz / 1e3).toFixed(1);
}

export function bandText(centerHz, bandwidthHz) {
  return `${mhzText(centerHz)} MHz, ${khzText(bandwidthHz)} kHz`;
}
