// actions.js: the one entry table of the page's popup menus. The box menu (#ctx, after a drag on
// the waterfall), a running job's menu (#ctx-outline: its outline's right-click, a long press on
// its tag, or its row's ⋯) and a job row's audio buttons (agent.js) all draw from it, so a feature adds a row
// here and not a line to each menu. This header says which entries there are, where they sit,
// when they are offered and what each does (its `run` calls menu.js's Menu, the host).
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
//               of a section: the Transcribe twins under the demodulators, a job's band steps
//               under Next); a string, or {box, job} per surface (sectionOf)
//     elsewhere(ctx) -> bool   true where the entry's data says the selection is not where it
//               fits: it is drawn after its group's fitting entries, under an `elsewhere`
//               heading, fully usable (the decoder rows outside their ranges or widths)
//     keep      true when the menu stays open while it runs (the fitted entries: the fit
//               closes it); every other entry closes the menu first
//     items(ctx) -> [{key, label, title, run(ctx)}]   entries drawn after it, one per element (a
//               plan's named groups)
//     sub(ctx) -> [{key, label, run(ctx)}]   the entry opens these as its submenu (`.ctx-sub
//               [data-for=<id>]`), each `.ctx-item[data-mode=<key>]`; an entry whose sub is
//               empty is not offered
//     run(ctx)  what choosing it does
//   GROUPS   per surface, its groups in menu order: [{id, label, direct, prefix}]. A group's
//            only line stands in the first level reading `<label> › <its label>` when the group
//            has `prefix` (Decode › RDS) and its label does not already start so, else its own
//            label. A group's entries open
//            as a submenu at its opener's side (`.ctx-item[data-group=<id>]` reading `label`,
//            its panel `.ctx-sub[data-group=<id>]`); a `direct` group's entries, or a group's
//            only line, stand in the first level instead, and a group with no entry offered is
//            left out
//   action(id) -> the entry, or undefined
//   groupOf(entry, surface) -> the entry's group id on that surface
//   sectionOf(entry, surface) -> the entry's section heading on that surface, or undefined
//   offered(surface, ctx) -> the entries offered on a surface for ctx, in table order
//   runAction(id, ctx)  runs an entry that applies to ctx (agent.js's row buttons)
//   rowRanges(row), rowMeets(row, sel), rowFits(id, row, sel)   what the box's decoder entries
//            read from modes_list (see Offered)
//   jobHandler(job, row) -> the entry of `row.handlers` the job runs (its `handler`), or
//            undefined; jobOutputs(job, row) -> the stream kinds the job emits: that handler's
//            `outputs`, else the row's own (a job with no `handler`), [] without a row. A mode's
//            `outputs` are the union over its ready handlers, so a job is judged by its own
//            handler's (the packets entry here, agent.js's audio rule)
//
// Offered: nothing the node would accept is hidden. Every decoder and demodulator entry is on
// the box menu; a decoder whose row does not fit the selection (rowFits: the width the entry
// asks for on it, jobTuning, at least the row's `min_hz`, else its `bandwidth_hz`, and the box
// meeting one of its `range_hz` spans; a row without ranges fits anywhere; a row the page has
// not read yet, everywhere) is drawn under its group's `elsewhere` heading. A row's ranges say
// where identify tries it, not where it may start. Decode across this band's rows are filtered
// by their width and ranges; the job's own entries by its kind and graph (`applies`).
//
// A ctx is the menu's own (menu.js builds it):
//   box: {surface: "box", host, sel, plan, sets, rows, src}: `host` the Menu (whose methods the
//        runs call), `sel` the selection with its span clamped ({sourceId, centerHz, bandwidthHz,
//        startNs, endNs, clamped}), `plan` the plan in view (an id or null), `sets` set_list's
//        ready sets, `rows` modes_list's rows (null until read), `src` the shown source's
//        source_list record (or undefined)
//   job: {surface: "job", host, job, sel, mute, audio, running, playing, role, play,
//        transcribe}: `job` a job_list record, `sel` its band as a selection on its source (its
//        span the newest painted on the shown source, else null; null without a band), `mute`
//        app.js muteFor(job) ({muted, title, run} or null), `packets` app.js packetsFor(job) (the
//        running packets job taking it, or null), `rows` as in a box's ctx, `playing` whether the page plays it, `audio`
//        whether the job has an audio stream to play (not an attached job), `running` whether it
//        runs, `role` the voice to play and transcribe (null: the main stream), `play(job,
//        {role})` and `transcribe(job, {role})` the row's ▶ and Transcribe callbacks (app.js)
//
// What each entry does (`.ctx-item[data-action]`; its run calls menu.js's Menu). The box's,
// on the selection (`sel`) on the source the waterfall shows:
//   classify   What is this?                  notes "classify: running…", then classify
//                                             {selection}; the layer-1 labels are drawn with
//                                             ov.label at the selection's centre (stacked
//                                             LABEL_STEP_PX apart, or LABEL_FALLBACK_STEP_HZ
//                                             while the band is unknown), the top layer-2
//                                             label one line under the last of them; every
//                                             label is listed in the transcript as
//                                             `L<layer> <name> <confidence>`, then each
//                                             mode decode-to-identify considered as
//                                             `tried <mode> <score> <outcome>`, then the
//                                             register's answer as units.js registerLines
//                                             words it (the verdict with the data's date,
//                                             the band, the top candidates, what the first
//                                             belongs to; an older node without `register`:
//                                             the registerText line), and its `origins`
//                                             light the map (onOrigins)
//   identify   Identify (thorough)            job_start kind=identify, bandwidth_hz at least
//                                             IDENTIFY_MIN_BANDWIDTH_HZ; each `candidate`
//                                             event shows as a ribbon (its `summary`, class
//                                             `candidate`), held by the feed until a poll
//                                             lists the job
//   watch      Watch this band                job_start kind=watch on the selection as is
//   decode_across  Decode across this band…   opens its submenu `.ctx-sub[data-for=
//                                             "decode_across"]` (a click, or the pointer on
//                                             it): one `.ctx-item[data-mode=<id>]` per `ready`
//                                             row of `modes_list` (read once per page, then
//                                             cached) whose band.bandwidth_hz is no wider than
//                                             the selection and whose ranges it meets
//                                             (rowMeets), `Decode <row name> across this
//                                             band`; the entry is left out when none fits.
//                                             Choosing a row: job_start kind=watch on the
//                                             selection as is, then graph_get and graph_put of
//                                             its `detect` node with modes [<id>], persist_ms
//                                             DECODE_ACROSS_PERSIST_MS and width_hz
//                                             DECODE_ACROSS_WIDTH_FIT × the row's width
//                                             ([0.5, 2]: the node's span fit, so the detector's
//                                             ceiling admits every signal the watch would
//                                             decode; plan 31 spec 4.7: two ops, one click)
//   aprs_decode  APRS                        job_start kind=aprs_decode, deviation_hz null
//   pocsag_decode  POCSAG                    job_start kind=pocsag_decode, deviation_hz null,
//                                             bandwidth_hz at least POCSAG_MIN_BANDWIDTH_HZ
//   rds_decode  RDS (FM station data)        job_start kind=rds_decode, deviation_hz null,
//                                             bandwidth_hz at least WFM_MIN_BANDWIDTH_HZ
//   adsb_decode  ADS-B (dump1090)            job_start kind=adsb_decode, deviation_hz null,
//                                             bandwidth_hz always ADSB_BANDWIDTH_HZ (the node
//                                             refuses under 2 MHz; a narrower source answers
//                                             out_of_range, shown in the transcript)
//   acars_decode  ACARS                      job_start kind=acars_decode, deviation_hz null,
//                                             bandwidth_hz at least ACARS_MIN_BANDWIDTH_HZ
//   video      Video                          job_start kind=video_decode at the box's centre
//                                             and width as drawn (never fitted, never a grid);
//                                             greyed (`disabled`: aria-disabled, its title the lease's
//                                             words, videoRefusal) when the shown source's
//                                             max_bandwidth_hz is under the box's width
//   fm_demod   NBFM audio                     job_start kind=fm_demod, deviation_hz 5000
//                                             (fitted first: menu.js Fitting)
//   wfm_demod  FM broadcast audio             job_start kind=wfm_demod, deviation_hz 75000,
//                                             bandwidth_hz at least WFM_MIN_BANDWIDTH_HZ
//                                             (fitted first)
//   am_demod   AM                             job_start kind=am_demod, deviation_hz null
//                                             (fitted first)
//   ssb_usb    SSB (USB)                      job_start kind=ssb_demod, sideband usb, at the
//                                             dial: center_hz = selection centre − 1500,
//                                             bandwidth_hz 6000 (ruling R6; fitted first)
//   ssb_lsb    SSB (LSB)                      likewise, sideband lsb, centre + 1500
//   fm_transcribe   NBFM voice                 the job_start of its twin (fm_demod,
//   wfm_transcribe  FM broadcast               wfm_demod, am_demod, ssb_usb, ssb_lsb: same
//   am_transcribe   AM                         kind, deviation, sideband and tuning), then on
//   ssb_usb_transcribe  SSB (USB)              success attachOp(<its job>): a transcribe job
//   ssb_lsb_transcribe  SSB (LSB)              attached to that demodulator (the twin is
//                                             fitted first, as the twin)
//   measure    Measure job                    job_start kind=measure, deviation_hz null
//                                             (every job_start sends sideband: null unless SSB)
//   scan       Scan this span                 (starts the default look, 20 ms x 1; the job's
//                                             row offers the others: agent.js SCAN_LOOKS) wf.release() (the
//                                             waterfall lets go; the node decides: on a source
//                                             another job holds the scan waits, its row saying
//                                             so), job_start
//                                             kind=scan on the selection's centre and
//                                             bandwidth, then wf.resume() (scan mode when the
//                                             scan runs, else the spectrum job again); in
//                                             sweep mode (a KiwiSDR's tab) it re-aims the
//                                             tab's own scan instead (wf.requestView: a
//                                             job_retune), starting none
//   plan       Start plan…                    shown only when the view's plan policy names a plan
//                                             (mountMenu's `plan()`); puts `plan start <plan>
//                                             fm_demod transcribe` in the command line
//                                             (`command(text)`) and focuses it. After it, one
//                                             `.ctx-item[data-plan-group=<id>]` per named group of
//                                             that plan (plans.js planGroups) but one of data
//                                             channels only (planNoVoice), `Start plan ›
//                                             <group name>`: puts `plan start <plan> fm_demod
//                                             transcribe group <id>` there the same way
//   set        Start set…                     shown only when `sets()` (mountMenu's: set_list's
//                                             entries, read with the jobs) has a `ready` set;
//                                             opens #ctx-set where the menu stood (plan 31
//                                             spec 5.5): `select.set-pick` of the ready sets
//                                             (the first chosen), then per role of the chosen
//                                             set (in its members' order) a
//                                             `select[data-role=<role>]` of every source of
//                                             `sources()` (source_list's records) plus
//                                             "pick" (value "", sent as null), the first
//                                             role on the shown source, the rest "pick";
//                                             Start (`button.start`) sends set_start {set,
//                                             radios, members: null, replace: []} (its line
//                                             counts started, existing, refused and warnings;
//                                             each refusal and warning is noted), Cancel,
//                                             Escape or a press outside close it
//   iq_read    Save selection as SigMF        iq_read {selection}; the transcript shows the path
//   ask        Send to agent                  /ask "Look at <c> Hz, <bw> Hz wide, on <source>,
//                                             from <start> to <end>: what is it?" (naming
//                                             the radio makes it the agent's default)
// A job's (menu.js #ctx-outline, the job's menu), on the job's own source and tuning; Fit and the
// width presets only for a job whose kind is in FIT_KINDS (the node would refuse a decoder's or
// a passthrough's narrowing as often as not), Stop always:
//   fit    Fit to signal   fitJob on the job's source's record (sourceRecord; a wide box or its
//                          own band: see fitJob), then job_retune to fitTuning(...), never wider
//                          than the job except by a plan's width (capped by the capture), its
//                          centre kept in the job's band; a signal that reaches past the job's
//                          band or fills it keeps the job as it is (no retune unless a plan
//                          channel widens it) and the note reads `(<label>, kept: the signal
//                          reaches past the job's band)` or `(<label>, kept: the signal fills the
//                          band)` (an ssb_demod keeps its width and moves its dial,
//                          the sideband read from its graph's ssb_demod node), and once the
//                          retune answered the note `fitted <kind> to <band> (<label>, <rule>)`; no
//                          signal: the note `no signal found in the band; left as it is` (with
//                          the capture-centre clause of menu.js Fitting when the band holds the centre of a
//                          local radio's capture: `no signal found in the band (the capture
//                          centre <mhz> MHz is in the band: the 50 kHz around it are not
//                          measured); left as it is`) and nothing changes. Shares the fit guard
//                          (menu.js Fitting) (entries aria-disabled meanwhile, closing the menu cancels it:
//                          `<kind>: fit cancelled`)
//   w2500 2.5 kHz, w5000 5 kHz, w6250 6.25 kHz, w12500 12.5 kHz, w25000 25 kHz
//                          job_retune to that bandwidth_hz about the same centre_hz
//   denoise Denoise on|off graph_put of the job's own graph with `denoise` flipped on its
//                          demodulator node (nbfm_demod, am_demod, ssb_demod: DENOISE_KINDS;
//                          left out for the others), the entry reading the state it would
//                          switch to; the note `<kind>: denoise on|off`
//   stereo Stereo on|off   the same for `stereo` on a wfm_demod job's wfm_demod node (two
//                          channels, L and R once the pilot locks; left out for the other
//                          kinds); the note `wfm_demod: stereo on|off`
//   mute   Mute in mix     graph_put of the covering mix's own graph with this channel added to
//                          its `muted` (mix.js mixCovering, muteOp; the entry reads `Unmute in
//                          mix` for a muted one, left out when no running mix covers the job's
//                          source); the note `<kind>: muted in mix` / `unmuted in mix`
//   listen Listen          (Next) the row's ▶: play(job, {role}) (app.js toggleAudio), reading
//                          `Stop listening` while the page plays any of its streams, and then
//                          stopping it (app.js); a job with an audio stream, not an
//                          attached one
//   transcribe Transcribe  (Next) the row's Transcribe: transcribe(job, {role}), job_attach
//                          kind=transcribe; a running job with audio
//   packets Capture packets  (Next, after Transcribe) job_attach kind=packets on a running job
//                          whose own outputs list `packet` (jobOutputs: the `outputs` of the
//                          handler it runs, from its modes_list row's `handlers`, never the
//                          mode's union; the row's `outputs` for a job with no `handler`:
//                          aprs_decode's built-in and the frame rows), so the node writes its
//                          packets to a pcapng capture (Explore lists it); reads `Stop packet
//                          capture` while a `packets` job takes its input (job_stop on that
//                          job)
//   classify, identify, measure, watch  (Next, after Listen and Transcribe under the heading
//                          `On its band`, with no heading when those two are not offered) the
//                          box entries' runs on the job's band (`sel`,
//                          menu.js jobCtx), for a job with a band (not a mix); What is this? only
//                          on the shown source, over its newest JOB_SPAN_NS
//   stop   Stop            job_stop

import { DENOISE_KINDS, FIT_KINDS, jobTuning, videoRefusal } from "./menu.js";

// The width presets of a running demodulator's menu, in Hz.
const WIDTHS = [2500, 5000, 6250, 12500, 25000];

export const GROUPS = {
  box: [
    { id: "what", label: "What is this?", direct: true },
    { id: "listen", label: "Listen", prefix: true },
    { id: "decode", label: "Decode", prefix: true },
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

// A modes_list row's `band.range_hz` as a list of [lo, hi] spans (one span or a list of them),
// or null for anywhere.
export function rowRanges(row) {
  const r = row && row.band ? row.band.range_hz : null;
  if (!Array.isArray(r) || r.length === 0) return null;
  return typeof r[0] === "number" ? [r] : r;
}

// Whether selection `sel`'s band ({centerHz, bandwidthHz}) meets one of `row`'s ranges (a row
// without ranges: anywhere).
export function rowMeets(row, sel) {
  const ranges = rowRanges(row);
  if (ranges === null) return true;
  const lo = sel.centerHz - sel.bandwidthHz / 2;
  const hi = sel.centerHz + sel.bandwidthHz / 2;
  return ranges.some(([a, b]) => lo <= b && hi >= a);
}

// Whether an entry `id` whose job is a modes_list row `row` fits selection `sel`: the width the
// entry asks for on it (menu.js jobTuning: the box as drawn, or widened to what the entry
// needs) is at least the row's narrowest (`min_hz`, else `bandwidth_hz`, as the node's
// Band::fits), and the selection meets the row's ranges. A row the page has not read fits.
export function rowFits(id, row, sel) {
  if (!row || !row.band) return true;
  const need = row.band.min_hz ?? row.band.bandwidth_hz;
  const asked = jobTuning(id, sel).bandwidthHz;
  return !(typeof need === "number" && asked < need) && rowMeets(row, sel);
}

// The modes_list row of job kind `kind` in ctx (undefined before the rows are read).
const rowOf = (c, kind) => (c.rows || []).find((r) => r.id === kind);

// The handler job `job` (a job_list record) runs, from its mode row `row` (modes_list's, or
// modes_show's as subjects.js keeps it, with `handlers`): the entry whose `id` is the job's
// `handler`; undefined for a job with no `handler`, a row not read yet, or a handler the row
// does not list.
export function jobHandler(job, row) {
  if (!job || job.handler == null || !row) return undefined;
  return (row.handlers || []).find((h) => h.id === job.handler);
}

// The stream kinds `job` emits by its mode row `row`: its own handler's `outputs`
// (jobHandler), not the mode's, which are the union over the mode's ready handlers; the
// mode's `outputs` only for a job with no `handler` or one the row does not list; [] without a
// row. The packets entry and agent.js's audio rule read it.
export function jobOutputs(job, row) {
  if (!row) return [];
  return jobHandler(job, row)?.outputs ?? row.outputs ?? [];
}

// A job's band as usable for its next steps: a tuning with a width (a mix has none).
const banded = (c) => {
  const t = c.job.tuning;
  return c.job.kind !== "mix" && !!t && Number.isFinite(t.center_hz)
    && Number.isFinite(t.bandwidth_hz) && t.bandwidth_hz > 0 && !!c.sel;
};
const start = (id) => (c) => c.host.startJob(id, c.sel);
const fitted = (id) => (c) => c.host.fitAndStart(id, c.sel);
const job = (kind, deviation = null, sideband = null) => ({ kind, deviation, sideband });

// A decoder entry: a job started on the selection as its tuning says (menu.js jobTuning).
// Always offered (the node starts a row anywhere); `elsewhere` where its row says the selection
// is not where it fits (rowFits).
const decoder = (id, label, kind = id) => ({
  id, label, group: "decode", on: ["box"], job: job(kind), run: start(id),
  elsewhere: (c) => !rowFits(id, rowOf(c, kind), c.sel),
});
// A demodulator entry: fitted to the signal first. Always offered: a demodulator has no row to
// read a width from.
const demod = (id, label, j) => ({
  id, label, group: "listen", on: ["box"], job: j, keep: true, run: fitted(id),
});
// A demodulator's Transcribe twin: the twin's job, then a transcriber attached to it.
const twin = (id, label, of) => ({
  id, label, group: "listen", section: "Transcribe", on: ["box"], twin: of, keep: true,
  run: fitted(id),
});

export const ACTIONS = [
  // A job's first next steps (the row's ▶ and Transcribe), before its band steps' heading.
  {
    id: "listen", group: "next", on: ["job"],
    label: (c) => (c.playing ? "Stop listening" : "Listen"),
    applies: (c) => c.audio,
    run: (c) => c.play(c.job, { role: c.role }),
  },
  {
    id: "transcribe", label: "Transcribe", group: "next", on: ["job"],
    applies: (c) => c.audio && c.running,
    run: (c) => c.transcribe(c.job, { role: c.role }),
  },
  // On a job: its band over the newest span of its source, which the page has only for the
  // shown source (menu.js jobCtx).
  {
    id: "classify", label: "What is this?", group: { box: "what", job: "next" }, on: ["box", "job"],
    section: { job: "On its band" },
    applies: (c) => c.surface === "box" || (banded(c) && c.sel.startNs !== null),
    run: (c) => c.host.classify(c.sel),
  },
  {
    id: "packets", group: "next", on: ["job"],
    label: (c) => (c.packets ? "Stop packet capture" : "Capture packets"),
    applies: (c) => c.running && !c.job.input && jobOutputs(c.job, rowOf(c, c.job.kind)).includes("packet"),
    run: (c) => c.host.capturePackets(c.job, c.packets),
  },
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
    // The ready rows no wider than the selection whose ranges it meets.
    sub: (c) =>
      (c.rows || [])
        .filter((r) => r.state === "ready" && typeof r.band?.bandwidth_hz === "number"
          && r.band.bandwidth_hz <= c.sel.bandwidthHz && rowMeets(r, c.sel))
        .map((row) => ({
          key: row.id,
          label: `Decode ${row.name} across this band`,
          run: (k) => k.host.decodeAcross(row, k.sel),
        })),
  },
  // On a job (its next steps): the same run on the job's band as the selection.
  {
    id: "identify", label: "Identify (thorough)", group: { box: "measure", job: "next" },
    on: ["box", "job"], job: job("identify"), run: start("identify"), section: { job: "On its band" }, applies: (c) => c.surface === "box" || banded(c),
  },
  {
    id: "watch", label: "Watch this band", group: { box: "measure", job: "next" },
    on: ["box", "job"], job: job("watch"), run: start("watch"), section: { job: "On its band" }, applies: (c) => c.surface === "box" || banded(c),
  },
  {
    id: "measure", label: "Measure job", group: { box: "measure", job: "next" },
    on: ["box", "job"], job: job("measure"), run: start("measure"), section: { job: "On its band" }, applies: (c) => c.surface === "box" || banded(c),
  },
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
  { id: "stop", label: "Stop", group: "stop", on: ["job"], run: (c) => c.host.stopJob(c.job) },
];

const BY_ID = new Map(ACTIONS.map((a) => [a.id, a]));

export function action(id) {
  return BY_ID.get(id);
}

export function groupOf(entry, surface) {
  return typeof entry.group === "string" ? entry.group : entry.group[surface];
}

export function sectionOf(entry, surface) {
  return typeof entry.section === "object" && entry.section !== null
    ? entry.section[surface]
    : entry.section;
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
