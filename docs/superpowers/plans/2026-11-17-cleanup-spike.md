# Cleanup spike (2026-11-17)

A sweep of `TODO.md` by size, so the small debts are paid in rounds rather than one at a time. Each entry names its `TODO.md` section; the section stays the authority for the detail, and is removed or trimmed when its item lands.

Sizes are estimates: **XS** a few lines and a test, **S** a focused session, **M** a plan of its own (spec first), **L** several plans.

## Round 0: the XS sweep (this run)

Built subagent-driven on `worktree-cleanup-xs`, one implementer per batch and a reviewer after each, then the full gate. Every fix carries a test that fails without it.

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

## Round 1: small node and worker fixes (S each)

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

## Round 2: small features (S each)

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
