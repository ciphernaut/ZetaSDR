// actions.js: the one entry table of the page's popup menus. The box menu (#ctx, after a drag on
// the waterfall), a running job's menu (#ctx-outline: its outline's right-click, a long press on
// its tag) and a job row's audio buttons (agent.js) all draw from it, so a feature adds a row
// here and not a line to each menu. What each entry does is in menu.js's header (the host's
// methods its `run` calls); this module says which entries there are, where they sit and when
// they are offered.
//
// Exports:
//   ACTIONS  the table, in menu order. Each entry:
//     id        the `data-action` of its `.ctx-item` (unique)
//     label     its text: a string, or (ctx) -> string for an entry that reads a state (Denoise
//               on|off, Mute|Unmute in mix)
//     group     the group it sits in: a group id of GROUPS[surface] (the same on every surface),
//               or {box, job} when it differs
//     on        the surfaces it is offered on: "box" (a selection) and/or "job" (a running job)
//     job       for an entry that starts a job: {kind, deviation, sideband} (deviation_hz and
//               sideband of its job_start); a Transcribe entry has `twin` instead, the id of the
//               entry whose job it starts before attaching a transcriber
//     applies(ctx) -> bool   whether it is offered (absent: always)
//     disabled(ctx) -> string | null   why it is offered greyed (aria-disabled, the string its
//               title); absent: never
//     section   a heading the entry starts in its group (a `.ctx-sep` before the first entry
//               of a section: the Transcribe twins under the demodulators)
//     keep      true when the menu stays open while it runs (the fitted entries: the fit
//               closes it); every other entry closes the menu first
//     items(ctx) -> [{key, label, title, run(ctx)}]   entries drawn after it, one per element (a
//               plan's named groups)
//     sub(ctx) -> [{key, label, run(ctx)}]   the entry opens these as its submenu (`.ctx-sub
//               [data-for=<id>]`), each `.ctx-item[data-mode=<key>]`; an entry whose sub is
//               empty is not offered
//     run(ctx)  what choosing it does
//   GROUPS   per surface, its groups in menu order: [{id, label, direct}]. A group's entries open
//            as a submenu at its opener's side (`.ctx-item[data-group=<id>]` reading `label`,
//            its panel `.ctx-sub[data-group=<id>]`); a `direct` group's entries, or a group's
//            only line, stand in the first level instead, and a group with no entry offered is
//            left out
//   action(id) -> the entry, or undefined
//   groupOf(entry, surface) -> the entry's group id on that surface
//   offered(surface, ctx) -> the entries offered on a surface for ctx, in table order
//   runAction(id, ctx)  runs an entry that applies to ctx (agent.js's row buttons)
//
// A ctx is the menu's own (menu.js builds it):
//   box: {surface: "box", host, sel, plan, sets, rows, src}: `host` the Menu (whose methods the
//        runs call), `sel` the selection with its span clamped ({sourceId, centerHz, bandwidthHz,
//        startNs, endNs, clamped}), `plan` the plan in view (an id or null), `sets` set_list's
//        ready sets, `rows` modes_list's rows (null until read), `src` the shown source's
//        source_list record (or undefined)
//   job: {surface: "job", host, job, mute, audio, running, role, play, transcribe}: `job` a
//        job_list record, `mute` app.js muteFor(job) ({muted, title, run} or null), `audio`
//        whether the job has an audio stream to play (not an attached job), `running` whether it
//        runs, `role` the voice to play and transcribe (null: the main stream), `play(job,
//        {role})` and `transcribe(job, {role})` the row's ▶ and Transcribe callbacks (app.js)

import { DENOISE_KINDS, FIT_KINDS, videoRefusal } from "./menu.js";

// The width presets of a running demodulator's menu, in Hz.
const WIDTHS = [2500, 5000, 6250, 12500, 25000];

export const GROUPS = {
  box: [
    { id: "what", label: "What is this?", direct: true },
    { id: "listen", label: "Listen" },
    { id: "decode", label: "Decode" },
    { id: "measure", label: "Measure" },
    { id: "plans", label: "Plans" },
    { id: "capture", label: "Capture" },
    { id: "agent", label: "Send to agent", direct: true },
  ],
  job: [
    { id: "fit", label: "Fit to signal", direct: true },
    { id: "width", label: "Width" },
    { id: "toggles", label: "Toggles", direct: true },
    { id: "mix", label: "Mix", direct: true },
    { id: "next", label: "Next" },
    { id: "stop", label: "Stop", direct: true },
  ],
};

// The demodulator node of `job` (a job_list record with its graph) that takes `denoise`, or null.
export function denoiseNode(job) {
  const kind = DENOISE_KINDS.get(job.kind);
  if (!kind) return null;
  return ((job.graph && job.graph.nodes) || []).find((n) => n.kind === kind) || null;
}

// The wfm_demod node of a wfm_demod `job`, the one that takes `stereo`, or null.
export function stereoNode(job) {
  if (job.kind !== "wfm_demod") return null;
  return ((job.graph && job.graph.nodes) || []).find((n) => n.kind === "wfm_demod") || null;
}

const fits = (c) => FIT_KINDS.has(c.job.kind);
const start = (id) => (c) => c.host.startJob(id, c.sel);
const fitted = (id) => (c) => c.host.fitAndStart(id, c.sel);
const job = (kind, deviation = null, sideband = null) => ({ kind, deviation, sideband });

// A decoder entry: a job started on the selection as its tuning says (menu.js jobTuning).
const decoder = (id, label, kind = id) => ({
  id, label, group: "decode", on: ["box"], job: job(kind), run: start(id),
});
// A demodulator entry: fitted to the signal first.
const demod = (id, label, j) => ({
  id, label, group: "listen", on: ["box"], job: j, keep: true, run: fitted(id),
});
// A demodulator's Transcribe twin: the twin's job, then a transcriber attached to it.
const twin = (id, label, of) => ({
  id, label, group: "listen", section: "Transcribe", on: ["box"], twin: of, keep: true,
  run: fitted(id),
});

export const ACTIONS = [
  { id: "classify", label: "What is this?", group: "what", on: ["box"], run: (c) => c.host.classify(c.sel) },
  demod("fm_demod", "NBFM audio", job("fm_demod", 5000)),
  demod("wfm_demod", "FM broadcast audio", job("wfm_demod", 75000)),
  demod("am_demod", "AM", job("am_demod")),
  demod("ssb_usb", "SSB (USB)", job("ssb_demod", null, "usb")),
  demod("ssb_lsb", "SSB (LSB)", job("ssb_demod", null, "lsb")),
  twin("fm_transcribe", "NBFM voice", "fm_demod"),
  twin("wfm_transcribe", "FM broadcast", "wfm_demod"),
  twin("am_transcribe", "AM", "am_demod"),
  twin("ssb_usb_transcribe", "SSB (USB)", "ssb_usb"),
  twin("ssb_lsb_transcribe", "SSB (LSB)", "ssb_lsb"),
  decoder("aprs_decode", "APRS"),
  decoder("pocsag_decode", "POCSAG"),
  decoder("rds_decode", "RDS (FM station data)"),
  decoder("adsb_decode", "ADS-B (dump1090)"),
  decoder("acars_decode", "ACARS"),
  {
    ...decoder("video", "Video", "video_decode"),
    disabled: (c) => videoRefusal(c.src, c.sel.bandwidthHz),
  },
  {
    id: "decode_across", label: "Decode across this band…", group: "decode", on: ["box"],
    // The ready rows no wider than the selection.
    sub: (c) =>
      (c.rows || [])
        .filter((r) => r.state === "ready" && typeof r.band?.bandwidth_hz === "number"
          && r.band.bandwidth_hz <= c.sel.bandwidthHz)
        .map((row) => ({
          key: row.id,
          label: `Decode ${row.name} across this band`,
          run: (k) => k.host.decodeAcross(row, k.sel),
        })),
  },
  { id: "identify", label: "Identify (thorough)", group: "measure", on: ["box"], job: job("identify"), run: start("identify") },
  { id: "watch", label: "Watch this band", group: "measure", on: ["box"], job: job("watch"), run: start("watch") },
  { id: "measure", label: "Measure job", group: "measure", on: ["box"], job: job("measure"), run: start("measure") },
  { id: "scan", label: "Scan this span", group: "measure", on: ["box"], run: (c) => c.host.scan(c.sel) },
  {
    id: "plan", label: "Start plan…", group: "plans", on: ["box"],
    applies: (c) => c.plan !== null,
    run: (c) => c.host.command(`plan start ${c.plan} fm_demod transcribe`),
    // One per named group of the plan but for a group of data channels only.
    items: (c) =>
      c.host.planGroupsOf(c.plan).map((g) => ({
        key: g.id,
        label: `Start plan › ${g.name}`,
        title: `${c.plan} channels ${g.channels.join(", ")}`,
        run: (k) => k.host.command(`plan start ${c.plan} fm_demod transcribe group ${g.id}`),
      })),
  },
  {
    id: "set", label: "Start set…", group: "plans", on: ["box"],
    applies: (c) => c.sets.length > 0, run: (c) => c.host.openSet(c.sel.sourceId),
  },
  {
    id: "capture", label: "Capture bursts", group: "capture", on: ["box"],
    job: job("capture"), keep: true, run: fitted("capture"),
  },
  { id: "iq_read", label: "Save selection as SigMF", group: "capture", on: ["box"], run: (c) => c.host.iqRead(c.sel) },
  { id: "ask", label: "Send to agent", group: "agent", on: ["box"], run: (c) => c.host.askAbout(c.sel) },

  // A running job's own entries.
  {
    id: "fit", label: "Fit to signal", group: "fit", on: ["job"], applies: fits, keep: true,
    run: (c) => c.host.fitOutline(c.job),
  },
  ...WIDTHS.map((hz) => ({
    id: `w${hz}`,
    label: `${hz / 1000} kHz`,
    group: "width",
    on: ["job"],
    applies: fits,
    run: (c) => c.host.retuneWidth(c.job, hz),
  })),
  {
    id: "denoise", group: "toggles", on: ["job"],
    label: (c) => (denoiseNode(c.job)?.params?.denoise ? "Denoise off" : "Denoise on"),
    applies: (c) => denoiseNode(c.job) !== null,
    run: (c) => c.host.toggle(c.job, denoiseNode(c.job), "denoise"),
  },
  {
    id: "stereo", group: "toggles", on: ["job"],
    label: (c) => (stereoNode(c.job)?.params?.stereo ? "Stereo off" : "Stereo on"),
    applies: (c) => stereoNode(c.job) !== null,
    run: (c) => c.host.toggle(c.job, stereoNode(c.job), "stereo"),
  },
  {
    id: "mute", group: "mix", on: ["job"],
    label: (c) => (c.mute && c.mute.muted ? "Unmute in mix" : "Mute in mix"),
    applies: (c) => c.mute !== null,
    run: (c) => c.host.mute(c.job),
  },
  // The job's next steps (the row's ▶ and Transcribe).
  {
    id: "listen", label: "Listen", group: "next", on: ["job"],
    applies: (c) => c.audio,
    run: (c) => c.play(c.job, { role: c.role }),
  },
  {
    id: "transcribe", label: "Transcribe", group: "next", on: ["job"],
    applies: (c) => c.audio && c.running,
    run: (c) => c.transcribe(c.job, { role: c.role }),
  },
  { id: "stop", label: "Stop", group: "stop", on: ["job"], run: (c) => c.host.stopJob(c.job) },
];

const BY_ID = new Map(ACTIONS.map((a) => [a.id, a]));

export function action(id) {
  return BY_ID.get(id);
}

export function groupOf(entry, surface) {
  return typeof entry.group === "string" ? entry.group : entry.group[surface];
}

// Whether `entry` is offered for ctx: on its surface, applying, and with a non-empty submenu.
function isOffered(entry, ctx) {
  if (!entry.on.includes(ctx.surface)) return false;
  if (entry.applies && !entry.applies(ctx)) return false;
  if (entry.sub && entry.sub(ctx).length === 0) return false;
  return true;
}

export function offered(surface, ctx) {
  return ACTIONS.filter((a) => isOffered(a, { ...ctx, surface }));
}

export function runAction(id, ctx) {
  const entry = action(id);
  if (!entry || !isOffered(entry, ctx)) return undefined;
  return entry.run(ctx);
}
