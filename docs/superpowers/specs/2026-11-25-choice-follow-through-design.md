# Choice follow-through: what the live run of plan 48 left (plan 49)

2026-11-25. Design, not yet reviewed by the operator: written without questions, so every call
that is theirs has a recommended default here and is listed under 9. The plan follows in
`docs/superpowers/plans/`.

Parent: `docs/superpowers/specs/2026-11-24-classify-chooses-the-job-design.md` (plan 48). Sources:
`TODO.md`, "Left from plan 48 (2026-11-25)" and the two "minimum-version probe" lines under
"Alternative handlers for a mode"; `UAT.md`, "Plan 48: classify chooses the job" with its run of
2026-11-25.

## 1. Intent and why

Plan 48 made the node answer "for this signal I would start this job". The live run showed where
the answer is wrong or says less than it knows:

- UAT 48.6 failed. On a channel that keys up and down, layer 1 reads FM voice as `am`, `ook` or
  `unknown`, and a watch with `on_signal: choose` then starts `am_demod` on an FM channel.
- On a broadcast station the answer can be `rds_decode`, where the operator wants to listen.
- A weak digital carrier reads `unknown` and nothing tries it. A box drawn a little narrow offers
  no decoder. Above about 8.4 MS/s step 2 never runs.
- The words are off: the entry names no deviation, and "step 2 proved nothing" is said when
  step 2 never ran.
- The UI suite is close to its deadline, and an old handler program is probed `ready`.

This plan is the follow-through. It adds no new ladder step and no new surface. Each part makes
the existing answer right more often, or makes it say what happened.

## 2. Goals, non-goals, anti-goals

Goals:
- **G1 A keyed FM channel answers `fm_demod`** whether the slice holds the whole over, its
  key-up, its key-down or a pause (49.1, 49.2). UAT 48.6 passes.
- **G2 A decoder row can ride with a demodulator** (49.3): one field in the row data. The first
  pair is `rds_decode` with `wfm_demod`; a subtone or selcall row on `fm_demod` is a row, not code.
- **G3 `unknown` is tried** (49.4): the ready decoder rows that fit its width go into
  `could_prove`, on the menu and on a watch.
- **G4 A trial runs at its row's own band** (49.5): a narrow box does not drop the decoders, and
  the trial slice is cut to that band, so step 2 runs on a wide capture.
- **G5 The answer says what ran** (49.6): which steps ran, why one did not, and the deviation.
- **G6 Trials do not get in each other's way** (49.7): replays go before dwells, and no slice is
  written under a blocking lock.
- **G7 The UI suite's longest shard is back under two thirds of its deadline** (49.8).
- **G8 A handler program older than its file needs is `unavailable`, with the reason** (49.9).

Non-goals (each stays where it is in `TODO.md`):
- Plan 48.5, `borrow`: a trial on an idle radio under a lease that yields.
- The layer-3 fast model, its serving and its training.
- Replaying a capture into a demodulator to listen to it.
- The transcript ribbon drawn at its line's own time.
- The two other leftovers of plan 48: a watch's child `choose` superseding signatures, and
  `transcripts.log` at a cancel.
- Decode entries generated from the rows (plan 40's remainder), fan-out inside one graph
  (`TODO.md`, "Architecture: fan-out"), and a host CPU baseline.

Anti-goals (a failure even if it works):
- **A proof from anything but a decode.** The register and a channel plan rank; they never prove.
  Plan 48's rule stands.
- **A special case per pair or per mode.** No `if rds`. Riding is one field any row may carry.
- **One station's facts in the data or the code.** No frequency, gain, band edge or device rate
  from the field host. Layer 1's new thresholds come from generators and are checked on captures
  from more than one radio and gain.
- **A channel-raster assumption.** Every job, trial and slice is centred on the measured signal.
  A plan channel may give the deviation, the width and an expected emission, never the centre.
- **A choice that stops, retunes or replaces an operator's or another agent's job.** A watch
  replaces or adds to only the child it started itself.
- **A page that tests differently from how it ships.** The suite gets faster because the page
  does less work, not because the tests turn the choice off.

## 3. Constraints (how things stand, 2026-11-25)

### 3.1 Why layer 1 misreads a keyed FM channel (the root cause of 48.6)

Layer 1 measures one window and then labels it. The two halves do not look at the same samples.

- **The measurement is gated, the label is not.** `measure_band_noted`
  (`crates/zeta-dsp/src/measure.rs`) takes `gated_view` (`measure/burst.rs`) and, when the window
  is a burst, measures the spectrum, SNR and occupied band over the on-time only.
  `measure_selection` then calls `layer1_label_noted` with the whole window. That function takes
  `envelope_cv(iq)` over every sample, and `classify_block` takes its spectrum, its carrier line
  (`centre_line_db`) and its envelope levels (`envelope_runs`, `two_levels`) from the whole window
  too. The gated CV reaches only the evidence text: the doc comment says "the note changes no
  name". So one evidence line can hold both, as in "carrier line 34.6 dB above the sidebands,
  cv 1.537; ... duty 0.150, cv 0.074".
- **The gate cannot see a drop-out.** `burst_runs` marks a 1 ms chunk on when it stands 6 dB over
  the 15th-percentile chunk. When the carrier is up for more than about 85 % of the window, that
  percentile is the carrier, nothing stands over it, and the window is "one run": `duty` reads
  1.00 with a hole in it. From 50 % to 85 % the runs are found but `BURST_DUTY_MAX` (0.5) keeps the
  window ungated. So `duty 1.00` in a reading does not mean the carrier was up throughout.
- **An envelope step is all the rules need.** A carrier up for a share `d` of the window has an
  envelope CV of `sqrt((1 - d) / d)`: 0.18 at 97 %, 0.23 at 95 %, 0.33 at 90 %.
  - From 0.25 (`AM_MIN_CV`) rule 4 reads the key edge as two envelope levels and, with any
    symbol-rate line, answers `ook`.
  - Rules 7 and 9 answer `am` when a carrier line stands at the centre, which an FM carrier in a
    pause between words has. Rule 9 starts at CV 0.1 and compares it with the phase deviation in
    radians per sample, a number that shrinks as the channel rate grows.
  - Rule 10 (`fm`) needs CV under 0.2 (`FM_MAX_CV`), so what the others leave is `unknown`.
- **A watch hits this by construction.** `classify_task` (`node/watch.rs`) classifies the newest
  `slice_s` of the ring the moment `detect` reports a new signal. That slice is noise, then the
  key-up.

A scratch probe (not committed) put a synthetic NBFM over through `measure_selection`: a 2 s
window at 48 kS/s, voice-like tones under a syllable envelope, at 2.5 and 5 kHz deviation, 15 and
36 dB.

| Carrier up for | Label | `duty` reported |
|---|---|---|
| the whole window | `fm` or `fsk` (both choose `fm_demod`) | 1.00 |
| all but 60 ms (97 %) | `am` in a quiet passage, else unchanged | 1.00 |
| 95 % | `am` or `unknown` | 1.00 |
| 90 % | `ook` or `am` | 1.00 |
| 75 % to 40 % | `ook` | 0.75 to 0.40 |
| the first or last 15 % | `am` or `ook` | 0.15 |

This matches the run: `am` at duty 1.00 and 36 dB on the repeater, `am` at duty 0.15 on the weaker
channel, `ook` and `unknown` at a key-up or key-down. What the probe cannot show is whether the
B210 at 76 dB adds real amplitude ripple on a strong carrier. The run's one pass at 60 dB was too
short to tell. 49.1 answers it with captures at both gains.

### 3.2 The rest

- **The choice** (`zeta-core/src/choice.rs`, `choose_now`):
  - A decoder row enters `could_prove` only when it is in range, fits the measured width, fits
    the layer-1 label (`fits_label`), is at least as narrow as the box
    (`band.fits(sel.bandwidth_hz)`) and is ready.
  - A demodulator row is a candidate only when it serves the label. The register's
    `allocation_term` is added to its score, but a row that does not serve the label is filtered
    out first, so the register cannot weigh against the label today.
  - A proved decoder row is the job, with the best demodulator as its runner-up.
- **The register and the plans.** `Classification.allocations` holds the register's assignments
  for the band, with a decoded emission designator. `modes/score.rs` maps a designator's letter
  to layer-1 labels (`DESIGNATOR_LABELS`). A channel plan (`plans.rs`) has a `deviation_hz` and no
  emission field.
- **Step 2 replays at the selection's band, not the row's.** `write_ring_slice` (`ops.rs`) records
  the slice with the selection's centre and width. `capture_decode_owned` (`node/mod.rs`) expands
  the row at the record's band. So a trial's channel is as wide as the box or the watch's
  `choice_band`, centred on its centre. Step 3 uses `job_for`, the row's width on the measured
  centre. The proved answer uses `job_for` too.
- **The slice is the ring's block.** `classify_samples` returns every sample of the span at the
  ring's rate. `StepOne` (`node/choose.rs`) keeps them until the verdict: 2 s at 20 MS/s is
  320 MB of memory. `write_slice` shortens the slice to the 64 MiB trial quota and refuses under
  1 s (`OVER_QUOTA`).
- **Two blocking locks around the write.** `write_slice` holds `Choosing::slice_gate`, a
  `std::sync::Mutex`, across the write. Inside it `BurstStore::write` (`bursts.rs`) holds the
  index mutex across `write_cf32`. Both run on a runtime worker, as `iq_read` does.
- **Trial slots.** `Inner::trial_slot` counts every trial against `budget.concurrent` (3 by
  default), first come first served. A step-3 trial holds its slot for its dwell.
- **The watch** (`node/watch.rs`):
  - `start_answer` drops an answer with no job (`choice_unknown`) before any `choose` starts.
  - `settle` replaces the kept job only on a proof of another kind.
  - Its `attach` keeps `transcribe` alone (`find(|k| *k == JobKind::Transcribe)`).
  - "Its choose's step 2 proved nothing" is said whenever the child is past step 2
    (`choose_past_two`), whether step 2 ran or not.
- **Attached jobs.** `JobKind::expand_attached` (`ops.rs`) knows `transcribe` and `packets`.
  `point_attached` (`node/launch.rs`) points a `stream_in` at the input job's audio or packet
  output. A demodulator's channelised IQ is an edge inside its graph, not a job output.
- **`zeta-dsp` depends on `zeta-core`**, not the other way round. The node reaches DSP through
  traits `zeta-core` defines and `zeta-dsp` implements: stages through the `StageRegistry`,
  classifiers through `Classifier` and `Node::add_classifier`. The dev profile optimises
  `zeta-dsp` and not `zeta-core`.
- **The page** (`zeta-ui/web/menu.js`): `pursueChoice` starts the transient `choose`, subscribes to
  its events and also reads `job_list --all` every 500 ms. Each answer calls `repaint`, which
  draws the whole menu again. `choiceLabel` (`actions.js`) gives the kind, the width and the row's
  name.
- **The UI suite**: 274 scenarios in four shards, about 343 s, the longest shard 327 s against
  `DEADLINE` 450 s (`crates/zeta/tests/ui.rs`). `menu_item` retries a path a repaint closed, and
  `still_menu` waits up to 20 s for the last answer (`zeta-ui/tests/e2e/ui.py`). `TODO.md` puts
  the growth on the repainting. Nothing here measured it.
- **Handler programs.** `probe_handler_files` (`modes/handlers.rs`) looks a program up on `PATH`
  and "runs nothing". `reload_modes` carries a probe over while the handler file is unchanged.
  `onair-rx --version` prints `onair-rx <x.y.z>` on its first line.
- **Strict clients.** `Choice`, `ChoiceJob` and the row types deny unknown fields. A new field is
  a release note, as plan 48's were. `weights.json` requires every key, so a new key without a
  default breaks an operator's file.

## 4. Approaches considered

### 4.1 The keyed carrier

1. **Label what was measured, and let the gate see a drop-out (chosen).** The label's rules read
   the same on-time samples as the measurement, and the gate finds the off part from either side.
   The cause is fixed where it is, and every reader of layer 1 gains: `classify`, the measure
   stage, the fit, identify. It is a DSP fix round, with its risk to the existing labels.
2. **Let the register and the plan overrule the label.** It needs no DSP change. But a station
   with no register, or a band with no assignments, keeps the fault, and the label and the
   measured features stay wrong for every other reader. Kept as the second line (49.2), not the
   fix.
3. **Have the watch wait and classify again.** It hides the key-up on a watch only. The menu, the
   agent and a key-down keep the fault, and every start is later.

### 4.2 Riding

1. **A `rides` field on the decoder row, run as an attached job (chosen).** The attached job is
   already the node's unit for "a job that lives on another job's stream": it stops with its
   input, follows its retune, and is drawn with it. The cost is one more stream kind for
   `point_attached`.
2. **Merge the decoder into the demodulator's graph.** One job, one channel. But the engine has
   no fan-out and a merged graph cannot be retuned (`TODO.md`, "Architecture: fan-out"), and the
   decoder could not be added or dropped without restarting the audio.
3. **A `wfm_rds` kind, or a flag on `wfm_demod`.** The N-variants shape the operator turned down.

### 4.3 Where the channeliser lives

1. **A trait in `zeta-core`, implemented in `zeta-dsp`, installed on the node (chosen).** It is
   how classifiers already arrive. `zeta-core` stays free of filter code, and the cut runs in the
   crate the dev profile optimises.
2. **Move `channel.rs` down into `zeta-core`.** No trait, but it drags SIMD filter code into the
   core crate and runs unoptimised in every `zeta-core` test.
3. **A leaf crate for the DSP kernels, under both.** The right move once a third crate needs
   them. Too large for one caller.
4. **Cut with a `channel` stage in a job graph.** A job, a bus and a store stage for a pure
   function over samples already in memory.

### 4.4 The UI suite

1. **Make the page do less (chosen):** revise the entry in place, and stop polling once the event
   stream is attached. Then rebalance the shards.
2. **A view setting that turns the menu's choose off, used by the tests.** Fast, but most
   scenarios would then run a page the operator does not have.
3. **A fifth shard, or a longer deadline.** It hides the cost. Kept as the fallback if 1 falls
   short.

## 5. The design

### 49.1 Layer 1 labels the carrier where it is up (M)

Touches `zeta-dsp/src/measure.rs`, `measure/burst.rs`, the generators in `synth.rs`, and the
tests in `zeta-dsp/tests/layer1.rs` and `classify.rs`. No operation changes shape.

- **One view for both halves.** `classify_block` takes the `View` the measurement used. Its
  envelope CV, spectrum, carrier line, discriminator audio and envelope levels come from the
  on-samples as received. The public `layer1_label(f, iq, rate)` builds the same view itself, so
  no caller can label a different span from the one it measured.
- **The gate finds the off part from either side.** A window whose chunk powers sit at two
  levels is split at the level, whichever level the 15th-percentile chunk is on. `duty` becomes
  the on share in every case, so a window with a 100 ms hole reads 0.95, not 1.00.
- **The gate applies at any duty** when the off chunks are noise (`off_is_noise`) and the runs
  are sparse (`BURST_MIN_PERIOD_MS`). Dense keying stays ungated, as now. What `BURST_DUTY_MAX`
  still guards is the builder's to find against the existing gate tests.
- **`ook` stays on-off keying.** It is answered by the run-length fit (rule 2, unchanged) or by
  two levels inside the measured samples (rule 4). A window gated into one or two long runs
  whose on-part is modulated is never `ook`.
- **A pause is a `carrier`.** An over whose slice holds no modulation reads `carrier`, as a
  steady unmodulated carrier does today. 49.2 is what turns that into `fm_demod` on a channel
  expected to be FM.
- **The evidence says how much was measured:** "over 0.31 s of 2.00 s".
- **Rule 9's phase test** is checked on the captures. If it still passes FM at a high channel
  rate once the gate is right, it is restated in Hz. If not, it is left alone.

The round follows `TODO.md`, "Process: lessons for DSP and layer-1 fix rounds":
- Fresh captures from the field host come first: a key-up, a key-down, a repeater mid-over and
  a pause, at 60 and at 76 dB, and one from a second radio. They stay out of the repository, as
  they hold speech.
- A generator for a keyed over goes into the repository.
- A base-against-head sweep of every existing layer-1 fixture is recorded in the plan's ledger.
  No existing fixture may change its label.

### 49.2 The expected emission weighs against the label (S feature)

Touches `zeta-core/src/choice.rs`, `modes/weights.rs`, `modes/weights.json`, `agents_md.rs`.

- **Two sources say what a channel is expected to carry:**
  - the register's `tx` assignments whose band holds the measured centre, by their designator's
    letter (`DESIGNATOR_LABELS`);
  - a channel plan of the station's region with a channel at the measured centre and a
    `deviation_hz`, which makes it an FM plan.
- **A demodulator row is a candidate** when it serves the layer-1 label, as now, or serves a
  label an expected emission is compatible with.
- **The label counts for less when it is contradicted.** When every expected emission at the
  centre is incompatible with the label, the label's term is multiplied by a new weight,
  `contradicted` (a factor from 0 to 1). A plan's FM counts as the register's `allocation` term
  does. The key has a default, so an operator's existing `weights.json` still loads.
- **The values** are the builder's, held by four cases:
  - `am` on a channel licensed or planned as FM answers `fm_demod`, with `am_demod` its runner-up;
  - `am` on a channel licensed as AM answers `am_demod`;
  - `carrier` or `unknown` on an FM channel answers `fm_demod`, not `cannot_tell`;
  - with no expectation, every answer is as before.
- **The evidence** gains `register: <designator> expected here (layer 1 read am)` or `plan: <id>
  is an FM plan (layer 1 read am)`: the designator and the plan id only.
- **Decoder rankings do not change.** `score_rows`, identify and a watch's nearest row are
  untouched.
- **It is still a `best_guess`.** On a watch the job starts held when a decoder could prove
  otherwise, as now.

### 49.3 A decoder row rides with a demodulator (M)

A decoder row may say which demodulators it rides with and which of their streams it reads:

```
"rides": { "with": ["wfm_demod"], "stream": "iq" }
```

- **`with`** names built-in demodulator kinds. An entry matches the built-in row and every
  variant row from `--modes-dir` that names the same `builtin`.
- **`stream`** is `iq`: the demodulator's channelised IQ, at its centre, width and rate. `audio`
  is the reserved second value, for a rider that reads what the listener hears. It is validated
  and refused until a row needs it.
- **The row** keeps everything else: its own decoder, proof, events, subject, test and handlers.
  Started by hand (`job_start --kind rds_decode`, the Decode entry, Send to) it runs as its own
  job, as today.

What riding changes:
- **The proof confirms the demodulator.** When a proved row rides with a ready demodulator row,
  the answer is the demodulator, `settled: proved`, sized by its own width rule and no narrower
  than the rider's `min_hz`. `ChoiceJob` gains `attach: ["<row id>"]`. The rider's
  `decode <row>: <summary>` line is in the evidence, so its fields (a station's name) are in the
  answer. One function in `choice.rs` makes this translation for a proof at step 1, 2 or 3.
- **A rider is never the runner-up of the demodulator it rides with.** It stays in `could_prove`.
- **Start gives both.** The consumer starts the job and then attaches each `attach` entry with
  `job_attach`: the page's `startChoice`, a `choose` with `start: true`, and the watch.
- **`job_attach` takes a rider row's id.** It refuses a job whose row the rider does not ride
  with, a band narrower than the rider's `min_hz`, and a rider that is not ready.
- **The attached graph** is the row's own graph with its root `channel` replaced by a
  `stream_in` of the input job's channel output. `point_attached` learns the `iq` kind.
- **On a watch:**
  - a proof of a rider of the kept job's row attaches the rider and lifts the hold; it does not
    replace the job;
  - `attach` takes a list of `transcribe` and rider row ids, each attached where it applies.
- **On the page:** the entry reads `Start wfm_demod here: 160 kHz, 75 kHz deviation · Wideband
  FM demodulator + RDS (<summary>)`. A running demodulator's menu offers each ready rider of its
  row. The attached job is drawn with its input, as a transcriber is.

Limits, stated:
- An attached rider runs the row's own in-process graph. A row's other handlers (an external
  program) still run only as its own job. Riding them needs the external adaptor to take a
  `stream_in`, which is a later item.
- **The builder's first check**: that a second job can subscribe to the channel edge of a running
  demodulator on both the bank path and the direct path, and that it follows a relaunch. If it
  cannot, the fallback is the rider's own `channel` on the demodulator's band, attached and
  holding no lease of its own. The operator sees the same thing either way.

Touches `modes/row.rs` (the field and its validation), `modes/summary.rs` (`modes_list`),
`choice.rs`, `node/choose.rs`, `node/watch.rs`, `ops.rs` (`expand_attached`, `job_attach`),
`node/launch.rs`, `actions.js` and `menu.js`, `agents_md.rs`, `modes/rds_decode.json`, and the
generated `AGENTS.md` and `SKILL.md`.

### 49.4 `unknown` is tried (S feature)

Touches `choice.rs`, `node/watch.rs`, `actions.js`.

- **For the label `unknown`, the label term is waived for `could_prove`.** A ready decoder row
  enters when it is in range, not passed over, and its width window holds one of three widths:
  the occupied width, `power99_hz`, or the selection's width (on a watch, the detector's). A weak
  carrier's occupied width is not trusted alone, as plan 48's width rule already says.
- **Such a row is tried, never guessed.** It does not become the `best_guess` job. The answer
  stays `cannot_tell`, or a demodulator by 49.2, with `could_prove` filled.
- **The watch tries it.** On an answer with no job and a non-empty `could_prove`, the watch
  starts its `choose` child with nothing kept and says so (event `choosing`, as `identifying`).
  A proof starts the proved job, `via: choose`. No proof is the unknown try it is today.
- **The menu** reads `Can't tell: no row serves unknown; trying dmr_decode, p25_decode`, revised
  when one proves.
- **The cost is bounded** by `budget.trial_s` and by 49.7. No new policy value.

### 49.5 A trial runs at its row's band, on a slice cut to it (M)

Touches `choice.rs`, `node/choose.rs`, `node/mod.rs` (`capture_decode_owned`), `ops.rs`
(`write_ring_slice`, `classify_samples`), a new trait in `zeta-core`, its implementation in
`zeta-dsp/src/channel.rs`, and the wiring in `crates/zeta`.

- **The box no longer filters.** `could_prove` drops `band.fits(sel.bandwidth_hz)`. A row is
  listed when the measured width fits it, as now, and its trial band lies inside the capture.
  The trial band is `job_for`'s: the row's width on the measured centre, or the selection as
  asked for an `as_asked` row. A row the capture does not hold goes to `not_tried` with `the
  capture does not hold <row>'s <width> Hz here`. `choose_now` is given the capture's span for
  this.
- **Step 2 replays each row at its own trial band.** `capture_decode_owned` takes a band to
  expand at, inside the recording. This also puts a step-2 trial on the measured centre, where
  step 3 and the proved job already are.
- **The slice is cut before it is written.**
  - Its band is the smallest that holds every `could_prove` row's trial band.
  - Its rate is the lowest exact divisor of the ring's rate that holds that band
    (`channel_rate_hz`).
  - The file is centred on that band.
  - The full-rate block is dropped as soon as the cut is made, so `StepOne` holds the cut only.
- **The cutter** is a trait in `zeta-core` (a band of a block in, the samples and their rate
  out), implemented in `zeta-dsp` over `extract_offset` and installed on the node by the `zeta`
  binary, as the classifiers are. With none installed the slice is written at the ring's rate,
  as today. The cut runs on a blocking thread.
- **What it buys:** at 20 MS/s a narrowband slice of 2 s is under 1 MB where it was 320 MB, so
  step 2 runs and `slice_s` is honoured. A row as wide as the capture (an 8 MHz multiplex at
  10 MS/s) gains nothing and still reads `not tried: the slice exceeds the trial quota`.
- **`iq_read` is unchanged.** It records what the operator asked for.

### 49.6 The answer says what ran (S fix)

Touches `choice.rs`, `node/choose.rs`, `node/watch.rs`, `actions.js`, the CLI's print.

- **`Choice` gains `steps_run`**: the ladder steps of which at least one trial started, `[1]`,
  `[1, 2]`, `[1, 3]` or `[1, 2, 3]`. `step` stays the step that settled the answer. A step
  skipped whole already has its reason in `not_tried`.
- **The watch's words follow it:** `its choose's step 2 proved nothing (step 3 goes on)` only
  when step 2 ran; else `its choose's step 2 did not run (<reason>); step 3 goes on`.
- **The first entry names the parameters `Start` sends:** `Start fm_demod here: 12.5 kHz, 5 kHz
  deviation · Narrow FM demodulator`, and the sideband for `ssb_demod`.
- **Its hover adds one line beside the runner-up:** `step 2: dmr_decode, p25_decode, no frame`,
  or `step 2 did not run: <reason>`.

### 49.7 Replays first, and no write under a lock (S fix)

Touches `node/choose.rs`, `bursts.rs`, `ops.rs` (`iq_read`).

- **Two classes of trial share `budget.concurrent`.** A step-3 trial may hold every slot but one.
  A waiting step-2 replay is served before a waiting step-3 trial. With `concurrent: 1` they
  share the one slot, replays first. A new signal's probation then waits about its slice's
  length, not behind another signal's dwell.
- **The slice write leaves the runtime.**
  - `BurstStore::write` writes the data file before it takes the index lock.
  - A trial reserves its bytes against the quota under a short lock, and writes outside it.
  - `write_slice` and `iq_read` run the write on a blocking thread.

### 49.8 The UI suite's time (S feature)

Touches `menu.js`, `actions.js`, `zeta-ui/tests/e2e/ui.py`, and `SHARD_STARTS`.

1. **Measure first.** One `ZETA_UI_PROFILE` run on the base commit, kept in the ledger, before
   any page change: the per-scenario times and the slowest lines. If the time is not in the
   menu's choose, the steps below are re-cut from what it shows.
2. **The entry is revised in place.** An answer rewrites the `choice_start` entry (its label,
   title, state and `revising` class), and the Listen group's order only when the chosen
   demodulator changed. The menu is not drawn again, its submenus do not close, and it is clamped
   only when it grew. The operator gains a menu that holds still under the pointer.
3. **One source of revisions.** The 500 ms `job_list --all` poll runs only until the event
   subscription is attached, and once more when it closes.
4. **The harness follows.** `menu_item` loses its retry, `still_menu` is kept for the scenarios
   that read the choice itself, and the scenarios that only click another entry stop waiting on
   it.
5. **Rebalance `SHARD_STARTS`** from a second profile run.

The target is a longest shard of 300 s or less locally, and a suite no slower than now with
this plan's own scenarios in it. This plan adds at most eight scenarios and folds the rest into
existing ones. A fifth shard is the fallback. The smoke set in `ci.yml` is updated with any
rename.

### 49.9 A handler program's least version (S fix)

Touches `modes/row.rs` (the program block), `modes/handler_file.rs`, `modes/handlers.rs`,
`node/mod.rs` (`reload_modes`), `handlers/onair.json`, `handlers/README.md`.

A handler file, and a program row's block, may carry beside `install`:

```
"version": { "args": ["--version"], "match": "^onair-rx (\\d+(?:\\.\\d+)*)", "min": "0.4.0" }
```

- **With the block,** the probe runs the found program once with `args`: no shell, no input, a
  short timeout, a capped read. It takes the first match's group as a dotted number and compares
  it with `min`.
  - Lower: the handler is `unavailable`, `onair-rx 0.2.0 is older than 0.4.0`, with the install
    hint.
  - No version read (a timeout, a non-zero exit, no match): `unavailable`, `its version could
    not be read`, with what happened. An old program that does not know the argument must not
    pass.
- **Without the block** nothing is run, as today. An operator's own files are unchanged.
- **A carried probe is kept** only while the program's path, size and modified time are the
  same, so installing a newer program and reloading the modes is enough.
- **The embedded `onair.json`** names 0.4.0, the version it is written for.
- **`zeta modes check` and the startup lines** show the version read.

## 6. Order, sizes and what is independent

Sizes are the operator's calibrated ones (`XS`, `S fix`, `S feature`, `M`).

| Part | Size | Depends on | Shares files with |
|---|---|---|---|
| 49.8 step 1, the profile | XS | nothing; first | |
| 49.1 layer 1 | M | captures from the field host | none (DSP only) |
| 49.9 version probe | S fix | nothing | none |
| 49.7 slots and the write | S fix | nothing | 49.5 (`choose.rs`) |
| 49.6 what ran | S fix | nothing | 49.5, 49.8 |
| 49.5 trial band and the cut | M | 49.7 | 49.4, 49.2, 49.3 (`choice.rs`) |
| 49.4 `unknown` | S feature | 49.5 | 49.2, 49.3 |
| 49.2 expected emission | S feature | 49.1 for its fixtures | 49.3 |
| 49.3 riding | M | 49.5 | 49.8 (`menu.js`) |
| 49.8 steps 2 to 5 | S feature | 49.6, 49.3 page work done | |

- **Three tracks can run side by side:** 49.1 (DSP); 49.9 (handlers); and the choice track in the
  order 49.7, 49.6, 49.5, 49.4, 49.2, 49.3, which all meet in `choice.rs` and `choose.rs`.
- **The page work is one sitting at the end:** 49.8's in-place revision first, then the labels of
  49.6, 49.4 and 49.3 on top of it, then the rebalance.
- **A rough total**, going by plans of this shape before it, is 3 to 3.5 M tokens with reviews.
  49.1 is the least certain part.

## 7. Testing

Per fix its own test and its neighbours; each crate's suite once per track; the workspace and
the UI suite once at the end.

- **49.1** (`zeta-dsp`, `--features test-fixtures`):
  - The keyed-over generator at carrier-up shares 0.15 to 0.97, as key-up, key-down and a hole,
    at 15 and 36 dB and at 2.5 and 5 kHz deviation, reads `fm` or `fsk`, never `am`, `ook` or
    `unknown`. Its `duty` is within 0.05 of the share.
  - An unmodulated keyed carrier reads `carrier`.
  - Every existing OOK, PSK, FSK, AM and SSB fixture keeps its label.
  - Through the operation: `classify` on a file source of a keyed over chooses `fm_demod`.
- **49.2** (`zeta-core`): the four cases of 49.2 as `choose_now` tests with made measurements and
  allocations, and an operator `weights.json` without the new key still loading.
- **49.3**:
  - A row test for `rides` validation.
  - A broadcast capture with RDS answers `wfm_demod`, proved, `attach: ["rds_decode"]`, with the
    station's name in the evidence.
  - `job_attach` of `rds_decode` to a running `wfm_demod` gives RDS events, follows a retune and
    stops with its input. It is refused on an `fm_demod`.
  - A watch's proof of a rider attaches and lifts.
  - A variant demodulator row from a modes directory is ridden.
- **49.4**:
  - The `dmr_iq` generator at 8 dB (or a capture, if the generator does not read `unknown`)
    answers `cannot_tell` with `dmr_decode` in `could_prove`.
  - A `choose` to step 2 tries it.
  - A watch starts a `choose` for it and, on a proof, the decoder. Noise starts nothing.
- **49.5**:
  - A 12 kHz box on a DMR capture lists `dmr_decode`.
  - Its replay's graph is 12.5 kHz wide on the measured centre.
  - At a ring rate of 20 MS/s a narrowband slice is 2 s long and under 1 MiB, and step 2 runs.
  - Each built-in row with a synthesised check still proves through a cut slice.
  - With no cutter installed the slice is as today.
- **49.6**: `steps_run` for a step 2 that ran and proved nothing, and for one refused by the
  quota; the watch's two sentences.
- **49.7**:
  - With every slot but one in step-3 dwells, a new replay starts at once.
  - Two slices written at once do not serialise on the write (a slow store in the test).
  - Polled, not read once, as `CLAUDE.md`'s note on CI flakes says.
- **49.8**: the profile before and after in the ledger; the choice scenarios pass ten times in a
  row under `taskset -c 0,1`.
- **49.9**: a stub program that prints an old version, a new one, nothing, and one that hangs: the
  four states and their reasons; a replaced program is probed again on reload.
- **Gates** at the end: fmt, clippy, the workspace tests, the whole UI suite locally,
  `agents-md --check` both ways, `public-check.sh`, `check-doc-links.sh`.

## 8. Acceptance checks (for `UAT.md`, plan 49)

Each is run on the web page of a node on this build, with a radio on an FM voice band and one on
the broadcast band.

- [ ] 49.1 On a channel with live FM voice traffic, open the box menu five times: during an
  over, right at a key-up, right at a key-down, in a pause between words, and between overs. The
  first entry reads `Start fm_demod here: ...` every time a carrier is in the box, and `Can't
  tell: the band holds noise only` when none is. It never names `am_demod`.
- [ ] 49.2 Start a watch with `on_signal: choose` over that band from the page and leave it for
  twenty overs. Every job in the Jobs pane is `fm_demod` or a proved decoder. Repeat at a
  lower gain on the radio.
- [ ] 49.3 With a register configured, draw a box on a licensed FM channel during a pause in
  speech. The entry is `fm_demod`, and its hover names the expected emission. On an airband AM
  channel it is `am_demod`.
- [ ] 49.4 Draw a box on a broadcast station that sends RDS. The first entry reads `Start
  wfm_demod here: ... + RDS (<station name>)`, at once or after a few seconds. Click it: the
  audio plays, the Jobs pane shows `rds_decode` attached under the `wfm_demod`, and the station's
  name appears. Stop the `wfm_demod`: both go.
- [ ] 49.5 On a running `wfm_demod` started by hand, open its menu and attach RDS from there.
- [ ] 49.6 Draw a box about 12 kHz wide on a 12.5 kHz digital voice channel. The entry's hover
  lists the digital voice rows as tried, and a live call revises the entry to its decoder.
- [ ] 49.7 Find a weak digital carrier that reads `unknown` in What is this?. Open the menu on
  it: the entry reads `Can't tell: ...; trying ...`. A watch with `on_signal: choose` over it
  shows `choosing` for that signal, and a decoder if one proves.
- [ ] 49.8 Set a radio to its widest capture and draw a box on a narrow signal. The entry's
  hover reads `step 2: ...` with the rows it replayed, not `step 2 did not run`.
- [ ] 49.9 On a keyed FM channel the entry reads `..., 5 kHz deviation ...`, or the plan's
  deviation on a plan channel, and the started job's row shows the same deviation.
- [ ] 49.10 Open the box menu and open its Decode submenu before the first entry settles. The
  submenu stays open and nothing moves when the entry changes.
- [ ] 49.11 Put an older `onair-rx` first on the node's `PATH` and reload the modes. On the page,
  the Decode entries that only OnAir serves are greyed with `onair-rx <old> is older than
  <min>`. Restore it and reload: they are offered again.

## 9. Open questions for the operator

Each has the default this design is written to.

1. **Does a rider attach only once it is proved, or whenever it is ready?**
   Default: only when proved, with the entry in the running job's menu and a watch's `attach`
   list for the rest. The alternative attaches RDS to every `wfm_demod` the choice starts. Every
   station with RDS then shows its name without a second click, at the cost of an RDS decoder
   running on stations without it.
2. **May the register or a plan overturn layer 1's label, or only name the runner-up?**
   Default: it may overturn, when every expected emission at the centre contradicts the label,
   by a weight in `weights.json`. The alternative keeps the label's answer and puts the expected
   demodulator in the runner-up. Then a pause in an over on a licensed FM channel stays
   `cannot_tell`, and a rare real AM signal on an FM channel is not mis-started.
3. **Should a watch spend trial time on every `unknown` signal?**
   Default: yes, to its policy's step, within `budget.trial_s`. The alternative stops at step 2
   for `unknown`. Then a carrier that keyed off before its slice was cut is never tried on air,
   and a busy band spends no dwells on noise-like signals.
4. **A version floor, or a list of what the program can do?**
   Default: the version floor `TODO.md` asks for, and a program whose version cannot be read is
   `unavailable`. The alternative is a probe that asks the program for its modes, closer to how
   the speech server is probed. It needs a new argument in each wrapper and a release of each, and
   it would mark single modes unavailable where a floor marks the whole handler.
5. **How do step-2 replays get ahead of step-3 dwells?**
   Default: one slot is kept for replays and they are served first. The alternative leaves a
   watch's own step-3 trial out of the cap. That is simpler, and the node's trials are then no
   longer bounded by `budget.concurrent`.
6. **Is a view setting that turns the menu's choose off wanted?**
   Default: no. The alternative is `view set` gaining it for a slow host. The tests could then
   use it, and most scenarios would no longer exercise the page as it ships.
7. **Should a slowly keyed bare carrier (hand-sent Morse) read `ook` or `carrier`?**
   Default: `carrier`, with its keying in `duty`, `bursts` and `burst_ms`, since no row serves
   either label today. The alternative keeps `ook` for it, which needs a rule that tells a keyed
   carrier from an over's edge by the count and length of its runs.
8. **Where do the voice captures for 49.1 live?**
   Default: on the field host, outside the repository, with generators in the repository
   standing for them. The alternative, a capture of the operator's own test transmission, could be
   committed and would make the fixture real.

## 10. Left to the builder

- The statistic that splits a two-level window, and what `BURST_DUTY_MAX` still guards.
- Whether rule 9's phase test needs restating once the gate is right.
- The values of `contradicted` and of a plan's term, inside the four cases of 49.2.
- Whether a rider reads the demodulator's channel edge or runs its own channel (49.3's first
  check).
- The cutter trait's name and place in `zeta-core`, and the margin its rate leaves the filter.
- The words of the new `not_tried` reasons and of the entry's hover.
- The version comparison for a version with a suffix.
