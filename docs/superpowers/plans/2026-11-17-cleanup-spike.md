# Cleanup spike (2026-11-17)

A sweep of `TODO.md` by size, so the small debts are paid in rounds rather than one at a time. Each entry names its `TODO.md` section; the section stays the authority for the detail, and is removed or trimmed when its item lands.

Sizes are estimates: **XS** a few lines and a test, **S** a focused session, **M** a plan of its own (spec first), **L** several plans.

## Budgets, from Round 0's actuals

Round 0 was estimated at about 150k tokens and its subagents used about 940k: implementers 136k to 302k a batch (25k to 50k an item, the most on lease and job-supervision code), reviewers 67k to 162k a batch (the most for an Opus review of locking), a fix round about 30k a batch. Lines changed say little; the cost is reading the code around a fix, building and testing it, the review, and the fix round each review has found a reason for. Every batch review found something real, so the review stays.

The test rule that applies from Round 1 on: per fix, only its new test and the tests beside it; a touched crate's whole suite once per batch; the workspace and page suites once on the merged branch; a repeat loop only where the brief asks for one (a race fix).

Budget per round from these figures: an XS item about 50k with its review; an S item about 150k to 250k. Round 1 was budgeted at 0.8M to 1M and used about 2.3M for twelve S entries (about 190k each, everything included). So from Round 2 on, budget about 200k per S item, and about 0.4M per round for the whole-branch review, its fix wave and their re-reviews. That puts Round 2 (eight items) near 2M, Round 3 (six page items, with UI runs) near 1.6M, and Round 4 about 100k to 200k per section.

Round 2 then used about 3.4M for nine items: a small feature costs about twice a small fix. Its section below has the figures, and they put Round 3 near 2.8M.

## Round 0: the XS sweep (done 2026-11-17)

Built subagent-driven on `worktree-cleanup-xs`, one implementer per batch and a reviewer after each, then the full gate. Every fix carries a test that fails without it.

Outcome: thirteen items landed; A6 needed no change (the ring's seconds already follow the rate; what cut the history short is "Source: the IQ history restarts on every timestamp gap"). Reviews changed four items: C1's retry now reruns the whole fit and start; B3 fails on any skip but a quiet band; B4 keeps a repeat `plan_start` from doubling a demodulator; A2 also guards `replace` and `graph_put`. Left over, in `TODO.md`: a conflict on a controller's child still offers a retry that is refused ("Jobs: a conflict on a controller's child offers a retry that is refused").

**Batch A, node and lease (Opus):**
- A1. "Bug: a refused `graph_put` persists its graph": refuse before persisting when the entry is `Running` again after this put's stop.
- A2. "Jobs: refuse job_retune on controller-owned jobs": `job_retune` (and `job_stop`, for the same reason) refuses a record with an `owner`, naming the owner.
- A3. "Bug: `source_ensure` and `source_pin` refusals from a faulted source lack the `source <id>: ` prefix": route them through the helper `job_start` uses.
- A4. "Lease: job_start on an unheld radio tunes twice": one tune.
- A5. "Jobs: an attached job's record keeps its input's old tuning after a retune": `job_retune` and `graph_put` update the attached records' `tuning` when they relaunch them.
- A6. "Bug: `source_list` reports the startup ring length after a retune": report the seconds the ring holds at the current rate.

**Batch B, one-liners (Sonnet):**
- B1. "Aircraft links: deferred minors from plan 30": `NodePort` forwards a remote source's `position`; `adsb_decode` gains `subject.merge` `["icao"]`.
- B2. "Digital voice: the next rows": M17's LICH Golay table corrects two-bit errors (the defect P25's table had).
- B3. "Register answer: follow-ups from the plan 37 build": `zeta registers check` exits non-zero when it skipped a query, and prints the skipped count.
- B4. "Audio: the default `deviation_hz` on a 12.5 kHz channel": a channel plan may carry the deviation its channels use, and `plan_start` passes it to the demodulator; the UHF CB plan says 2500. Data on the plan, not a guess from the channel width.

**Batch C, page and tests (Sonnet):**
- C1. "UI: route the fit's measure start through applyOp": a conflict on the fit's `measure` offers the stop-and-retry button.
- C2. "Test: three real-time tests failed once each on a loaded host": the page scenario `job_row_two_lines` waits for the row before measuring it.
- C3. "Test: `modes_check::a_bad_synth_block_fails_its_row_and_the_rest_still_run` flaked once on a leftover scratch dir": the checker retries the removal or waits for the node to finish.
- C4. "Test: the UI suite's heavier shard is near its deadline": rebalance `SHARD_STARTS` so the two shards finish closer together.

## Round 1: small node and worker fixes (S each, done 2026-11-17)

Built subagent-driven on `worktree-cleanup-r1` in five batches, run one after another, with a batch's review overlapping the next batch's build where the files were apart. Each batch had one implementer and one review, a fix round with a scoped re-review where the review asked for one, then a whole-branch review, one fix wave and the full gate. Every fix carries a test that fails without it; the refactors carry none.

Outcome: all twelve entries landed, plus two items from the same code: "Jobs: an attached job whose relaunch fails keeps its input's old tuning", and a `held_s` in the ring report.

The reviews changed five things:
- Batch E: a `graph_put` takes its restart lock before it reads the record. Two overlapping puts had left the second graph stored but not run.
- Batch G: `held_s` counts the samples held, and a new `run_s` gives the newest contiguous run. A backward time jump still restarts the ring.
- Batch H: a sink restart waits for its decoder's restart lock cancellably. Without that, a capture move relaunching the watch could hold the source's gate for good.
- Batch H: `--then-tail` picks the decoder structurally rather than by node order, and the watch's sink is fed by the same pick.
- The final review: the ring report's new fields default when an older node omits them, so `--source-node` still opens an older node.

Left over, in `TODO.md`:
- the page's classify retry knows only `too_old`;
- an ungated `job_stop` during a restart's teardown is undone by the relaunch;
- per-client fairness for the stream pool.

**Rollback:** every job record now carries `sinks`. The previous binary skips such records, so it resumes none of them, and none are deleted. Rolling forward again recovers them. A `set_start` under the old binary prunes the set instances it cannot see.

Batches:
- D, the worker (Opus): the watchdog; control frames ahead of IQ.
- E, restarts (Opus): the restart lock; the failed relaunch's tuning; heal watchers after the resume; the job history (a re-run replaces its predecessor, cap 50).
- F, refactors (Sonnet): the source-record helper with the shared test fixtures; `node/self_sweep.rs`; `--max-streams`.
- G, the ring (Sonnet, reviewed by Opus): segments across gaps; `held_s`, `run_s`, `gaps`.
- H, the watch's sinks (Opus): the `gated_stop` token; `--then-tail`; sinks stored beside the row's expansion; the generic `sink` param.

Actuals, subagents only (about 2.3M against an estimate of 1.4M to 1.8M):

| stage | per batch | total |
|---|---|---|
| implementer | 124k to 353k (Sonnet 124-140k, Opus 157-353k) | 1.06M |
| review | Sonnet 71k; Opus 110-154k | 0.62M |
| fix round | 22-52k | 0.10M |
| scoped re-review | 56-61k, however small the fix | 0.18M |
| whole-branch review, fix wave and its re-review | | about 0.36M |

The full gate ran once, about 30 minutes of wall clock.

- "Worker: a parent-side status-silence watchdog".
- "Worker: control frames queue behind IQ blocks".
- "Jobs: serialise restarts of one input".
- "Node: `watch_healed` starts before `open` has resumed the jobs".
- "Sources: one helper for the source record update" and "Tests: a shared fixture module for the node tests" (refactors, together).
- "Scan: the self-sweep half of `node/scan.rs` in its own module".
- "Node: stream sockets are a fixed pool": the `--max-streams` flag; per-client fairness is a second step.
- "Jobs: test runs push the operator's stopped records out of the job history".
- "Source: the IQ history restarts on every timestamp gap".
- "Bug: `gated_stop` is cleared unconditionally by a sink restart".
- "Bug: `--then-tail` on a program row with a squelched demod tails the squelch events first".
- "Bug: a sink-bearing decoder is frozen as an operator graph", then "Architecture: a generic `sink` param on the watch".

## Round 2: small features (S each, done 2026-11-18)

Built subagent-driven on `worktree-cleanup-r2` in five batches, one after another, each with one implementer and one review; then a whole-branch review, one fix wave with a scoped re-review, and the full gate.

Outcome: all nine items landed (the "Digital voice" entry is two).
- I, speech (Sonnet): `--stt-url` posts each utterance to an OpenAI-compatible or whisper.cpp server, with `--stt-url-model`, `--stt-url-languages` and a bearer from `ZETA_STT_URL_KEY`.
- J, FM broadcast (Opus): `wfm_demod` takes `stereo`, always two channels when on, mono in both while the pilot is absent or weak.
- L, packaging (Sonnet): `--run-dir` (`ZETA_RUN_DIR`); a handler's install hint follows the package managers the host has, and a row may name `snap` and `flatpak`.
- K, modes and identify (Opus): `band.range_hz` is one range or a list, and the rows that lacked ranges have them; an identify takes `dwell_scale`, which a watch raises to 3 then 6 on its retries; `c2dec` is optional for M17.
- M, operations (Opus): a set member may carry an audio policy; a plan carries named channel groups that `plan_start` and `jobs_stop` take; `signature_retract` and `signature_delete`, with Undo, Retract and Delete in the Explore pane.

The reviews changed these:
- Batch J: a weak station now plays mono. The first build read the pilot against the multiplex, which only detects a pilot; the fix reads the pilot's SNR on the PLL's quadrature arm (stereo over 35 dB, mono under 30). The pilot PLL is shared with RDS, and a mono run clears `job_context.wfm`.
- Batch K: an M17 job without `c2dec` says so in `job_context.m17.vocoder`, as P25 and DMR do.
- Batch M: a retract or delete removes only a learned row that is unchanged since the accept, and reports a row it kept or could not remove instead of answering as if it had.
- Batch I: the reply is read up to a cap, the key is blanked before an error's body is cut, and no error prints a URL's credentials.
- The first gate run: the page scenario for Undo and Delete depended on the scenario before it.

Decided along the way, and worth the operator's eye:
- K changed a scoring rule: a range gives centre credit only to a row that lists no centres. Without it POCSAG's VHF range tied APRS on its own frequency. A row with both centres and a range scores lower between its centres than before.
- `plan_start` with neither `channels` nor `group` still means every channel; only both together are refused.
- The speech upload format (FLAC, 8 kHz) was not built. By the operator's word it is to be a probed capability of the server, not a flag; the `TODO.md` section says so.
- A watch's third try dwells six times as long, and the watch identifies one signal at a time, so on a band with slow rows it holds other new signals back for minutes. `TODO.md` has the figures.

**Rollback and older clients:** this build reads everything the previous build wrote. The previous binary skips a `retracted` signature record, marks a row file that names `snap` or `flatpak` as broken, refuses a set file whose member carries an audio policy, and refuses a stored `wfm_demod` graph with `stereo: true`. A client of the previous build refuses this node's `channel_plans` answer, its `signature_*` answers and a modes answer with a several-range row. Nothing is deleted by rolling back.

Left over: "Left from cleanup round 2" in `TODO.md`.

Actuals, subagents only (about 3.4M against an estimate of 2.1M; the operator's budget was 2M):

| stage | per batch | total |
|---|---|---|
| implementer | Sonnet 145-165k; Opus 316-503k | 1.62M |
| review | Sonnet 79k; Opus 126-216k | 0.81M |
| fix round (J only) and its re-review | 128k and 96k | 0.22M |
| whole-branch review, fix wave and its re-review | 257k, 326k, 155k | 0.74M |

Per item, everything included: about 375k, twice Round 1's 190k. Why:
- These were features, not fixes. Each added an op field, a flag or a row field, and so touched `ops.rs`, the CLI, the agent prose, the guide and the generated agent docs; three had page work. One field added to the job-start parameters meant a line in forty test files.
- A review's cost follows the diff's size, and a feature's diff with its tests and docs ran 270 to 580 kB; reviews ran 126k to 216k on Opus.
- J cost 734k for one item: the brief prescribed a measure that could not do what the section asked, and the fix round built the right one.
- The fix wave carried 40 items (two Important, the rest parked minors) and cost as much as a batch.

For Round 3 (six page items): budget about 350k per item and about 0.7M for the whole-branch review and fix wave, so near 2.8M, not 1.6M. Two things that held the cost down and are worth keeping: reviewers were given the diff without the generated agent docs, and the two late Important findings went into the fix wave rather than each into a resumed implementer and a re-review of its own.

The full gate ran twice, about 32 minutes each.

- "Speech: a Whisper server over the network".
- "Feature: stereo for `wfm_demod`".
- "Signatures: take back a wrong accept".
- "Plans: named channel groups (`lower 40 voice`) for `plan_start`".
- "Modes: several ranges per row, and centres-only rows tried anywhere".
- "Watch: the identify dwell misses a marginal sonde the detector now sees" (option 1, a dwell that grows with the retry, first).
- "Packaging: the run directory and install hints for handlers".
- "Digital voice: the next rows": an audio policy in a set, and `c2dec` optional for M17.

## Round 3: the page (S each)

- "Page: pointer movement over the page is costly" (profile first).
- "Mix: the page's audio stutters under channel load while every node counter reads 0" (the page counter first, then the adaptive lead).
- "UI: view policies in localStorage", then "UI: `view set dir ltr|rtl|auto`" and "UI: `view set time_flow left|right`".
- "UI: on-waterfall messages cover the newest columns".
- "UI: repeated Fit to signal walks the width down to the floor" and "Fit: a weak voice channel fits to the width floor" (together: both are the fit's width rule).
- "Page: positioned jobs on other sources onto their feeds".

## Round 4: deferred review minors

Each of these sections is a list of one-line nits from a plan's reviews. Take one section per round, fix what is still true, and delete the rest with a line saying why.

- "UI: plan 26 deferred minors".
- "UI: deferred minors from plan 24".
- "Bits: deferred minors from plan 19.2".
- "Signatures: deferred minors from plan 19.3".
- "Layer 1 and explore: deferred minors from plan 19.1".
- "Decoder wave 2: leftovers from plan 29" (the "Deferred minors from review" item).
- "Aircraft links: deferred minors from plan 30" (what Round 0 leaves).
- "Watch: deferred minors from plan 31".
- "Map layer: deferred minors from plan 32".
- "Feed: follow-ups from plan 27's final review".
- "Register answer: follow-ups from the plan 37 build" (what Round 0 leaves).
- "Digital voice: the next rows", "Left from plan 38's reviews".
- "Follow-ups from the live view and the plan 36 run (2026-11-12)".
- "Test: `ops::too_old_carries_the_rings_earliest_ns` flaked once with a subtract overflow" and the rest of "Test: three real-time tests failed once each on a loaded host": reproduce under `taskset -c 0,1` with a busy loop on each CPU before widening a margin.

## Not cleanup

The M and L entries are features, specced and planned on their own, and stay in `TODO.md` as gists. In rough order of the operator's recent asks: "Agent: band names come from data, not the model's memory"; "Watch: start a voice demodulator and a transcriber on each new signal"; "Sources: attach and detach a radio on a running node"; "Subjects: a running job keeps its tables on the node"; "Detect: a per-device spur list" with "Calibration: a set of receiver and antenna calibration tools"; "Channel bank: a capture under 2 Msps gets none, so 80 CB channels run direct at 5 cores"; "Page: resizable side column and bottom pane"; "Architecture: recipes the node stores and runs by name"; "Node: a baseline of the host's capabilities"; "Access: ACLs on restricted rows"; "Policies: one place for the operator's choices, and the waterfall behind the map as the first"; the DVB-T service list. The L ones (trunking, the node editor, the agent's automation language, SatNOGS, SatDump, DVB-T picture, GEO DVB-S2, the TX path, the B210-clone gateware, video chroma) wait for the operator's word.

Entries that need the operator or the radios, not code (the B210's USB port and GPS module, the worker's restart count, the open UAT checks), are in `UAT.md` and their `TODO.md` sections.
