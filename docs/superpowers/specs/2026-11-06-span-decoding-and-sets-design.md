# Span decoding and named sets

Design spec, 2026-11-06. Two gists from TODO become one design: "Whole-span POCSAG and APRS as a
`watch` with `decode_on: capture`" (decoder wave 2) and the "airband" set from the VDL2 gist (a
named set that starts ACARS + VDL2 on one radio and ADS-B on another, as `plan_start` does for a
channel plan). It also answers "Span decoders and trunking" (the span half), "named channel groups"
(the set's `members` are the analogue) and the recipes gist (a set is its declarative core; see 5.6).
Builds on the watch (`crates/zeta-core/src/node/watch.rs`, plan 17 spec section 6 and plan 19.1 spec
5.5), `plan_start` (`ops.rs`), the lease manager (`lease.rs`), the mode registry (`modes/`) and the
page's jobs pane (`crates/zeta-ui/web/agent.js`). A reviewer read it as the builder before hand-over;
its questions are folded in, settled in section 9 or left to the operator in section 8.

## 1. Intent and why

A pager band is many channels. In AU, POCSAG sits across 148-149 MHz on several carriers (Telstra at
148.6375 MHz among them); in Europe it is 466 MHz, in the US 929-932 MHz. Today the node decodes one
channel per hand-started `pocsag_decode`, so "decode the pagers" is a hunt: measure the band, read the
register, start a job per carrier, and the carrier that comes up an hour later is missed. OnAir and
OpenWebRX+ take the whole span and decode every channel in it at once; that is what the operator
wants, and VDL2 beside ACARS (130-137 MHz) and later other data bands are the same shape.

The node already has the pieces. The `watch` holds a band, runs `fft → detect` over it, identifies
each new signal with an owned `identify` run and keeps the proved decoder, inside its own capture under
`decode_on: capture`. What it lacks for a pager band is the knowledge that every signal there is one
mode (so identify's serial dwell per signal is waste and would miss a one-second burst), a cap and a
cooldown so a busy band does not churn jobs, and a hold long enough for a channel that pages every few
minutes. The operator's rule decides the shape: **a watch that decodes, not a span-decoder per mode.**
One `watch` kind, with a `modes` list on its policy, is the span decoder for any mode row; nothing in
it knows what POCSAG is.

The second half is the operator's "airband": on the field host, ACARS and VDL2 fit one 8 MHz capture
of the B210 around 133.5 MHz, and ADS-B at 1090 MHz has to go on the HackRF because the B210's two
RX channels share one LO. Today that is seven `job_start`s, one more, and one on the other radio,
reproduced from `RECIPES.md` each time; the node cannot tell "the airband set" from nine unrelated
jobs. `plan_start` already starts many jobs on one source as one unit under one lease decision: a
**named set** is that across several sources, a record the node stores like a mode row or a channel
plan, started, listed and stopped by name.

## 2. Goals, non-goals, anti-goals

### Goals

- `job start --kind watch` over a band, then a `modes` list on its `detect` node (or a set that
  carries it), decodes every channel of those modes the detector finds in the band: one ordinary
  decoder job per signal, started within a couple of seconds of the signal's first appearance,
  without an identify run when one listed mode fits, held through the channel's quiet spells, stopped
  when the channel is gone for good, with a cap and a per-channel cooldown.
- A span watch obeys every rule the sonde watch obeys: it holds a rigid lease on its band and never
  retunes the radio by itself; it ignores a signal another job already holds; it owns what it starts
  and stops it when it stops; a `graph_put` or a node restart re-adopts its decoders.
- A set (`sets/*.json` built in, plus the operator's directory) names members across radio roles;
  `set_start` maps roles to sources and starts every member under one lease decision per source,
  `set_list` names the sets and what runs, `set_stop` stops one instance's jobs and nothing else. The
  built-in `airband` set is the operator's recipe; `pagers_au` is the span watch as a set.
- Both obey the field craft: a `conflict` names the role, the source and the holders and starts
  nothing; nothing is stopped or replaced unless the request named it; the agent's `allow` gate covers
  the new ops exactly as it covers `plan_start` and `job_stop`.
- The page folds a watch's decoders under the watch's row and a set's jobs under the set's row, as it
  folds a plan's channels, and the Subjects tab shows one table for a watch whose decoders share a
  `subject` key.

### Non-goals

- Trunking (P25, DMR Tier III, MPT1327): a follower that starts voice jobs from control-channel
  events is the other half of the "Span decoders and trunking" bullet and stays in TODO. The span
  watch's "known channels" seam (4.3) is where a control-channel follower would plug in later.
- A `vdl2_decode` row. The sibling spec `2026-11-06-aircraft-links-design.md` writes it (and hands the
  airband set to this one); the `airband` set ships with the VDL2 member and reports it `refused`
  (`unavailable`) until that row is on the host (9.9).
- Recipes as ordered arbitrary ops with preconditions and verify steps (`recipe_start`): a set is the
  part of that the node can check before anything happens (5.6). The recipes gist stays, narrowed.
- The generic `sink` param on the watch: `sondehub` stays as it is and applies to a span watch's
  decoders as to any watch's; an APRS-IS sink is that gist's work.
- The "decoder placed on a free source keeps running if the watch never recovers" bug: unchanged
  here; `decode_on: free` is pointless for a span watch (every decoder wants the watch's own capture)
  and section 4.5 says so, but the policy is honoured as today.
- A `params` field on `job_start` (9.1).
- Plan groups (`lower 40 voice`): a set's `members` filter is the cross-radio analogue; the one-field
  group on `plan_start` stays its own gist.

### Anti-goals

- A span-decoder stage or job kind per mode (`pocsag_span`, `vdl2_span`). One mechanism, the watch,
  takes any row that has a `band.bandwidth_hz`.
- A watch that retunes, widens or pins the radio to fit what it finds. It takes the band it was given;
  a signal at the band's edge is the operator's to widen for.
- A decoder centred on a raster rather than on the signal. The detector's centroid is the decoder's
  centre (the field craft's "no channel-grid assumptions"); a known channel from the register or the
  row's `centers_hz` ranks and holds a signal, it never moves it.
- A set that stops or replaces anything it did not start. A set file never carries `replace` ids or
  source ids; both come from the request.
- A set that is a script: no ordering, no waits, no conditionals in the file. Every member is one op
  the node already has, so the whole set can be checked before the first start.

## 3. Constraints

- **The watch today** (`watch.rs`): its policy is read from the `detect` node's params, which the stage
  ignores (`decode_on`, `prefer`, `sondehub`, `on_unknown`, `capture`, and the detector's own
  `gone_ms`); `Policy::of` reads them, `DetectParams` (`zeta-dsp/src/detect.rs`) declares them so
  `graph::validate` checks them. The loop ticks every 50 ms: `signals` (detect's `new`/`gone` inside the
  watch's band only), `verdict`, `decoders` (events noted; `lost` after `SILENCE_MS` 120 s of silence
  once `gone`, or `not seen again` after a re-adoption's `gone_ms`), `sweep`, `next` (one identify at
  a time, best score first; a signal under another job's band on the watch's source is dropped,
  `held_elsewhere`). Kept decoders are ordinary records (no `owner`; they survive in the history and
  `job_list`); identify runs and their trial children are owned and ephemeral. Teardown: `job_stop`, a
  `replace` or a failure of the loop stops every decoder; a port fault or the node's shutdown leaves them
  to the next run's re-adoption. `RETRY_MS` 60 s × `RETRY_MAX` 3 for unknowns; the test-fixtures build
  can override the timings (`test_timing`).
- **Identify** (`identify.rs`): measures `IDENTIFY_MEASURE_MS` (1 s) after waiting for the ring, scores
  the rows, tries each plausible row as a child for its `identify.dwell_ms` (5 s for POCSAG and APRS),
  one at a time. Ten pager channels would take over a minute to get through once, and a burst shorter
  than the dwell is never proved.
- **Detect** (`detect.rs`): median-based occupancy (`threshold_db`, `energy_db`), `persist_ms` (3000 by
  default: a run must persist that long to be `new`), `width_hz` `[min, max]`, `gone_ms`, DC and edge
  masks, `spurs_hz`; at most `SIGNAL_EVENTS_PER_S` (10) events a second per stage. The watch's FFT is
  sized for about 1.5 kHz bins (`WATCH_BIN_TARGET_HZ`), 4096 bins at 6-8 MHz.
- **Mode rows**: `band.bandwidth_hz` (the channel width), `band.centers_hz` (well-known centres, empty
  for "anywhere"), `band.range_hz`, `band.min_hz`; `identify.{proof, min_events, dwell_ms}`; an
  optional `subject` block (key, name, columns) read by the page's Subjects store. Rows load embedded
  first, then `--modes-dir` (an operator row replaces a built-in by id; a broken row is kept `broken`
  with its reason and never stops the node). `JobKind::Mode` is any id matching `^[a-z][a-z0-9_]*$`.
- **Leases** (`lease.rs`): one gate per source held across a tune; a rigid request inside the current
  capture is granted without a tune; outside it, a tune only when nothing rigid holds the source, else
  `Conflict {holders, current, options}`; a pin is rigid but not a holder and is never in `replace`;
  elastic jobs never block. `free_sources` lists sources with no rigid holder, no pin and no fault that
  cover a band. No operation holds two sources' gates today.
- **`plan_start`** (`ops.rs`): validate everything first (nothing has happened), find `existing`
  identical rigid jobs, check the attach stage's readiness, check the bounding band against the source,
  stop `replace` under the gate, one lease decision for the bounding band, start each channel inside the
  capture (a failure is that channel's `refused`), attach, answer `capture`, `started`, `existing`,
  `refused`, `warnings`. Its tests (`crates/zeta-core/tests/plan_start.rs`) use a fake `Radio` that
  counts tunes and can be slow.
- **Ops and records**: every op field is required and unknown fields are rejected; a new field on an
  existing op breaks every caller (the page's menu, `RECIPES.md`, the field host's scripts, an older
  `zeta --node` client). `JobRecord` is `deny_unknown_fields` with a format version; `StreamInfo` too
  (plan 27 added no field for that reason). AGENTS.md and `skills/zetasdr/SKILL.md` are generated from
  `ops.rs` doc text and `crates/zeta/src/agents_md.rs`; CI fails on drift.
- **The agent's gate** (`zeta-agent/src/agent.rs`): `Allow {stop_jobs, replace, delete_captures}`;
  `GATED_REPLACE_OPS` lists the ops whose non-empty `replace` is refused without `allow.replace`;
  `job_stop` passes only for a job the run started (plan 23 spec 4.6).
- **The page**: `agent.js` hides owned jobs and folds a plan's channels under a group row
  (`foldPlan`, `JOBS_FOLD_MIN`); plan 27 gave it one event feed per source, so a watch's own events
  (`decoding`, `lost`) reach it with their `job`; `subjects.js` keys its store by (job, key) and draws
  one section per job; the selection menu (`menu.js`) has "Watch this band" and "Start plan…"; "panes
  earn a tab" (a new view goes behind a tab or a menu, never into the waterfall).
- **The e2e suite** runs about 474 s of a 690 s deadline (`crates/zeta/tests/ui.rs` `DEADLINE`), with
  the traps of the "UI e2e gotchas" note; its node has the `tone` and `cb` file fixtures and the
  POCSAG and APRS fixtures the ribbon scenarios use.
- **The register**: `knowledge_lookup` answers `Allocation`s with `center_hz`, `bandwidth_hz`,
  `emission`/`emission_decoded`, `service`, `licensee`, `site`, `distance_km`, `kind`
  (`plan`/`assignment`); the ACMA provider is a remote service that may be unreachable, which is logged,
  never fatal. The station gives the region (`DEFAULT_REGION` without one).
- **Radios on the field host**: the B210's two RX channels share one LO, so 130 MHz and 1090 MHz
  cannot be captured at once; the HackRF (20 MHz max) holds ADS-B. Neither id may be baked into a
  tracked file as a real host name would be; `b210` and `hackrf` are the roles `RECIPES.md` already uses.

## 4. The span watch

### 4.1 Shape

No new job kind and no new op. A `watch` gains policy on its `detect` node, read by `Policy::of` and
declared in `DetectParams` like the rest (every field defaults so an existing watch's stored graph
loads unchanged; the e2e scenario's `graph_put` of a `detect` node with the old fields still
validates):

- `modes`: `null` (today's watch: identify decides) or a non-empty list of mode row ids. Each must be
  a loaded row (`broken` or `unavailable` rows are refused by `graph_put` naming `modes[<i>]` and the
  row's state, as `prefer[<i>]` names an unknown source today) with a graph or program decoder and a
  `band.bandwidth_hz` no wider than the watch's band.
- `max_decoders` (16): kept decoders at once, counting those placed on free sources. The constant the
  sonde watch never reaches becomes a parameter; a watch with `decode_on: free` is bounded by it too.
- `hold_ms` (120 000, today's `SILENCE_MS`): how long a kept decoder whose signal is `gone` may stay
  silent before it is `lost`. A pager channel pages every few minutes; `pagers_au` sets 600 000.
- `cooldown_ms` (30 000): after a decoder on a channel is `lost` or `unproven`, no new decoder starts
  on a signal overlapping that channel for this long. Today's watch has no churn case (a sonde does not
  flap) so the default is small; `pagers_au` sets 120 000.
- `confirm_ms` (0, off): with `modes`, a decoder started without an identify run must publish an
  event within this long while its signal is present, else it is stopped `unproven` and its channel
  cooled; `0` keeps it as long as its signal is there. `pagers_au` sets 180 000 (9.4).
- `seed_known` (false): with `modes`, start a decoder on every known channel (4.3) in the band at
  once, up to the cap, without waiting for the detector.

`decode_on`, `prefer`, `sondehub`, `on_unknown` and `capture` keep their meaning. `on_unknown:
capture` with `modes` captures a signal that fits no listed row's width (4.2), which is the same rule
as today's ("the first identify ended unknown") applied to the width test.

The operator or the agent sets the policy as the two-radio sonde form does today: `job_start` the
watch on the band, `graph_get`, set `modes` (and the detector's `persist_ms`, `width_hz`) on the
`detect` node, `graph_put`. The restart re-adopts nothing yet (the watch just started) and runs with
the new policy. A set carries the params so `set_start pagers_au` is one op (5.3); the page's
"Decode <mode> across this band" entry does the two steps itself (4.7).

### 4.2 Choosing what to start

For a `new` signal inside the band that no other job holds (`held_elsewhere` unchanged) and that is
not on a cooled channel:

1. **Fit.** The listed rows whose `band.bandwidth_hz` the signal's measured width fits (half to one
   and a half times the row's width, the test `identify_band` applies today) and that fit the watch's
   band. None: the signal is `unknown` at once (event `unknown`, `summary` `"unknown at <MHz>: no
   listed mode fits <width> kHz"`, no identify run, retried after `RETRY_MS` while it persists as
   today, captured once under `on_unknown: capture`).
2. **One fits:** start that row's decoder directly on the signal's centroid with the row's
   `bandwidth_hz`, inside the watch's capture (`start_on`, the path `decode_on: free` uses for the free
   source, aimed at the watch's own source and no identify child), as a kept decoder. No proof: the
   decoder is its own identify; `confirm_ms` is the bound on a false start. Event `decoding` with a new
   field `via: "modes"` (`"identify"` on the other path, `null` where it does not apply, the events'
   convention).
3. **Several fit:** run the identify child as today, restricted to the fitting rows: `identify` gains
   an internal candidate filter the watch hands to its child (a list of row ids `start_identify` passes
   through `Run`), no op change; its verdict places the decoder as today (`via: "identify"`). ACARS (10
   kHz) and VDL2 (25 kHz) in one watch rarely both fit one signal, so the slow path is the exception.

A signal is tracked (`job_context.watch.signals[]`) as today, with `fits: [row ids]` and `waiting`
added. Ranking when a slot frees (4.4) and the one-at-a-time identify queue both use `score_rows`'
score as now (known channels add to it, 4.3).

### 4.3 Known channels

A **known channel** is a centre the watch expected before the detector saw anything: a listed row's
`band.centers_hz` inside the band, and, when the register answers, each `tx` assignment inside the
band whose decoded emission bandwidth fits a listed row's width (the 4.2 fit test on
`emission_decoded.bandwidth_hz`; an assignment with no decodable designator is not a known channel).
The register is consulted once per run at start, for the watch's band and the station's region.
Not through `nearest_allocations`: that call (identify's, per verdict) keeps the `NEAREST_ASSIGNMENTS`
(5) nearest by distance, which a city's pager band exceeds many times over, and the same carrier is
licensed at several sites. `knowledge.rs` gains `band_assignments(region, center_hz, bandwidth_hz)`:
every `tx` assignment in the band from every provider, uncapped, deduplicated by centre (two
assignments within half a listed row's width of each other are one known channel, the nearer site
named), sorted by frequency; the watch's test suite's fake provider answers it. Unreachable, the run
goes on with the rows' centres and `job_context.watch.known.register` says `"unreachable"`.
`job_context.watch.known` lists `[{center_hz, bandwidth_hz, mode, from: "row" | "register", licensee,
site, sites}]` (`sites` the count folded into one centre).

A known channel does three things and nothing else: a signal overlapping it ranks first for a slot
(4.4) and its `decoding` summary names the licensee (`"decoding pocsag_decode at 148.6375 MHz (modes);
register says Telstra, <site>, 3 km"`, the `register_says` text identify already writes); it is held
through quiet like any kept channel (`hold_ms` is the same for all; a known channel is not immortal);
and with `seed_known` its decoder starts at once, the detector's `new` later confirming it (a seeded
decoder whose signal is never seen is `lost` `not seen again` after `hold_ms`, not `gone_ms`: a seeded
channel is a bet the operator placed, and `pagers_au` leaves `seed_known` off, 9.3). It never moves
the decoder's centre: the detector's centroid wins, and a seeded decoder sits on the known centre only
until the first `new` on it, when the watch leaves it where it is (a re-centre would be a restart for a
few hundred hertz; the channel filter covers it).

### 4.4 The cap, the hold and the cooldown

- **Cap.** When `max_decoders` are kept, a signal that would start a decoder is marked `waiting` (event
  `waiting`, once per signal, `summary` `"waiting at <MHz>: <n> of <cap> decoding"`). When a slot frees
  (a `lost`, an `unproven`, a stop of a decoder by the operator), the waiting signal that is present
  and ranks highest takes it: known channels first, then score, then SNR. Nothing is pre-empted: a
  stronger signal does not evict a decoding one (the operator asked for every channel; over the cap
  the watch says so rather than guess which matter).
- **Hold.** A kept decoder whose signal goes `gone` is kept while any event arrives and for `hold_ms`
  of silence after the later of its last event and its start, then `lost` (`why: "gone and silent"`),
  as today with the constant made a parameter. A re-adopted decoder (restart, `graph_put`) is confirmed
  within `max(gone_ms, hold_ms)` instead of `gone_ms`, so a resumed pager channel in a quiet spell is
  not dropped and restarted on its next burst (9.5).
- **Cooldown.** `lost` and `unproven` put the channel (the decoder's centre and width) on
  `job_context.watch.cooled[] = {center_hz, bandwidth_hz, until_ns, why}`; a `new` overlapping it before
  `until_ns` is tracked but not started (and not identified), and the cooldown is cleared when it
  expires or when the watch is stopped. A `new` after the cooldown starts afresh. The `lost` event
  gains `why` (`"gone and silent"`, `"not seen again"`, `"unproven"`), today's summary text made a field.

### 4.5 Placement, leases and the field craft

- The watch holds a rigid lease on its band as today; every decoder it places in the capture takes a
  rigid lease on its channel inside it (`start_on`, inside the capture, no tune). `source_list.holders`
  lists them all, as the seven ACARS jobs are listed today; `jobs_stop` with `source` and `kind` stops
  them like any job, and the watch notes each as gone from `job_list` (a slot frees).
- A hand-started decoder inside the band is never touched: its signal is `held_elsewhere` and skipped.
  A span watch therefore composes with `plan_start` on the same capture (the ACARS channels by plan,
  the rest of 130-137 MHz by watch).
- `decode_on: free` is honoured as today, bounded by the cap. The field craft says what it means for a
  span watch: each decoder would take a free radio for one channel, so leave it `capture`.
- The watch is started inside a pin or on a free band and conflicts as today when neither; it never
  retunes anything after start. Stopping it stops its decoders (today's rule; a decoder the operator
  wants kept is restarted by hand, or the watch is left running). `replace` naming the watch stops it
  and its decoders under the gate as today.
- The agent's rules are unchanged: a `conflict` on the watch's start is reported; the decoders are the
  watch's, not the agent's, so `job_stop` on one is refused without the allowance (plan 23 spec 4.6
  records the watch's id as the agent's when it started it: stopping the watch it started passes).

### 4.6 Events, context and the Subjects seam

- Events (`protocol: "watch"`) gain `via` on `decoding`, `why` on `lost`, and the state `waiting`;
  `unproven` is a `lost` with `why: "unproven"`. `job_context.watch` gains `known`, `cooled`, `cap`
  (`{max, decoding, waiting}`) and `signals[].fits`, `signals[].waiting`; `decoding[]` gains `via`.
- Every decoder's events are its own, on the source's feed with its job, as today. The decoders of
  one watch share a `watch` in the page's eyes only (4.7); the node adds no record field (constraint:
  `JobRecord` strictness and the `--node` client).

### 4.7 The page

- **Menu.** The selection menu gains "Decode <mode> across this band…" beside "Watch this band": a
  submenu of the `ready` rows whose `band.bandwidth_hz` fits the selection (the mode list `modes_list`
  already gives the menu), which starts a `watch` on the selection and `graph_put`s `modes: [<id>]`,
  `persist_ms: 500`, `width_hz` `[0.5×, 1.5×]` of the row's width on its `detect` node. Two ops, one
  click; the transcript line names both.
- **Jobs pane.** A watch's kept decoders fold under the watch's row as a plan's channels fold under
  their group (`foldPlan`'s mechanism with a new key: the watch's id). Membership comes from the
  source's feed (`decoding` and `lost` events carry `job`) seeded from `job_context.watch.decoding` when
  a watch appears in a poll (the plan-27 `seedSquelch` pattern, one read per watch). The group's
  summary line: `"<watch> · pocsag_decode × 6 decoding, 2 waiting"`. A decoder the operator stops from
  the group is a `job_stop` of that job, as today.
- **Waterfall.** Each decoder has its own outline and ribbons on its own channel, as any job: a span
  watch over a pager band reads as a row of outlines inside the watch's band outline, with `signal`
  marks where the detector sees something not yet decoded. Nothing new.
- **Subjects.** Decoders of one watch whose row has a `subject` block merge into one section titled by
  the watch, keyed by (watch, key) instead of (job, key), with a `Channel` column (the decoder's
  centre in MHz) first after the key; an APRS or a future VDL2 span is one table of stations or
  aircraft across the band. A watch whose rows have no `subject` (POCSAG today) adds no section (9.6).
  The Subjects store already drops a section when its job goes; a merged section goes with the watch.

## 5. Named sets

### 5.1 The record

A set is one JSON file, `sets/<id>.json`, embedded from the repo's `sets/` directory and read again
from the operator's directory (`--sets-dir`, default `$XDG_CONFIG_HOME/zetasdr/sets`, else
`~/.config/zetasdr/sets`), with the mode registry's rules: an operator file with a built-in's `id`
replaces it, a file that does not parse or validate is kept `broken` with its reason and never stops
the node, ids match `^[a-z][a-z0-9_]*$`.

```json
{
  "id": "airband",
  "name": "Airband data links",
  "notes": "ACARS and VDL2 share one capture around 133.5 MHz; ADS-B needs its own radio at 1090 MHz.",
  "radios": {
    "wide": {"note": "a radio that captures 130.0-137.0 MHz at once (8 MHz)"},
    "second": {"note": "a radio for 1090 MHz with 2.4 MHz"}
  },
  "members": [
    {"id": "acars", "radio": "wide", "op": "plan_start", "plan": "acars_vhf", "channels": null,
     "kind": "acars_decode", "deviation_hz": null, "sideband": null, "attach": null, "language": null},
    {"id": "vdl2", "radio": "wide", "op": "job_start", "kind": "vdl2_decode",
     "center_hz": 136975000, "bandwidth_hz": 25000, "deviation_hz": null, "sideband": null},
    {"id": "adsb", "radio": "second", "op": "job_start", "kind": "adsb_decode",
     "center_hz": 1090000000, "bandwidth_hz": 2400000, "deviation_hz": null, "sideband": null}
  ]
}
```

- `radios`: the roles, each with a `note` for the page and the agent. No source ids (they come from
  the request; a built-in file cannot know a host's radios and a tracked file never names one).
- `members[]`: `id` (unique in the set), `radio` (a role), `op` (`job_start` or `plan_start`; a
  `watch` member is a `job_start` with `kind: "watch"` and a `params` object for its `detect` node,
  applied by the same `graph_put` path the menu uses, 4.1), and that op's fields except `source`,
  `elastic` (always false) and `replace` (never in a file). `attach` is allowed on a `plan_start` member
  (a CB set with transcribers is a one-file recipe for the daily setup).
- Validation at load: every member's op fields parse as that op would (the same `check_start_params`
  the ops run); a member's `kind` need not be `ready` at load (a set may name a row this host lacks;
  that is a start-time refusal, 5.3), but it must be a known kind name or a mode id shape.

Two built-in sets ship: `airband` (above) and `pagers_au`: one `watch` member on role `radio` over
148.0-149.0 MHz (`center_hz` 148 500 000, `bandwidth_hz` 1 000 000; the row's 148.6375 MHz is off the
centre) with `params: {modes: ["pocsag_decode"], persist_ms: 500, width_hz: [6000, 20000], gone_ms:
60000, hold_ms: 600000, cooldown_ms: 120000, confirm_ms: 180000}`. The CB recipe in `RECIPES.md` is
left as a recipe (its pin and its mix are not members; 9.8).

### 5.2 The ops

Three ops, in `ops.rs` with doc text that becomes AGENTS.md:

- `set_list {}`: every set, `{id, name, notes, state: ready | broken, reason, radios, members: [{id,
  radio, op, kind, summary}], running: null | {radios: {role: source}, since_ns, members: [{id, job,
  state}]}}`. CLI `zeta sets`.
- `set_start {set, radios, members, replace}`: `radios` maps every role the chosen members use to a
  source id or to `null` (pick one, 5.4); `members` is `null` for all or a list of member ids (a CB
  set's narrowing, a `members: ["acars", "vdl2"]` when the second radio is busy); `replace` is passed to
  every member's start on its source, checked as `job_start` checks it (each id must hold one of the
  sources in use). CLI `zeta set start --set airband --radio wide=b210 --radio second=hackrf`.
- `set_stop {set}`: stop the jobs of the set's running instance (those still running; one that the
  operator stopped by hand is listed `ended`), as `jobs_stop` stops them (jobs on a radio first, then
  attached ones), and drop the instance. CLI `zeta set stop --set airband`.

`set_start` is the whole of `plan_start`'s discipline across sources, in this order:

1. **Check, nothing has happened yet.** Unknown `set` is `unknown` naming the known ids; a `broken` set
   is `other` with its reason; an unknown member id, a role left unmapped, an unknown source, a role
   mapped twice to one source with members whose bounding band the source cannot capture, are
   `unknown`/`other` naming the field (`radios.wide`, `members[1]`). Each member whose `kind` is a mode
   row that is not `ready` is set aside as `refused` with the row's reason (`unavailable: dumpvdl2 not
   found`) and does not block the rest (a host without dumpvdl2 still gets ACARS and ADS-B; 9.9). A
   `plan_start` member whose `attach` stage is not ready starts without the attach and a warning says
   so ("members[0]: transcribe: <reason>; started without attach"), where a hand `plan_start` would
   refuse the whole op: the file cannot be edited per request as a call can, and the demodulators are
   worth having while the speech backend is down (9.18).
2. **Bounding bands.** Per source in use: the band bounding every member on it (a `plan_start`
   member's channels, a `job_start`'s band), checked against the source's ranges (`out_of_range` on
   `radios.<role>` with the width, as `plan_start` answers on `bandwidth_hz`).
3. **Gates.** The gates of every source in use are taken in source-id order and held to the end: a
   fixed order is deadlock-free given that no other operation takes two, and it is what makes the
   next step atomic across radios (9.2).
4. **Existing.** Each member's identical rigid jobs (`duplicate_of`, a `plan_start` member's per
   channel) are `existing`; running the request twice adds only what is missing. A `watch` member's
   identity includes its `params`: a watch on the same band with the same policy is `existing`; one
   with another policy (a hand-started "Watch this band", a sonde watch) is `other` naming it
   ("watch <id> holds this band with other policy; stop it or `graph_put` it"), nothing started, since
   two detectors over one band are waste and a `graph_put` on a job the request did not name is a
   restart the field craft forbids (9.16).
5. **Replace,** under the gates, as `plan_start` does.
6. **One lease decision per source, decided before anything is tuned.** The lease manager gains
   `decide(port, want)` under a held gate: `Fits` (inside the capture, no tune), `Tune` (only elastic
   leases hold the source) or `Conflict {holders, options}` without touching the port. Every source in
   use is decided first; a `Conflict` on any is the whole op's answer with no port retuned anywhere,
   naming the role and the source with the holders and the options (`"radios.second (hackrf) is held
   by [...]"`). Then each source's `hold_band` of its bounding band runs as `plan_start`'s does (a tune
   the port itself refuses is that source's failure, as `job_start` words it; the sources already tuned
   stay tuned, nothing has started). The operator decides a conflict; `members` narrows the next
   request (9.17).
7. **Starts,** source by source in role order (the file's order), member by member, each inside its
   capture: a member's own failure is its `refused` entry (`{member, channel?, error}`) and the rest
   go on; a `plan_start` member's per-channel results are flattened under it (`started[].member`,
   `started[].channel`).
8. **The instance** is written: `state_dir/sets/<set id>.json` with `radios`, `since_ns`, `members:
   [{id, jobs: [ids]}]` (an `attach`ed job beside its demodulator). One instance per set at a time: a
   `set_start` of a set that runs with a different `radios` mapping is `other` ("set airband runs on
   {...}; set_stop it first"); with the same mapping it is the idempotent re-run of step 4.

The result: `{set, radios: {role: {source, capture}}, started: [{member, channel, center_hz, job,
attached, attach_error}], existing: [...], refused: [{member, channel, error}], warnings: [strings]}`,
with `plan_start`'s truncation rule (counts survive the ops log's size cap).

### 5.3 A watch as a member

A `watch` member starts the watch (`job_start`) and then applies its `params` to the `detect` node
(`graph_get`, merge, `graph_put`), both inside `set_start`, so `set_start pagers_au` is one op and the
watch's `graph_put` restart happens before any signal is tracked. The member's `params` are validated
at load against `DetectParams`' schema and `check` (`sets/<id>.json: members[0].params.hold_ms: ...`),
so a bad set is `broken` at load, not at start.

### 5.4 Picking a radio

A role mapped to `null` is filled by the node: the first source in `source_list` order that
`free_sources` lists for the role's bounding band and is not already taken by another role of this
request, roles in the file's order (`wide` before `second`, so the radio that can take 8 MHz is not
spent on ADS-B). None: `other` naming the role and the band ("radios.second: no free source covers
1 090 000 000 Hz ± 1 200 000 Hz; busy: hackrf (holders [...])", the `source_free` answer's shape). The
result's `radios` says what was picked. A picked source is a suggestion that just became a lease: the
gate is taken in step 3 after the pick, so a conflict is still possible and is reported as one.

### 5.5 Restart, status and the page

- On a node restart the instance file is read back; the members' jobs resume as ordinary jobs (they
  are); `set_list.running` names them with their states. The instance is dropped when `set_stop` runs
  or when every job it names has ended; the ops log keeps the history.
- `set_list` is the status: `running.members[].state` is each job's `state`; a member whose jobs all
  ended reads `ended`.
- **The page**: "Start set…" beside "Start plan…" in the selection menu, listing `set_list`'s ready
  sets; choosing one opens a small role → source picker (the shown source for the first role, `null`
  = "pick" for the rest, every attached source offered) and sends `set_start`; the transcript line
  reports the four lists as the plan entry does. The jobs pane folds an instance's jobs under a group
  row titled by the set's name (`foldPlan`'s mechanism, key the set id, membership from `set_list`
  polled with the jobs, one call), the members on the shown source listed and the others counted
  ("+1 on hackrf"); the group's stop button is `set_stop` (confirm, as the agent's refusals do;
  9.7). The two folds nest, two levels at most: a `watch` member's row sits inside its set's group
  and its decoders fold under that row (4.7), so `pagers_au` reads set → watch → decoders, the set's
  stop is `set_stop`, the watch row's stop is a `job_stop` of the watch (which stops its decoders, 4.5),
  and a decoder row's stop is its own `job_stop`; a plan member's channels fold one level as today
  (9.19). No new pane.

### 5.6 Is a set a recipe?

Partly, and on purpose. The recipes gist asks for a name, preconditions, ops in order, and start,
status and stop by name. A set has the name, the start, the status and the stop, and its "ops" are
members the node can check before the first one runs, which an arbitrary ordered list cannot be: that
is the property `plan_start` has and a recipe would lose. What a set does not do: pins (`source_pin`
as a step), a mix, waits and verify steps, anything conditional. `RECIPES.md` keeps those as prose,
and its ACARS and ADS-B recipes become "run `set_start airband`" with the sets' JSON as the spec
material it promised. If a recipe mechanism comes, `{"op": "set_start", ...}` is one of its steps and
the set stays the unit the node owns.

## 6. Docs, TODO and UAT

- `ops.rs` doc text for the three ops and the watch's new params (`DetectParams` field docs are the
  stage's AGENTS.md text); `crates/zeta/src/agents_md.rs` per-op notes: `set_start` beside
  `plan_start` in the rules ("For a named set use `set_start`, never its members by hand; set ids come
  from `set_list`"); regenerate AGENTS.md and the skill.
- Field craft: "Decoding every channel of a band" (the two-op form, the set form, what `waiting`,
  `lost` and `unproven` mean, why `decode_on: free` is wrong here, that a hand-started decoder inside
  the band is left alone) and "Starting a named set" (conflict names the role; `members` narrows; never
  `replace` unnamed; `set_stop` is the operator's). "Decoding pagers, aircraft and FM data" points the
  ACARS + ADS-B example at `airband`.
- `RECIPES.md`: the ACARS and ADS-B recipes become the `airband` set with a pointer; the convention
  paragraph says sets are the stored form.
- TODO: strike the whole-span bullet of decoder wave 2 and the airband half of the VDL2 gist; narrow
  "Span decoders and trunking" to trunking; narrow the recipes gist to what 5.6 leaves; add "Sets: a
  `source_pin` member" and "Watch: a `subject` block for `pocsag_decode`" if the operator wants them
  (8.3, 8.1).
- UAT (the operator's, on the field host): (a) `set_start pagers_au` with `radios: {radio: b210}`:
  within five minutes `job_context.watch.decoding` lists a decoder on each active pager carrier, the
  `decoding` summaries name the licensee from the register, the page shows the decoders folded under
  the watch, and a tail of a decoder's events shows pages; after an hour the watch has not churned
  (count `lost` events against `decoding`). (b) `set_start airband` with `{wide: b210, second:
  hackrf}`: seven ACARS and one ADS-B start, `vdl2` is `refused unavailable`, the page groups them under
  "Airband data links"; a second `set_start` answers all `existing`; `set_stop` leaves the page's own
  spectrum and nothing else.

## 7. Tests

### 7.1 The watch (`crates/zeta-core/tests/watch.rs`, `--features test-fixtures`)

The suite's harness stands: 1.024 MHz file sources (under the bank's threshold, filtered directly), a
POCSAG page every 2 s 300 kHz above the centre, `test_timing` for the clock, `until` for every
asynchronous read (the CI-flake rule). New fixtures: `pocsag3`, three pages at +200, +300 and
+400 kHz, the third present only in seconds 6-14 of a 20 s loop; `pocsag_fsk`, a page beside the
no-row GFSK burst. Tests:

- `a_span_watch_starts_a_decoder_per_signal_without_identify`: `modes: ["pocsag_decode"]` on
  `pocsag3`; three decoders within a cycle or two, no owned identify record ever runs (`owned` stays
  empty), each `decoding` event has `via: "modes"`, each decoder's centre within a bin of its page.
- `a_signal_fitting_no_listed_mode_is_unknown_at_once`: `pocsag_fsk` with `modes: ["pocsag_decode"]`
  and `width_hz` wide enough to see the burst: the page decodes, the burst's `unknown` arrives with the
  "no listed mode fits" summary and no identify ran; with `on_unknown: capture` it is captured once.
- `two_fitting_modes_run_a_restricted_identify`: `modes: ["pocsag_decode", "aprs_decode"]` (both
  12.5 kHz): one owned identify runs on the page and tries only those rows (its `candidate` events name
  no other), the proof is kept with `via: "identify"`.
- `the_cap_queues_and_a_freed_slot_is_taken_by_rank`: `max_decoders: 2` on `pocsag3`: two decode, the
  third is `waiting` (event and `signals[].waiting`); `job_stop` one decoder; the waiting one starts
  within a tick; `job_context.watch.cap` reads `{max: 2, decoding: 2, waiting: 0}`.
- `a_quiet_channel_is_held_then_lost_and_cooled`: `hold_ms` short, `cooldown_ms` long; the third
  page's decoder is `lost` `why: "gone and silent"` after the hold, `cooled[]` names its channel, and
  the page's return inside the cooldown starts nothing (`waiting` absent, no `decoding`); after the
  cooldown it starts again.
- `an_unproven_decoder_is_stopped_and_its_channel_cooled`: a noise-only signal that fits the width
  (a shaped-noise fixture at +350 kHz) with `confirm_ms` short: `lost` `why: "unproven"`, cooled.
- `known_channels_rank_first_and_name_the_licensee`: the suite's fake knowledge provider answers an
  assignment on the +300 kHz page with a 12.5 kHz designator; with `max_decoders: 1` the known page
  wins the slot and its summary carries `register says`; `job_context.watch.known` lists it `from:
  "register"` and the row's centre `from: "row"`; with `seed_known: true` its decoder runs before any
  `new`.
- `an_unreachable_register_leaves_known_from_rows_only`: the provider errors; `known.register` is
  `"unreachable"`, the run goes on.
- `knowledge.rs`: `band_assignments_is_uncapped_and_deduplicates_by_centre`: nine assignments on
  three centres from two providers come back as three known channels, `sites` 3 each, nearest site
  named, in frequency order; `rx` rows and plan rows are left out.
- `a_hand_started_decoder_inside_the_band_is_left_alone`: a `pocsag_decode` started by hand on the
  +300 kHz page before the watch; the watch decodes the other two and never touches it; stopping the
  watch leaves it running.
- `a_span_watch_is_readopted_with_the_hold`: restart (the suite's `a_restart_readopts_or_drops`
  shape) during the third page's quiet spell with `hold_ms > gone_ms`: the decoder is kept and
  confirmed when the page returns.
- `stopping_a_span_watch_stops_every_decoder` and `a_replace_of_a_span_watch_stops_its_decoders`
  (the existing shapes with three decoders).
- `graph_put_with_a_bad_modes_list_is_refused`: an unknown id, a `broken` row, a row wider than the
  watch's band, an empty list: each refused naming `modes[<i>]`, the watch unchanged.
- The existing suite passes unchanged: `modes: null` is today's watch, and the defaults for the new
  params are today's constants.

### 7.2 Sets (`crates/zeta-core/tests/sets.rs`)

The `plan_start` suite's fake `Radio` (tunes counted, optionally slow), two of them, and a sets
directory the test writes (a built-in-shaped `two_radio` set: a `plan_start` member on role `a`, a
`job_start` member on role `b`, and a `watch` member with `params`).

- `a_set_starts_its_members_under_one_tune_per_radio`: both radios tune once; the result's `radios`
  carry both captures; `set_list.running` names every job.
- `a_conflict_on_either_radio_starts_nothing_and_names_the_role`: a rigid holder on radio `b`
  outside the band; the error names `radios.b`, the holders and the options; radio `a` has no new
  holder and its tune count is unchanged, whichever of the two ids sorts first (run both orders).
- `lease.rs`: `decide_reports_fits_tune_or_conflict_without_touching_the_port` (the fake port's tune
  count stays 0 through all three answers).
- `a_watch_member_on_a_band_another_watch_holds_is_other_unless_the_policy_matches`: a hand
  `watch` on the band with default params makes `set_start pagers_au` `other` naming it and starts
  nothing; the same set started twice answers `existing` and does not `graph_put` the watch again
  (its loop is not restarted: the watch's `since_ns`, or its context's run count, is unchanged).
- `an_unready_attach_starts_the_member_without_it_and_warns` (the `plan_start` suite's
  `attach_registry` with an unready readiness).
- `a_null_radio_picks_the_first_free_source_in_role_order`, `no_free_source_is_other_naming_the_role`.
- `a_member_whose_row_is_unavailable_is_refused_and_the_rest_start` (a mode registry with an
  `unavailable` program row).
- `members_narrows_and_unmapped_roles_may_be_omitted`.
- `a_second_start_adds_only_what_is_missing`, `a_different_mapping_while_running_is_refused`.
- `a_watch_member_runs_with_its_params`: the watch's `detect` node carries `modes` after the start
  and its loop runs once (no second restart).
- `set_stop_stops_the_instances_jobs_only`: a hand-started job on radio `a` inside the capture
  survives; attached jobs end with their inputs; the instance is gone from `set_list`.
- `a_restart_keeps_the_instance`: the instance file resumes and names the resumed jobs; one the
  operator stopped before the restart reads `ended`.
- `gates_are_taken_in_source_order_and_a_dropped_start_leaves_no_holder`: the slow radio and a
  dropped future (the `a_plan_start_dropped_mid_way_leaves_no_holder` shape) on two sources.
- `an_operator_set_replaces_a_builtin_and_a_broken_one_is_listed_broken`,
  `a_set_file_never_carries_replace_or_source` (load refuses them naming the field),
  `a_watch_members_params_are_checked_at_load`.
- `crates/zeta-agent`: `set_start` with a non-empty `replace` is refused without `allow.replace`;
  `set_stop` is refused without `allow.stop_jobs` unless the run's own `set_start` started it;
  `set_list` passes.
- `crates/zeta/tests/agents_md.rs` keeps the generated files honest; `zeta set start` and `zeta sets`
  get CLI tests beside `zeta plan start`'s.

### 7.3 The page (`crates/zeta-ui/tests/e2e/ui.py`)

- `scenario_decode_across_entry_starts_a_watch_with_modes`: the selection menu on the POCSAG fixture
  shows "Decode POCSAG pagers across this band…"; one click gives a running watch whose `detect` node
  has `modes: ["pocsag_decode"]`, and within the fixture's cycle a `pocsag_decode` job appears folded
  under the watch's row in the jobs pane with the group's summary text.
- `scenario_set_menu_starts_a_set_and_groups_its_jobs`: a test set in the e2e node's `--sets-dir`
  with two members on roles mapped to `tone` and `cb` (bands inside each fixture's capture: a
  `measure` on `tone`, an `fm_demod` on ch20 of `cb`); "Start set…" → picker → `set_start`; the jobs
  pane shows the group with "+1 on cb" on the `tone` source and the member row on `cb` after a source
  switch; `set_stop` from the group's button clears both.
- `scenario_subjects_merge_a_watchs_decoders`: two `aprs_decode` decoders under one watch (the APRS
  fixture twice at two offsets, or a stub watch context the test hook can seed) draw one section with
  a Channel column.
- Budget: three scenarios of 10-20 s each against about 216 s of headroom; if the suite nears the
  deadline the Subjects scenario is folded into the first (the builder's call, said in the plan).

## 8. Questions for the operator

1. **Pager text and the Subjects table.** `pocsag_decode` has no `subject` block, so a span watch over
   the pager band shows ribbons per channel and no table. Do you want a pager table (key `address`,
   columns `kind`, `text`), knowing it lists every address heard with its message text in the page and
   the merged store? If so it is a row edit under this plan; if not, the ribbons stand.
2. **Radio choice for `null` roles.** The node picks the first free source in `source_list` order that
   covers the role's band, roles in file order. On the field host that puts ADS-B on whichever of the
   B210 and HackRF is left after the wide role: fine, or should the ADS-B role name a preference (by
   driver, since no file may name a host's ids)?
3. **Pins in sets.** The CB recipe pins the capture before `plan_start`. A set holds its capture through
   its members' leases instead, so the capture goes when the last member stops. Do you want a
   `source_pin` member kind now (the set then also unpins on `set_stop`), or is "no pins in sets"
   right?
4. **Stopping the watch stops its decoders** (today's rule, unchanged). For a pager watch that has run
   all day, is that what you want from `job_stop` on the watch, or should a span watch's decoders be
   left running (they are ordinary jobs) for a `jobs_stop` to clear?
5. **`seed_known` off by default** for `pagers_au`: the first burst on a known channel after a quiet
   spell is lost while the decoder starts (a second or so). Would you rather the set seed every
   register-known pager channel at start and accept decoders sitting on dead assignments?

## 9. Calls made in this document (check these while reading)

1. **No `params` on `job_start`.** The watch's `modes` and the rest are `detect` node params set by
   `graph_put`, as every watch policy is today (the two-radio sonde form), and a set carries them so
   the common case is one op. A `params` field on `job_start` would break every existing caller (every
   op field is required); the alternative is noted here and nowhere else.
2. **Two gates at once in `set_start`,** in source-id order, held through the checks and the starts, so
   a conflict on one radio starts nothing on the other. No operation holds two gates today, and the
   watch's free placement takes one at a time, so the fixed order cannot deadlock; a sequential per-
   radio start with a rollback would stop jobs the operator saw start, which the field craft forbids.
3. **Known channels rank and hold, never move.** The decoder sits on the detector's centroid; a known
   centre is a bet about priority and a name for the summary. `seed_known` is off in `pagers_au` (8.5).
4. **`confirm_ms` 180 s for `pagers_au`.** A direct start has no proof; a pager carrier transmits idle
   codewords between pages and `pocsag_decode` publishes only messages, so a minute is too short for a
   quiet carrier and three is the author's guess; the UAT checks churn against it.
5. **Re-adoption uses `max(gone_ms, hold_ms)`.** Today's `gone_ms` confirmation would drop and restart
   a resumed pager channel in a quiet spell.
6. **Subjects merge per watch,** keyed (watch, key), with a Channel column; POCSAG adds no table (8.1).
7. **The set group's stop button sends `set_stop`** with a confirm step; a member row's stop stays a
   `job_stop` of that job.
8. **The CB recipe stays a recipe**; `airband` and `pagers_au` are the two built-in sets. A set has no
   pin member (8.3) and no mix.
9. **A not-`ready` member is refused, not blocking:** the airband set ships with `vdl2` and reports it
   `unavailable` until the row and dumpvdl2 exist, so ACARS and ADS-B start today.
10. **A conflict anywhere starts nothing anywhere.** `members` is how the operator takes what is free.
11. **One instance per set at a time,** keyed by set id; a different mapping while running is refused.
12. **The cap does not pre-empt.** Over the cap the watch says `waiting`; it never swaps a decoding
    channel for a stronger one.
13. **`max_decoders` applies to every watch** (16 by default), bounding `decode_on: free` too; the sonde
    watch never reaches it.
14. **Several fitting modes fall back to a restricted identify**, reusing the child rather than
    inventing a second disambiguation.
15. **The watch's own source only** for direct starts under `decode_on: capture`; `free` keeps its
    meaning and the field craft says not to use it for a span.
16. **A watch member's identity is band plus policy.** Same band, same `params`: `existing`. Same
    band, other policy: `other` naming the watch; never a silent `existing` that span-decodes nothing,
    never a `graph_put` on a job the request did not name.
17. **Decide, then tune.** `set_start` decides every source's lease under the held gates before the
    first tune, through a new side-effect-free `decide` on the lease manager, so a conflict on the
    second radio leaves the first untouched. `plan_start` keeps its one-source `hold_band`.
18. **An unready `attach` in a set member is a warning, not a refusal**: the member's demodulators
    start without it. The hand `plan_start` keeps its whole-op refusal.
19. **Folds nest two levels at most** (set → watch → decoders); each row's stop is the op for that
    row's own unit.
20. **Known channels come from an uncapped band query** (`band_assignments`, deduplicated by centre),
    not from identify's five-nearest `nearest_allocations`.

## 10. Left to the plan

- Where the restricted candidate list enters identify (`Run` field or `Seed`), and whether
  `start_on` grows a flag or a sibling for the direct start.
- The exact shape of `job_context.watch.cap`, `known` and `cooled`, and whether `cooled` is pruned on
  each tick or on read.
- Whether sets load through the mode registry's loader generalised (`Loaded<T>` with `broken`) or a
  copy; the modes loader's tests are the contract either way.
- `plan_start`'s refactor into a gated inner the set calls under gates it already holds, with an
  "attach optional" variant for sets (9.18), and how the `graph_put` of a watch member reuses the
  op's path.
- The shape of `decide` (a method returning an enum, or `ensure_locked` split into a decision and an
  application) and whether `hold_band` is rewritten on top of it.
- The `foldPlan` generalisation (a group key function per fold kind) and the Subjects store's merged
  key.
- Order of work: watch policy and tests first (deployable alone; `pagers_au` can be run as the two-op
  form), then sets on the node with their tests and the agent gate, then the page, then docs and the
  generated files; the UAT runs after the second step on the field host.
