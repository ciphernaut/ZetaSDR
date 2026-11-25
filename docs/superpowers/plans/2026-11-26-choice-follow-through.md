# Choice Follow-Through (Plan 49) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

## RESUME HERE

The runner updates this section after every task and commits it with the task. A fresh session
reads it first.

- **Branch and worktree:** trunk `worktree-agent-a5dfd60975f4d81eb`; tracks A and B on `plan49-a`, `plan49-b`. The run ledger is `.superpowers/sdd/2026-11-26-choice-follow-through/progress.md` in the trunk worktree.
- **Private target dir:** (fill in; delete it when the gates are done)
- **Ledger of findings:** `scratch/plan49/ledger.md` in the main checkout (ignored by git): the
  capture list, both sweeps, both UI profiles, A1's and C9's findings.
- **Next task:** P1 first; then A1, A2, B1 and C1 can all start.
- **Waiting on the operator:** nothing yet. A1 may need a go-ahead (see the task).

| Task | State | Commit | Note |
|---|---|---|---|
| P1 | todo | | |
| A1 | todo | | |
| A2 | todo | | |
| A3 | todo | | |
| A4 | todo | | |
| A5 | todo | | |
| B1 | todo | | |
| C1 | todo | | |
| C2 | todo | | |
| C3 | todo | | |
| C4 | todo | | |
| C5 | todo | | |
| C6 | todo | | |
| C7 | todo | | |
| C8 | todo | | |
| C9 | todo | | which branch C11 takes: |
| C10 | todo | | |
| C11 | todo | | |
| C12 | todo | | |
| C13 | todo | | |
| P2 | todo | | |
| P3 | todo | | |
| P4 | todo | | |
| P5 | todo | | |
| F1 | todo | | |
| F2 | todo | | |

---

**Goal:**
- A keyed FM channel answers `fm_demod` whatever part of an over the slice holds (UAT 48.6).
- A decoder row can ride with a demodulator; `rds_decode` rides with `wfm_demod`.
- `unknown` is tried, a narrow box keeps its decoders, and step 2 runs on a wide capture.
- The answer says which steps ran; replays go before dwells; an old handler program is
  `unavailable`; the UI suite's longest shard is 300 s or less.

**Architecture:**
- **Layer 1** labels the same on-time samples it measured, and its gate finds a drop-out.
- **The choice** admits a demodulator by the expected emission, lists rows for `unknown`, and
  translates a rider's proof into its demodulator with the rider attached.
- **Trials** run at each row's own band, on a slice cut to that band by a cutter `zeta-dsp`
  installs on the node.
- **Riding** is a row field; the rider runs as an attached job on the demodulator's channel IQ.
- **The page** revises the choice entry in place.

**Tech Stack:** Rust 1.97 (`zeta-dsp`, `zeta-core`, `zeta`), the plain-JS page
(`crates/zeta-ui/web`), Python Playwright scenarios (`crates/zeta-ui/tests/e2e/ui.py`) started by
`crates/zeta/tests/ui.rs`, JSON rows and handler files.

**Spec:** `docs/superpowers/specs/2026-11-25-choice-follow-through-design.md` (approved
2026-11-26). Cited below as "spec 49.5", "spec 3.1" and so on. A task's subagent reads the
sections its task names, and section 2's anti-goals, before it writes anything.

**Scope:** spec parts 49.1 to 49.9. Out: plan 48.5 (`borrow`), the layer-3 model, replaying a
capture into a demodulator, the ribbon's timing, and plan 48's two other leftovers (spec 2).

## Tracks and order

| Track | Tasks, in order | Files it owns |
|---|---|---|
| A, layer 1 | A1, A2, A3, A4, A5 | `crates/zeta-dsp/src/measure.rs`, `measure/burst.rs`, `synth.rs`, `tests/layer1.rs` |
| B, handlers | B1 | `crates/zeta-core/src/modes/{handlers,handler_file}.rs`, the program block in `row.rs`, `handlers/` |
| C, the choice | C1 to C13 | `crates/zeta-core/src/{choice,bursts,ops}.rs`, `node/{choose,watch,launch,mod}.rs`, `modes/{row,summary,weights}.rs` |
| P, the page | P1 first, then P2 to P5 after C13 | `crates/zeta-ui/web/{menu,actions}.js`, `ui.py` |
| F, the end | F1, F2 | docs, `UAT.md`, `TODO.md`, generated files |

- **Parallel:** tracks A, B and C touch no file in common except `modes/row.rs`: B1 adds a field
  to `ProgramBlock`, C10 adds one to `ModeRow`. Run B1 before C10 starts, or rebase C10 on it.
- **Never parallel:** two tasks of track C (they meet in `choice.rs`, `choose.rs` and `watch.rs`),
  or two tasks of track P.
- **P1** runs on the base commit, before any other task changes the page. It is the first task
  of the run.
- **C8** takes A2's generator for one test. If A2 has not landed, C8 writes that test
  `#[ignore]`d with a note in the ledger, and A5 removes the `ignore`.

## Global Constraints

- **One operation set.** New fields go in `crates/zeta-core/src/ops.rs` or the types it serves;
  the transports only translate.
- **Generated files.** `AGENTS.md` and `skills/zetasdr/SKILL.md` are regenerated in F1 by
  `cargo run -q -p zeta -- agents-md > AGENTS.md` and
  `cargo run -q -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md`. Never by hand, and not
  in any earlier task.
- **Existing behaviour holds.** Every existing layer-1 fixture keeps its label. `score_rows`,
  identify's ranking and a watch's nearest row are unchanged. A handler file without a `version`
  block is probed as now, with nothing run. A row without `rides` behaves as now.
- **Never proof from evidence.** The register and a plan rank. Only a decode passing its row's
  own rule proves.
- **An agent never stops, replaces or retunes a job it did not start.** This binds the runner
  on the live node (A1) as it binds the code: a watch replaces or adds to only its own child.
- **No station facts.** No frequency, gain, site, callsign or device rate from the field host in
  any tracked file, test name or commit message. Thresholds come from generators.
- **No raster.** Jobs, trials and slices are centred on the measured centre.
- **New fields on answer and row types** take `#[serde(default)]`, so an older client's file or
  request still reads. A new `weights.json` key has a default.
- **Exact names and values** (spec 5):
  - `Choice.steps_run: Vec<u8>`; `ChoiceJob.attach: Vec<String>`;
  - row field `rides: { "with": [<demodulator kind>], "stream": "iq" }`;
  - weights key `contradicted`, a factor from 0 to 1;
  - version block `{ "args": [...], "match": "<regex with one group>", "min": "<dotted>" }`,
    probe timeout 2 s, read cap 4096 bytes;
  - a step-3 trial may hold at most `max(1, budget.concurrent - 1)` slots;
  - the watch's event for a signal it is trying with nothing kept: `choosing`.
- **Exact strings:**
  - `not tried: the capture does not hold <row>'s <width> Hz here`
  - `its choose's step 2 proved nothing (step 3 goes on)`
  - `its choose's step 2 did not run (<reason>); step 3 goes on`
  - `<program> <version> is older than <min>`
  - `<program>: its version could not be read (<what happened>)`
  - evidence `register: <designator> expected here (layer 1 read <label>)`
  - evidence `plan: <id> is an FM plan (layer 1 read <label>)`
- **Public repository.** Roles, not host names. Captures of real traffic never enter git.
  `scripts/public-check.sh` and `scripts/check-doc-links.sh` clean before the branch lands.
- **Gates** (`CLAUDE.md`):
  - a private `CARGO_TARGET_DIR` for every cargo run of this plan, deleted when F2 is done;
  - `cargo fmt -p <crate> -- --check` per changed crate (a nested worktree);
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings` once per track and
    in F2;
  - `zeta-core` and `zeta-dsp` test runs take `--features test-fixtures`.
- **Testing policy.** A task runs its own tests and the neighbours it names. A crate's suite
  runs once at the end of its track (A5, B1, C13). The workspace suite, the whole UI suite, both
  `agents-md --check` runs and the two scripts run once, in F2.
- **Polling.** A test that reads state after an operation polls for the end state
  (`common::until`, ui.py `wait_until`).
- **Commit messages** start `Choice 49: <what changed>`.
- **A task is done** when its tests pass, the neighbours it names pass, fmt is clean for its
  crates, its checkbox steps are ticked, and RESUME HERE has its row.

## Review Focus

- **A window that is all carrier, or all noise, or shorter than one transform.** The new gate
  finds no second level: the view is the whole window and every label is as before. (A3)
- **A capture near the ring's edge.** A row's trial band reaches past the capture: the row is in
  `not_tried` with the capture string, the others are still tried, and the cut never reads
  outside the block. (C5)
- **The demodulator a rider is attached to is retuned, relaunched by a capture change, or
  stopped.** The rider follows a retune, comes back with a relaunch, and ends with a stop; it is
  never left running on a stream that is gone. (C11)
- **A version probe against a program that hangs, prints megabytes, or is replaced while the
  node runs.** The node's start is delayed by at most the timeout per file, the read is capped,
  and a reload probes the new program. (B1)
- **A menu closed, or a watch stopped, while a `choose` with nothing kept is still trying an
  `unknown`.** The `choose` and its replays stop, no trial slice stays pinned, and no job is
  started after the stop. (C7, P3)

---

## Track P, first task

### P1: Profile the UI suite on the base commit (XS, 50k)

**Read:** spec 3.2 (the page, the UI suite) and 49.8.

**Files:** none in the repository. Output: `scratch/plan49/ui-profile-base.txt` and a summary in
the ledger.

- [ ] **Step 1:** On the plan's base commit, with a private target dir, run
  `ZETA_UI_PROFILE=<abs path>/ui-profile-base.txt cargo test -p zeta --test ui -- --nocapture`.
  Check the output for `ui e2e skipped:`; a skipped run is not a result.
- [ ] **Step 2:** Write to the ledger:
  - each shard's wall time;
  - the twenty slowest scenarios;
  - the share of the total spent in lines of `menu_item`, `still_menu`, `drag_menu`,
    `open_menu_on_tone` and in waits on `choice_start`.
- [ ] **Step 3:** State the finding in one line in RESUME HERE: either "the menu's choose costs
  about N s of M s" or "the time is elsewhere: <where>". P2 and P5 are written against it.

**Done:** the profile file exists, the ledger has the three items, RESUME HERE has the line.

---

## Track A: layer 1 (spec 3.1, 49.1)

A DSP fix round. `TODO.md`, "Process: lessons for DSP and layer-1 fix rounds", is required
reading for every task of this track.

### A1: Captures of keyed FM from the field host (S fix, 150k)

**Read:** spec 3.1, 49.1, and section 9 ruling 8.

**This task needs the live node and the operator's word.** The runner:
- lists the node's jobs and each source's capture first, and writes them to the ledger;
- **never stops, replaces or retunes a job it did not start.** `replace` is never sent;
- reads with `iq_read` only from a capture that is already running and already holds an FM
  voice channel. `iq_read` moves nothing;
- if no running capture holds such a channel, or a second gain is needed, **stops and asks the
  operator**: a retune, a pin, a new capture on an idle radio and a gain change each need their
  go-ahead, named one by one. RESUME HERE gets the request under "Waiting on the operator".
  Tracks B and C go on meanwhile; A2 does not wait for A1;
- stops every job it started, restores a gain it changed, and confirms the job list equals the
  one it wrote down.

**Wanted**, each 2 to 4 s of IQ around one channel, with its node build and the radio's gain
noted:
- a key-up, a key-down, a repeater mid-over, a pause between words, a whole over;
- at the gain the operator runs and at one about 16 dB lower;
- at least one from a second radio.

**Kept** outside the repository, in a directory on the field host the ledger names. The files
hold speech and an observed frequency: no file, name, frequency or site goes into git, the plan
or a commit message.

- [ ] **Step 1:** List jobs and captures; decide whether a go-ahead is needed; ask if so.
- [ ] **Step 2:** Record. For each capture note in the ledger what it holds (by the list above),
  its length, rate and gain. No frequency.
- [ ] **Step 3:** Put each through `zeta classify` (or `capture_classify`) on the current build
  and note the label and `duty`: this is the "base" column A5 compares with.
- [ ] **Step 4:** Restore and confirm the job list.

**Done:** at least five of the wanted kinds exist at one gain and three at the other, the
ledger lists them with base labels, and the node's jobs are as they were. Nothing to commit but
RESUME HERE.

### A2: The keyed-over generator, the sweep tool, and the failing tests (S fix, 200k)

**Read:** spec 3.1 and 49.1.

**Files:**
- Modify: `crates/zeta-dsp/src/synth.rs` (the generator), `crates/zeta-dsp/src/fixtures.rs`
  (re-export, as the other generators are), `crates/zeta-dsp/Cargo.toml` (the example).
- Create: `crates/zeta-dsp/examples/l1_sweep.rs` (`required-features = ["test-fixtures"]`).
- Test: `crates/zeta-dsp/tests/layer1.rs`.

**Interfaces, produces:**

```rust
pub struct KeyedOver { pub rate_hz: u32, pub seconds: f32, pub deviation_hz: f64,
    /// 1.0: speech at full deviation; 0.1: a quiet passage; 0.0: an unmodulated carrier.
    pub audio: f32,
    /// The carrier is up over this share of the window, `(from, to)` in 0..=1.
    pub on: (f64, f64), pub snr_db: f32, pub seed: u64 }
pub fn keyed_over(spec: &KeyedOver) -> Vec<Complex<f32>>;   // noise everywhere, the carrier added over `on`
```

The audio is three tones (about 430, 1130 and 2210 Hz) under a 4 Hz syllable envelope, through
`nbfm_modulate`.

`l1_sweep` prints one line per input, `<name> <label> duty=<d> occ=<hz> snr=<db>`, through
`measure_selection` (the real measure path): every fixture `tests/layer1.rs` builds, every file
in `crates/zeta-dsp/tests/data/`, and each `*.sigmf-meta` in directories given as arguments.

- [ ] **Step 1: Write the failing tests** in `tests/layer1.rs`, each through `measure_selection`
  over a 2 s window at 48 kS/s, band 12.5 kHz at 2.5 kHz deviation and 25 kHz at 5 kHz:

```rust
#[test] fn a_keyed_over_reads_fm_at_every_carrier_up_share()
// on = (1-d, 1) and (0, d) for d in [0.97, 0.95, 0.9, 0.75, 0.6, 0.4, 0.15]; a hole: on (0,1) minus 0.45..0.55
// audio in [1.0, 0.1]; snr in [36, 15]: label is "fm" or "fsk"; never "am", "ook", "unknown"
#[test] fn a_keyed_overs_duty_is_its_carrier_up_share()        // |duty - d| <= 0.05 for the same cases
#[test] fn an_unmodulated_keyed_carrier_reads_carrier()         // audio 0.0, d in [0.9, 0.4]: "carrier"
#[test] fn the_label_and_the_record_describe_the_same_samples() // evidence holds one cv; it equals the view's
```

- [ ] **Step 2: Run**
  `cargo test -p zeta-dsp --features test-fixtures --test layer1 -- a_keyed an_unmodulated the_label`.
  Expected: the four FAIL (labels `am`, `ook`, `unknown`; duty 1.00). Mark them
  `#[ignore = "plan 49 A3/A4"]` so the branch stays green, and note it in RESUME HERE.
- [ ] **Step 3: The base sweep.**
  `cargo run -q -p zeta-dsp --features test-fixtures --example l1_sweep > scratch/plan49/sweep-base.txt`,
  and once more with A1's directory when it exists. Keep both in the ledger directory.
- [ ] **Step 4: Run** `--test layer1` whole. Expected: PASS, four ignored.
- [ ] **Step 5: Commit** `Choice 49: a keyed-over generator, the layer-1 sweep, and the tests the fix must pass`.

### A3: The gate finds the off part from either side (S fix, 200k)

**Read:** spec 3.1 (second bullet) and 49.1 (second and third bullets).

**Files:** `crates/zeta-dsp/src/measure/burst.rs` (`burst_runs`, `gated_view`, the constants);
tests in `measure.rs`'s own `mod tests` (the gate tests live there).

**Interfaces:** `Bursts` and `gated_view` keep their signatures. `Bursts.duty` becomes the on
share in every case; `Bursts.runs` are the on parts.

- [ ] **Step 1:** Remove the `ignore` from `a_keyed_overs_duty_is_its_carrier_up_share`. Add in
  `measure.rs` tests:

```rust
#[test] fn a_hole_in_a_carrier_is_an_off_run()            // carrier with a 100 ms gap: 2 runs, duty about 0.95
#[test] fn a_window_of_one_level_is_one_run()             // all carrier, and all noise: 1 run, duty 1.0
#[test] fn a_keyed_window_is_gated_at_any_duty()          // d = 0.75: gated_view(..).gated, on-time holds no noise chunk
```

- [ ] **Step 2: Run** them. Expected: FAIL.
- [ ] **Step 3: Implement.** A window whose 1 ms chunk powers sit at two levels at least
  `BURST_ON_DB` apart is split at the level, whichever level the `FLOOR_PERCENTILE` chunk is
  on. The gate applies at any duty when `off_is_noise` and the runs are sparse. The statistic is
  yours; what `BURST_DUTY_MAX` still guards is found against the existing gate tests
  (`dense_on_off_keying_is_not_gated`, `two_clicks_over_a_continuous_signal_do_not_gate`,
  `a_lone_1_ms_outlier_chunk_is_not_gated`, `channel_filtered_noise_still_lets_a_burst_gate`),
  none of which may change.
- [ ] **Step 4: Run** `cargo test -p zeta-dsp --features test-fixtures --lib measure` and
  `--test layer1`. Expected: PASS, three still ignored.
- [ ] **Step 5: Commit** `Choice 49: layer 1's gate finds a drop-out, and duty is the on share`.

### A4: The label reads the view the measurement used (S fix, 250k)

**Read:** spec 3.1 (first and third bullets), 49.1, and section 9 ruling 7.

**Files:** `crates/zeta-dsp/src/measure.rs` (`layer1_label`, `layer1_label_noted`,
`classify_block`, `measure_selection`, `carry_extras`, the measure stage's `window_event`).

**Interfaces:**
- `classify_block` takes the `View` (`burst::View`): its CV is `View::cv`; its spectrum,
  carrier line, discriminator audio and envelope levels are over the on-samples as received.
- `pub fn layer1_label(f, iq, rate_hz) -> Label` keeps its signature and builds the same view
  itself.
- The label's evidence gains `over <on> s of <window> s` and holds one CV.

**Rules:**
- `ook` is answered by the run-length fit (rule 2, unchanged) or by two levels inside the
  measured samples (rule 4). A window gated into one or two long runs whose on-part is modulated
  is never `ook`.
- A keyed bare carrier reads `carrier`.
- The doc comment listing the rules is rewritten to say what each rule now reads.

- [ ] **Step 1:** Remove the `ignore` from the other three A2 tests. Add:

```rust
#[test] fn every_ook_fixture_still_reads_ook()   // the real remotes in tests/data and synth's OOK: unchanged labels
```

- [ ] **Step 2: Run.** Expected: the three FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `--lib measure`, `--test layer1`, `--test classify`. Expected: PASS, none
  ignored.
- [ ] **Step 5: Commit** `Choice 49: layer 1 labels the samples it measured`.

### A5: The head sweep, the captures, rule 9, and the operation (S fix, 200k)

**Read:** spec 49.1 (last bullet and the fix-round list), spec 7 (49.1).

**Files:** `crates/zeta-dsp/src/measure.rs` only if rule 9 needs it; test in
`crates/zeta-core/tests/choice.rs`.

- [ ] **Step 1: The head sweep.** Run `l1_sweep` as A2 did into `sweep-head.txt`. `diff` it
  with the base. Expected: no line of an existing fixture differs in its label. A difference is
  a failure of A3 or A4: fix it there, do not edit a fixture's expectation.
- [ ] **Step 2: The captures.** Run the sweep over A1's directory. Write base and head labels
  side by side in the ledger. Expected: every capture that holds a carrier reads `fm`, `fsk` or
  (a pause) `carrier`, at both gains. If A1 is still waiting on the operator, say so in RESUME
  HERE and leave this step open; the branch does not land with it open.
- [ ] **Step 3: Rule 9.** If a capture or a generator case still reads `am` through rule 9 (its
  evidence names `phase rms`), restate the phase test in Hz and add a test at 48 kS/s and at
  192 kS/s. If none does, change nothing and say so in the ledger.
- [ ] **Step 4: Through the operation.** In `tests/choice.rs`:

```rust
#[tokio::test] async fn classify_on_a_keyed_over_chooses_fm_demod()
// a file source of keyed_over (d = 0.9 key-up, then d = 0.15): choice.job.kind == "fm_demod" both times
```

  Run `cargo test -p zeta-core --features test-fixtures --test choice -- --exact classify_on_a_keyed_over_chooses_fm_demod`.
- [ ] **Step 5: Track gate.** `cargo test -p zeta-dsp --features test-fixtures`, fmt for
  `zeta-dsp`. Expected: PASS.
- [ ] **Step 6: Commit** `Choice 49: the layer-1 sweep holds every fixture's label; a keyed over chooses fm_demod`.

---

## Track B: handlers (spec 49.9)

### B1: A handler program's least version (S feature, 375k)

**Read:** spec 3.2 (handler programs), 49.9, section 9 ruling 4.

**Files:**
- Modify: `crates/zeta-core/src/modes/row.rs` (`ProgramBlock`, `validate_program`),
  `modes/handler_file.rs` (`HandlerFile`, `parse_handler_file`), `modes/handlers.rs`
  (`probe_handler_files`, the row-program probe beside it), `modes/mod.rs` (`ModeHandler`,
  `carry_programs`), `crates/zeta-core/src/node/mod.rs` (the two doc comments that say "runs
  nothing"), `handlers/onair.json`, `handlers/README.md`.
- Test: `crates/zeta-core/tests/modes.rs`.

**Interfaces, produces:**

```rust
pub struct VersionProbe { pub args: Vec<String>, #[serde(rename = "match")] pub pattern: String, pub min: String }
// HandlerFile.version: Option<VersionProbe>  and  ProgramBlock.version: Option<VersionProbe>, both #[serde(default)]
pub fn probe_version(program: &Path, v: &VersionProbe) -> Result<String, String>;  // Ok(version read) or Err(reason)
pub fn version_at_least(read: &str, min: &str) -> bool;                             // dotted numbers; a suffix is ignored
```

**Rules:**
- With a block: run the found program once with `args`; no shell, stdin closed, 2 s timeout,
  at most 4096 bytes of stdout and stderr read; the first match's first group is the version.
- Lower than `min`: `unavailable`, reason `<program> <version> is older than <min>` then the
  install hint. Not read: `unavailable`, `<program>: its version could not be read (<what>)`.
- Without a block: nothing is run.
- A carried probe is kept only while the program's path, size and modified time are unchanged.
- A `pattern` that is not a regex with one group, or a `min` that is not dotted numbers, breaks
  the file at load.
- `handlers/onair.json`: `"version": {"args": ["--version"], "match": "^onair-rx (\\d+(?:\\.\\d+)*)", "min": "0.4.0"}`.
- The startup line and `zeta modes check` show the version read.
- Check `crates/zeta-core/tests/no_privileged_deps.rs` before adding a regex dependency; use
  one the workspace already has if there is one.

- [ ] **Step 1: Write the failing tests** in `tests/modes.rs`, with stub programs written as
  executable scripts in a temp dir (as the existing handler tests make theirs):

```rust
#[test] fn a_handler_older_than_its_file_needs_is_unavailable_with_both_versions()
#[test] fn a_handler_at_or_over_its_floor_is_ready_and_its_version_is_reported()
#[test] fn a_handler_whose_version_cannot_be_read_is_unavailable()   // exits 2 with usage; prints nothing
#[test] fn a_version_probe_that_hangs_is_cut_at_its_timeout()         // sleeps 30 s: returns within 3 s, unavailable
#[test] fn a_handler_file_without_a_version_block_runs_nothing()      // the stub writes a marker file when run: none
#[test] fn a_replaced_program_is_probed_again_on_reload()             // old stub, reload unchanged: carried; swap: ready
#[test] fn a_bad_version_block_breaks_the_file_alone()
#[test] fn a_dotted_version_compares_by_number()                      // 0.10.0 >= 0.4.0; 0.4 >= 0.4.0; 0.4.0-rc1 reads 0.4.0
```

- [ ] **Step 2: Run**
  `cargo test -p zeta-core --features test-fixtures --test modes -- version handler_older replaced_program a_dotted`.
  Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p zeta-core --features test-fixtures --test modes` and
  `cargo test -p zeta --test modes_check`. Expected: PASS. An environment with `onair-rx` set
  through `ZETA_TEST_ONAIR_RX` runs the OnAir tests against the real program: it must be 0.4.0.
- [ ] **Step 5: Commit** `Choice 49: a handler file names the least version its program must report`.

---

## Track C: the choice (spec 49.7, 49.6, 49.5, 49.4, 49.2, 49.3)

One task at a time, in this order.

### C1: Replays are served before dwells (S fix, 200k)

**Read:** spec 3.2 (trial slots), 49.7 first bullet, section 9 ruling 5.

**Files:** `crates/zeta-core/src/node/choose.rs` (`Choosing`, `TrialSlot`, `Inner::trial_slot`,
its callers `replay_trial` and `step_three`). Test: `crates/zeta-core/tests/choose.rs`.

**Interfaces:**

```rust
pub(super) enum TrialClass { Replay, Live }
async fn trial_slot(self: &Arc<Self>, class: TrialClass, cancel: &CancellationToken, deadline: Instant) -> Option<TrialSlot>;
```

**Rules:** all trials share `budget.concurrent`. `Live` may hold at most
`max(1, concurrent - 1)`. A `Live` waiter is not served while a `Replay` waits.

- [ ] **Step 1: Failing tests:**

```rust
#[tokio::test] async fn a_replay_starts_while_live_trials_hold_every_slot_but_one()
#[tokio::test] async fn a_waiting_replay_is_served_before_a_waiting_live_trial()
#[tokio::test] async fn with_one_slot_live_and_replay_share_it()        // concurrent: 1, nothing deadlocks
```

  Use the test-fixtures hooks already there (`test_trial_peak`); add one counting peaks per
  class if needed. Poll, never read once.
- [ ] **Step 2: Run** `cargo test -p zeta-core --features test-fixtures --test choose -- replay live_and`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `--test choose`. Expected: PASS.
- [ ] **Step 5: Commit** `Choice 49: a step-2 replay never queues behind step-3 dwells`.

### C2: No slice is written under a blocking lock (S fix, 200k)

**Read:** spec 3.2 (two blocking locks), 49.7 second bullet.

**Files:** `crates/zeta-core/src/bursts.rs` (`BurstStore::write`, a reservation),
`node/choose.rs` (`write_slice`, `Choosing::slice_gate` goes), `ops.rs` (`iq_read`,
`write_ring_slice`). Tests: `bursts.rs`'s own tests, `tests/choose.rs`.

**Interfaces:**

```rust
impl BurstStore { pub fn reserve_trial(&self, bytes: u64) -> Option<TrialReservation>; }  // None: no room beside pinned and reserved
// TrialReservation gives its bytes back on drop; `write` takes it for a trial capture.
```

**Rules:** `BurstStore::write` writes the data file before it takes the index lock.
`write_slice` becomes async and writes inside `tokio::task::spawn_blocking`. `iq_read` does the
same. The trial quota still holds with two chooses at once.

- [ ] **Step 1: Failing tests:**

```rust
#[test] fn the_index_is_free_while_a_capture_is_written()               // bursts.rs: list() answers during a slow write
#[tokio::test] async fn two_slices_at_once_both_fit_or_one_is_not_tried() // choose.rs: never both over the quota
#[tokio::test] async fn a_reservation_dropped_unwritten_frees_its_bytes()
```

- [ ] **Step 2: Run** `cargo test -p zeta-core --features test-fixtures --lib bursts` and
  `--test choose -- slices reservation`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `--lib bursts`, `--test choose`, `--test capture_ops`. Expected: PASS.
- [ ] **Step 5: Commit** `Choice 49: a trial slice reserves its bytes and is written off the runtime`.

### C3: The answer says which steps ran (S fix, 200k)

**Read:** spec 49.6 (first two bullets); `UAT.md`, plan 48, run notes for 48.5.

**Files:** `crates/zeta-core/src/choice.rs` (`Choice`), `node/choose.rs` (`choose_loop`,
`step_two`, `step_three`, `PastTwo`, `Choosing::past_two`, `summary`), `node/watch.rs`
(`settle`, `lift`), `crates/zeta/src/cli.rs` (the choice's print). Tests:
`tests/choose.rs`, `tests/watch.rs`.

**Interfaces:**
- `Choice.steps_run: Vec<u8>`, `#[serde(default)]`: `[1]` from `choose_now`; a step is added
  when one of its trials started.
- `Inner::choose_past_two(&self, id) -> Option<Option<String>>`: `None` not past; `Some(None)`
  step 2 ran; `Some(Some(reason))` it did not run.

- [ ] **Step 1: Failing tests:**

```rust
#[tokio::test] async fn a_step_two_that_ran_and_proved_nothing_is_in_steps_run()   // choose.rs: step 1, steps_run [1, 2]
#[tokio::test] async fn a_step_two_the_quota_refused_is_not_in_steps_run()          // steps_run [1]; not_tried has the quota string
#[tokio::test] async fn a_watch_says_step_two_did_not_run_when_it_did_not()         // watch.rs: the exact string, with the reason
#[tokio::test] async fn a_watch_says_step_two_proved_nothing_only_when_it_ran()
```

- [ ] **Step 2: Run** `cargo test -p zeta-core --features test-fixtures --test choose -- steps_run`
  and `--test watch -- step_two`. Expected: FAIL.
- [ ] **Step 3: Implement.** The CLI prints `step 2: <rows>, no frame` or
  `step 2 did not run: <reason>` under the choice.
- [ ] **Step 4: Run** `--test choose`, `--test watch`, `--test choice`. Expected: PASS.
- [ ] **Step 5: Commit** `Choice 49: a choice lists the steps that ran, and the watch says which`.

### C4: A band cutter the node can reach (S feature, 375k)

**Read:** spec 3.2 (`zeta-dsp` depends on `zeta-core`), 4.3, 49.5 (the cutter bullet).

**Files:**
- Create: `crates/zeta-core/src/cut.rs`; register it in `lib.rs`.
- Modify: `crates/zeta-core/src/node/mod.rs` (the field, `Node::set_cutter`, an accessor),
  `crates/zeta-dsp/src/channel.rs` (the implementation), `crates/zeta/src/node_setup.rs` (the
  install, beside the classifiers), `crates/zeta-core/tests/common/mod.rs` if the tests share a
  node builder.
- Test: `crates/zeta-dsp/src/channel.rs` tests; `crates/zeta-core/tests/capture_ops.rs`.

**Interfaces, produces:**

```rust
// zeta-core/src/cut.rs
pub trait BandCutter: Send + Sync {
    /// The band `bandwidth_hz` wide at `offset_hz` from the block's centre, at the lowest exact
    /// divisor of `rate_hz` that holds it; `None` when it cannot be cut.
    fn cut(&self, iq: &[Complex<f32>], rate_hz: Hz, offset_hz: i64, bandwidth_hz: Hz) -> Option<(Vec<Complex<f32>>, Hz)>;
}
impl Node { pub fn set_cutter(&self, c: Arc<dyn BandCutter>); pub(crate) fn cutter(&self) -> Option<Arc<dyn BandCutter>>; }
// zeta-dsp/src/channel.rs
pub struct Cutter;   // impl BandCutter over extract_offset and zeta_core::ops::channel_rate_hz
```

- [ ] **Step 1: Failing tests:**

```rust
#[test] fn a_cut_holds_the_band_at_the_lowest_divisor_rate()     // channel.rs: 2.4 MS/s, 12.5 kHz at +300 kHz -> 48 kS/s; a tone there is at 0 Hz
#[test] fn a_cut_as_wide_as_the_block_is_the_block()
#[test] fn a_band_outside_the_block_cannot_be_cut()               // None
#[tokio::test] async fn a_node_without_a_cutter_has_none()        // capture_ops.rs
```

- [ ] **Step 2: Run** `cargo test -p zeta-dsp --features test-fixtures --lib channel -- cut` and
  `cargo test -p zeta-core --features test-fixtures --test capture_ops -- cutter`. Expected: FAIL.
- [ ] **Step 3: Implement.** The margin the rate leaves the filter is yours; the first test
  pins the result for one case.
- [ ] **Step 4: Run** `--lib channel`, `--test capture_ops`, and `cargo build -p zeta`.
- [ ] **Step 5: Commit** `Choice 49: a band cutter zeta-dsp installs on the node`.

### C5: A trial runs at its row's band, on a slice cut to it (S feature, 400k)

**Read:** spec 3.2 ("Step 2 replays at the selection's band", "The slice is the ring's block"),
49.5 whole.

**Files:** `crates/zeta-core/src/choice.rs` (`choose_now`), `ops.rs` (`classify`,
`classify_samples`, `write_ring_slice`), `node/mod.rs` (`capture_decode_owned`),
`node/choose.rs` (`StepOne`, `step_one`, `write_slice`, `step_two`, `replay_trials`,
`replay_task`), `node/watch.rs` (`classify_task`). Tests: `tests/choice.rs`,
`tests/choose.rs`, `tests/capture_decode.rs`.

**Interfaces:**
- Consumes: C4's `BandCutter`, C2's async `write_slice`.
- Produces:

```rust
pub fn choose_now(modes, weights, c, sel, region, capture: Option<(Hz, Hz)>) -> Choice;  // (ring centre, ring rate); None: no capture test
pub(crate) async fn capture_decode_owned(&self, capture, kind, handler, packets, owner, band: Option<Tuning>) -> Result<JobId, NodeError>;
pub(crate) fn trial_band(modes, choice: &Choice, m: &Measurement, sel: &Selection, region: &str) -> Option<Tuning>;  // the smallest band holding every could_prove row's job_for band
```

**Rules:**
- `could_prove` no longer tests `band.fits(sel.bandwidth_hz)`. A row whose `job_for` band is not
  inside the capture goes to `not_tried` with the exact capture string.
- `step_one` and `classify_task` cut the block to `trial_band` as soon as the choice is made and
  keep the cut only; with no cutter, or nothing in `could_prove`, they keep what they do today
  (nothing is kept when no slice will be written).
- The slice's file is centred on the trial band, its record's band is the trial band.
- Step 2 replays each row with `band: Some(job_for(row))`.
- `capture_decode` from the operation passes `None`: unchanged.

- [ ] **Step 1: Failing tests:**

```rust
#[test] fn a_box_narrower_than_a_row_still_lists_it()                       // choice.rs: 12 kHz box, fm 9 kHz: dmr_decode in could_prove
#[test] fn a_row_the_capture_does_not_hold_is_not_tried_with_the_reason()   // choice.rs: centre 5 kHz inside the edge
#[tokio::test] async fn a_replay_runs_at_its_rows_band_on_the_measured_centre() // choose.rs: the replay's graph channel: 12500 Hz, centre = measured
#[tokio::test] async fn a_slice_on_a_wide_capture_is_cut_and_step_two_runs()    // choose.rs: file source at 20 MS/s: slice 2 s, under 1 MiB, steps_run has 2
#[tokio::test] async fn a_row_as_wide_as_the_capture_is_still_refused_by_the_quota()
#[tokio::test] async fn without_a_cutter_the_slice_is_at_the_ring_rate()
#[tokio::test] async fn every_checked_row_still_proves_through_a_cut_slice()    // each built-in row with a synth test, classified then chosen to step 2
#[tokio::test] async fn a_capture_decode_with_a_band_outside_the_recording_is_refused() // capture_decode.rs
```

- [ ] **Step 2: Run** `--test choice -- box_narrower capture_does_not` and
  `--test choose -- replay_runs wide_capture cutter cut_slice` (each with
  `cargo test -p zeta-core --features test-fixtures`). Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `--test choice`, `--test choose`, `--test capture_decode`, `--test watch`.
- [ ] **Step 5: Commit** `Choice 49: a trial runs at its row's band, on a slice cut to it`.

### C6: `unknown` lists the rows that fit its width (S fix, 200k)

**Read:** spec 49.4 (first two bullets).

**Files:** `crates/zeta-core/src/choice.rs`. Test: `tests/choice.rs`.

**Rules:**
- For the label `unknown` only, a ready decoder row enters `could_prove` without `fits_label`,
  when it is in range, not passed over, and its 0.5 to 1.5 width window holds the occupied
  width, `power99_hz` or the selection's width.
- Such a row is never the `best_guess` job: with no demodulator the answer is `cannot_tell`,
  reason `no row serves unknown`, `could_prove` filled, in score order.

- [ ] **Step 1: Failing tests:**

```rust
#[test] fn unknown_lists_the_ready_rows_its_width_fits()                 // unknown, occ 3 kHz, power99 9 kHz, no range: dmr, p25, m17 listed; rds, adsb not
#[test] fn unknown_never_starts_an_unproved_decoder()                    // job None, settled cannot_tell
#[test] fn a_label_other_than_unknown_keeps_its_label_test()             // "carrier": could_prove empty, as before
```

- [ ] **Step 2: Run** `cargo test -p zeta-core --features test-fixtures --test choice -- unknown a_label_other`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `--test choice`, `--test choose`.
- [ ] **Step 5: Commit** `Choice 49: unknown lists the decoder rows its width fits, to be tried`.

### C7: A watch tries a signal it cannot tell (S feature, 375k)

**Read:** spec 3.2 (the watch), 49.4 (third and last bullets), section 9 ruling 3.

**Files:** `crates/zeta-core/src/node/watch.rs` (`start_answer`, `settle`, `State`, the events),
`node/choose.rs` only if the child needs a seed with no job. Test: `tests/watch.rs`.

**Rules:**
- On a step-1 answer with no job and a non-empty `could_prove`, the watch starts its `choose`
  child (the seed handed over, as now) with nothing kept. Event `choosing`, naming the rows.
- One such `choose` per tracked signal. It ends with the signal's stop and with the watch's.
- Its verdict: a proof starts the proved job, kept `via: choose`, with the policy's `attach`.
  No proof is `choice_unknown` as today, with its cooldown.
- A proof that arrives after the watch stopped starts nothing.

- [ ] **Step 1: Failing tests** (`dmr_iq` at about 8 dB when layer 1 reads it `unknown`; if it
  does not, force the label with the existing `test_rename` style hook and say so in the test):

```rust
#[tokio::test] async fn a_watch_tries_an_unknown_signal_and_starts_what_proves()
#[tokio::test] async fn a_watch_whose_unknown_proves_nothing_counts_an_unknown_try()
#[tokio::test] async fn noise_and_an_unknown_no_row_fits_start_no_choose()
#[tokio::test] async fn a_watch_stopped_while_choosing_starts_nothing_and_leaves_no_slice_pinned()
```

- [ ] **Step 2: Run** `cargo test -p zeta-core --features test-fixtures --test watch -- unknown choosing`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `--test watch`, `--test choose`.
- [ ] **Step 5: Commit** `Choice 49: a watch tries a signal layer 1 cannot name`.

### C8: The expected emission weighs against the label (S feature, 375k)

**Read:** spec 3.2 (the register and the plans), 49.2 whole, section 9 ruling 2.

**Files:** `crates/zeta-core/src/choice.rs`, `modes/weights.rs`, `modes/weights.json`,
`modes/score.rs` (a helper beside `designator_fits`; `score_rows` itself untouched). Tests:
`tests/choice.rs`, `weights.rs`'s own tests.

**Interfaces:**
- `Weights.contradicted: f32`, with a serde default equal to the embedded value.
- `pub(crate) fn expected_labels(allocs: &[Allocation], center_hz: u64, region: &str) -> Vec<(Expected, Vec<&'static str>)>`
  where `Expected` is `Register { designator }` or `Plan { id }`.

**Rules:** spec 49.2's four cases are the tests. The values of `contradicted` and of a plan's
term are yours inside them; record them in the ledger. `the_embedded_weights_are_the_old_constants`
gains the new key and keeps every old value.

- [ ] **Step 1: Failing tests:**

```rust
#[test] fn am_on_a_channel_licensed_fm_answers_fm_demod_with_am_its_runner_up()   // evidence has the register line
#[test] fn am_on_a_plan_fm_channel_answers_fm_demod()                              // no register; a plan channel with deviation_hz; evidence has the plan line
#[test] fn am_on_a_channel_licensed_am_answers_am_demod()
#[test] fn carrier_and_unknown_on_an_fm_channel_answer_fm_demod()
#[test] fn with_no_expectation_every_answer_is_as_before()                         // the existing choice tests' inputs, same outputs
#[test] fn a_weights_file_without_contradicted_still_loads()                       // weights.rs
#[test] fn identify_ranks_the_decoders_as_before()                                 // score_rows output equal with and without allocations of another letter
#[tokio::test] async fn a_keyed_over_in_a_pause_on_a_plan_channel_chooses_fm_demod() // needs A2's generator; see "Tracks and order"
```

- [ ] **Step 2: Run** `cargo test -p zeta-core --features test-fixtures --test choice -- licensed plan_fm expectation` and `--lib weights`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `--test choice`, `--lib modes`, `--test identify`, `--test watch`.
- [ ] **Step 5: Commit** `Choice 49: a register's or a plan's emission can outweigh a contradicted label`.

### C9: The builder's check: can an attached job take a demodulator's channel IQ (XS, 100k)

**Read:** spec 3.2 (attached jobs), 49.3 ("The builder's first check" and the limits).

**Files:** none kept. A scratch test in `crates/zeta-core/tests/attach.rs`, removed or kept as
C11's first test.

- [ ] **Step 1:** Read `node/launch.rs` (`launch_replacing`, `node_outputs`, `point_attached`),
  `stream_in.rs` and `zeta-dsp/src/channel.rs` (the bank path and the direct path).
- [ ] **Step 2:** Write a scratch test: start a `wfm_demod` on a file source, find the stream
  leaving its `channel` node, subscribe a second job to it with a `stream_in` of kind `iq`.
  Check, on a capture rate with a bank and on one without:
  - frames arrive, at the channel's rate and centre;
  - after a `job_retune` of the demodulator;
  - after a relaunch forced by a capture change;
  - the demodulator's audio is unchanged while the second job reads.
- [ ] **Step 3:** Write the finding in the ledger and in RESUME HERE's C9 row, as one of:
  - **Branch A, the edge:** it works; `point_attached` needs an `iq` case that resolves the
    edge leaving the input's `channel` node.
  - **Branch B, the fallback:** it does not (say which check failed). The rider runs its own
    `channel` on the demodulator's band from the source, as an attached job that holds no lease
    and is relaunched when its input's tuning or capture changes.

**Done:** the branch is named with its evidence. No commit but RESUME HERE.

### C10: The `rides` field (S fix, 200k)

**Read:** spec 49.3 (the field and its three bullets), section 2 anti-goals.

**Files:** `crates/zeta-core/src/modes/row.rs` (`ModeRow`, `validate`), `modes/summary.rs` and
`ops.rs` (`ModeInfo`), `modes/rds_decode.json`. Test: `tests/modes.rs`.

**Interfaces, produces:**

```rust
pub struct Rides { pub with: Vec<String>, pub stream: RideStream }      // deny_unknown_fields
#[serde(rename_all = "snake_case")] pub enum RideStream { Iq, Audio }
// ModeRow.rides: Option<Rides>, #[serde(default)]; ModeInfo.rides: Option<Rides>, #[serde(default)]
impl ModeRegistry { pub fn riders_of(&self, demod: &LoadedMode) -> Vec<&LoadedMode>;      // ready or not
                    pub fn ridden_by(&self, rider: &LoadedMode) -> Vec<&LoadedMode>; }     // demodulator rows whose builtin is in rides.with
```

**Rules (a row that breaks them loads broken, naming the rule):** `with` is not empty and each
entry is in `DEMOD_KINDS`; the row is a decoder row; its own decoder is an in-process graph with
exactly one root `channel`; `stream` is `iq` (`audio` is refused with `rides.stream: audio is
not built yet`). `rds_decode.json` gains `"rides": {"with": ["wfm_demod"], "stream": "iq"}`.

- [ ] **Step 1: Failing tests:**

```rust
#[test] fn rds_decode_rides_with_wfm_demod()
#[test] fn a_variant_demodulator_row_is_ridden_too()            // modes dir: id wfm_wide, builtin wfm_demod: in ridden_by(rds)
#[test] fn a_rides_naming_no_demodulator_kind_breaks_the_row()
#[test] fn a_demodulator_or_a_program_row_cannot_ride()
#[test] fn rides_audio_is_refused_until_built()
#[test] fn modes_list_carries_rides()
```

- [ ] **Step 2: Run** `cargo test -p zeta-core --features test-fixtures --test modes -- rides ridden`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `--test modes`, `cargo test -p zeta --test modes_check`.
- [ ] **Step 5: Commit** `Choice 49: a decoder row names the demodulators it rides with`.

### C11: `job_attach` of a rider (S feature, 400k)

**Read:** spec 49.3 ("`job_attach` takes a rider row's id", "The attached graph", the limits);
C9's finding in the ledger.

**Files:** `crates/zeta-core/src/ops.rs` (`JobKind::expand_attached`, `attach_one`, the
`job_attach` dispatch and its refusals), `node/launch.rs` (`point_attached`), `node/mod.rs`
(a channel-stream lookup beside `audio_stream`), `node/retune.rs` if the rider must follow.
Tests: `crates/zeta-core/tests/attach.rs`, `crates/zeta-dsp/tests/rds_stage.rs` as a neighbour.

**Interfaces, produces:** `job_attach` with `kind` a rider row's id. The attached record: kind
the row's id, `input` the demodulator, handler the row's own.

**Rules:**
- **Branch A:** the graph is the row's graph with its root `channel` replaced by a `stream_in`
  of kind `iq` on the input's channel edge.
- **Branch B:** the graph is the row's own, its `channel` at the input's centre and width,
  rooted at the source; the record holds no lease; it is relaunched when its input's tuning or
  capture changes and stopped with it.
- Refusals, each naming both jobs or rows: the job's row is not one the rider rides with; the
  job's band is narrower than the rider's `min_hz`; the rider is not ready; the job already has
  that rider attached.
- A row's external handlers are not offered for a riding attach.

- [ ] **Step 1: Failing tests** (a file source of `wfm_stereo_modulate` with RDS):

```rust
#[tokio::test] async fn rds_attached_to_a_wfm_demod_gives_rds_events()
#[tokio::test] async fn an_attached_rider_follows_its_inputs_retune()
#[tokio::test] async fn an_attached_rider_comes_back_with_its_inputs_relaunch()
#[tokio::test] async fn an_attached_rider_stops_with_its_input()
#[tokio::test] async fn a_rider_is_refused_on_a_job_it_does_not_ride_with()        // fm_demod
#[tokio::test] async fn a_rider_is_refused_on_a_band_under_its_min_hz()
#[tokio::test] async fn the_demodulators_audio_is_unchanged_with_a_rider_attached()
```

- [ ] **Step 2: Run** `cargo test -p zeta-core --features test-fixtures --test attach -- rider rds_attached`. Expected: FAIL.
- [ ] **Step 3: Implement** the branch C9 named.
- [ ] **Step 4: Run** `--test attach`, `--test retune`, `--test resume`.
- [ ] **Step 5: Commit** `Choice 49: job_attach takes a decoder row that rides with the job's demodulator`.

### C12: A rider's proof answers its demodulator (S feature, 375k)

**Read:** spec 49.3 ("What riding changes", first three bullets), section 9 ruling 1.

**Files:** `crates/zeta-core/src/choice.rs` (`ChoiceJob`, `choose_now`, a translation
function), `node/choose.rs` (`step_two`, `step_three`, `proved_at`, `start_task`). Tests:
`tests/choice.rs`, `tests/choose.rs`.

**Interfaces, produces:**

```rust
// ChoiceJob.attach: Vec<String>, #[serde(default)]
pub fn job_for_proof(modes: &ModeRegistry, weights: &Weights, row: &LoadedMode, m: &Measurement, sel: &Selection, region: &str) -> (ChoiceJob, String);
// (the job, its row id): the best ready demodulator `row` rides with, attach [row], no narrower than row's min_hz; else row's own job
```

**Rules:** a proof at step 1, 2 or 3 goes through `job_for_proof`. The answer's `row` is the
job's row; the rider's `decode <row>: <summary>` line is in the evidence. A rider is never the
runner-up of a demodulator it rides with; it stays in `could_prove`. A `choose` with
`start: true` starts the job and then attaches each `attach` entry; an attach that fails is
`attach_error` in its context and the job stays. An unproved answer never carries `attach`.

- [ ] **Step 1: Failing tests:**

```rust
#[test] fn a_proved_rider_answers_its_demodulator_with_itself_attached()   // choice.rs: wfm + rds proved: kind wfm_demod, proved, attach ["rds_decode"], evidence has "decode rds_decode:"
#[test] fn a_rider_is_not_the_runner_up_of_its_demodulator()
#[test] fn a_rider_with_no_ready_demodulator_is_its_own_job()
#[test] fn the_demodulators_width_is_no_narrower_than_the_riders_min()
#[tokio::test] async fn a_choose_that_starts_a_ridden_answer_attaches_the_rider()  // choose.rs: both jobs run, the rider's input is the demodulator
#[tokio::test] async fn a_step_two_proof_of_a_rider_revises_to_the_demodulator()
```

- [ ] **Step 2: Run** `--test choice -- rider demodulators_width` and `--test choose -- rider ridden`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `--test choice`, `--test choose`.
- [ ] **Step 5: Commit** `Choice 49: a rider's proof answers its demodulator with the rider attached`.

### C13: The watch attaches riders (S feature, 375k)

**Read:** spec 49.3 ("On a watch"), 3.2 (the watch).

**Files:** `crates/zeta-core/src/node/watch.rs` (`Policy.attach`, `check` of the `attach`
param, `attach_to`, `Decoding.attached`, `settle`, `replace`, teardown), `ops.rs` (the watch's
`attach` schema and its refusals). Test: `tests/watch.rs`.

**Rules:**
- `attach` is a list of `transcribe` and rider row ids. Anything else is refused at start,
  naming it.
- Each entry is attached where it applies: `transcribe` to a job with audio; a rider to a job
  whose row it rides with, once that rider is proved or the watch names it.
- A proof of a rider of the kept job's row attaches it and lifts the hold; the job is not
  replaced. `Decoding.attached` becomes a list; teardown stops each before the job.
- A watch with `attach: ["transcribe"]` behaves exactly as before.

- [ ] **Step 1: Failing tests:**

```rust
#[tokio::test] async fn a_watch_attaches_a_proved_rider_and_lifts_the_hold()   // the wfm_demod keeps its id
#[tokio::test] async fn a_watch_that_names_a_rider_attaches_it_to_each_job_it_rides_with()
#[tokio::test] async fn a_watch_with_transcribe_and_a_rider_attaches_both_where_they_apply()
#[tokio::test] async fn a_watch_refuses_an_attach_that_is_neither()
#[tokio::test] async fn stopping_the_watch_stops_every_attached_job_first()
```

- [ ] **Step 2: Run** `cargo test -p zeta-core --features test-fixtures --test watch -- rider attach`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Track gate.** `cargo test -p zeta-core --features test-fixtures`, clippy, fmt
  for `zeta-core`, `zeta-dsp`, `zeta`. Expected: PASS.
- [ ] **Step 5: Commit** `Choice 49: a watch attaches a rider to the job it rides with`.

---

## Track P: the page (spec 49.8, and the page halves of 49.3, 49.4, 49.6)

After C13. One task at a time. Each runs only its own scenarios with `ZETA_UI_ONLY=<a,b,...>`;
the whole suite runs in P5 and F2.

### P2: The choice entry is revised in place (S feature, 400k)

**Read:** spec 3.2 (the page), 4.4, 49.8 steps 2 to 4; P1's finding in RESUME HERE.

**Conditional on P1.** If P1 found the menu's choose is where the time goes, do all of this
task. If P1 found the time elsewhere, still do steps 2 and 3 below (the operator's menu holds
still, acceptance check 49.10), skip the harness changes that would not save time, and write in
RESUME HERE what P5 must cut instead.

**Files:** `crates/zeta-ui/web/menu.js` (`pursueChoice`, `repaint`, `paint`, a new
`reviseChoice`), `actions.js` only if the entry needs a hook; `crates/zeta-ui/tests/e2e/ui.py`
(`menu_item`, `still_menu`, their callers).

**Rules:**
- An answer rewrites the `choice_start` element: label, title, disabled state, `revising`
  class. The Listen group is re-ordered only when the chosen demodulator changed. Nothing else
  in the menu is touched; open submenus and focus stay; the menu is clamped only when it grew.
- `repaint` remains for the rows' first answer.
- The `job_list --all` poll runs until the event subscription is attached, and once when it
  closes. Not every 500 ms.
- `menu_item` loses its retry loop. `still_menu` stays for scenarios that read the choice
  itself; scenarios that click another entry stop calling it.

- [ ] **Step 1: Failing scenarios** in `ui.py`:

```python
def scenario_an_open_submenu_stays_open_when_the_choice_arrives(page, url, secret)   # open Decode before the entry settles; same DOM node after
def scenario_the_menu_does_not_move_when_its_choice_is_revised(page, url, secret)    # bounding box equal before and after, unless it grew
def scenario_the_menus_choose_is_not_polled_once_its_events_flow(page, url, secret)  # count job_list all=true requests over 3 s: at most 2
```

- [ ] **Step 2: Run**
  `ZETA_UI_ONLY=an_open_submenu_stays_open_when_the_choice_arrives,the_menu_does_not_move_when_its_choice_is_revised,the_menus_choose_is_not_polled_once_its_events_flow cargo test -p zeta --test ui -- --nocapture`.
  Expected: FAIL (and no `ui e2e skipped`).
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the three, plus `the_first_box_entry_is_the_choice`,
  `the_menus_choose_takes_the_box_span`, `closing_the_menu_stops_its_choose`,
  `menu_entry_table_is_whole` and the scenarios that called `still_menu`.
- [ ] **Step 5: Commit** `Choice 49: the box menu's first entry is revised in place`.

### P3: The entry's words (S feature, 375k)

**Read:** spec 49.6 (last two bullets), 49.4 (the menu bullet).

**Files:** `crates/zeta-ui/web/actions.js` (`choiceLabel`, `choiceTitle`), `units.js` if a
formatter is missing, `ui.py`.

**Rules:**
- Label: `Start <kind> here: <width> kHz, <deviation> kHz deviation · <row name>` when the
  job's params hold `deviation_hz`; `, USB` or `, LSB` when they hold `sideband`; no parameter
  text otherwise.
- `Can't tell: <reason>; trying <rows>` while the menu's choose runs with rows in
  `could_prove`; `Can't tell: <reason>` when it has ended.
- Title: the runner-up line, then `step 2: <rows>, no frame` when `steps_run` holds 2, or
  `step 2 did not run: <reason>` when it does not and `not_tried` has a reason with no row.

- [ ] **Step 1: Failing scenarios:**

```python
def scenario_the_first_entry_names_the_deviation(page, url, secret)             # cbvoice: "2.5 kHz deviation" (the plan's); the started job's deviation_hz is the same
def scenario_the_first_entrys_hover_says_what_step_two_did(page, url, secret)    # a dmr fixture: "step 2: ..."; a too-wide one: "step 2 did not run: ..."
def scenario_an_unknown_box_says_what_it_is_trying(page, url, secret)
```

- [ ] **Step 2: Run** them with `ZETA_UI_ONLY`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** them and the choice scenarios P2 names.
- [ ] **Step 5: Commit** `Choice 49: the first entry names its deviation and what step 2 did`.

### P4: Riders on the page (S feature, 375k)

**Read:** spec 49.3 ("On the page", "Start gives both"), section 9 ruling 1.

**Files:** `crates/zeta-ui/web/actions.js` (`choiceLabel`, a job-menu entry built from the
rows' `rides`), `menu.js` (`startChoice`, the job menu's context), `panes.js` only if the Jobs
pane does not already draw an attached job under its input; `ui.py`.

**Rules:**
- A choice with `job.attach` reads `... · <row name> + <rider label> (<summary>)`, the summary
  from the rider's `decode <row>:` evidence line.
- `startChoice` sends `job_start`, then one `job_attach` per `attach` entry with the started
  job's id. A failed attach is shown in the transcript; the job stays.
- A running demodulator's menu offers `Decode: <rider label>` for each ready rider of its row
  not already attached, greyed with the reason for one that is not ready.
- No entry is written per rider: they come from `modes_list`.

- [ ] **Step 1: Failing scenarios** (the `wfm` fixture with RDS):

```python
def scenario_the_first_entry_on_a_station_with_rds_starts_wfm_with_rds_attached(page, url, secret)
def scenario_a_running_wfm_demod_offers_its_riders(page, url, secret)            # attach from the job's menu; the row appears under its input
```

- [ ] **Step 2: Run** them. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** them, `wfm_decode_plays_audio`, `menu_entry_table_is_whole`.
- [ ] **Step 5: Commit** `Choice 49: Start gives a demodulator with its proved rider, and a job's menu offers the rest`.

### P5: The second profile and the shards (XS, 100k)

**Read:** spec 49.8 (step 5 and the target); P1's and P2's lines in RESUME HERE.

**Files:** `crates/zeta-ui/tests/e2e/ui.py` (`SHARD_STARTS`), `crates/zeta/tests/ui.rs` (the
`DEADLINE` comment's history line), `.github/workflows/ci.yml` (`SMOKE`, only if a smoke
scenario was renamed).

- [ ] **Step 1:** Run the whole suite with `ZETA_UI_PROFILE` into
  `scratch/plan49/ui-profile-head.txt`.
- [ ] **Step 2:** Move `SHARD_STARTS[4]` so the four shards are within 20 s of each other. Each
  block's first scenario must pass alone: run each with `ZETA_UI_ONLY`.
- [ ] **Step 3:** Run the suite again. **Target:** longest shard 300 s or less, the suite no
  slower than P1's.
- [ ] **Step 4, only if the target is missed:** from the head profile, name the five costliest
  remaining waits in the ledger. Cut the ones that are harness waits. If it is still missed,
  add a fifth shard (`SHARDS`, `SHARD_STARTS[5]`), say so in RESUME HERE, and do not raise
  `DEADLINE`.
- [ ] **Step 5: Commit** `Choice 49: the UI shards rebalanced from a profile run`.

---

## Track F: the end

### F1: Docs, UAT, TODO, prose, generated files (S fix, 250k)

**Read:** the whole spec; `UAT.md`, plan 48 with its run; `TODO.md`, "Left from plan 48".

**Files:** `crates/zeta/src/agents_md.rs`, `docs/agents/field-craft.md`, `handlers/README.md`
(if B1 left anything), `RECIPES.md` (a short recipe: RDS with the station you listen to),
`CHANGELOG.md` (the strict-client note), `UAT.md`, `TODO.md`, then `AGENTS.md` and
`skills/zetasdr/SKILL.md` regenerated.

- **`agents_md.rs` and the field craft:** `Choice.steps_run` and `ChoiceJob.attach`; the `rides`
  row field and `job_attach` of a rider; the watch's `attach` list and its `choosing` event;
  what `unknown` now gets; the expected-emission evidence lines; layer 1's `duty` as the on
  share and a keyed over reading `fm`; the handler `version` block.
- **`CHANGELOG.md`:** the new fields on `Choice`, `ChoiceJob`, `ModeInfo`, the handler file and
  `weights.json`, for a client built with strict types.
- **`UAT.md`, a "Plan 49" section:** the spec's eleven acceptance checks (spec 8) as
  `- [ ] 49.1` to `49.11`, each run on the web page, plus:
  - `49.12` **48.6 again:** on live CB traffic, open the menu during an over and between overs;
    the first entry names `fm_demod` either time;
  - `49.13` **48.1 again:** the first entry on live FM voice, on a broadcast station and, where
    one can be heard, a pager: the job, its width and, for FM, its deviation; Start gives it.
- **`TODO.md`:**
  - under "Left from plan 48", mark each item plan 49 took as done in plan 49, in one line each,
    and leave the rest;
  - turn the plan 49 line under "Classify: layers 1 and 2 choose the job" from "approved, not
    built" to built;
  - mark the two "minimum-version probe" lines under "Alternative handlers" as done;
  - close "Fit: the layer-1 label of a weak voice carrier" only if A5's sweep shows it fixed;
  - add a "Left from plan 49" section with what this plan leaves: riding for a row's external
    handlers; `rides.stream: audio`; a capability probe for handlers; a plan's own emission
    field; `iq_read` cut to its selection; whatever A5, C9 and P5 wrote down as left.

- [ ] **Step 1:** Write the prose, the UAT section, the TODO changes.
- [ ] **Step 2:** Regenerate both files with the two commands in Global Constraints.
- [ ] **Step 3:** `cargo run -q -p zeta -- agents-md --check` and
  `cargo run -q -p zeta -- agents-md --skill --check`. Expected: no drift.
- [ ] **Step 4: Commit** `Choice 49: docs, UAT plan 49, TODO`.

### F2: The whole-branch gates, review and fix wave (review wave, 800k)

- [ ] **Step 1: Gates,** with the private target dir:
  - `cargo fmt -p <crate> -- --check` for each changed crate;
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
  - `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`;
  - `cargo test -p zeta --test ui -- --nocapture` (the whole suite: rows and a handler file
    changed; check no `ui e2e skipped` line);
  - `cargo run -q -p zeta -- agents-md --check` and `--skill --check`;
  - `bash scripts/public-check.sh`;
  - `bash scripts/check-doc-links.sh`.
- [ ] **Step 2: A whole-branch review** by a fresh reviewer given the diff and the spec, not
  the runner's summary. It checks the Review Focus list, the anti-goals, and that A5's capture
  step is closed.
- [ ] **Step 3: Fix wave,** each fix with its own test, then Step 1 again.
- [ ] **Step 4:** Delete the private target dir. RESUME HERE: "built, gates clean, not landed".
  Landing on `main`, the deploy and the publish wait for the operator's word.

---

## Sizes and estimate

| Task | Title | Size | Estimate | Depends on | Parallel with |
|---|---|---|---|---|---|
| P1 | Profile the UI suite on the base | XS | 50k | none (first) | A, B, C |
| A1 | Captures from the field host | S fix | 150k | the operator's go-ahead for any retune or gain change | B, C, A2 |
| A2 | Generator, sweep tool, failing tests | S fix | 200k | none | B, C |
| A3 | The gate finds a drop-out | S fix | 200k | A2 | B, C |
| A4 | The label reads the view | S fix | 250k | A3 | B, C |
| A5 | Head sweep, captures, rule 9, the operation | S fix | 200k | A4, A1 | B, C |
| B1 | A handler's least version | S feature | 375k | none | A, C (before C10) |
| C1 | Replays before dwells | S fix | 200k | none | A, B |
| C2 | The write off the runtime | S fix | 200k | C1 | A, B |
| C3 | Which steps ran | S fix | 200k | C2 | A, B |
| C4 | The band cutter | S feature | 375k | C3 | A, B |
| C5 | The trial band and the cut slice | S feature | 400k | C4 | A, B |
| C6 | `unknown` lists rows | S fix | 200k | C5 | A, B |
| C7 | A watch tries `unknown` | S feature | 375k | C6 | A, B |
| C8 | The expected emission | S feature | 375k | C7; A2 for one test | A, B |
| C9 | The builder's check for riding | XS | 100k | C8 | A, B |
| C10 | The `rides` field | S fix | 200k | C9, B1 | A |
| C11 | `job_attach` of a rider | S feature | 400k | C10 | A |
| C12 | A rider's proof answers its demodulator | S feature | 375k | C11 | A |
| C13 | The watch attaches riders | S feature | 375k | C12 | A |
| P2 | The entry revised in place | S feature | 400k | P1, C13 | A |
| P3 | The entry's words | S feature | 375k | P2, C3, C7 | A |
| P4 | Riders on the page | S feature | 375k | P3, C12 | A |
| P5 | Second profile, the shards | XS | 100k | P4 | A |
| F1 | Docs, UAT, TODO, generated files | S fix | 250k | everything above | none |
| F2 | Gates, whole-branch review, fix wave | review wave | 800k | F1 | none |

**Total: about 7.5 M tokens** (track A 1.0 M, B 0.4 M, C 3.8 M, P 1.3 M, F 1.05 M), by this
repository's calibration: XS 50k, an S fix 200k, an S feature or page item 375 to 400k all-in, a
whole-branch review and fix wave 0.75 to 0.85 M. The spec's earlier guess of 3 to 3.5 M counted
parts, not tasks, and was low.

## Self-review

- **Spec coverage:** 49.1: A1 to A5. 49.2: C8. 49.3: C9 to C13, P4. 49.4: C6, C7, P3. 49.5: C4,
  C5. 49.6: C3, P3. 49.7: C1, C2. 49.8: P1, P2, P5. 49.9: B1. Spec 8's checks: F1. Rulings 1 to
  8: C12 and P4 (1), C8 (2), C7 (3), B1 (4), C1 (5), P2 (6), A4 (7), A1 (8).
- **Review Focus:** each line names the task whose tests hold it: A3's one-level window, C5's
  capture edge, C11's retune, relaunch and stop, B1's hang and replaced program, C7's stopped
  watch (the menu's half is P3's `an_unknown_box_says_what_it_is_trying`, which closes the menu
  and polls for no `choose` job).
