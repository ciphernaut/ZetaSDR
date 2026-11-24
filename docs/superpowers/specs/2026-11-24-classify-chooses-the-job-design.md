# Classify chooses the job: demodulators as rows and the proof ladder (plan 48)

2026-11-24. Design, approved in conversation part by part. The parts are listed under 8. The
plan follows in `docs/superpowers/plans/`.

## 1. Intent and why

From the operator, 2026-11-18: "eventually we want to be able to use the L1/L2 classifier to set
the right job type without needing an agent or llm" (`TODO.md`, "Classify: layers 1 and 2 choose
the job, with no agent or model").

Today three things choose the job for a signal:
- the operator, who reads the box menu and picks an entry;
- the agent, which reads a `classify` or `identify` answer and reasons from the field craft's prose;
- an identify, which can prove a decoder row but never starts a demodulator.

The knowledge to choose is already in the node, but it is spread out:
- layer 1 measures width, modulation family, symbol rate and a label;
- layer 2 proves decoder rows and matches catalogue entries and learned signatures;
- the mode rows say what each decoder takes;
- the demodulators' knowledge (which label they serve, how wide to start them, which deviation)
  lives in prose (`docs/agents/field-craft.md:175, :238, :251, :294`) and in the page's
  constants (`menu.js` `FIT_MIN_HZ`, `jobTuning`; `actions.js` demod entries at fixed widths).

The feature makes the node answer, with no agent and no model: "for this selection, or this
detected signal, I would start this job (kind, centre, width, parameters), for this evidence;
the runner-up is that; or I cannot tell, and why". The page, the watch and the agent all use
that one answer. The mapping is data, not code.

## 2. Goals, non-goals, anti-goals

Goals:
- **G1 Demodulators are mode rows.** `fm_demod`, `wfm_demod`, `am_demod` and `ssb_demod` get rows
  in `modes/*.json`, beside the decoders: the layer-1 labels each serves, its width rule, its
  parameter defaults. An operator can add a variant from `--modes-dir`.
- **G2 One ranked answer.** `classify` answers a `choice`: the job, the row, the evidence, a
  runner-up, the rows that could still prove otherwise, and `cannot_tell` with its reason.
- **G3 Measured features as fields.** The classify answer carries layer 1's measurement (width,
  `power99_hz`, symbol rate, RMS deviation, SSB side and dial, SNR) as fields, not only inside an
  evidence string.
- **G4 The proof ladder** (section 5.3): the node climbs only as far as the policy allows and the
  hardware and the ready decoders make possible, and revises its answer as it climbs.
- **G5 The policy** (section 5.4): how far each surface (menu, watch, agent) may climb, and what a
  trial may use.
- **G6 The consumers:**
  - the box menu's first entry is the answer;
  - Listen is ordered by it;
  - a watch can start the answer on each new signal (the voice case is `TODO.md`'s "Watch: start
    a voice demodulator and a transcriber on each new signal");
  - the agent calls the same operation instead of working the choice out.
- **G7 A slot for a fast model** (section 5.6): a layer-3 source plugs in as evidence through a
  shared vocabulary and a weights table, and the ladder keeps the records that would train it.
  The model itself is not built.

Non-goals:
- A layer-3 model, its serving and its training (G7 is the slot only).
- Band names as data (`TODO.md`, "Agent: band names come from data"): its own gist. This design
  only does not stand in its way (section 5.2's region note).
- A measured CPU baseline (`TODO.md`, "Node: a baseline of the host's capabilities"). The budget
  keeps a slot for it (section 5.4).
- Trunking, and access rules on `content: network_traffic` rows.
- The rest of plan 40's remainder (Decode entries generated from the rows, the tuning footer, the
  one transcription setting). G1 makes it easier, and it stays its own item.

Anti-goals (a failure even if it works):
- **A proof from anything but a decode.** A register match, a catalogue range, a learned signature
  or a model's guess ranks rows; only a decode passing its own check proves one, as now.
- **A choice that stops or retunes another job.** A trial never stops a job, never retunes a radio
  away from its holders, and never takes a radio a foreground request wants. A scan or trial is
  secondary and never blocks other jobs.
- **A channel-grid assumption.** The answer is centred on the measured signal, never snapped to a
  12.5, 25 or 100 kHz raster. A plan channel can supply the deviation and the width when the
  measurement is untrusted, never the centre.
- **One station's limits in the data.** No region box, frequency ceiling or device cap borrowed
  from the field host. Region facts come from the region data that plans already carry.
- **A start without a request.** The answer is an answer. Starting it is the consumer's act: a
  click, a watch's policy, an agent call.

## 3. Constraints (how things stand, 2026-11-24)

- **`classify`** (`ops.rs:8500`) takes 2 s of the ring and runs `ClassifierSet::run_all`
  (`classify.rs:262`): layer 1, `DecodeToIdentify` with up to `CLASSIFY_TRIES` = 5 quick in-process
  proofs (APRS, POCSAG, ACARS, RDS, AIS, frame rows), the catalogue, learned signatures, the
  layer-3 stub (`classify.rs:224`). Its answer, `OpResult::Classification { labels, tried,
  allocations, origins, register }` (`ops.rs:4043`), is a flat list in layer order; nothing picks a
  winner. A layer-2 proof's label is the row's display `label`, not its id (the id is in
  `tried[].mode`).
- **Layer 1's ten labels** (`measure.rs:691-737`, first match wins): `noise`, `ook`, `fsk`, `wfm`,
  `psk`, `am`, `ssb`, `fm`, `carrier`, `unknown`. The generated note in `agents_md.rs:210` lists
  eight (it omits `ook` and `psk`): fix it here.
  - `fm` is coarse. It covers FM voice, DMR, P25, M17 (rows with no `range_hz`, which fit any
    narrow FM signal) and pagers.
  - A weak FM voice carrier can read `fsk` (`TODO.md`, "Fit: the layer-1 label of a weak voice
    carrier").
  - A weak FM carrier's occupied width is the carrier line only: the field craft's rule distrusts
    widths under the floor for `fm`, `fsk` and `unknown`.
- **Scores do not compare across sources.**
  - `score_rows` (`modes/score.rs:174`) gives 0.4 centre + 0.3 width + 0.3 label in `modulation`,
    plus the catalogue's score, plus the register's `allocation_term`.
  - The catalogue gives 0.7 for range and bandwidth alone, which outranks layer 1's 0.6
    (`TODO.md:631`).
  - A learned signature's score is the record's confidence × its own match score.
- **Identify** (`node/identify.rs`) trials rows as child jobs at the band's width recentred on the
  measured centre (`as_asked` rows as asked). It starts only mode rows (`JobKind::from_name(row
  id)`, `identify.rs:361`), scores with the band's width rather than the measured one
  (`identify.rs:583`), and dwells 15 to 30 s per narrow FM signal.
- **Demodulators are code.**
  - The kinds are at `ops.rs:1610-1615`; `fm_demod` expands to the `nbfm_demod` stage.
  - The defaults are constants: `FM_DEVIATION_DEFAULT_HZ` 5000, `WFM_DEVIATION_DEFAULT_HZ` 75000,
    `WFM_DEEMPHASIS_US` 50, `FM_AUDIO_LOWPASS_HZ` 3400, `FM_DEEMPHASIS_US` 530,
    `SSB_AUDIO_BANDWIDTH_HZ` 2800.
  - `job_start` requires `bandwidth_hz`, and no kind has a default width.
  - Channel plans (`plans.rs`, compiled in, with a `region`) carry `deviation_hz`, which only
    `plan_start` applies (`ops.rs:6536`); a direct `fm_demod` on a plan channel does not get it
    (`TODO.md:185`).
- **The watch.**
  - Its detected signals carry `{center_hz, bandwidth_hz, snr_db, first_ns, last_ns}` and no label
    (`watch.rs:592`).
  - With `modes: null` it identifies each signal.
  - With `modes` it starts the single nearest fitting row or a restricted identify.
  - `check_modes_list` refuses anything that is not a mode row (`watch.rs:532`).
- **Replay.** `capture_decode` (plan 44) replays a stored burst capture through a mode's handler as
  a leaseless job. A program fed faster than real time loses frames today, so a replay used as a
  trial must be paced. It takes burst-store ids only.
- **Older clients.** A client built with strict types refuses a `modes_list` answer that holds a
  new row shape. Say so in the release notes, as cleanup round 2 did for `channel_plans`.
- **The transports translate `Op` only**: every new field or operation goes into `ops.rs`, with
  its agent prose in `agents_md.rs` or the field craft, and `AGENTS.md` and `SKILL.md` regenerated.

## 4. Approaches

1. **Demodulators become mode rows (chosen by the operator, 2026-11-24).**
   - **How:** the four demodulators get rows. `classify` ranks every row, decoders and
     demodulators together, and answers the top one with a runner-up. A controller job climbs
     ladder steps 2 and 3 and sends a revised answer when a proof arrives.
   - **Gains:** one description of every job a signal can become. Listen is ordered for free. A
     watch can name a demodulator. An operator can add a variant. The prose and JS width constants
     go, rather than gaining a third copy. It is the same move as plan 40's "entries from the rows".
   - **Costs:** the largest of the three. A row needs a form with no proof. Strict older clients
     refuse the new rows. Every `modes_list` consumer is touched.
2. **A separate choice table.** A data file maps a label and measured features to a job template,
   a new `choose` operation answers, and rows and kinds stay as they are. It is smaller and
   additive, but it gives two tables describing what a signal is, which drift apart; the width
   rules need a small expression language; and Listen ordering needs this table too.
3. **Inside classify and identify.** `classify` gains a `job` field from a built-in table, and
   identify falls back to a demodulator. It needs no new operation, but the mapping stays code
   (against "data, not code"), and ladder steps 2 and 3, which are jobs, have no home in a one-shot
   operation.

## 5. Design

### 5.1 Demodulator rows (G1)

A demodulator row is a mode row whose `decoder` names a built-in kind:
`"decoder": {"builtin": "fm_demod"}`. Its id is the kind's name (`fm_demod`), so `job_start
--kind fm_demod`, `JobKind::from_name` and `modes_list` all use one name. It carries:
- **`modulation`**: the layer-1 labels it serves. `fm_demod`: `fm` (and `fsk` when the width
  rule's distrust applies, section 5.2). `wfm_demod`: `wfm`. `am_demod`: `am`. `ssb_demod`: `ssb`.
- **`band`**: `bandwidth_hz` (the nominal start width: 12500, 200000, 10000, 6000), `min_hz`,
  and a **width rule**: start at the measured occupied width × `occupied_scale` (1.25), no narrower
  than `floor_hz` (5000 for fm and am, 150000 for wfm), and when the label is one the rule
  distrusts below its floor (`fm`, `fsk`, `unknown`), take in order the plan channel's width,
  then `power99_hz`, then the selection's width. `ssb_demod`'s rule keeps 6000 Hz and centres on
  the dial frequency layer 1 reports, not the middle of the voice.
- **`params`**: the defaults the kind's constants hold today (`deviation_hz` 5000 for `fm_demod`,
  75000 for `wfm_demod`; de-emphasis; audio low-pass). For each parameter the first of these wins:
  the request, a plan channel that holds the centre, the row. This also closes `TODO.md:185`: a
  direct `fm_demod` on a plan channel gets the plan's deviation.
- **`identify.proof`: `none`.** Identify never trials a demodulator row. It is the answer when no
  decoder proves.
- **`voices`/`outputs`**: audio, as the kinds give today, so the transcriber and mix rules that
  read a row's outputs apply.

**Where demodulator rows appear.** They are first-class rows marked by their decoder form: a row
whose `decoder` is `builtin` has `role: demodulator`, derived and served in `modes_list` (every
other row `role: decoder`). `modes_list` takes a `role` filter. Each consumer takes the role it
means, and no consumer that ranks or lists decoders sees a demodulator unless named here:
- **Take demodulator rows**: the choice (section 5.2), the page's Listen, a watch's `modes`, a set
  member (as `job_start` of the kind takes it today), `zeta modes check` (it checks that the row
  expands).
- **Skip them**: `score_rows` and so every decoder ranking that calls it (identify's
  `identify.rs:617`, classify's quick proofs `zeta-dsp/src/identify.rs:156`, the watch's nearest
  row `watch.rs:145`). This matters because a demodulator row with no `range_hz` and `modulation:
  fm` would otherwise score 1.0 on any narrow FM signal, above DMR and P25. Also skipped: the page's
  Decode entries and Send to, the handler policy, `capture_decode` (replaying a capture into a
  demodulator to listen to it is worth having, and is a later item).
- The regression guard is that identify's and the watch's rankings are unchanged on every
  existing test once the rows are in.

The kinds' constants are no longer the source: the expansion reads the row, and a test pins each
built-in row's values to the values the kinds used before this plan. A row's `range_hz` is absent
(a demodulator serves anywhere). A demodulator variant from `--modes-dir` (a narrower FM voice
row, say) names the same `builtin` and its own defaults.

### 5.2 The choice (G2, G3)

`OpResult::Classification` gains `features` (layer 1's measurement as fields) and `choice`:

```
choice: {
  job:      { kind, center_hz, bandwidth_hz, params } | null,
  row:      "<row id>" | null,
  step:     1 | 2 | 3,              // the ladder step that settled it
  settled:  "proved" | "best_guess" | "cannot_tell",
  evidence: [ "<source>: <line>", ... ],
  runner_up:   { job, row, evidence } | null,
  could_prove: [ "<row id>", ... ],  // ready decoder rows a later step could still prove
  not_tried:   [ { row, reason } ], // e.g. "no running capture covers it", "budget"
  reason:   "<why cannot_tell>" | null
}
```

The ranking, in order:
1. **A proof wins.** A decoder row proved by a quick proof (step 1) or by a later step is the job:
   its width from the row (`bandwidth_hz`, `min_hz`), centred as identify's trial was (recentred
   on the measured centre, or as asked for an `as_asked` row). `settled: proved`.
2. **Otherwise the best demodulator row** for the layer-1 label, sized by its width rule and
   given its parameters (section 5.1). `settled: best_guess`. The ready decoder rows that fit
   the label, band and width but did not prove go in `could_prove`, ranked, and the first of them
   is `runner_up` when its score is within a margin of the demodulator's. The margin is a value in
   the weights table, below.
   When no demodulator row serves the label (`ook`, `psk`, a `fsk` wider than `fm_demod`'s rule
   takes) but a ready decoder row fits it, the top fitting decoder is the job, `settled:
   best_guess`, its runner-up the next. This is what a watch with `modes` does today, when it
   starts the nearest fitting row unproved. The menu's entry says "not proved" for such an answer.
3. **`cannot_tell`** when neither proves nor serves: `noise`; a label no row serves (`carrier`,
   or `ook` or `psk` with no ready decoder fitting); `unknown` with no fitting row. `job: null`, the
   reason in plain words, and `could_prove` still listed, so a menu can fall back to its groups.

Ranking terms come from **one weights table**, data with defaults (`modes/weights.json`, or a
policy value: the builder's call, section 10). Each source of evidence has a row there: layer 1's
label match, width fit, centre fit, the catalogue, a learned signature, the register's allocation
term, the margin for a runner-up, and later the layer-3 source. This is where the catalogue's
range-only 0.7 stops outranking layer 1 (`TODO.md:631`): its weight is set so a range-only match
can break a tie and cannot overturn the label.

Region: a plan channel and a catalogue entry apply only in the station's region, as the plans'
`region` field and the catalogue's `regions` say. The default `"AU"` (`knowledge.rs:240`) stays
the node's setting, and no region is written into the rows.

### 5.3 The proof ladder (G4)

The node climbs only as far as the policy (section 5.4) allows and the hardware and the ready
decoders make possible. A step that cannot run is skipped and recorded in `not_tried` with its
reason, and the answer stands at the last step that ran.

1. **Free.** Layer 1 and layer 2 on the 2 s ring sample, with the quick in-process proofs, as
   `classify` does now. Answers in about 2 s.
2. **Off air.** The ring slice step 1 classified is written into the burst store, and replayed
   through the `could_prove` rows with `capture_decode`, side by side (as many at once as the
   budget's concurrent trials allow), each paced to real time, so the step settles in about the
   slice's length, 2 to 3 s. It
   holds no radio. It costs CPU and only the slice's seconds of signal: a signal that keyed off
   before the slice proves nothing.
   - **A new path: ring slice to burst store.** Nothing writes a ring slice that has already
     passed into the burst store today. The `capture` job kind records forward, from its start,
     and holds a channel. `iq_read` writes ring slices to `captures/`, outside the burst store,
     where `capture_decode` refuses them. This plan adds the write. It is the shape of `TODO.md`'s
     gist "`capture_decode` cannot replay an `iq read` recording" ("have `iq_read` write into the
     burst store"), so `iq_read` uses the same path and that gist closes with it.
   - **The slice's length.** The 2 s classify sample, or as much of the ring before it as the
     ring still holds up to the budget's slice seconds, whichever is longer.
   - **Trial slices never cost the operator a capture.** They are tagged `trial`, held under a
     quota of their own inside the burst store's limit, expire first and are never counted against
     the operator's captures. A slice is pinned while its ladder runs. After that it expires,
     unless it carries a training record (section 5.6), which keeps it under the same quota.
3. **On air.** An identify-style dwell of the remaining `could_prove` rows on the live band, using
   identify's trial loop (`identify.rs:539`) restricted to those rows. What the trial may use is
   set by the lease setting (section 5.4).

Steps 2 and 3 run as a **controller job of kind `choose`**, beside `identify` and `watch` in
`node/`, with a feed. Its events:
- `choice` with `step` 1 when it starts;
- `choice` again whenever a step changes the answer;
- `verdict` when it ends, carrying the final `choice`.

Like identify's, it records its children. It stops a trial child when the trial ends and keeps
nothing running unless the request says `start: true`, in which case it starts the final job and
ends.

**The menu's background step 2 is a transient `choose`.** `transient: true` means:
- it is not persisted and never resumed on restart;
- it is cancelled when the menu that asked for it closes (the page cancels it);
- it is listed by `job_list` only with `--all`.

It is still an applied operation, so it is in `ops.log`. One menu open makes at most one. A new
open cancels the last.

`classify` itself stays a one-shot operation answering step 1. A request that wants more
starts a `choose` job: `job_start --kind choose` with the selection, or a signal from a watch.

### 5.4 The policy (G5)

A `choice_policy` value, set like `handler_policy` (`zeta choice policy set ...`, an operation in
`ops.rs`, kept in the state directory):
- **Who is asking.** A `classify` or `choose` request carries `surface`: `menu`, `watch`, `agent`
  or `client`. When the request leaves it out, the node infers it from the transport: `ui` is
  `menu`, `agent` is `agent`, a watch's own `choose` is `watch`, and `local`, `http` and MCP are
  `client`.
- **Per surface, a default step and a cap.** A request may ask for a step (`up_to`). It gets the
  surface's default when it asks for none. A request above the cap is clamped, and the answer
  says so in `not_tried`. The node enforces the cap.
- **The defaults and caps.** These are the author's, open to correction (section 9). `client`
  defaults to step 1 with cap 3.
  - `menu`: default step 1 at once, with step 2 run in the background (a transient `choose`) and
    the entry revised in place; cap 2;
  - `watch`: default and cap 3, on the watch's own capture;
  - `agent`: default step 1, cap 3, so a request that asks for proof gets it.
- **Lease setting**, what a step 3 trial may use:
  - `share` (the default everywhere): only a capture already running covers it. Otherwise the
    step is skipped, with "not tried: no running capture covers it".
  - `widen`: it may grow a held capture with the existing widening mechanism, every holder
    keeping its band, never a retune away.
  - `borrow`: it may also tune an idle radio that covers the frequency, holding a lease that
    yields: the trial ends itself the moment a foreground request wants the radio. This is the
    trial stopping itself, not one job stopping another. Offered for the watch.
- **Budget**: seconds of trial per answer and the number of trials at once, with a slot for a CPU
  figure once the host baseline exists.
- **Confidence stop** (for the layer-3 slot): a threshold above which a `best_guess` from a
  demodulator row stops at step 1. It has no effect until a source gives confidences worth
  trusting.

### 5.5 The consumers (G6)

- **The box menu.**
  - A first entry above What is this? (in `GROUPS.box` and `ACTIONS`, `actions.js:322`) shows the
    step 1 answer: "Start `fm_demod` here: 12.5 kHz FM voice", with the runner-up on hover. It is
    revised in place if the background step 2 settles otherwise.
  - On `cannot_tell` it reads "Can't tell: <reason>" and the groups follow as now.
  - Listen lists the demodulator rows in the answer's order.
- **The watch.**
  - `modes` may name demodulator rows. `check_modes_list` accepts a row of any decoder form.
  - A new `on_signal: choose` starts the answer for each new signal, with a transcriber attached
    when the watch asks (`attach: ["transcribe"]`) and the job stopped when the signal is gone and
    the squelch has stayed shut for a hold time.
  - Idle is judged by squelch events, since a demodulator gives no decoder events.
  - **Probation** (the operator, 2026-11-24). The watch starts the step 1 answer at once. When that
    answer is an unproved demodulator and `could_prove` is not empty, it starts the job **held**:
    - the job runs, and its squelch and level show on the page;
    - the mix leaves it out;
    - its transcriber runs and holds its lines.

    Once the watch's `choose` settles (step 2 in about the slice's length, then step 3 if the
    ceiling allows and the budget has time), one of two things happens:
    - **It stays the demodulator.** The hold is lifted: the job joins the mix, and the held lines
      are released with their own times, so the transcript loses nothing.
    - **A decoder proves.** The held job and its held lines are dropped, and the decoder starts in
      its place, with the transcriber attached to the decoder's voice where the row has one.

    The mix and the transcript only ever get the settled job. The cost is the first 2 to 3 s of
    each new over missing from the mix, not from the transcript. A step 1 answer with nothing in
    `could_prove` (a plain FM channel no ready decoder fits), and every proved answer, starts
    unheld. Replacing the held job is the watch stopping a child it started itself.
  - **`held` on a job record.** A held job carries `held: true` with the reason (`probation`). The
    mix skips a held job and takes it in when the hold lifts, without a restart. An attached
    `transcribe` buffers while its input is held, and publishes or drops on the lift or the drop.
    `held` is not a mute: mutes stay per channel, as now.
  - **Known behaviour, not changed here.** A mute matches the channel (source and centre within
    1 kHz), so a channel muted while a held job is shown stays muted for the decoder that replaces
    it.
  - **Menu starts are not held.** The operator clicked the entry and hears what they chose.
- **The agent.** `classify`'s `choice` is in its tool answer, and a `choose` job is its way to ask
  for proof. The field craft's width rule and demodulator prose are replaced by a pointer to the
  rows.
- **`classify` from the CLI** prints the choice first.

### 5.6 The slot for a fast model (G7)

A fast model, one that names a signal from a spectrogram or an IQ slice in milliseconds, plugs in
as a layer-3 `Classifier` behind a peer URL, probed for what it accepts, as the speech server is.
Three things in this design make that a configuration rather than code:
1. **Shared vocabulary**: the node sends it the ids of the candidate rows, decoders and
   demodulators, and it answers with row ids and confidences.
2. **The weights table** gives its rows their weight. It ranks and reorders `could_prove` (so
   step 2 tries its pick first) and never proves.
3. **The training record**: when a `choose` job settles at step 2 or 3, it writes on the kept
   capture the sample's reference, each source's guess and the settled answer. That is labelled
   data, kept for later. Nothing in this plan trains on it.

Training stays off the GPUs that run inference.

## 6. Order and parts

1. **48.1 Rows and the answer.**
   - The demodulator rows and the `builtin` decoder form; the expansion reading the row.
   - `features` and `choice` in the classify answer, step 1 only.
   - The weights table, the plan channel's deviation on a direct start, and `agents_md.rs`'s ten
     labels.
   - The CLI, the agent prose, `AGENTS.md`/`SKILL.md` regenerated.
2. **48.2 The ladder.** The `choose` controller job (steps 2 and 3), its events and verdict,
   `choice_policy` with the ceilings, the `share` and `widen` lease settings and the budget.
3. **48.3 The page.** The first entry, its in-place revision, Listen ordered by the answer.
4. **48.4 The watch.** Demodulator rows in `modes`, `on_signal: choose`, the attached transcriber,
   the squelch hold, probation (`held` on the record, the mix and `transcribe` honouring it).
5. **48.5 Borrow.** The yielding lease and the `borrow` setting. This part is last and may become
   its own plan.
6. **48.6 The training record.** Writing the record on the kept capture (section 5.6). It is small,
   and it is what makes the slot worth having.

## 7. Tests and acceptance

- **Fixtures**:
  - The repository's real captures, where they exist: the OOK remotes in
    `crates/zeta-dsp/tests/data/` and the VDL2 capture in `modes/fixtures/`.
  - `zeta-dsp`'s generators (`synth.rs`, behind `test-fixtures`) for the rest: `nbfm_modulate`
    (CB voice at 2500 Hz deviation), `wfm_stereo_modulate`, `am_modulate`, `ssb_modulate`,
    `aprs_packet_iq`, `pocsag_packet_iq`, `dmr_iq`.
  - Fresh captures from the field host for the cases a generator cannot stand for: a weak FM
    voice carrier that reads `fsk`, and a real pager. These are kept as fixtures where their
    content allows it.
  - Test through the operations (`classify`, `job_start --kind choose`), not the internals.
- **48.1**:
  - a CB channel answers `fm_demod`, deviation 2500 (from the plan), width per the rule, centred
    on the measured centre (not the plan's channel centre);
  - a POCSAG capture answers `pocsag_decode`, `settled: proved`;
  - a broadcast FM station answers `wfm_demod`;
  - noise answers `cannot_tell`;
  - each built-in demodulator row's values equal the old constants.
- **48.2**: on a capture that is DMR with no quick proof, step 1 answers `fm_demod` with
  `dmr_decode` in `could_prove`, and the `choose` job revises to `dmr_decode` at step 2. A
  `share`-only trial that no capture covers is skipped with its reason. A trial never stops or
  retunes another job (a lease test beside a rigid job).
- **48.3, 48.4**: UI scenarios for the first entry and its revision; a watch over a CB capture
  starting `fm_demod` plus `transcribe` per signal and stopping it after the hold. Probation:
  - a DMR signal's held `fm_demod` never reaches the mix or the transcript, and is replaced by
    `dmr_decode`;
  - an FM voice signal's held job joins the mix after step 2, with its held transcript lines
    released at their own times;
  - a signal with nothing in `could_prove` starts unheld.
- **UAT** (`UAT.md`, plan 48): the first entry on live voice, a pager and a broadcast station; a
  watch's demodulators on a live voice band; the `choose` job's revision on live digital voice.

## 8. Decided by the operator (2026-11-24)

- The ladder: free, then off air, then on air, then the fallback, each surface climbing only as
  far as its policy allows.
- Approach 1: demodulators as rows.
- A fast model plugs in as evidence (section 5.6) and is not built in this plan.
- A watch starts the step 1 answer at once, on probation when a decoder could still prove
  otherwise: out of the mix and the transcript held until it settles (section 5.5).

## 9. The author's calls (open to correction)

- The ceilings per surface (section 5.4): menu at step 1 with step 2 in the background, watch to
  step 3, agent at step 1 unless asked.
- `share` as the default lease setting everywhere, `borrow` offered for the watch.
- The row id equals the kind name (`fm_demod`), not a new name (`fm_voice`).
- `classify` stays one-shot. The ladder beyond step 1 is a job of its own (`choose`), not a slow
  `classify`.
- The training record is in scope (48.6). The model is not.
- Demodulator rows are first-class with a derived `role`, and every decoder ranking and decoder
  list skips them (section 5.1).
- With no demodulator for the label, the top fitting ready decoder is the unproved `best_guess`
  (section 5.2).
- Step 2 replays the slice step 1 classified, through a new ring-to-burst-store write that `iq_read`
  shares. Trial slices get their own quota and never push out the operator's captures (section 5.3).
- The menu's background step 2 is a transient `choose`: unpersisted, cancelled on close.
- Ceilings are a default and an enforced cap per surface. The surface is inferred from the
  transport when a request does not name it, with `client` for the CLI, HTTP and MCP.
- `could_prove` lists only ready rows. A row whose program or handler is missing goes in
  `not_tried` with its reason, so the answer never promises a trial that cannot run.

## 10. Left to the builder

- Where the weights table lives (a data file beside the rows, or a policy value with defaults),
  and its starting weights, as long as the catalogue's range-only match cannot overturn layer 1's
  label and the existing identify ranking is unchanged for rows that do not use the new terms.
- The width rule's spelling in the row JSON.
- Whether `choose` reuses identify's loop by calling it or by extracting a shared trial engine.
- How the menu marks an entry revised in place.
- The hold time before a watch stops a demodulator whose squelch has stayed shut.
