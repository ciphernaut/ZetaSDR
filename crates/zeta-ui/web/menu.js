// menu.js: the context menu on a waterfall selection. Every entry applies the same operation
// the agent would (through the page's op path, so its ops-stream line shows in the transcript)
// or asks the agent.
//
// Exports:
//   mountMenu(wf, {applyOp, attachOp, ask, transcript, ov, plan, command, muteFor, onOrigins})
//       -> Menu   (`onOrigins(key, answer)`: What is this's classification `origins`, keyed
//       `classify:<t_ns>`, and the Origins entry's band-form answer, keyed
//       `band:<source>:<centre Hz>` and noted in the transcript as units.js originSummary
//       words it, for the map to light)
//       Opens #ctx at the pointer after each wf.onSelect. `applyOp(op) -> Promise<result|null>`
//       is app.js's command-line op path (shows its outcome per ruling R6; null when it
//       failed, the failure already reported); `attachOp(jobId)` the `job_attach` op that
//       transcribes a job (as the job rows' Transcribe button applies it); `ask(text)` is
//       its /ask path. `muteFor(job) -> {muted, title, run} | null` is app.js's (null: no
//       running mix covers the job; `run()` applies the mute or unmute op and answers its
//       result).
//   FIT_KINDS, FIT_MIN_HZ, FIT_MIN_HZ_DEFAULT, FIT_FACTOR, FIT_TIMEOUT_MS, FIT_NOISE_SNR_DB,
//   MEASURE_DC_MASK_HZ
//       the fit of a demodulator entry (see Fitting below)
//   fitBand(api, source, centerHz, bandwidthHz)
//       -> Promise<{centerHz, bandwidthHz, label, snrDb, dialHz} | null>
//       Starts a `measure` job on the band (api.op, so an OpError propagates), tails its
//       `event` stream (stream_tail, up to 3 frames within FIT_TIMEOUT_MS) and stops it
//       (always). The frame used is the first whose payload has `partial: false`,
//       else the last; null when no frame came, when its label is `noise`, or when it is
//       `unknown` under FIT_NOISE_SNR_DB. `label` is the label's name, `snrDb` its `snr_db`,
//       `dialHz` the integer of `dial ≈ <n> Hz` in the label's evidence (an SSB label's), else
//       null.
//   fitTuning(job, fit, box) -> {centerHz, bandwidthHz}
//       The tuning a fitted demodulator starts with (`job` a JOBS entry or {kind, sideband};
//       only `box.bandwidthHz` is read). An ssb_demod moves its dial only: the centre is
//       fit.dialHz when known, else fit.centerHz ∓ SSB_DIAL_OFFSET_HZ (lsb +, otherwise −),
//       and the width stays box.bandwidthHz (the entry's SSB_BANDWIDTH_HZ, or the running
//       job's own width): the channel low-passes at half the band about the dial, and USB
//       voice reaches about 2.7 kHz above it. Every other kind: the centre is fit.centerHz,
//       the width fit.bandwidthHz × FIT_FACTOR, at least FIT_MIN_HZ[kind]
//       (FIT_MIN_HZ_DEFAULT for a kind without one), at most box.bandwidthHz. Integers.
//   dcClause(rec, lo, hi, what) -> string
//       ` (the capture centre <mhz> MHz is in the <what>: the 50 kHz around it are not
//       measured)` when lo..hi Hz is wider than 2 * MEASURE_DC_MASK_HZ, holds `rec`'s capture
//       centre off its own centre and `rec` (a `source_list` record) is local hardware
//       (capabilities remote and push false), else "".
//   Menu:
//     menu.open(sel, x, y)  shows the menu for a waterfall selection at client (x, y)
//     menu.close()          hides it and erases the selection rectangle
//     menu.openOutline(job, x, y)  shows #ctx-outline for a running job (a `job_list` record) at
//                           client (x, y), kept inside the viewport; the first entry is focused
//     menu.closeOutline()   hides it
//     menu.destroy()
//
// Entries (`.ctx-item[data-action]`):
//   classify   What is this?                  notes "classify: running…", then classify
//                                             {selection}; the layer-1 labels are drawn with
//                                             ov.label at the selection's centre (stacked
//                                             LABEL_STEP_PX apart, or LABEL_FALLBACK_STEP_HZ
//                                             while the band is unknown), the top layer-2
//                                             label one line under the last of them; every
//                                             label is listed in the transcript as
//                                             `L<layer> <name> <confidence>`, then each
//                                             mode decode-to-identify considered as
//                                             `tried <mode> <score> <outcome>`
//   identify   Identify (thorough)            job_start kind=identify, bandwidth_hz at least
//                                             IDENTIFY_MIN_BANDWIDTH_HZ; each `candidate`
//                                             event shows as a ribbon (its `summary`, class
//                                             `candidate`), held by the feed until a poll
//                                             lists the job
//   watch      Watch this band                job_start kind=watch on the selection as is
//   aprs_decode  Decode › APRS                job_start kind=aprs_decode, deviation_hz null
//   pocsag_decode  Decode › POCSAG            job_start kind=pocsag_decode, deviation_hz null,
//                                             bandwidth_hz at least POCSAG_MIN_BANDWIDTH_HZ
//   rds_decode  Decode › RDS (FM station data)  job_start kind=rds_decode, deviation_hz null,
//                                             bandwidth_hz at least WFM_MIN_BANDWIDTH_HZ
//   adsb_decode  Decode › ADS-B (dump1090)    job_start kind=adsb_decode, deviation_hz null,
//                                             bandwidth_hz always ADSB_BANDWIDTH_HZ (the node
//                                             refuses under 2 MHz; a narrower source answers
//                                             out_of_range, shown in the transcript)
//   acars_decode  Decode › ACARS              job_start kind=acars_decode, deviation_hz null,
//                                             bandwidth_hz at least ACARS_MIN_BANDWIDTH_HZ
//   fm_demod   Decode › NBFM audio            job_start kind=fm_demod, deviation_hz 5000
//                                             (fitted first: see Fitting)
//   wfm_demod  Decode › FM broadcast audio    job_start kind=wfm_demod, deviation_hz 75000,
//                                             bandwidth_hz at least WFM_MIN_BANDWIDTH_HZ
//                                             (fitted first)
//   am_demod   Decode › AM                    job_start kind=am_demod, deviation_hz null
//                                             (fitted first)
//   ssb_usb    Decode › SSB (USB)             job_start kind=ssb_demod, sideband usb, at the
//                                             dial: center_hz = selection centre − 1500,
//                                             bandwidth_hz 6000 (ruling R6; fitted first)
//   ssb_lsb    Decode › SSB (LSB)             likewise, sideband lsb, centre + 1500
//   wfm_transcribe  Transcribe › FM broadcast  the job_start of its Decode twin (wfm_demod,
//   fm_transcribe   Transcribe › NBFM voice    fm_demod, am_demod, ssb_usb, ssb_lsb: same
//   am_transcribe   Transcribe › AM            kind, deviation, sideband and tuning), then on
//   ssb_usb_transcribe  Transcribe › SSB (USB) success attachOp(<its job>): a transcribe job
//   ssb_lsb_transcribe  Transcribe › SSB (LSB) attached to that demodulator (the twin is
//                                             fitted first, as its Decode entry)
//   measure    Start background job › measure job_start kind=measure, deviation_hz null
//                                             (every job_start sends sideband: null unless SSB)
//   scan       Scan this span                 wf.release() (stops the page's own spectrum job
//                                             unless another job holds the source: then a
//                                             `conflict: …` line and nothing more), job_start
//                                             kind=scan on the selection's centre and
//                                             bandwidth, then wf.resume() (scan mode when the
//                                             scan runs, else the spectrum job again)
//   plan       Start plan…                    shown only when the view's plan policy names a plan
//                                             (mountMenu's `plan()`); puts `plan start <plan>
//                                             fm_demod transcribe` in the command line
//                                             (`command(text)`) and focuses it
//   iq_read    Save selection as SigMF        iq_read {selection}; the transcript shows the path
//   ask        Send to agent                  /ask "Look at <c> Hz, <bw> Hz wide, from <start>
//                                             to <end>: what is it?"
// The jobs use the selection's centre and bandwidth on the source the waterfall shows (the
// SSB entries their dial frequency and SSB_BANDWIDTH_HZ instead; FM broadcast and RDS widen
// the bandwidth, ADS-B replaces it, see jobTuning).
// #ctx-footer shows the selection in the vocabulary: center_hz=… bandwidth_hz=… start_ns=…
// end_ns=…, with the span already clamped (see below); while an SSB entry (Decode or
// Transcribe) is focused or hovered it shows that entry's dial instead: center_hz=<dial>
// bandwidth_hz=6000 sideband=usb|lsb start_ns=… end_ns=….
//
// Fitting: the demodulator entries (a kind in FIT_KINDS: fm_demod, wfm_demod, am_demod,
// ssb_demod; their Decode and Transcribe entries alike) measure the box before starting. The
// menu stays open, the box drawn and #ctx-footer reading `measuring…`, while fitBand runs on
// the selection's centre and bandwidth; then the menu closes and:
//   - fitBand threw: the note `<kind>: <message>` (an error line) and nothing starts;
//   - null (noise, a weak unknown, no event): the job starts with the tuning above
//     (jobTuning) and, once it started, the note reads `no signal found in <lo>-<hi> MHz;
//     started <kind> at the box as drawn` (the box's edges, mhzText). When the box holds the
//     shown source's capture centre off its own centre and the source is local hardware (`source_list`
//     capabilities remote and push both false), the note says why the fit may have missed:
//     `no signal found in <lo>-<hi> MHz (the capture centre <mhz> MHz is in the box: the
//     50 kHz around it are not measured); started <kind> at the box as drawn` (measure masks
//     MEASURE_DC_MASK_HZ either side of the DC spike when the box holds it off-centre);
//   - a fit: the job starts with fitTuning(job, fit, jobTuning(...)) and, once it started,
//     the note reads `fitted <kind> to <bandText(centre, width)> (<label>)`, e.g. `fitted
//     fm_demod to 100.0011 MHz, 6.0 kHz (carrier)`.
// A start that fails writes its own error line and no note.
// While a fit runs the entries are aria-disabled and choosing again does nothing (a double
// click starts one job). Closing the menu meanwhile (Escape, a press outside, a new box)
// cancels the start: the measure job is stopped, nothing starts, and the transcript notes
// `<kind>: fit cancelled`; the fit leaves any menu opened later alone.
// Every other entry closes the menu first.
//
// Span: the node reads the selection from the source ring, which keeps RING_HISTORY_S. The
// span is clamped to the newest SPAN_LIMIT_NS before the newest painted column (on opening
// and again when an entry is chosen); the footer says so when it was.
//
// Closing: Escape, a pointer press outside the menu, or choosing an entry. Up/Down move the
// focus between entries; Enter or Space chooses one.
//
// The outline menu (#ctx-outline, role menu; overlays.js opens it by right-click on a retunable
// outline, or a long press on its label in the narrow footprint). Entries
// (`.ctx-item[data-action]`, OUTLINE_ENTRIES), on the job's own source and tuning; Fit and the
// width presets only for a job whose kind is in FIT_KINDS (the others are hidden: the node
// would refuse a decoder's or a passthrough's narrowing as often as not), Stop always:
//   fit    Fit to signal   fitBand on the job's band, then job_retune to fitTuning(...) capped at
//                          the job's bandwidth (an ssb_demod keeps its width and moves its dial,
//                          the sideband read from its graph's ssb_demod node), and once the
//                          retune answered the note `fitted <kind> to <band> (<label>)`; no
//                          signal: the note `no signal found in the band; left as it is` (with
//                          the capture-centre clause above when the band holds the centre of a
//                          local radio's capture: `no signal found in the band (the capture
//                          centre <mhz> MHz is in the band: the 50 kHz around it are not
//                          measured); left as it is`) and nothing changes. Shares the fit guard
//                          above (entries aria-disabled meanwhile, closing the menu cancels it:
//                          `<kind>: fit cancelled`)
//   w2500 2.5 kHz, w5000 5 kHz, w6250 6.25 kHz, w12500 12.5 kHz, w25000 25 kHz
//                          job_retune to that bandwidth_hz about the same centre_hz
//   denoise Denoise on|off graph_put of the job's own graph with `denoise` flipped on its
//                          demodulator node (nbfm_demod, am_demod, ssb_demod: DENOISE_KINDS;
//                          hidden for the others), the entry reading the state it would
//                          switch to; the note `<kind>: denoise on|off`
//   mute   Mute in mix     graph_put of the covering mix's own graph with this channel added to
//                          its `muted` (mix.js mixCovering, muteOp; the entry reads `Unmute in
//                          mix` for a muted one, hidden when no running mix covers the job's
//                          source); the note `<kind>: muted in mix` / `unmuted in mix`
//   stop   Stop            job_stop
// Every retune is {op: job_retune, job, center_hz, bandwidth_hz, replace: []}. #ctx-outline-footer
// reads job=<id> center_hz=<c> bandwidth_hz=<bw>. Escape, a press outside, Up/Down and Enter act
// as for #ctx.

import { bandText, mhzText, originSummary, registerText } from "./units.js";

// How far past the ring's earliest sample a `too_old` retry starts, as the front moves on.
const RING_FRONT_MARGIN_NS = 300_000_000;
export const SPAN_LIMIT_NS = 8_000_000_000;
export const LABEL_STEP_PX = 18;
// The label spacing in Hz while the waterfall cannot map Hz to pixels (band not known yet).
export const LABEL_FALLBACK_STEP_HZ = 1000;

// The SSB entries tune the carrier this far below (USB) or above (LSB) the selection's centre
// (ruling R6: a voice signal's energy sits about 1.5 kHz from its carrier) ...
export const SSB_DIAL_OFFSET_HZ = 1500;
// ... and ask for this bandwidth.
export const SSB_BANDWIDTH_HZ = 6000;
// The FM broadcast entry asks for at least this bandwidth: a narrow drag over a station still
// takes its whole 75 kHz-deviation band.
export const WFM_MIN_BANDWIDTH_HZ = 200000;
// The POCSAG and ACARS entries ask for at least these bandwidths: a narrow drag would make the
// channel filter cut the signal (POCSAG's +-4.5 kHz deviation, ACARS's 2400 Hz MSK sidebands),
// and the decode would silently never fire.
export const POCSAG_MIN_BANDWIDTH_HZ = 12500;
export const ACARS_MIN_BANDWIDTH_HZ = 10000;
// The ADS-B entry always asks for this bandwidth (dump1090's 2.4 MHz; the node refuses
// adsb_decode under 2 MHz), whatever was selected.
export const ADSB_BANDWIDTH_HZ = 2_400_000;
// The identify entry asks for at least this bandwidth, the narrowest box classify fits the
// mode rows to: a narrow drag would skip every row as wider than the box.
export const IDENTIFY_MIN_BANDWIDTH_HZ = 12500;
// The demodulators fitted to the signal before they start (see Fitting).
export const FIT_KINDS = new Set(["fm_demod", "wfm_demod", "am_demod", "ssb_demod"]);
// The narrowest band a fitted demodulator asks for, per kind (ssb_demod has none: its fit
// keeps its band and moves the dial) ...
export const FIT_MIN_HZ = { fm_demod: 5000, am_demod: 5000, wfm_demod: 150000 };
// ... and for a kind without an entry.
export const FIT_MIN_HZ_DEFAULT = 5000;
// The fitted band is the measured occupied width times this.
export const FIT_FACTOR = 1.25;
// How long the fit waits for a measure event.
export const FIT_TIMEOUT_MS = 3000;
// An `unknown` label under this SNR (dB) counts as no signal.
export const FIT_NOISE_SNR_DB = 6;
// On local hardware `measure` leaves this many Hz either side of the capture centre (the DC
// spike) unmeasured when its band holds the centre off-centre. The node's
// zeta_core::ops::MEASURE_DC_MASK_HZ is the source of truth; this copy only words the note.
export const MEASURE_DC_MASK_HZ = 25000;

const OUTLINE_ENTRIES = [
  ["fit", "Fit to signal"],
  ["w2500", "2.5 kHz"],
  ["w5000", "5 kHz"],
  ["w6250", "6.25 kHz"],
  ["w12500", "12.5 kHz"],
  ["w25000", "25 kHz"],
  ["denoise", "Denoise on"],
  ["mute", "Mute in mix"],
  ["stop", "Stop"],
];

// The job kinds whose demodulator node takes `denoise` (RNNoise on its audio), by the node's
// stage kind: the voice demodulators; wfm_demod (music) has none.
export const DENOISE_KINDS = new Map([
  ["fm_demod", "nbfm_demod"],
  ["am_demod", "am_demod"],
  ["ssb_demod", "ssb_demod"],
]);

// The demodulator node of `job` (a job_list record with its graph) that takes `denoise`,
// or null.
function denoiseNode(job) {
  const kind = DENOISE_KINDS.get(job.kind);
  if (!kind) return null;
  return ((job.graph && job.graph.nodes) || []).find((n) => n.kind === kind) || null;
}

const ENTRIES = [
  ["classify", "What is this?"],
  ["origins", "Origins"],
  ["capture", "Capture bursts"],
  ["identify", "Identify (thorough)"],
  ["watch", "Watch this band"],
  ["aprs_decode", "Decode › APRS"],
  ["pocsag_decode", "Decode › POCSAG"],
  ["rds_decode", "Decode › RDS (FM station data)"],
  ["adsb_decode", "Decode › ADS-B (dump1090)"],
  ["acars_decode", "Decode › ACARS"],
  ["fm_demod", "Decode › NBFM audio"],
  ["wfm_demod", "Decode › FM broadcast audio"],
  ["am_demod", "Decode › AM"],
  ["ssb_usb", "Decode › SSB (USB)"],
  ["ssb_lsb", "Decode › SSB (LSB)"],
  ["wfm_transcribe", "Transcribe › FM broadcast"],
  ["fm_transcribe", "Transcribe › NBFM voice"],
  ["am_transcribe", "Transcribe › AM"],
  ["ssb_usb_transcribe", "Transcribe › SSB (USB)"],
  ["ssb_lsb_transcribe", "Transcribe › SSB (LSB)"],
  ["measure", "Start background job › measure"],
  ["plan", "Start plan…"],
  ["scan", "Scan this span"],
  ["iq_read", "Save selection as SigMF"],
  ["ask", "Send to agent"],
];
// The job each decode entry starts: kind, deviation_hz and sideband.
const JOBS = {
  capture: { kind: "capture", deviation: null, sideband: null },
  identify: { kind: "identify", deviation: null, sideband: null },
  watch: { kind: "watch", deviation: null, sideband: null },
  aprs_decode: { kind: "aprs_decode", deviation: null, sideband: null },
  pocsag_decode: { kind: "pocsag_decode", deviation: null, sideband: null },
  rds_decode: { kind: "rds_decode", deviation: null, sideband: null },
  adsb_decode: { kind: "adsb_decode", deviation: null, sideband: null },
  acars_decode: { kind: "acars_decode", deviation: null, sideband: null },
  fm_demod: { kind: "fm_demod", deviation: 5000, sideband: null },
  wfm_demod: { kind: "wfm_demod", deviation: 75000, sideband: null },
  am_demod: { kind: "am_demod", deviation: null, sideband: null },
  ssb_usb: { kind: "ssb_demod", deviation: null, sideband: "usb" },
  ssb_lsb: { kind: "ssb_demod", deviation: null, sideband: "lsb" },
  measure: { kind: "measure", deviation: null, sideband: null },
};
// Each Transcribe entry's Decode twin: the job it starts before attaching to it.
const TRANSCRIBE = {
  wfm_transcribe: "wfm_demod",
  fm_transcribe: "fm_demod",
  am_transcribe: "am_demod",
  ssb_usb_transcribe: "ssb_usb",
  ssb_lsb_transcribe: "ssb_lsb",
};
// The JOBS entry an action starts (a Transcribe entry's twin's), or undefined.
const jobOf = (action) => JOBS[TRANSCRIBE[action] ?? action];

// The tuning a job entry uses for a selection: {centerHz, bandwidthHz}. SSB tunes the dial; FM
// broadcast and RDS widen the band to at least WFM_MIN_BANDWIDTH_HZ, POCSAG to at least
// POCSAG_MIN_BANDWIDTH_HZ and ACARS to at least ACARS_MIN_BANDWIDTH_HZ; ADS-B always asks for
// ADSB_BANDWIDTH_HZ; everything else uses the selection as is. A Transcribe entry tunes as
// its Decode twin.
export function jobTuning(action, s) {
  action = TRANSCRIBE[action] ?? action;
  const sb = JOBS[action] ? JOBS[action].sideband : null;
  if (sb === "usb") return { centerHz: s.centerHz - SSB_DIAL_OFFSET_HZ, bandwidthHz: SSB_BANDWIDTH_HZ };
  if (sb === "lsb") return { centerHz: s.centerHz + SSB_DIAL_OFFSET_HZ, bandwidthHz: SSB_BANDWIDTH_HZ };
  if (action === "adsb_decode") return { centerHz: s.centerHz, bandwidthHz: ADSB_BANDWIDTH_HZ };
  if (action === "wfm_demod" || action === "rds_decode") {
    return { centerHz: s.centerHz, bandwidthHz: Math.max(s.bandwidthHz, WFM_MIN_BANDWIDTH_HZ) };
  }
  if (action === "pocsag_decode") {
    return { centerHz: s.centerHz, bandwidthHz: Math.max(s.bandwidthHz, POCSAG_MIN_BANDWIDTH_HZ) };
  }
  if (action === "identify") {
    return { centerHz: s.centerHz, bandwidthHz: Math.max(s.bandwidthHz, IDENTIFY_MIN_BANDWIDTH_HZ) };
  }
  if (action === "acars_decode") {
    return { centerHz: s.centerHz, bandwidthHz: Math.max(s.bandwidthHz, ACARS_MIN_BANDWIDTH_HZ) };
  }
  return { centerHz: s.centerHz, bandwidthHz: s.bandwidthHz };
}

// Measures a band with a short `measure` job; the fit, or null when there is no signal.
export async function fitBand(api, source, centerHz, bandwidthHz) {
  const r = await api.op({
    op: "job_start",
    kind: "measure",
    source,
    center_hz: centerHz,
    bandwidth_hz: bandwidthHz,
    deviation_hz: null,
    sideband: null,
    elastic: false,
    replace: [],
  });
  const id = r.value;
  let frames = [];
  try {
    const streams = await api.streams();
    const stream = streams.find(
      (x) => x.kind === "event" && x.provenance && x.provenance.job === id,
    );
    if (stream) {
      const t = await api.op({
        op: "stream_tail",
        stream: stream.id,
        max_frames: 3,
        timeout_ms: FIT_TIMEOUT_MS,
      });
      frames = (t && t.value && t.value.frames) || [];
    }
  } finally {
    try {
      await api.op({ op: "job_stop", job: id });
    } catch {
      // Best effort: a measure job that already ended needs no stop.
    }
  }
  if (frames.length === 0) return null;
  const frame = frames.find((f) => f.payload && f.payload.partial === false) ?? frames.at(-1);
  const p = frame.payload || {};
  const label = p.label || {};
  if (label.name === "noise") return null;
  if (label.name === "unknown" && p.snr_db < FIT_NOISE_SNR_DB) return null;
  const dial = /dial ≈ (\d+) Hz/.exec(label.evidence || "");
  return {
    centerHz: p.center_hz,
    bandwidthHz: p.bandwidth_hz,
    label: label.name,
    snrDb: p.snr_db,
    dialHz: dial ? parseInt(dial[1], 10) : null,
  };
}

// The tuning a fitted demodulator starts with (see the header).
export function fitTuning(job, fit, box) {
  if (job.kind === "ssb_demod") {
    // The dial moves; the band stays what the entry (or the running job) has.
    let dial = fit.dialHz;
    if (dial === null || dial === undefined) {
      dial =
        job.sideband === "lsb"
          ? fit.centerHz + SSB_DIAL_OFFSET_HZ
          : fit.centerHz - SSB_DIAL_OFFSET_HZ;
    }
    return { centerHz: Math.round(dial), bandwidthHz: Math.round(box.bandwidthHz) };
  }
  const floor = FIT_MIN_HZ[job.kind] ?? FIT_MIN_HZ_DEFAULT;
  const bandwidthHz = Math.min(
    box.bandwidthHz,
    Math.max(floor, Math.round(fit.bandwidthHz * FIT_FACTOR)),
  );
  return { centerHz: Math.round(fit.centerHz), bandwidthHz: Math.round(bandwidthHz) };
}

// When the band lo..hi Hz holds the capture centre of `rec` (a `source_list` record) off its
// own centre and that source is local hardware, the clause the no-signal notes add (`(the capture centre
// <mhz> MHz is in the <what>: the 50 kHz around it are not measured)`), else "".
export function dcClause(rec, lo, hi, what) {
  if (!rec || !rec.current || typeof rec.current.center_hz !== "number") return "";
  const c = rec.capabilities || {};
  if (c.remote !== false || c.push !== false) return "";
  const dc = rec.current.center_hz;
  // A band centred on the DC (the band the radio was tuned for) is measured whole.
  if (dc < lo || dc > hi || Math.round(dc) === Math.round((lo + hi) / 2)) return "";
  // A band no wider than the mask is measured unmasked (the mask would cover every bin).
  if (hi - lo <= 2 * MEASURE_DC_MASK_HZ) return "";
  const khz = (2 * MEASURE_DC_MASK_HZ) / 1000;
  return ` (the capture centre ${mhzText(dc)} MHz is in the ${what}: the ${khz} kHz around it are not measured)`;
}

export function mountMenu(wf, opts) {
  return new Menu(wf, opts);
}

// End time of the newest column the waterfall painted (null before any).
function newestNs(wf) {
  if (!wf.columns) return null;
  const slot = (wf.columns - 1) % wf.ringEnd.length;
  return wf.ringEnd[slot] || null;
}

// The selection's span limited to the last SPAN_LIMIT_NS: {startNs, endNs, clamped}.
export function clampSpan(startNs, endNs, newest) {
  if (newest === null) return { startNs, endNs, clamped: false };
  let s = startNs;
  let e = Math.min(endNs, newest);
  const lo = newest - SPAN_LIMIT_NS;
  if (s < lo) s = lo;
  if (e <= s) e = newest;
  s = Math.round(s);
  e = Math.round(e);
  return { startNs: s, endNs: e, clamped: s !== startNs || e !== endNs };
}

class Menu {
  constructor(
    wf,
    { applyOp, attachOp, ask, transcript, ov, plan = () => null, ringSeconds = () => null, command = () => {}, muteFor = () => null, onOrigins = () => {} },
  ) {
    this.wf = wf;
    this.onOrigins = onOrigins;
    this.muteFor = muteFor;
    this.applyOp = applyOp;
    this.ringSeconds = ringSeconds;
    this.attachOp = attachOp;
    this.ask = ask;
    this.plan = plan;
    this.command = command;
    this.t = transcript;
    this.ov = ov;
    this.sel = null;
    // The fit in progress (its generation token) or null; close() bumps fitGen.
    this.fitting = null;
    this.fitGen = 0;
    this.labelKeys = [];
    this.boxLabel = null; // the last classification's top layer-1 label {centerHz, bandwidthHz, name}
    this.pointer = { x: 0, y: 0 };

    this.el = document.createElement("div");
    this.el.id = "ctx";
    this.el.setAttribute("role", "menu");
    this.el.hidden = true;
    for (const [action, text] of ENTRIES) {
      const item = document.createElement("button");
      item.className = "ctx-item";
      item.dataset.action = action;
      item.setAttribute("role", "menuitem");
      item.textContent = text;
      item.addEventListener("click", () => this.choose(action));
      if (jobOf(action) && jobOf(action).sideband) {
        const idle = () => this.sel && this.fitting === null;
        const show = () => idle() && this.showFooter(this.current(), action);
        const hide = () => idle() && this.showFooter(this.current());
        item.addEventListener("focus", show);
        item.addEventListener("pointerenter", show);
        item.addEventListener("blur", hide);
        item.addEventListener("pointerleave", hide);
      }
      this.el.append(item);
    }
    this.footer = document.createElement("div");
    this.footer.id = "ctx-footer";
    this.el.append(this.footer);
    document.body.append(this.el);

    // The outline menu and the job it is open for (a job_list record) or null.
    this.outlineJob = null;
    this.outlineEl = document.createElement("div");
    this.outlineEl.id = "ctx-outline";
    this.outlineEl.setAttribute("role", "menu");
    this.outlineEl.hidden = true;
    for (const [action, text] of OUTLINE_ENTRIES) {
      const item = document.createElement("button");
      item.className = "ctx-item";
      item.dataset.action = action;
      item.setAttribute("role", "menuitem");
      item.textContent = text;
      item.addEventListener("click", () => this.chooseOutline(action));
      this.outlineEl.append(item);
    }
    this.outlineFooter = document.createElement("div");
    this.outlineFooter.id = "ctx-outline-footer";
    this.outlineEl.append(this.outlineFooter);
    document.body.append(this.outlineEl);

    // The pointer position of the drag's end: pointerup reaches the document (capture) before
    // the waterfall's own handler calls onSelect.
    this.onPointerUp = (ev) => {
      this.pointer = { x: ev.clientX, y: ev.clientY };
    };
    this.onPointerDown = (ev) => {
      if (!this.el.hidden && !this.el.contains(ev.target)) this.close();
      if (!this.outlineEl.hidden && !this.outlineEl.contains(ev.target)) this.closeOutline();
    };
    this.onKey = (ev) => {
      const outline = !this.outlineEl.hidden;
      const menu = outline ? this.outlineEl : this.el;
      if (menu.hidden) return;
      if (ev.key === "Escape") {
        ev.preventDefault();
        if (outline) this.closeOutline();
        else this.close();
      } else if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
        ev.preventDefault();
        const items = [...menu.querySelectorAll(".ctx-item:not([hidden])")];
        const at = items.indexOf(document.activeElement);
        const step = ev.key === "ArrowDown" ? 1 : -1;
        items[(at + step + items.length) % items.length].focus();
      }
    };
    document.addEventListener("pointerup", this.onPointerUp, true);
    document.addEventListener("pointerdown", this.onPointerDown, true);
    document.addEventListener("keydown", this.onKey);
    wf.onSelect((sel) => this.open(sel, this.pointer.x, this.pointer.y));
  }

  // The selection with its span clamped, as the footer shows it.
  current() {
    const s = this.sel;
    return { ...s, ...clampSpan(s.startNs, s.endNs, newestNs(this.wf)) };
  }

  // The selection, or with an SSB `action` the dial that entry tunes.
  showFooter(s, action = null) {
    const t = jobTuning(action, s);
    const sb = action && jobOf(action) ? jobOf(action).sideband : null;
    this.footer.textContent =
      `center_hz=${t.centerHz} bandwidth_hz=${t.bandwidthHz} ` +
      (sb ? `sideband=${sb} ` : "") +
      `start_ns=${s.startNs} end_ns=${s.endNs}` +
      (s.clamped ? " (span clamped to the last 8 s)" : "") +
      this.ringText();
  }

  // ` ring <d> s`: the selected source's IQ history, when its record carries one.
  ringText() {
    const d = this.sel ? this.ringSeconds(this.sel.sourceId) : null;
    return typeof d === "number" ? ` ring ${d.toFixed(1)} s` : "";
  }

  open(sel, x, y) {
    if (!this.wf.sourceId) return;
    // A new box cancels a fit still running for the previous one.
    this.fitGen += 1;
    this.setFitting(null);
    this.sel = { ...sel, sourceId: this.wf.sourceId };
    this.showFooter(this.current());
    // `Start plan…` only where the view's plan policy names a plan.
    this.el.querySelector('[data-action="plan"]').hidden = this.plan() === null;
    this.el.hidden = false;
    // Keep it inside the viewport.
    const w = this.el.offsetWidth;
    const h = this.el.offsetHeight;
    this.el.style.left = `${Math.max(0, Math.min(x, window.innerWidth - w))}px`;
    this.el.style.top = `${Math.max(0, Math.min(y, window.innerHeight - h))}px`;
    this.el.querySelector(".ctx-item").focus({ preventScroll: true });
  }

  close() {
    this.el.hidden = true;
    this.wf.clearSelection();
    // A fit in progress is cancelled: its result no longer belongs to an open menu.
    this.fitGen += 1;
    this.setFitting(null);
  }

  openOutline(job, x, y) {
    // One menu at a time; opening cancels a fit still running for the previous one.
    if (!this.el.hidden) this.close();
    this.fitGen += 1;
    this.setFitting(null);
    this.outlineJob = job;
    const t = job.tuning;
    // Fit and the width presets only for the demodulators, Denoise for the voice ones
    // (reading the state it would switch to); Stop always.
    const fits = FIT_KINDS.has(job.kind);
    const dn = denoiseNode(job);
    for (const item of this.outlineEl.querySelectorAll(".ctx-item")) {
      const action = item.dataset.action;
      if (action === "mute") {
        const m = this.muteFor(job);
        item.hidden = m === null;
        item.textContent = m && m.muted ? "Unmute in mix" : "Mute in mix";
      } else if (action === "denoise") {
        item.hidden = dn === null;
        item.textContent = dn && dn.params && dn.params.denoise ? "Denoise off" : "Denoise on";
      } else {
        item.hidden = action !== "stop" && !fits;
      }
    }
    this.outlineFooter.textContent =
      `job=${job.id} center_hz=${t.center_hz} bandwidth_hz=${t.bandwidth_hz}`;
    this.outlineEl.hidden = false;
    const w = this.outlineEl.offsetWidth;
    const h = this.outlineEl.offsetHeight;
    this.outlineEl.style.left = `${Math.max(0, Math.min(x, window.innerWidth - w))}px`;
    this.outlineEl.style.top = `${Math.max(0, Math.min(y, window.innerHeight - h))}px`;
    this.outlineEl.querySelector(".ctx-item:not([hidden])").focus({ preventScroll: true });
  }

  closeOutline() {
    this.outlineEl.hidden = true;
    this.outlineJob = null;
    this.fitGen += 1;
    this.setFitting(null);
  }

  async chooseOutline(action) {
    const job = this.outlineJob;
    if (!job || this.fitting !== null) return;
    const t = job.tuning;
    if (action === "fit") {
      const token = ++this.fitGen;
      this.setFitting(token);
      const kind = job.kind;
      let fit;
      let error = null;
      try {
        fit = await fitBand(this.ov.api, job.source, t.center_hz, t.bandwidth_hz);
      } catch (e) {
        error = e;
      }
      if (token !== this.fitGen) {
        this.t.note(`${kind}: fit cancelled`);
        return;
      }
      this.closeOutline();
      if (error) {
        this.t.note(`${kind}: ${error.message}`, true);
      } else if (fit === null) {
        const lo = t.center_hz - t.bandwidth_hz / 2;
        const hi = t.center_hz + t.bandwidth_hz / 2;
        const why = dcClause(this.sourceRec(job.source), lo, hi, "band");
        this.t.note(`no signal found in the band${why}; left as it is`);
      } else {
        // The sideband from the job's ssb_demod node (a record carries none); USB without one.
        const ssb = ((job.graph && job.graph.nodes) || []).find((n) => n.kind === "ssb_demod");
        const sideband = (ssb && ssb.params && ssb.params.sideband) || null;
        const f = fitTuning({ kind, sideband }, fit, { bandwidthHz: t.bandwidth_hz });
        const r = await this.applyOp({
          op: "job_retune",
          job: job.id,
          center_hz: f.centerHz,
          bandwidth_hz: f.bandwidthHz,
          replace: [],
        });
        if (r) {
          this.t.note(`fitted ${kind} to ${bandText(f.centerHz, f.bandwidthHz)} (${fit.label})`);
        }
      }
      return;
    }
    this.closeOutline();
    if (action === "stop") {
      await this.applyOp({ op: "job_stop", job: job.id });
    } else if (action === "mute") {
      // The mix's own graph, read now, with this channel added to (or taken off) `muted`.
      const m = this.muteFor(job);
      if (!m) return;
      const r = await m.run();
      if (r) this.t.note(`${job.kind}: ${r.muted ? "muted" : "unmuted"} in mix`);
    } else if (action === "denoise") {
      // The job's own graph with the flag flipped: a carried param, so the node keeps the
      // job re-expandable and relaunches it under its id (an attached transcribe follows).
      const dn = denoiseNode(job);
      if (!dn) return;
      const on = !(dn.params && dn.params.denoise);
      const graph = JSON.parse(JSON.stringify(job.graph));
      for (const n of graph.nodes) {
        if (n.kind === dn.kind) n.params = { ...(n.params || {}), denoise: on };
      }
      const r = await this.applyOp({ op: "graph_put", job: job.id, graph });
      if (r) this.t.note(`${job.kind}: denoise ${on ? "on" : "off"}`);
    } else if (/^w\d+$/.test(action)) {
      await this.applyOp({
        op: "job_retune",
        job: job.id,
        center_hz: t.center_hz,
        bandwidth_hz: Number(action.slice(1)),
        replace: [],
      });
    }
  }

  // The last `source_list` record overlays.js holds for source `id` (null when it holds
  // another source's or none).
  sourceRec(id) {
    const rec = this.ov && this.ov.sourceRec;
    return rec && rec.id === id ? rec : null;
  }

  // Marks fit `token` in progress (null: none), the entries aria-disabled meanwhile.
  setFitting(token) {
    this.fitting = token;
    const items = [
      ...this.el.querySelectorAll(".ctx-item"),
      ...this.outlineEl.querySelectorAll(".ctx-item"),
    ];
    for (const item of items) {
      if (token === null) item.removeAttribute("aria-disabled");
      else item.setAttribute("aria-disabled", "true");
    }
  }

  // The ring no longer holds the span (`too_old`): once, on the latest span of the same
  // length the ring still covers (not before `earliest_ns`). The result, or null.
  async reclassify(selection, detail) {
    const end = newestNs(this.wf);
    const len = selection.span.end_ns - selection.span.start_ns;
    if (end === null || !Number.isFinite(detail.earliest_ns)) return null;
    // The ring's front keeps moving while the retry is in flight: keep clear of it.
    const start = Math.round(Math.max(end - len, detail.earliest_ns + RING_FRONT_MARGIN_NS));
    if (start >= end) return null;
    this.t.note(`buffer moved on; classified the last ${((end - start) / 1e9).toFixed(1)} s`);
    return this.applyOp({
      op: "classify",
      selection: { ...selection, span: { start_ns: start, end_ns: Math.round(end) } },
    });
  }

  async choose(action) {
    // A fit in progress ignores further choices (a double click starts one job).
    if (!this.sel || this.fitting !== null) return;
    const s = this.current();
    this.showFooter(s, action);
    const job = jobOf(action);
    // "Capture bursts" is fitted first like a demodulator, but a running capture has no retune.
    if (job && (FIT_KINDS.has(job.kind) || job.kind === "capture")) {
      await this.fitAndStart(action, s, job);
      return;
    }
    this.close();
    if (action === "origins") {
      // The band form; the box's layer-1 label (when it was classified) names its modulation.
      const bl = this.boxLabel;
      // A `noise` label measures no modulation (the node's measured_modulation drops it too).
      // The label counts for a selection centred inside the classified box.
      const inBox = bl && Math.abs(bl.centerHz - s.centerHz) <= bl.bandwidthHz / 2;
      const named = inBox && bl.name !== "noise" ? bl.name : null;
      const r = await this.applyOp({
        op: "origin_lookup",
        job: null,
        source: s.sourceId,
        center_hz: s.centerHz,
        bandwidth_hz: s.bandwidthHz,
        modulation: named ? [named] : null,
      });
      if (r && r.value) {
        this.t.note(originSummary(r.value));
        this.onOrigins(`band:${s.sourceId}:${Math.round(s.centerHz)}`, r.value);
      }
      return;
    }
    const selection = {
      source: s.sourceId,
      span: { start_ns: s.startNs, end_ns: s.endNs },
      center_hz: s.centerHz,
      bandwidth_hz: s.bandwidthHz,
    };
    if (action === "classify") {
      this.t.note("classify: running…");
      let r = await this.applyOp({ op: "classify", selection }, { errors: true });
      if (r && r.error === "ring" && r.detail?.cause === "too_old") r = await this.reclassify(selection, r.detail);
      if (r && r.result === "classification" && r.value) {
        this.showLabels(s, r.value.labels || []);
        for (const t of r.value.tried || []) {
          this.t.note(`tried ${t.mode} ${Number(t.score).toFixed(2)} ${t.outcome}`);
        }
        const reg = registerText(r.value.allocations);
        if (reg) this.t.note(reg);
        const o = r.value.origins;
        if (o) this.onOrigins(`classify:${o.t_ns}`, o);
      }
    } else if (action === "iq_read") {
      const r = await this.applyOp({ op: "iq_read", selection });
      if (r && r.result === "iq" && r.value) {
        this.t.note(`saved ${r.value.samples} samples to ${r.value.path}`);
      }
    } else if (action === "ask") {
      await this.ask(
        `Look at ${s.centerHz} Hz, ${s.bandwidthHz} Hz wide, ` +
          `from ${s.startNs} to ${s.endNs}: what is it?`,
      );
    } else if (action === "plan") {
      const plan = this.plan();
      if (plan !== null) this.command(`plan start ${plan} fm_demod transcribe`);
    } else if (action === "scan") {
      await this.scan(s);
    } else if (jobOf(action)) {
      const r = await this.startJob(action, s);
      if (action in TRANSCRIBE && r && r.result === "job" && r.value) {
        await this.applyOp(this.attachOp(r.value));
      }
    }
  }

  // A demodulator entry (Decode or Transcribe): fit, then start (see Fitting); a Transcribe
  // entry attaches to the job after, as choose does.
  async fitAndStart(action, s, job) {
    this.footer.textContent = "measuring…";
    // While it runs, an SSB entry's hover no longer rewrites the footer and choose ignores
    // further choices; a close() meanwhile (Escape, a press outside, a new box) bumps fitGen.
    const token = ++this.fitGen;
    this.setFitting(token);
    const kind = job.kind;
    let fit;
    let error = null;
    try {
      fit = await fitBand(this.ov.api, s.sourceId, s.centerHz, s.bandwidthHz);
    } catch (e) {
      error = e;
    }
    if (token !== this.fitGen) {
      // The menu was closed meanwhile: nothing starts, and a later menu is left alone.
      this.t.note(`${kind}: fit cancelled`);
      return;
    }
    this.close();
    if (error) {
      this.t.note(`${kind}: ${error.message}`, true);
      return;
    }
    let r;
    if (fit === null) {
      const lo = s.centerHz - s.bandwidthHz / 2;
      const hi = s.centerHz + s.bandwidthHz / 2;
      const why = dcClause(this.sourceRec(s.sourceId), lo, hi, "box");
      r = await this.startJob(action, s);
      if (r) {
        this.t.note(
          `no signal found in ${mhzText(lo)}-${mhzText(hi)} MHz${why}; ` +
            `started ${kind} at the box as drawn`,
        );
      }
    } else {
      const t = fitTuning(job, fit, jobTuning(action, s));
      r = await this.startJob(action, s, t);
      if (r) {
        this.t.note(`fitted ${kind} to ${bandText(t.centerHz, t.bandwidthHz)} (${fit.label})`);
      }
    }
    if (action in TRANSCRIBE && r && r.result === "job" && r.value) {
      await this.applyOp(this.attachOp(r.value));
    }
  }

  // The job_start of a job entry (a Transcribe entry: its Decode twin's) with `tuning` (by
  // default the entry's jobTuning); its result or null.
  startJob(action, s, tuning = jobTuning(action, s)) {
    const job = jobOf(action);
    const t = tuning;
    return this.applyOp({
      op: "job_start",
      kind: job.kind,
      source: s.sourceId,
      center_hz: t.centerHz,
      bandwidth_hz: t.bandwidthHz,
      deviation_hz: job.deviation,
      sideband: job.sideband,
      elastic: false,
      replace: [],
    });
  }

  // A scan time-shares the radio: the waterfall lets go of the source first (refused while a
  // rigid job holds it), the scan stops the page's elastic spectrum job on the node, and the
  // waterfall follows the scan after (and starts its own job again when the scan ends).
  async scan(s) {
    if (this.wf.sourceId !== s.sourceId) return;
    let released = false;
    try {
      released = await this.wf.release(this.wf.src);
    } catch (e) {
      this.t.note(`scan: ${e.message}`, true);
    }
    if (!released) return;
    await this.applyOp({
      op: "job_start",
      kind: "scan",
      source: s.sourceId,
      center_hz: s.centerHz,
      bandwidth_hz: s.bandwidthHz,
      deviation_hz: null,
      sideband: null,
      elastic: false,
      replace: [],
    });
    this.wf.resume();
  }

  // Draws the layer-1 labels at the selection's centre, stacked downwards, and the top
  // layer-2 label one line under the last of them (overlays.js offsets layer 2), replacing
  // the previous classification's; lists every label in the transcript.
  showLabels(s, labels) {
    for (const k of this.labelKeys) this.ov.unlabel(k);
    this.labelKeys = [];
    if (labels.length === 0) {
      this.t.note(`classify: no labels for ${s.centerHz} Hz`);
      return;
    }
    const y0 = this.wf.yForHz(s.centerHz);
    const y1 = this.wf.yForHz(s.centerHz + 1000);
    const stepHz =
      y0 !== null && y1 !== null && y0 !== y1
        ? (LABEL_STEP_PX * 1000) / (y0 - y1)
        : LABEL_FALLBACK_STEP_HZ;
    // Only the top layer-2 label is drawn; the others appear in the transcript.
    const l1 = labels.filter((l) => l.layer !== 2);
    this.boxLabel = l1[0] ? { centerHz: s.centerHz, bandwidthHz: s.bandwidthHz, name: l1[0].name } : null;
    const top2 = labels
      .filter((l) => l.layer === 2)
      .reduce((a, b) => (a && (a.confidence ?? 0) >= (b.confidence ?? 0) ? a : b), null);
    l1.forEach((l, i) => {
      const key = `classify:${i}`;
      this.ov.label(s.centerHz - i * stepHz, l, key);
      this.labelKeys.push(key);
    });
    if (top2) {
      this.ov.label(s.centerHz - Math.max(0, l1.length - 1) * stepHz, top2, "classify:l2");
      this.labelKeys.push("classify:l2");
    }
    labels.forEach((l) => {
      const conf = typeof l.confidence === "number" ? ` ${l.confidence.toFixed(2)}` : "";
      this.t.note(`L${l.layer} ${l.name}${conf}${l.evidence ? ` (${l.evidence})` : ""}`);
    });
  }

  destroy() {
    document.removeEventListener("pointerup", this.onPointerUp, true);
    document.removeEventListener("pointerdown", this.onPointerDown, true);
    document.removeEventListener("keydown", this.onKey);
    this.el.remove();
    this.outlineEl.remove();
  }
}
