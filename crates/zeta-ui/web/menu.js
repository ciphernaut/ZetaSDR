// menu.js: the context menu on a waterfall selection. Every entry applies the same operation
// the agent would (through the page's op path, so its ops-stream line shows in the transcript)
// or asks the agent.
//
// Exports:
//   mountMenu(wf, {applyOp, attachOp, ask, transcript, ov, plan, planPolicy, command, muteFor, packetsFor,
//                  onOrigins, sets, sources})
//       -> Menu   (`onOrigins(key, answer)`: What is this's classification `origins`, keyed
//       `classify:<t_ns>`, for the map to light; the box menu has no Origins entry of its
//       own: What is this? answers it, and the job rows' and the Bits pane's Origins buttons
//       ask by job)
//       Opens #ctx at the pointer after each wf.onSelect. `applyOp(op) -> Promise<result|null>`
//       is app.js's command-line op path (shows its outcome per ruling R6; null when it
//       failed, the failure already reported); `attachOp(jobId)` the `job_attach` op that
//       transcribes a job (as the job rows' Transcribe button applies it); `ask(text)` is
//       its /ask path. `muteFor(job) -> {muted, title, run} | null` is app.js's (null: no
//       running mix covers the job; `run()` applies the mute or unmute op and answers its
//       result). `packetsFor(job)` the running `packets` job taking `job` as its input, or
//       null (app.js; the job menu's Capture packets reads Stop packet capture while one runs).
//       `planPolicy()` is the view's `plan` policy (`auto`, a plan id or `none`), which
//       the fit's plan rule reads.
//   FIT_KINDS, FIT_MIN_HZ, FIT_MIN_HZ_DEFAULT, FIT_FACTOR, FIT_TIMEOUT_MS, FIT_NOISE_SNR_DB,
//   MEASURE_DC_MASK_HZ
//       the fit of a demodulator entry (see Fitting below)
//   fitBand(api, source, centerHz, bandwidthHz, applyOp = null, replace = [])
//       -> Promise<{centerHz, bandwidthHz, power99Hz, label, snrDb, dialHz} | null | false>
//       Starts a `measure` job on the band (api.op, so an OpError propagates; with `applyOp` the
//       start goes through it and a start it could not apply gives false), tails its
//       `event` stream (stream_tail, up to 3 frames within FIT_TIMEOUT_MS) and stops it
//       (always). The frame used is the first whose payload has `partial: false`,
//       else the last; null when no frame came, when its label is `noise`, or when it is
//       `unknown` under FIT_NOISE_SNR_DB. `power99Hz` is the event's `power99_hz` (the width that
//       holds 99 % of the band's excess power; null when the event has none: an older node).
//       `label` is the label's name, `snrDb` its `snr_db`,
//       `dialHz` the integer of `dial ≈ <n> Hz` in the label's evidence (an SSB label's), else
//       null.
//   fitBox(job, capture) -> hz
//       The width of the box a running job is measured in (`job` {kind, centerHz, bandwidthHz},
//       `capture` the source's `current` {center_hz, bandwidth_hz} or null): about the job's
//       centre, twice its width, no wider than the capture holds about that centre and never
//       under the job's own width. An ssb_demod keeps its own width (its fit moves the dial only).
//   sourceRecord(id, sources, shown) -> record | null
//       Source `id`'s `source_list` record: the one in `sources` (app.js's list) with that id,
//       else `shown` (the shown source's record overlays.js holds) when it is that source's.
//   fitFirstBox(job, rec) -> hz
//       The box fitJob measures first: the job's own width when `rec` (its source's record) is
//       null or has no capture, and when `rec` is local hardware (capabilities remote and push
//       false) whose capture centre the wide box holds off the job's centre (the node's DC mask
//       would leave it out, and the job's signal with it; a capture centred on the job is
//       measured whole); else fitBox on the capture.
//   fitJob(api, job, rec, applyOp = null, replace = [])
//       -> Promise<{fit, box} | null | false>
//       Measures a running job (`job` {kind, source, centerHz, bandwidthHz}, `rec` its source's
//       `source_list` record or null) in fitFirstBox's box. The wide box (fitBox) is for a
//       signal narrower than the job, whose width a narrow measurement under-reads: its result
//       is used only when its occupied run lies inside the job's own band (a margin of 1/128 of
//       the box). A run that reaches outside it is a wider signal or a neighbour, which the page
//       cannot tell apart, and which a narrower box would under-read too: the job is neither
//       narrowed nor moved (`box.keep` `the signal reaches past the job's band`), with no second
//       measure. The job's own band is measured when fitFirstBox says so and when the wide
//       measurement finds no signal; a run there that touches both its edges keeps the job as
//       well (`box.keep` `the signal fills the band`; never for an ssb_demod). `box` is
//       fitTuning's: `bandwidthHz` and `keptHz` the job's width (an occupied width never widens
//       a job), `band` the job's band, and the plan's width capped by the capture only
//       (`capture` the record's `current`), or by the job's band when the capture is not known.
//       false and null as fitBand.
//   fitTuning(job, fit, box, planAt = null) -> {centerHz, bandwidthHz, rule}
//       The tuning a fitted demodulator starts with (`job` a JOBS entry or {kind, sideband};
//       `box` {bandwidthHz, keptHz?, band?, keep?, planCapHz?, planCapBy?, capture?}:
//       `bandwidthHz` the cap of the result, `keptHz` the width kept when no measure is trusted
//       (default `bandwidthHz`: the selection's own width), `band` {centerHz, bandwidthHz} a
//       running job's own band, which the centre never leaves, `keep` the reason a running job
//       is at least as wide as its band (fitJob); `planCapHz` the cap of a plan's width (default
//       `bandwidthHz`), `planCapBy` the words naming it (default `box`), `capture`
//       {center_hz, bandwidth_hz} the capture the plan's band is held inside; `planAt(hz)` the
//       plan lookup, plans.js channelHolding with the view's policy, null for none). The width
//       rule, for a kind but ssb_demod: with `keep`, the centre is the band's and the width the
//       kept one (`kept: <keep>`) unless the plan channel holding the centre is wider (the plan
//       rule below). Else `occupied` (the label is not fm, fsk or unknown, or the occupied width
//       fit.bandwidthHz is at least FIT_MIN_HZ[kind]): the occupied width × FIT_FACTOR.
//       Otherwise that width is only a carrier line, not trusted: `plan <id> ch <n>`, the width
//       of the plan channel whose band holds the fitted centre (the centre stays the measured
//       one), at most `planCapHz` and no wider than `capture` holds about the centre, `, capped
//       by the <planCapBy>` or `, capped by the capture` added when a cap cut it; else
//       `99 % power`, fit.power99Hz when it is at least FIT_MIN_HZ[kind] (under it the sidebands
//       were below the stage's gate and it is only the carrier too); else `kept`, box.keptHz.
//       Whichever, at least FIT_MIN_HZ[kind] (FIT_MIN_HZ_DEFAULT for a kind without one), and
//       at most `bandwidthHz` but for the plan's width. `rule` is those words, null for
//       ssb_demod. An ssb_demod moves its dial only: the centre is
//       fit.dialHz when known, else fit.centerHz ∓ SSB_DIAL_OFFSET_HZ (lsb +, otherwise −),
//       and the width stays box.bandwidthHz (the entry's SSB_BANDWIDTH_HZ, or the running
//       job's own width): the channel low-passes at half the band about the dial, and USB
//       voice reaches about 2.7 kHz above it. Every other kind: the centre is fit.centerHz.
//       Integers.
//   dcClause(rec, lo, hi, what) -> string
//       ` (the capture centre <mhz> MHz is in the <what>: the 50 kHz around it are not
//       measured)` when lo..hi Hz is wider than 2 * MEASURE_DC_MASK_HZ, holds `rec`'s capture
//       centre off its own centre and `rec` (a `source_list` record) is local hardware
//       (capabilities remote and push false), else "".
//   Menu:
//     menu.open(sel, x, y)  shows the menu for a waterfall selection at client (x, y)
//     menu.close()          hides it and erases the selection rectangle
//     menu.openOutline(job, x, y, role = null)  shows #ctx-outline for a running job (a
//                           `job_list` record) at client (x, y), kept inside the viewport; the
//                           first entry is focused; `role` the voice its Listen and Transcribe use
//     menu.closeOutline()   hides it
//     menu.closeAll()       hides both menus and the set picker and erases the box (app.js, when
//                           `dir` or `time_flow` changes under them: they were placed for the
//                           other direction)
//     menu.destroy()
//     The table's runs (actions.js) call the Menu's own methods: classify, iqRead, askAbout,
//     startJob, fitAndStart, decodeAcross, scan, openSet, command, planGroupsOf (a box's ctx
//     `sel`), and fitOutline, retuneWidth, toggle, mute, stopJob (a job). A job's ctx carries
//     its band as `sel` (jobCtx), so its next steps run the box entries' runs.
//   HOVER_DWELL_MS  the hover intent's dwell (see Groups); ELSEWHERE  the `elsewhere` heading
//   JOB_SPAN_NS     the span a job's What is this? reads (the newest painted, on the shown source)
//   mountMenu's opts also take isAudio(job), play(job, {role}), transcribe(job, {role}) and
//     playing(id): app.js's, for a job menu's Listen and Transcribe (the row's ▶ and Transcribe)
//   DENOISE_KINDS   the job kinds whose demodulator node takes `denoise`, by its stage kind
//
// Entries: actions.js's table, whose header says what each does and when it is offered;
// the runs are this module's Menu methods.
// The jobs use the selection's centre and bandwidth on the source the waterfall shows (the
// SSB entries their dial frequency and SSB_BANDWIDTH_HZ instead; FM broadcast and RDS widen
// the bandwidth, ADS-B replaces it, see jobTuning).
// #ctx-footer shows the selection in the vocabulary: center_hz=… bandwidth_hz=… start_ns=…
// end_ns=…, with the span already clamped (see below); while an SSB entry (Listen or its
// Transcribe twin) is focused or hovered it shows that entry's dial instead: center_hz=<dial>
// bandwidth_hz=6000 sideband=usb|lsb start_ns=… end_ns=….
//
// Fitting: the demodulator entries (a kind in FIT_KINDS: fm_demod, wfm_demod, am_demod,
// ssb_demod; their Listen entries and Transcribe twins alike) measure the box before starting. The
// menu stays open, the box drawn and #ctx-footer reading `measuring…`, while fitBand runs on
// the selection's centre and bandwidth; then the menu closes and:
//   - false (the measure would not start; applyOp noted it, a conflict with its retry): nothing starts;
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
//     the note reads `fitted <kind> to <bandText(centre, width)> (<label>, <rule>)`, e.g.
//     `fitted fm_demod to 100.0011 MHz, 6.0 kHz (carrier, occupied)`; <rule> is fitTuning's
//     (`occupied`, `plan <id> ch <n>` with `, capped by the box` when the box cut it,
//     `99 % power`, `kept`), left out for an ssb_demod.
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
// focus between the entries of one level; Enter or Space chooses one.
//
// Groups: both menus are drawn per group of actions.js GROUPS[surface], in order (box: What is
// this?, Listen, Decode, Measure, Plans, Capture, Send to agent; job: Fit to signal, Width, the
// toggles, Mute in mix, Next, Stop), each group's offered entries in table order, those an
// entry's `elsewhere` marks after the others under an `elsewhere` (ELSEWHERE) heading. A
// `direct` group's entries (its `elsewhere` ones under the heading there too), and a group
// whose entries take one line (an entry with `items` takes one more per item) and that has no
// `elsewhere` entry, stand in the first level; any
// other group is one opener, `.ctx-item[data-group=<id>]` reading the group's label with an
// arrow towards the inline end (aria-haspopup), whose entries stand in its submenu,
// `.ctx-sub[data-group=<id>]` (role menu). An entry with a `sub` (Decode across this band…)
// opens its own, `.ctx-sub[data-for=<id>]`, the same way: one mechanism, every panel a direct
// child of the menu (`position: absolute` in it, as wide as its widest entry). A submenu opens
// when the pointer reaches its opener,
// on a click or tap (which also focuses its first entry), and by keyboard on the opener (the
// arrow towards the inline end, or Enter); the pointer on another entry of the opener's level
// closes it, as do Escape (one level, the focus back on the opener; the whole menu while a fit
// runs) and the arrow towards the inline start. placeSub puts it at the opener's side towards
// the inline end (`dir`), on the other side when there is no room there, and shifts it up to
// stay inside the viewport, at most the viewport's height (it scrolls; a focused entry is
// scrolled into view). While a submenu is open, another opener (or entry) of its level takes
// over only after the pointer rests on it HOVER_DWELL_MS. An entry's section (sectionOf)
// starts a `.ctx-sep` heading, unless nothing precedes it in its level (a heading that does
// stand first, `elsewhere` over no fitting entry, has no rule above it). A menu drawn again when
// modes_list answers is kept inside the viewport first and then keeps its open submenus, placed
// again beside their openers, and its focus (repaint).
//
// The job's menu (#ctx-outline, role menu): overlays.js opens it by right-click on a retunable
// outline, or a long press on its label in the narrow footprint; the job row's ⋯ opens it too
// (openOutline, with the voice the row plays). Its entries are actions.js's `job` ones, on the
// job's own source and tuning.
// Every retune is {op: job_retune, job, center_hz, bandwidth_hz, replace: []}. #ctx-outline-footer
// reads job=<id> center_hz=<c> bandwidth_hz=<bw>. Escape, a press outside, Up/Down and Enter act
// as for #ctx.

import { GROUPS, action as entryOf, groupOf, offered, sectionOf, subOf } from "./actions.js";
import { channelHolding, planGroups, planNoVoice } from "./plans.js";
import { bandText, mhzText, registerLines, registerText } from "./units.js";
import { capHz } from "./waterfall.js";

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
// "Decode across this band…" (plan 31 spec 4.7): the `detect` params its watch gets, the
// widths as fractions of the chosen row's band.bandwidth_hz, the node's span fit (0.5-2x).
export const DECODE_ACROSS_PERSIST_MS = 500;
export const DECODE_ACROSS_WIDTH_FIT = [0.5, 2];
// What is this? on a running job (its menu's Next) reads its band over the newest this many ns
// the waterfall painted (the box menu reads the span dragged).
export const JOB_SPAN_NS = 2_000_000_000;

// The job kinds whose demodulator node takes `denoise` (RNNoise on its audio), by the node's
// stage kind: the voice demodulators; wfm_demod (music) has none.
export const DENOISE_KINDS = new Map([
  ["fm_demod", "nbfm_demod"],
  ["am_demod", "am_demod"],
  ["ssb_demod", "ssb_demod"],
]);

// The job an entry of the table starts ({kind, deviation, sideband}; a Transcribe entry's
// twin's), or undefined.
function jobOf(id) {
  const e = entryOf(id);
  if (!e) return undefined;
  return e.twin ? entryOf(e.twin).job : e.job;
}

// Why "Decode video" cannot start on `src` (a source_list record) for a box `bandwidthHz`
// wide: the lease's refusal, `insufficient bandwidth: <id> delivers at most <cap> Hz`, with
// ` (measured at open)` for a measured cap; null when the radio carries it or says nothing.
export function videoRefusal(src, bandwidthHz) {
  const claim = src && src.capabilities ? src.capabilities.max_bandwidth_hz : null;
  const cap = capHz(claim);
  if (cap === null || !(cap < bandwidthHz)) return null;
  const measured = claim && typeof claim === "object" && "measured" in claim;
  return `insufficient bandwidth: ${src.id} delivers at most ${cap} Hz${measured ? " (measured at open)" : ""}`;
}

// The tuning a job entry uses for a selection: {centerHz, bandwidthHz}. SSB tunes the dial; FM
// broadcast and RDS widen the band to at least WFM_MIN_BANDWIDTH_HZ, POCSAG to at least
// POCSAG_MIN_BANDWIDTH_HZ and ACARS to at least ACARS_MIN_BANDWIDTH_HZ; ADS-B always asks for
// ADSB_BANDWIDTH_HZ; everything else uses the selection as is. A Transcribe entry tunes as
// its Decode twin.
export function jobTuning(action, s) {
  action = (action && entryOf(action)?.twin) ?? action;
  const sb = jobOf(action)?.sideband ?? null;
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

// Measures a band with a short `measure` job; the fit, or null when there is no signal. With
// `applyOp` the start goes through it (a conflict gets its stop-and-retry line there) and a
// start it could not apply returns false, already reported; without it a failed start throws.
export async function fitBand(api, source, centerHz, bandwidthHz, applyOp = null, replace = []) {
  const start = {
    op: "job_start",
    kind: "measure",
    source,
    center_hz: centerHz,
    bandwidth_hz: bandwidthHz,
    deviation_hz: null,
    sideband: null,
    elastic: false,
    replace,
  };
  const r = applyOp ? await applyOp(start) : await api.op(start);
  if (!r) return false;
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
    power99Hz: typeof p.power99_hz === "number" ? p.power99_hz : null,
    label: label.name,
    snrDb: p.snr_db,
    dialHz: dial ? parseInt(dial[1], 10) : null,
  };
}

// The labels whose occupied width the rule does not trust under the kind's floor (see the header).
const FIT_DOUBTED_LABELS = new Set(["fm", "fsk", "unknown"]);

// The width of the box a running job is measured in (see the header).
export function fitBox(job, capture) {
  const own = job.bandwidthHz;
  if (job.kind === "ssb_demod") return own;
  let box = 2 * own;
  if (capture && typeof capture.center_hz === "number" && typeof capture.bandwidth_hz === "number") {
    const room = Math.min(
      job.centerHz - (capture.center_hz - capture.bandwidth_hz / 2),
      capture.center_hz + capture.bandwidth_hz / 2 - job.centerHz,
    );
    if (room >= 0) box = Math.min(box, 2 * room);
  }
  return Math.round(Math.max(box, own));
}

// A measurement is about this many bins wide: a run within 1/FIT_EDGE_SLACK of the band's width
// of an edge is at that edge.
const FIT_EDGE_SLACK = 128;

// The `source_list` record of source `id`: the one in `sources` (app.js's list) with that id,
// else `shown` (overlays.js's record of the shown source) when it is that source's, else null.
export function sourceRecord(id, sources, shown) {
  const rec = (sources || []).find((s) => s && s.id === id);
  if (rec) return rec;
  return shown && shown.id === id ? shown : null;
}

// The capture of `rec` ({center_hz, bandwidth_hz}) or null when it carries none.
function captureOf(rec) {
  const c = rec && rec.current;
  return c && typeof c.center_hz === "number" && typeof c.bandwidth_hz === "number" ? c : null;
}

// The width of the box fitJob measures first (see the header).
export function fitFirstBox(job, rec) {
  const own = job.bandwidthHz;
  const capture = captureOf(rec);
  if (capture === null) return own;
  const wide = fitBox(job, capture);
  const c = rec.capabilities || {};
  if (c.remote === false && c.push === false) {
    // A local receiver's DC mask: the node leaves it out of a box that holds the capture
    // centre off the box's own centre, which can hide the job's signal.
    const dc = capture.center_hz;
    const held = Math.abs(dc - job.centerHz) <= wide / 2;
    if (held && Math.round(dc) !== Math.round(job.centerHz)) return own;
  }
  return wide;
}

// Measures a running job (see the header).
export async function fitJob(api, job, rec, applyOp = null, replace = []) {
  const own = job.bandwidthHz;
  const capture = captureOf(rec);
  const box = (keep) => ({
    bandwidthHz: own,
    keptHz: own,
    band: { centerHz: job.centerHz, bandwidthHz: own },
    // The plan's width is data: only the capture caps it (unknown: the job's band).
    ...(capture ? { planCapHz: Infinity, capture } : { planCapBy: "job's band" }),
    ...(keep && job.kind !== "ssb_demod" ? { keep } : {}),
  });
  const measureOwn = async (rep) => {
    const fit = await fitBand(api, job.source, job.centerHz, own, applyOp, rep);
    if (!fit) return fit;
    const slack = own / FIT_EDGE_SLACK;
    const fills =
      fit.centerHz - fit.bandwidthHz / 2 <= job.centerHz - own / 2 + slack &&
      fit.centerHz + fit.bandwidthHz / 2 >= job.centerHz + own / 2 - slack;
    return { fit, box: box(fills ? "the signal fills the band" : null) };
  };
  const wide = fitFirstBox(job, rec);
  if (wide <= own) return measureOwn(replace);
  const fit = await fitBand(api, job.source, job.centerHz, wide, applyOp, replace);
  if (fit === false) return false;
  if (fit === null) return measureOwn([]);
  const slack = wide / FIT_EDGE_SLACK;
  const inside =
    fit.centerHz - fit.bandwidthHz / 2 >= job.centerHz - own / 2 - slack &&
    fit.centerHz + fit.bandwidthHz / 2 <= job.centerHz + own / 2 + slack;
  if (inside) return { fit, box: box(null) };
  // The run reaches past the job's band: a wider signal or a neighbour. A box no wider than the
  // signal under-reads it, so the job is neither narrowed nor moved.
  return { fit, box: box("the signal reaches past the job's band") };
}

// The tuning a fitted demodulator starts with (see the header).
export function fitTuning(job, fit, box, planAt = null) {
  if (job.kind === "ssb_demod") {
    // The dial moves; the band stays what the entry (or the running job) has.
    let dial = fit.dialHz;
    if (dial === null || dial === undefined) {
      dial =
        job.sideband === "lsb"
          ? fit.centerHz + SSB_DIAL_OFFSET_HZ
          : fit.centerHz - SSB_DIAL_OFFSET_HZ;
    }
    return { centerHz: Math.round(dial), bandwidthHz: Math.round(box.bandwidthHz), rule: null };
  }
  const floor = FIT_MIN_HZ[job.kind] ?? FIT_MIN_HZ_DEFAULT;
  const kept = box.keptHz ?? box.bandwidthHz;
  let centerHz = fit.centerHz;
  if (box.keep && box.band) {
    // At least as wide as the band: the centre stays where it is.
    centerHz = box.band.centerHz;
  } else if (box.band) {
    // A fit never moves a running job's centre out of its own band.
    centerHz = Math.min(
      box.band.centerHz + box.band.bandwidthHz / 2,
      Math.max(box.band.centerHz - box.band.bandwidthHz / 2, centerHz),
    );
  }
  centerHz = Math.round(centerHz);
  const plan = planAt ? planAt(centerHz) : null;
  let width;
  let rule;
  if (box.keep) {
    // Never narrowed; a plan channel's width may still widen it.
    if (plan && plan.bandwidthHz > kept) return planTuning(plan, centerHz, floor, box);
    width = kept;
    rule = `kept: ${box.keep}`;
  } else if (!FIT_DOUBTED_LABELS.has(fit.label) || fit.bandwidthHz >= floor) {
    width = fit.bandwidthHz * FIT_FACTOR;
    rule = "occupied";
  } else if (plan) {
    // Only a carrier line cleared the threshold: not the signal's width.
    return planTuning(plan, centerHz, floor, box);
  } else if (typeof fit.power99Hz === "number" && fit.power99Hz >= floor) {
    width = fit.power99Hz;
    rule = "99 % power";
  } else {
    width = kept;
    rule = "kept";
  }
  const bandwidthHz = Math.min(box.bandwidthHz, Math.max(floor, Math.round(width)));
  return { centerHz, bandwidthHz: Math.round(bandwidthHz), rule };
}

// fitTuning's plan rule: the channel's width (at least the floor) about `centerHz`, at most
// box.planCapHz (default box.bandwidthHz, named box.planCapBy, default `box`) and held inside
// box.capture about that centre; the rule names the cap that cut it.
function planTuning(plan, centerHz, floor, box) {
  let width = Math.max(floor, Math.round(plan.bandwidthHz));
  let by = null;
  const cap = box.planCapHz ?? box.bandwidthHz;
  if (width > cap) {
    width = cap;
    by = box.planCapBy ?? "box";
  }
  const c = box.capture;
  if (c) {
    const room = Math.min(
      centerHz - (c.center_hz - c.bandwidth_hz / 2),
      c.center_hz + c.bandwidth_hz / 2 - centerHz,
    );
    if (room >= 0 && width > 2 * room) {
      width = Math.floor(2 * room);
      by = "capture";
    }
  }
  const rule = `plan ${plan.plan} ch ${plan.number}${by ? `, capped by the ${by}` : ""}`;
  return { centerHz, bandwidthHz: Math.round(width), rule };
}

// The transcript line of a fit that was applied (see the header).
function fittedNote(kind, t, fit) {
  const why = t.rule ? `, ${t.rule}` : "";
  return `fitted ${kind} to ${bandText(t.centerHz, t.bandwidthHz)} (${fit.label}${why})`;
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

// Places submenu `panel` beside `opener`, an item of `parent` (the menu or a panel): towards the
// inline end (the right, or the left under `rtl`), on the other side when it would leave the
// viewport there; its top at the opener's, shifted up to stay inside it. The panel is
// `position: absolute` in its menu (`root`): the place is worked out in the viewport and written
// as offsets from the menu's padding box, so a theme that makes the menu a containing block
// (filter, backdrop-filter, transform, contain) cannot move it.
const SUB_INSET_PX = 5;
export function placeSub(panel, opener, parent, rtl, root) {
  const p = parent.getBoundingClientRect();
  const o = opener.getBoundingClientRect();
  const m = root.getBoundingClientRect();
  // Measured at the menu's corner and as wide as its entries, whatever the last place was.
  panel.style.left = "0px";
  panel.style.top = "0px";
  panel.style.width = "max-content";
  const w = panel.offsetWidth;
  const h = panel.offsetHeight;
  const end = rtl ? p.left - w : p.right;
  const other = rtl ? p.right : p.left - w;
  const fitsEnd = rtl ? end >= 0 : end + w <= window.innerWidth;
  const left = Math.max(0, Math.min(fitsEnd ? end : other, window.innerWidth - w));
  // The panel's first entry level with the opener (its padding and border above it).
  const top = Math.max(0, Math.min(o.top - SUB_INSET_PX, window.innerHeight - h));
  panel.style.left = `${left - m.left - root.clientLeft}px`;
  panel.style.top = `${top - m.top - root.clientTop}px`;
}

// How long the pointer rests on another entry before the open submenu gives way (hover intent).
export const HOVER_DWELL_MS = 250;
// The heading of a group's entries whose data says the selection is not where they fit.
export const ELSEWHERE = "elsewhere";

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
    { applyOp, attachOp, ask, transcript, ov, plan = () => null, planPolicy = () => "none", ringSeconds = () => null, command = () => {}, muteFor = () => null, packetsFor = () => null, jobs = () => [], onOrigins = () => {}, sets = () => [], sources = () => [], isAudio = () => false, play = () => {}, transcribe = () => {}, playing = () => false },
  ) {
    this.isAudio = isAudio;
    this.play = play;
    this.transcribe = transcribe;
    this.playing = playing;
    this.wf = wf;
    this.sets = sets;
    this.sources = sources;
    this.onOrigins = onOrigins;
    this.muteFor = muteFor;
    this.packetsFor = packetsFor;
    this.jobs = jobs;
    this.applyOp = applyOp;
    this.ringSeconds = ringSeconds;
    this.attachOp = attachOp;
    this.ask = ask;
    this.plan = plan;
    // The fit's plan rule: the channel of the view's plan policy that holds a frequency.
    this.planAt = (hz) => channelHolding(hz, planPolicy());
    this.command = command;
    this.t = transcript;
    this.ov = ov;
    this.sel = null;
    // The fit in progress (its generation token) or null; close() bumps fitGen.
    this.fitting = null;
    this.fitGen = 0;
    this.labelKeys = [];
    this.pointer = { x: 0, y: 0 };
    // modes_list's rows (a promise), read once per page; `rows` once it answered.
    this.modesRead = null;
    this.rows = null;

    this.el = document.createElement("div");
    this.el.id = "ctx";
    this.el.setAttribute("role", "menu");
    this.el.hidden = true;
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
    this.outlineFooter = document.createElement("div");
    this.outlineFooter.id = "ctx-outline-footer";
    this.outlineEl.append(this.outlineFooter);
    document.body.append(this.outlineEl);

    // "Start set…"'s role → source picker (#ctx-set).
    this.setEl = document.createElement("form");
    this.setEl.id = "ctx-set";
    this.setEl.setAttribute("role", "dialog");
    this.setEl.setAttribute("aria-label", "Start set");
    this.setEl.hidden = true;
    const pickLabel = document.createElement("label");
    pickLabel.textContent = "Set ";
    this.setPick = document.createElement("select");
    this.setPick.className = "set-pick";
    pickLabel.append(this.setPick);
    this.setRoles = document.createElement("div");
    this.setRoles.className = "set-roles";
    const buttons = document.createElement("div");
    buttons.className = "set-buttons";
    const start = document.createElement("button");
    start.type = "submit";
    start.className = "start";
    start.textContent = "Start";
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "cancel";
    cancel.textContent = "Cancel";
    cancel.addEventListener("click", () => this.closeSet());
    buttons.append(start, cancel);
    this.setEl.append(pickLabel, this.setRoles, buttons);
    this.setPick.addEventListener("change", () => this.fillRoles());
    this.setEl.addEventListener("submit", (ev) => {
      ev.preventDefault();
      this.startSet();
    });
    this.setSource = null; // the source shown when the picker opened (its first role's)
    document.body.append(this.setEl);

    // The pointer position of the drag's end: pointerup reaches the document (capture) before
    // the waterfall's own handler calls onSelect.
    this.onPointerUp = (ev) => {
      this.pointer = { x: ev.clientX, y: ev.clientY };
    };
    this.onPointerDown = (ev) => {
      if (!this.el.hidden && !this.el.contains(ev.target)) this.close();
      if (!this.outlineEl.hidden && !this.outlineEl.contains(ev.target)) this.closeOutline();
      if (!this.setEl.hidden && !this.setEl.contains(ev.target)) this.closeSet();
    };
    this.onKey = (ev) => {
      if (!this.setEl.hidden && ev.key === "Escape") {
        ev.preventDefault();
        this.closeSet();
        return;
      }
      const outline = !this.outlineEl.hidden;
      const menu = outline ? this.outlineEl : this.el;
      if (menu.hidden) return;
      this.cancelDwell();
      const active = menu.contains(document.activeElement) ? document.activeElement : null;
      const level = (active && active.closest(".ctx-sub")) || menu;
      const rtl = getComputedStyle(menu).direction === "rtl";
      const toEnd = rtl ? "ArrowLeft" : "ArrowRight";
      const toStart = rtl ? "ArrowRight" : "ArrowLeft";
      if (ev.key === "Escape") {
        ev.preventDefault();
        // One level: the deepest open submenu, else the menu (a fit in progress: the menu).
        const deep = this.fitting === null ? this.deepestSub(menu) : null;
        if (deep) {
          this.closeSub(deep);
          deep.ctxOpener.focus({ preventScroll: true });
        } else if (outline) this.closeOutline();
        else this.close();
      } else if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
        ev.preventDefault();
        const items = this.levelItems(level);
        if (items.length === 0) return;
        const at = items.indexOf(active);
        const step = ev.key === "ArrowDown" ? 1 : -1;
        items[(at + step + items.length) % items.length].focus();
      } else if (ev.key === toEnd && active && active.ctxPanel && this.fitting === null) {
        ev.preventDefault();
        this.openSub(active.ctxPanel);
        this.levelItems(active.ctxPanel)[0]?.focus({ preventScroll: true });
      } else if (ev.key === toStart && level !== menu) {
        ev.preventDefault();
        this.closeSub(level);
        level.ctxOpener.focus({ preventScroll: true });
      }
    };
    document.addEventListener("pointerup", this.onPointerUp, true);
    document.addEventListener("pointerdown", this.onPointerDown, true);
    document.addEventListener("keydown", this.onKey);
    wf.onSelect((sel) => this.open(sel, this.pointer.x, this.pointer.y));
    // Read the rows before the first box, so its menu opens with them.
    this.modeRows();
  }

  // The selection with its span clamped, as the footer shows it.
  current() {
    const s = this.sel;
    return { ...s, ...clampSpan(s.startNs, s.endNs, newestNs(this.wf)) };
  }

  // The box menu's ctx (actions.js) for the open selection, its span clamped now.
  boxCtx() {
    return {
      surface: "box",
      host: this,
      sel: this.current(),
      plan: this.plan(),
      sets: this.readySets(),
      rows: this.rows,
      src: (this.sources() || []).find((x) => x.id === this.sel.sourceId),
    };
  }

  // The job menu's ctx (actions.js) for `job`, playing and transcribing voice `role` (null:
  // the main stream). Its `sel` is the job's band as a selection on its source (null without
  // a band); its span the newest JOB_SPAN_NS the waterfall painted when the job is on the
  // shown source, else null (the page has no time base for another source's ring).
  jobCtx(job, role = null) {
    const t = job.tuning;
    let sel = null;
    if (t && Number.isFinite(t.center_hz) && Number.isFinite(t.bandwidth_hz)) {
      const end = job.source === this.wf.sourceId ? newestNs(this.wf) : null;
      sel = {
        sourceId: job.source,
        centerHz: t.center_hz,
        bandwidthHz: t.bandwidth_hz,
        startNs: end === null ? null : Math.round(end - JOB_SPAN_NS),
        endNs: end === null ? null : Math.round(end),
        clamped: false,
      };
    }
    return {
      surface: "job",
      host: this,
      job,
      sel,
      mute: this.muteFor(job),
      packets: this.packetsFor(job),
      jobs: this.jobs(),
      rows: this.rows,
      audio: !job.input && this.isAudio(job),
      running: job.state?.status === "running",
      playing: this.playing(job.id),
      role,
      play: this.play,
      transcribe: this.transcribe,
    };
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

  // Draws `root` for `ctx` from the table (see Groups in the header), `footer` after the
  // first level and the submenu panels after it.
  paint(root, footer, ctx) {
    const offer = offered(ctx.surface, ctx);
    const nodes = [];
    const panels = [];
    for (const g of GROUPS[ctx.surface]) {
      const all = offer.filter((e) => groupOf(e, ctx.surface) === g.id);
      if (all.length === 0) continue;
      const far = all.filter((e) => e.elsewhere && e.elsewhere(ctx));
      const es = all.filter((e) => !far.includes(e));
      const lines = es.reduce((n, e) => n + 1 + (e.items ? e.items(ctx).length : 0), 0);
      if (g.direct || (lines === 1 && far.length === 0)) {
        const promoted = this.entryNodes(es, ctx, root, panels, nodes.length > 0);
        const item = promoted.find((n) => n.classList.contains("ctx-item"));
        if (!g.direct && g.prefix && item && !item.textContent.startsWith(g.label)) {
          item.textContent = `${g.label} › ${item.textContent}`;
        }
        nodes.push(...promoted);
        // A direct group's entries that do not fit stand under the heading in the first level.
        if (far.length > 0) {
          nodes.push(this.sep(ELSEWHERE), ...this.entryNodes(far, ctx, root, panels, true));
        }
        continue;
      }
      const opener = this.button(null, g.label);
      opener.dataset.group = g.id;
      const panel = this.panel(root, opener, root);
      panel.dataset.group = g.id;
      panel.setAttribute("aria-label", g.label);
      panel.append(...this.entryNodes(es, ctx, panel, panels));
      if (far.length > 0) {
        // The heading's rule only under fitting entries (style.css: a first `.ctx-sep` has none).
        panel.append(this.sep(ELSEWHERE), ...this.entryNodes(far, ctx, panel, panels, true));
      }
      nodes.push(opener);
      panels.push(panel);
    }
    root.replaceChildren(...nodes, footer, ...panels);
  }

  // The nodes of entries `es` in `container` (the menu or a submenu panel): each entry's item,
  // a `.ctx-sep` heading where a section starts after something (`after`: nodes precede these
  // in the container; a section that starts the container takes no heading), the items an
  // entry draws after it, and for an entry with a submenu its opener (its panel added to
  // `panels`).
  entryNodes(es, ctx, container, panels, after = false) {
    const nodes = [];
    let section;
    for (const entry of es) {
      const now = sectionOf(entry, ctx.surface);
      if (now && now !== section && (after || nodes.length > 0)) nodes.push(this.sep(now));
      section = now;
      const item = this.item(entry, ctx);
      this.leaves(item, container);
      nodes.push(item);
      if (entry.items) {
        const group = document.createElement("div");
        group.className = "ctx-plan-groups";
        group.setAttribute("role", "group");
        group.append(
          ...entry.items(ctx).map((x) => {
            const el = this.button(null, x.label, () => this.choose(entry, x.run));
            el.dataset.planGroup = x.key;
            if (x.title) el.title = x.title;
            this.leaves(el, container);
            return el;
          }),
        );
        nodes.push(group);
      }
      const sub = subOf(entry, ctx);
      if (sub) {
        const panel = this.panel(this.rootOf(container), item, container);
        panel.dataset.for = entry.id;
        panel.append(
          ...sub.map((x) => {
            // A greyed element says why in its title and runs nothing.
            const el = this.button(null, x.label, x.disabled ? null : () => this.choose(entry, x.run));
            el.dataset.mode = x.key;
            if (x.disabled) el.setAttribute("aria-disabled", "true");
            if (x.title) el.title = x.title;
            this.leaves(el, panel);
            return el;
          }),
        );
        panels.push(panel);
      }
    }
    return nodes;
  }

  // A `.ctx-sep` heading reading `text`.
  sep(text) {
    const sep = document.createElement("div");
    sep.className = "ctx-sep";
    sep.setAttribute("role", "separator");
    sep.textContent = text;
    return sep;
  }

  // The menu (#ctx or #ctx-outline) `node` belongs to.
  rootOf(node) {
    return node.closest("#ctx, #ctx-outline") || node;
  }

  // A submenu panel (`.ctx-sub`, hidden) that `opener`, an item in `parent` (the menu or a
  // panel), opens at its side: on the pointer reaching it (after HOVER_DWELL_MS while another
  // panel of its level is open: see dwell), on a click or a tap (which also focuses its first
  // entry); never toggled shut by them.
  panel(root, opener, parent) {
    const panel = document.createElement("div");
    panel.className = "ctx-sub";
    panel.setAttribute("role", "menu");
    panel.hidden = true;
    panel.ctxOpener = opener;
    panel.ctxParent = parent;
    opener.ctxPanel = panel;
    opener.setAttribute("aria-haspopup", "menu");
    opener.setAttribute("aria-expanded", "false");
    this.dwell(opener, parent, () => this.openSub(panel));
    opener.addEventListener("click", () => {
      if (this.fitting !== null) return;
      this.cancelDwell();
      this.openSub(panel);
      this.levelItems(panel)[0]?.focus({ preventScroll: true });
    });
    return panel;
  }

  // An item that opens nothing closes, as the pointer reaches it (see dwell), the submenus
  // open from its level (`container`).
  leaves(item, container) {
    if (item.ctxPanel) return;
    this.dwell(item, container, () => {
      for (const p of this.openSubs(this.rootOf(container))) {
        if (p.ctxParent === container) this.closeSub(p);
      }
    });
  }

  // Hover intent: the pointer reaching `item` (of level `container`) runs `act` at once when no
  // submenu of that level is open, else only once it has rested there HOVER_DWELL_MS, so a
  // pointer crossing other entries on its way into an open panel leaves the panel alone.
  // Leaving the item cancels it, as do a click and a key (cancelDwell).
  dwell(item, container, act) {
    item.addEventListener("pointerenter", () => {
      if (this.fitting !== null) return;
      this.cancelDwell();
      const open = this.openSubs(this.rootOf(container)).some((p) => p.ctxParent === container);
      if (!open) {
        act();
        return;
      }
      this.dwellTimer = setTimeout(() => {
        this.dwellTimer = null;
        if (this.fitting === null && item.isConnected) act();
      }, HOVER_DWELL_MS);
    });
    item.addEventListener("pointerleave", () => this.cancelDwell());
  }

  cancelDwell() {
    if (this.dwellTimer) clearTimeout(this.dwellTimer);
    this.dwellTimer = null;
  }

  // The open submenu panels of menu `root`.
  openSubs(root) {
    return [...root.querySelectorAll(":scope > .ctx-sub")].filter((p) => !p.hidden);
  }

  // Opens `panel` beside its opener, closing the others open from the same level first.
  openSub(panel) {
    const root = this.rootOf(panel.ctxParent);
    for (const p of this.openSubs(root)) {
      if (p !== panel && p.ctxParent === panel.ctxParent) this.closeSub(p);
    }
    panel.hidden = false;
    panel.ctxOpener.setAttribute("aria-expanded", "true");
    placeSub(panel, panel.ctxOpener, panel.ctxParent, getComputedStyle(root).direction === "rtl", root);
  }

  // Closes `panel` and the submenus open from it.
  closeSub(panel) {
    const root = this.rootOf(panel.ctxParent);
    for (const p of this.openSubs(root)) if (p.ctxParent === panel) this.closeSub(p);
    panel.hidden = true;
    panel.ctxOpener.setAttribute("aria-expanded", "false");
  }

  // The deepest open submenu of `root` (one with none open from it), or null.
  deepestSub(root) {
    const open = this.openSubs(root);
    return open.find((p) => !open.some((q) => q.ctxParent === p)) || null;
  }

  // The items of one level (`container`: the menu or a panel), shown ones only.
  levelItems(container) {
    const root = this.rootOf(container);
    return [...container.querySelectorAll(".ctx-item")].filter(
      (i) => (i.closest(".ctx-sub") || root) === container && i.closest("[hidden]") === null,
    );
  }

  // A `.ctx-item` reading `label` (`id` its data-action, none for null), calling `onClick`.
  button(id, label, onClick) {
    const item = document.createElement("button");
    item.className = "ctx-item";
    if (id !== null) item.dataset.action = id;
    item.setAttribute("role", "menuitem");
    item.textContent = label;
    if (onClick) item.addEventListener("click", onClick);
    return item;
  }

  // The `.ctx-item` of a table entry: its label for ctx, greyed when `disabled` says why, an
  // SSB entry's dial in the footer while it is hovered or focused.
  item(entry, ctx) {
    const label = typeof entry.label === "function" ? entry.label(ctx) : entry.label;
    const item = this.button(entry.id, label, subOf(entry, ctx) ? null : () => this.choose(entry));
    const refused = entry.disabled ? entry.disabled(ctx) : null;
    if (refused) {
      item.setAttribute("aria-disabled", "true");
      item.dataset.refused = "true";
      item.title = refused;
    }
    if (ctx.surface === "box" && jobOf(entry.id)?.sideband) {
      const idle = () => this.sel && this.fitting === null;
      const show = () => idle() && this.showFooter(this.current(), entry.id);
      const hide = () => idle() && this.showFooter(this.current());
      item.addEventListener("focus", show);
      item.addEventListener("pointerenter", show);
      item.addEventListener("blur", hide);
      item.addEventListener("pointerleave", hide);
    }
    return item;
  }

  // Chooses `entry` (or one of its items or submenu rows, `run`) on the open menu: nothing
  // while a fit runs or while it is greyed; the menu closes first unless the entry keeps it
  // open; the box's ctx is taken now (its span clamped again).
  async choose(entry, run = (c) => entry.run(c)) {
    if (this.fitting !== null) return;
    const box = !this.el.hidden;
    if (box ? !this.sel : !this.outlineJob) return;
    const ctx = box ? this.boxCtx() : this.jobCtx(this.outlineJob, this.outlineRole);
    if (entry.disabled && entry.disabled(ctx)) return;
    if (box) this.showFooter(ctx.sel, entry.id);
    if (!entry.keep) {
      if (box) this.close();
      else this.closeOutline();
    }
    return run(ctx);
  }

  open(sel, x, y) {
    if (!this.wf.sourceId) return;
    // A new box cancels a fit still running for the previous one.
    this.fitGen += 1;
    this.setFitting(null);
    this.sel = { ...sel, sourceId: this.wf.sourceId };
    this.paint(this.el, this.footer, this.boxCtx());
    this.showFooter(this.current());
    // The rows' first answer draws the menu again (Decode across's rows).
    if (this.rows === null) {
      const at = this.sel;
      this.modeRows().then(() => {
        if (this.sel === at && !this.el.hidden && this.fitting === null) this.repaint();
      });
    }
    this.el.hidden = false;
    // Keep it inside the viewport.
    const w = this.el.offsetWidth;
    const h = this.el.offsetHeight;
    this.el.style.left = `${Math.max(0, Math.min(x, window.innerWidth - w))}px`;
    this.el.style.top = `${Math.max(0, Math.min(y, window.innerHeight - h))}px`;
    this.el.querySelector(".ctx-item")?.focus({ preventScroll: true });
  }

  // Draws the open box menu again (the rows came after it opened), keeping its open submenus
  // and the focused entry, and keeping it inside the viewport.
  repaint() {
    const key = (n) =>
      n && (n.dataset.group ? `g:${n.dataset.group}` : n.dataset.for ? `f:${n.dataset.for}`
        : n.dataset.action ? `a:${n.dataset.action}` : n.dataset.planGroup ? `p:${n.dataset.planGroup}`
          : n.dataset.mode ? `m:${n.dataset.mode}` : null);
    const opened = this.openSubs(this.el).map((p) => key(p));
    const active = this.el.contains(document.activeElement) ? document.activeElement : null;
    const focusKey = active ? key(active) : null;
    const focusIn = active ? key(active.closest(".ctx-sub")) : null;
    this.paint(this.el, this.footer, this.boxCtx());
    const panelOf = (k) =>
      [...this.el.querySelectorAll(":scope > .ctx-sub")].find((p) => key(p) === k);
    // The menu first, so the panels are placed beside where their openers end up.
    this.clamp(this.el);
    for (const k of opened) {
      const p = panelOf(k);
      if (p) this.openSub(p);
    }
    if (focusKey) {
      const level = focusIn ? panelOf(focusIn) : this.el;
      const again = level && this.levelItems(level).find((i) => key(i) === focusKey);
      (again || this.levelItems(this.el)[0])?.focus({ preventScroll: true });
    }
  }

  // Keeps menu `el` inside the viewport where it stands.
  clamp(el) {
    const x = parseFloat(el.style.left) || 0;
    const y = parseFloat(el.style.top) || 0;
    el.style.left = `${Math.max(0, Math.min(x, window.innerWidth - el.offsetWidth))}px`;
    el.style.top = `${Math.max(0, Math.min(y, window.innerHeight - el.offsetHeight))}px`;
  }

  close() {
    this.cancelDwell();
    this.el.hidden = true;
    this.wf.clearSelection();
    // A fit in progress is cancelled: its result no longer belongs to an open menu.
    this.fitGen += 1;
    this.setFitting(null);
  }

  openOutline(job, x, y, role = null) {
    // One menu at a time; opening cancels a fit still running for the previous one.
    if (!this.el.hidden) this.close();
    this.fitGen += 1;
    this.setFitting(null);
    this.outlineJob = job;
    this.outlineRole = role;
    const t = job.tuning || {};
    this.paint(this.outlineEl, this.outlineFooter, this.jobCtx(job, role));
    this.outlineFooter.textContent =
      `job=${job.id} center_hz=${t.center_hz} bandwidth_hz=${t.bandwidth_hz}`;
    this.outlineEl.hidden = false;
    const w = this.outlineEl.offsetWidth;
    const h = this.outlineEl.offsetHeight;
    this.outlineEl.style.left = `${Math.max(0, Math.min(x, window.innerWidth - w))}px`;
    this.outlineEl.style.top = `${Math.max(0, Math.min(y, window.innerHeight - h))}px`;
    this.outlineEl.querySelector(".ctx-item")?.focus({ preventScroll: true });
  }

  closeOutline() {
    this.cancelDwell();
    this.outlineEl.hidden = true;
    this.outlineJob = null;
    this.fitGen += 1;
    this.setFitting(null);
  }

  // Closes whatever is open (both menus, a fit in either cancelled, and the set picker) and
  // erases the box.
  closeAll() {
    this.close();
    this.closeOutline();
    this.closeSet();
  }

  // Fit to signal on a running job's band. A conflict on the measure start gets the usual
  // stop-and-retry line, whose retry runs this whole fit again with the holders `replace`d.
  async fitOutline(job, replace = []) {
    const t = job.tuning;
    {
      const token = ++this.fitGen;
      this.setFitting(token);
      const kind = job.kind;
      let fit;
      let measured = null;
      let error = null;
      const start = (op) =>
        this.applyOp(op, { retry: (holders) => this.fitOutline(job, holders) });
      try {
        const own = { kind, source: job.source, centerHz: t.center_hz, bandwidthHz: t.bandwidth_hz };
        const r = await fitJob(this.ov.api, own, this.sourceRec(job.source), start, replace);
        if (r) {
          fit = r.fit;
          measured = r;
        } else fit = r;
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
      } else if (fit === false) {
        // The measure would not start: applyOp already noted why (a conflict offers its retry).
      } else if (fit === null) {
        const lo = t.center_hz - t.bandwidth_hz / 2;
        const hi = t.center_hz + t.bandwidth_hz / 2;
        const why = dcClause(this.sourceRec(job.source), lo, hi, "band");
        this.t.note(`no signal found in the band${why}; left as it is`);
      } else {
        // The sideband from the job's ssb_demod node (a record carries none); USB without one.
        const ssb = ((job.graph && job.graph.nodes) || []).find((n) => n.kind === "ssb_demod");
        const sideband = (ssb && ssb.params && ssb.params.sideband) || null;
        const f = fitTuning({ kind, sideband }, fit, measured.box, this.planAt);
        if (measured.box.keep && f.centerHz === t.center_hz && f.bandwidthHz === t.bandwidth_hz) {
          // Kept as it is (at least this wide): nothing to retune.
          this.t.note(fittedNote(kind, f, fit));
          return;
        }
        const r = await this.applyOp({
          op: "job_retune",
          job: job.id,
          center_hz: f.centerHz,
          bandwidth_hz: f.bandwidthHz,
          replace: [],
        });
        if (r) {
          this.t.note(fittedNote(kind, f, fit));
        }
      }
      return;
    }
  }

  // A width preset: job_retune to `hz` about the job's centre.
  retuneWidth(job, hz) {
    return this.applyOp({
      op: "job_retune",
      job: job.id,
      center_hz: job.tuning.center_hz,
      bandwidth_hz: hz,
      replace: [],
    });
  }

  // Denoise or Stereo: the job's own graph with `param` flipped on `node`'s kind. A carried
  // param, so the node keeps the job re-expandable and relaunches it under its id (an attached
  // transcribe follows); stereo gives the relaunched job two channels.
  async toggle(job, node, param) {
    if (!node) return;
    const on = !(node.params && node.params[param]);
    const graph = JSON.parse(JSON.stringify(job.graph));
    for (const n of graph.nodes) {
      if (n.kind === node.kind) n.params = { ...(n.params || {}), [param]: on };
    }
    const r = await this.applyOp({ op: "graph_put", job: job.id, graph });
    if (r) this.t.note(`${job.kind}: ${param} ${on ? "on" : "off"}`);
  }

  // Mute in mix: the covering mix's own graph, read now, with this channel added to (or taken
  // off) `muted`.
  async mute(job) {
    const m = this.muteFor(job);
    if (!m) return;
    const r = await m.run();
    if (r) this.t.note(`${job.kind}: ${r.muted ? "muted" : "unmuted"} in mix`);
  }

  stopJob(job) {
    return this.applyOp({ op: "job_stop", job: job.id });
  }

  // Capture packets: attach a packets job to `job`, or stop `running` (its packets job).
  capturePackets(job, running) {
    return this.applyOp(
      running
        ? { op: "job_stop", job: running.id }
        : { op: "job_attach", kind: "packets", job: job.id, language: null, stream: null },
    );
  }

  // modes_list's rows, read on first use and kept (a failure is tried again at the next
  // opening); `this.rows` once they came.
  modeRows() {
    if (!this.modesRead) {
      this.modesRead = this.ov.api
        .op({ op: "modes_list", family: null, state: null })
        .then((r) => {
          this.rows = (r && r.value && r.value.modes) || [];
          return this.rows;
        })
        .catch(() => {
          this.modesRead = null;
          return [];
        });
    }
    return this.modesRead;
  }

  // The named groups of `plan` that a voice entry may start (none for null): every one but a
  // group whose channels are all data channels (`no_voice`).
  planGroupsOf(plan) {
    if (plan === null) return [];
    const noVoice = planNoVoice(plan);
    return planGroups(plan).filter((g) => !g.channels.every((n) => noVoice.includes(n)));
  }

  // A watch on the selection that decodes `row` directly (plan 31 spec 4.7, call 5.1: the
  // page keeps the two-op form): job_start, then its `detect` node's modes, persist_ms and
  // width_hz by graph_put.
  async decodeAcross(row, s) {
    const r = await this.startJob("watch", s);
    if (!r || r.result !== "job" || !r.value) return;
    const id = r.value;
    let graph;
    try {
      graph = (await this.ov.api.op({ op: "graph_get", job: id })).value;
    } catch (e) {
      this.t.note(`graph_get: ${e.message}`, true);
      return;
    }
    const detect = ((graph && graph.nodes) || []).find((n) => n.kind === "detect");
    if (!detect) {
      this.t.note(`watch ${id}: no detect node`, true);
      return;
    }
    const bw = row.band.bandwidth_hz;
    const [lo, hi] = DECODE_ACROSS_WIDTH_FIT;
    detect.params = {
      ...(detect.params || {}),
      modes: [row.id],
      persist_ms: DECODE_ACROSS_PERSIST_MS,
      width_hz: [Math.round(lo * bw), Math.round(hi * bw)],
    };
    await this.applyOp({ op: "graph_put", job: id, graph });
  }

  // Source `id`'s `source_list` record (sourceRecord: app.js's list, else the one overlays.js
  // holds for the shown source), or null.
  sourceRec(id) {
    return sourceRecord(id, this.sources(), this.ov && this.ov.sourceRec);
  }

  // Marks fit `token` in progress (null: none), the entries aria-disabled meanwhile (a greyed
  // entry, `data-refused`, stays so).
  setFitting(token) {
    this.fitting = token;
    const items = [
      ...this.el.querySelectorAll(".ctx-item"),
      ...this.outlineEl.querySelectorAll(".ctx-item"),
    ];
    for (const item of items) {
      if (token === null && !item.dataset.refused) item.removeAttribute("aria-disabled");
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

  // The node's `selection` for `s` (a selection with its span).
  selection(s) {
    return {
      source: s.sourceId,
      span: { start_ns: s.startNs, end_ns: s.endNs },
      center_hz: s.centerHz,
      bandwidth_hz: s.bandwidthHz,
    };
  }

  // What is this?: classify on `s` (see the header).
  async classify(s) {
    const selection = this.selection(s);
    this.t.note("classify: running…");
    let r = await this.applyOp({ op: "classify", selection }, { errors: true });
    if (r && r.error === "ring" && r.detail?.cause === "too_old") r = await this.reclassify(selection, r.detail);
    if (r && r.result === "classification" && r.value) {
      this.showLabels(s, r.value.labels || []);
      for (const t of r.value.tried || []) {
        this.t.note(`tried ${t.mode} ${Number(t.score).toFixed(2)} ${t.outcome}`);
      }
      // An older node sends no `register`: its allocations' one line.
      const reg = r.value.register
        ? registerLines(r.value.register)
        : [registerText(r.value.allocations)].filter(Boolean);
      for (const line of reg) this.t.note(line);
      const o = r.value.origins;
      if (o) this.onOrigins(`classify:${o.t_ns}`, o);
    }
  }

  // Save selection as SigMF: iq_read on `s`.
  async iqRead(s) {
    const r = await this.applyOp({ op: "iq_read", selection: this.selection(s) });
    if (r && r.result === "iq" && r.value) {
      this.t.note(`saved ${r.value.samples} samples to ${r.value.path}`);
    }
  }

  // Send to agent: asks about `s`, naming its radio.
  askAbout(s) {
    return this.ask(
      `Look at ${s.centerHz} Hz, ${s.bandwidthHz} Hz wide, on ${s.sourceId}, ` +
        `from ${s.startNs} to ${s.endNs}: what is it?`,
    );
  }

  // A demodulator entry (Decode or Transcribe) or Capture bursts: fit, then start (see
  // Fitting); a Transcribe entry attaches to the job after.
  async fitAndStart(action, s, replace = []) {
    const job = jobOf(action);
    this.footer.textContent = "measuring…";
    // While it runs, an SSB entry's hover no longer rewrites the footer and choose ignores
    // further choices; a close() meanwhile (Escape, a press outside, a new box) bumps fitGen.
    const token = ++this.fitGen;
    this.setFitting(token);
    const kind = job.kind;
    let fit;
    let error = null;
    try {
      // A conflict's retry runs this whole fit-and-start again with the holders `replace`d.
      const start = (op) =>
        this.applyOp(op, { retry: (holders) => this.fitAndStart(action, s, holders) });
      fit = await fitBand(this.ov.api, s.sourceId, s.centerHz, s.bandwidthHz, start, replace);
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
    if (fit === false) return; // applyOp already noted why the measure would not start
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
      const t = fitTuning(job, fit, jobTuning(action, s), this.planAt);
      r = await this.startJob(action, s, t);
      if (r) {
        this.t.note(fittedNote(kind, t, fit));
      }
    }
    if (entryOf(action)?.twin && r && r.result === "job" && r.value) {
      await this.applyOp(this.attachOp(r.value));
    }
  }

  // The job_start of a job entry (a Transcribe entry: its Decode twin's) with `tuning` (by
  // default the entry's jobTuning); its result or null.
  // `handler` (a handler id) is sent only when given: without it the policy picks.
  startJob(action, s, tuning = jobTuning(action, s), handler = null) {
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
      ...(handler === null ? {} : { handler }),
    });
  }

  // Send to: a sibling of `job` running `handler` on its band (job_start beside it, with the
  // streams `needs`); the new job's id, or null when the node refused (applyOp has noted why,
  // as for a failed startJob).
  async sendTo(job, handler, needs = []) {
    const t = job.tuning;
    const r = await this.applyOp({
      op: "job_start",
      kind: job.kind,
      source: job.source,
      center_hz: t.center_hz,
      bandwidth_hz: t.bandwidth_hz,
      deviation_hz: null,
      sideband: null,
      elastic: false,
      replace: [],
      handler,
      needs,
      beside: job.id,
    });
    return r && r.result === "job" && r.value ? r.value : null;
  }

  // A scan time-shares the radio but blocks nothing: the waterfall lets go of the source first
  // and the node decides (a stepped scan on a source another job holds waits for it, its row
  // saying so, while the waterfall shows the capture); a scan that takes the source stops the
  // page's elastic spectrum job on the node, and the waterfall follows the scan after (and
  // starts its own job again when the scan ends). On a source that sweeps for itself, shown
  // by the tab's own scan (sweep mode, a KiwiSDR), the entry re-aims that scan to the span
  // (wf.requestView, a job_retune) instead: the source has one sweep, which that scan holds,
  // and a second scan would be refused it.
  async scan(s) {
    if (this.wf.sourceId !== s.sourceId) return;
    if (this.wf.mode === "sweep" && this.wf.jobId) {
      this.wf.requestView(s.centerHz - s.bandwidthHz / 2, s.centerHz + s.bandwidthHz / 2);
      return;
    }
    if (!(await this.wf.release())) return;
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

  // set_list's ready sets (the page reads set_list with each job poll).
  readySets() {
    return (this.sets() || []).filter((s) => s && s.state === "ready");
  }

  // Opens #ctx-set where the menu stood: the ready sets, the first chosen, and its roles.
  openSet(sourceId) {
    const ready = this.readySets();
    if (ready.length === 0) return;
    this.setSource = sourceId;
    this.setPick.replaceChildren(
      ...ready.map((s) => {
        const o = document.createElement("option");
        o.value = s.id;
        o.textContent = s.name;
        o.title = s.notes || "";
        return o;
      }),
    );
    this.setPick.value = ready[0].id;
    this.fillRoles();
    this.setEl.hidden = false;
    const w = this.setEl.offsetWidth;
    const h = this.setEl.offsetHeight;
    const x = parseFloat(this.el.style.left) || 0;
    const y = parseFloat(this.el.style.top) || 0;
    this.setEl.style.left = `${Math.max(0, Math.min(x, window.innerWidth - w))}px`;
    this.setEl.style.top = `${Math.max(0, Math.min(y, window.innerHeight - h))}px`;
    this.setPick.focus({ preventScroll: true });
  }

  // One row per role of the chosen set (in the order its members first use them), each a
  // select of every attached source plus "pick" (null: the node picks a free one); the first
  // role defaults to the source shown when the picker opened, the rest to "pick".
  fillRoles() {
    const set = this.readySets().find((s) => s.id === this.setPick.value);
    if (!set) {
      this.setRoles.replaceChildren();
      return;
    }
    const roles = [];
    for (const m of set.members || []) if (!roles.includes(m.radio)) roles.push(m.radio);
    for (const r of Object.keys(set.radios || {})) if (!roles.includes(r)) roles.push(r);
    const ids = (this.sources() || []).map((s) => s.id);
    if (this.setSource && !ids.includes(this.setSource)) ids.unshift(this.setSource);
    this.setRoles.replaceChildren(
      ...roles.map((role, i) => {
        const label = document.createElement("label");
        label.className = "set-role";
        label.title = set.radios?.[role]?.note || "";
        label.append(`${role} `);
        const sel = document.createElement("select");
        sel.dataset.role = role;
        const pick = document.createElement("option");
        pick.value = "";
        pick.textContent = "pick";
        sel.append(pick);
        for (const id of ids) {
          const o = document.createElement("option");
          o.value = id;
          o.textContent = id;
          sel.append(o);
        }
        sel.value = i === 0 && this.setSource ? this.setSource : "";
        label.append(sel);
        return label;
      }),
    );
  }

  // Start: set_start of the chosen set with each role's source (null for "pick"), every
  // member, replacing nothing; its line (and any refusal) reaches the transcript through
  // applyOp.
  async startSet() {
    const set = this.setPick.value;
    const radios = {};
    for (const sel of this.setRoles.querySelectorAll("select[data-role]")) {
      radios[sel.dataset.role] = sel.value === "" ? null : sel.value;
    }
    this.closeSet();
    if (!set) return;
    await this.applyOp({ op: "set_start", set, radios, members: null, replace: [] });
  }

  closeSet() {
    this.setEl.hidden = true;
  }

  destroy() {
    document.removeEventListener("pointerup", this.onPointerUp, true);
    document.removeEventListener("pointerdown", this.onPointerDown, true);
    document.removeEventListener("keydown", this.onKey);
    this.el.remove();
    this.outlineEl.remove();
    this.setEl.remove();
  }
}
