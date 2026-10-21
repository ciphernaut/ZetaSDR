# Channel Plans Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The node, the page, the CLI and the agent name channels by number from one table-driven channel-plan registry (`uhf_cb_au` now; `channel_plans {}`, `zeta plans`, `view set plan auto|<id>|none`); an `fm_demod` on a plan channel never opens its squelch on an adjacent channel's energy (the guard); utterances under `min_utterance_ms` (700) never reach the daemon; the speech daemon sees at most 4 connections, a busy daemon delays (`queued_ms`) or drops with a reason instead of failing jobs; optionally, demodulators only on active channels (`examples/python/cb_watch.py`).

**Architecture:** In `zeta-core`, a new `plans.rs` holds `PLANS` and `channel_at`; `Op::ChannelPlans` serves it; `JobKind::FmDemod::expand` writes `wide_hz` on the channel node and `guard_hz`/`guard_db`/`channel_hz` on the demod node for plan channels, and `reexpand` carries them. In `zeta-dsp`, the channel stage widens its cut to `wide_hz` when it fits the block; `nbfm_demod` filters the centre itself and holds its gate closed while a neighbour is `guard_db` stronger. In `zeta-speech`, `TranscribeParams.min_utterance_ms`, a semaphore in `SocketBackend`, `Transcript.queued_ms`, `SpeechError::Busy`. In the page, `plans.js` and a `plan` view policy label rows, outlines and the axis.

**Tech Stack:** Rust 2024 (`zeta-core`, `zeta-dsp`, `zeta-speech`, `zeta`), vanilla JS (`crates/zeta-ui/web`), Playwright scenarios in `crates/zeta-ui/tests/e2e/ui.py` driven by `crates/zeta/tests/ui.rs`, a stdlib Python example.

**Spec:** `docs/superpowers/specs/2026-10-21-channel-plans-design.md` (3.1 registry, 3.2 guard, 3.3 transcribe, 3.4 watcher; 4 page; 5 CLI; 6 constraints; 8 open questions; 9 calls made).

## Global Constraints

- `zeta-core` gains no dependency. `JobRecord`, `job_start`, `job_list`, the lease rules and `knowledge_lookup` are unchanged. One new op, `channel_plans {}` → `{"plans": [{"id", "name", "region", "bandwidth_hz", "adjacent_hz", "channels": [{"number", "center_hz", "note": string | null}]}]}`. Every new stage param is `#[serde(default)]` so stored graphs load; `deny_unknown_fields` stays.
- A job off every plan gets byte-for-byte today's graph (pinned in Task 2); the guard can only close a gate, never open one.
- Event fields are additive only: `squelch` gains `adjacent_db: number | null`, `guarded: bool`; `transcript` gains `queued_ms`; a `dropped` transcript gains `reason: "queue" | "busy"`. `transcripts.log` lines keep plan 12's fields exactly.
- Constants: `CHANNEL_MATCH_HZ = 1_000`, `GUARD_DB_DEFAULT = 12`, `GUARD_TAPS = 63`, `MIN_UTTERANCE_MS_DEFAULT = 700`, `QUEUE = 4`, `STT_CONCURRENCY = 4`, `STT_QUEUE_WAIT = 30 s`, `STT_RETRY_DELAY = 200 ms`.
- Every task that changes an op, result, param or note regenerates `AGENTS.md` (`cargo run -p zeta -- agents-md > AGENTS.md`) and `skills/zetasdr/SKILL.md` (`--skill`) and runs `cargo test -p zeta --test agents_md --test transports`.
- Gates per task: `cargo fmt -p <touched crates> -- --check` (the `--all` form fails in a nested worktree), `cargo clippy --workspace --all-targets --all-features -- -D warnings`, the crate's tests (`--features test-fixtures` where gated). UI: `cargo test -p zeta --test ui` with no `ui e2e skipped` line within its 300 s deadline (plan 12 left about 237 s: one new scenario, under 8 s). Final gate: `cargo test --workspace --all-features --no-fail-fast` (no `FAILED`, no skip line), `agents-md --check`, `--skill --check`. Temp files under the job's tmp directory, never `/tmp`; Python via `python3 -I` except the Playwright runner.

## Review Focus

1. The guard never opens what would be closed, and a job off a plan is unchanged: `expand` for 147 050 000 Hz equals the plan 12 graph; with a guard, `open` implies the unguarded squelch's `open`. Pinned in Task 2 (`off_plan_graphs_are_unchanged`, `the_guard_only_closes`).
2. The widened channel at the capture's edge: ch1 on the pinned 1 024 000 Hz capture narrows back, notes `wide: off: ...`, and runs; a widened band near a bin edge stays bank-served or says why. Pinned in Task 2 (`wide_band_off_the_block_falls_back_and_says_so`).
3. The daemon queue under load: 6 jobs, a fake daemon answering in 300 ms: never more than 4 connections at once, no stage fails, `queued_ms > 0` on later events, a permit wait past the bound is `dropped` `reason: "busy"` and not an error. Pinned in Task 3 (`concurrency_is_bounded_and_busy_is_a_drop`).
4. Resumed CB jobs gain the guard without being re-created, and a `graph_put` that set `guard_hz: null` survives a resume. Pinned in Task 2 (`reexpand_carries_the_guard_params`).

---

## File map

- Create `crates/zeta-core/src/plans.rs` (`Segment`, `ChannelPlan`, `PLANS`, `UHF_CB_AU`, `channel_at`, `adjacent_hz`, `channels`); modify `src/lib.rs` (`pub mod plans`), `src/ops.rs` (`Op::ChannelPlans`, `OpResult::ChannelPlans`, `PlanInfo`, `PlanChannel`; `expand` `FmDemod` arm `:579-610`; `reexpand` `:422-452`).
- Modify `crates/zeta-dsp/src/channel.rs:204-211` (`ChannelParams.wide_hz`), `:255-280` (the cut, the note), `src/nbfm.rs:83-200` (guard params, centre filter, neighbour powers, gate), `src/squelch.rs:228-236` (`squelch_event` fields).
- Modify `crates/zeta-speech/src/transcribe.rs:28-33, 44-61, 94, 191-193, 275-352` (`min_utterance_ms`, `QUEUE`, `queued_ms`, reasons, `Busy`), `src/socket.rs` (semaphore, retry, `queued_ms`), `src/backend.rs` (`Transcript.queued_ms`, `SpeechError::Busy`), `src/node.rs:60`, `src/testing.rs` (fake daemon delay, concurrency counter).
- Modify `crates/zeta/src/cli.rs`, `src/main.rs` (`zeta plans`), `src/agents_md.rs` (notes, field craft); `crates/zeta/tests/ui.rs:233-260` (fixture `cb`).
- Create `crates/zeta-ui/web/plans.js`; modify `view.js:37-58` (`plan`), `agent.js:329-362` (`.tag.ch`), `overlays.js:255-275` (outline text), `waterfall.js:901-917` (channel ticks), `app.js` (load plans at mount), `style.css`; scenario in `crates/zeta-ui/tests/e2e/ui.py`.
- Create `examples/python/cb_watch.py`. Modify `README.md`, `docs/agents/field-craft.md`, `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (row 13, "Plan 13 status"), `AGENTS.md`, `skills/zetasdr/SKILL.md`.

---

### Task 1: The channel-plan registry and `channel_plans` (Sonnet)

**Files:**
- Create: `crates/zeta-core/src/plans.rs`
- Modify: `crates/zeta-core/src/lib.rs`, `src/ops.rs` (op, result, dispatch beside `KnowledgeLookup` `:1901-1915`, op name `:1109-1142`), `crates/zeta/src/cli.rs`, `src/main.rs`, `src/agents_md.rs`, `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-core/src/plans.rs` (unit), `crates/zeta-core/tests/ops.rs`, `crates/zeta/tests/cli.rs`

**Interfaces:**
- Produces: `pub struct Segment { pub first: u32, pub count: u32, pub first_hz: u64, pub step_hz: u64 }`; `pub struct ChannelPlan { pub id, pub name, pub region: &'static str, pub bandwidth_hz: u64, pub segments: &'static [Segment], pub notes: &'static [(u32, &'static str)] }`; `pub const PLANS: &[ChannelPlan]`; `pub fn channels(plan) -> Vec<(u32, u64)>` sorted by frequency; `pub fn channel_at(hz: u64) -> Option<(&'static ChannelPlan, u32)>` (nearest centre within `CHANNEL_MATCH_HZ`, first plan in `PLANS` wins); `pub fn adjacent_hz(plan) -> u64` (smallest gap between sorted centres); `Op::ChannelPlans {}` → `OpResult::ChannelPlans { plans: Vec<PlanInfo> }`, `PlanInfo { id, name, region, bandwidth_hz, adjacent_hz, channels: Vec<PlanChannel> }`, `PlanChannel { number: u32, center_hz: Hz, note: Option<String> }` (nullable required); CLI `zeta plans [--plan ID]` (unknown: exit 2, `"error":"other"`, `unknown plan <id> (known: uhf_cb_au)`).
- Consumes: nothing new.

- [ ] **Step 1: Write the failing tests** in `plans.rs`:

```rust
#[test]
fn uhf_cb_au_has_80_channels_on_two_interleaved_rasters() {
    // channels(&UHF_CB_AU).len() == 80; ch1 476_425_000, ch40 477_400_000, ch41 476_437_500,
    // ch80 477_412_500; adjacent_hz == 12_500.
}
#[test]
fn channel_at_matches_within_a_kilohertz() {
    // 476_900_000 -> ch20; 476_887_500 -> ch59; 476_912_500 -> ch60; 476_900_900 -> ch20;
    // 476_901_500 -> None; 147_050_000 -> None.
}
```
`tests/ops.rs`: `channel_plans_lists_uhf_cb_au` (apply the op: one plan, 80 channels, ch22 `note` non-null, ch20 `note` null; schema: `channel_plans` takes `{}`, `PlanChannel.note` nullable required). `cli.rs`: `plans_prints_the_table_and_refuses_an_unknown_id`.
- [ ] **Step 2: Run to fail**: `cargo test -p zeta-core plans`, `--test ops`, `cargo test -p zeta --test cli plans`.
- [ ] **Step 3: Implement**; the notes (5, 35 emergency; 11 calling; 22, 23 data, no voice; 40 road) checked against the ACMA CB class licence. Guide note: "`channel_plans` lists the channel plans the node knows (`uhf_cb_au`: Australian UHF CB, 80 channels, two rasters 12.5 kHz apart). Name a channel by its number to the user ("ch20") and start jobs at its `center_hz` with the plan's `bandwidth_hz`."
- [ ] **Step 4: Run to pass**, then `cargo test -p zeta-core -p zeta --test agents_md --test transports`.
- [ ] **Step 5: Gate and commit**: `git commit -m "Channel plans: a table-driven registry (uhf_cb_au), channel_plans op, zeta plans"`.

---

### Task 2: The adjacent-channel guard in `nbfm_demod` (Opus)

**Files:**
- Modify: `crates/zeta-core/src/ops.rs:422-452` (carry `guard_hz`, `guard_db`, `channel_hz`), `:579-610` (`FmDemod` arm), `crates/zeta-dsp/src/channel.rs:204-211, 255-280`, `src/nbfm.rs`, `src/squelch.rs:228-236`, `crates/zeta/src/agents_md.rs`, `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-core/tests/ops.rs`, `crates/zeta-dsp/src/channel.rs`, `src/nbfm.rs` (with `test-fixtures`)

**Interfaces:**
- Produces: `expand` for `FmDemod` with `channel_at(center)` = `(plan, _)` and `g = adjacent_hz(plan)`, `bw + 2g <= out`: channel node `"wide_hz": bw + 2g`, demod node `"guard_hz": g, "guard_db": GUARD_DB_DEFAULT, "channel_hz": bw`; else neither key. `ChannelParams { center_hz, bandwidth_hz, out_rate_hz, #[serde(default)] wide_hz: Option<u64> }`: cut at `wide_hz / 2` when `center ± wide_hz/2` is inside the block and `wide_hz <= out_rate_hz`, else at `bandwidth_hz / 2` with the note `{"channel": {<the plan 12 path fields>, "wide": "off: <reason>"}}` (`reason`: `leaves the block <min>..=<max>` or `wider than out_rate_hz <o>`); the bank-path width rule is judged on the band actually cut. `NbfmParams` gains `#[serde(default)] guard_hz: Option<u64>, guard_db: Option<i64>, channel_hz: Option<u64>` (`guard_hz` without `channel_hz`, or `channel_hz + 2·guard_hz > rate`, fails the stage `nbfm_demod params: guard_hz needs channel_hz with channel_hz + 2 guard_hz <= the channel rate`; `guard_db` null = 12; outside 0..=60 fails `guard_db <v> out of range 0..=60`); with a guard: a `GUARD_TAPS` Hann low-pass at `channel_hz / 2` (`util::lowpass_taps`) filters every block, the filtered block feeds `Squelch::feed` and the discriminator; when `Squelch::is_open()` after the feed, neighbour powers `P±` from the same taps on the block mixed by `∓guard_hz` (continuous NCO phase across blocks); `guarded = max(P+, P-) - P_centre > guard_db`; the gate is `open && !guarded`; events on each change of the gate: `squelch_event(sq, gate, adjacent_db, guarded)` with `adjacent_db: null` without a guard.
- Consumes: Task 1's `channel_at`, `adjacent_hz`; plan 12's `ctx.note`, `Squelch`.

- [ ] **Step 1: Write the failing tests**. `tests/ops.rs`:

```rust
#[test]
fn plan_channels_carry_the_guard() {
    // FmDemod at 476_887_500 / 12_500 on a 256_000 bin rate (out 51_200): channel wide_hz
    // 37_500; nbfm guard_hz 12_500, guard_db 12, channel_hz 12_500.
}
#[test]
fn off_plan_graphs_are_unchanged() { /* 147_050_000: no wide_hz, no guard keys; params as plan 12 */ }
#[test]
fn reexpand_carries_the_guard_params() { /* stored guard_hz null survives reexpand at another rate */ }
```
`channel.rs`: `wide_band_off_the_block_falls_back_and_says_so` (block 476_918_750 ± 512 000, channel 476_425_000 with `wide_hz` 37_500: output as without `wide_hz`, note `wide: off: leaves the block ...`); `wide_hz_passes_the_neighbour` (a tone 12.5 kHz off the centre is within 1 dB through `wide_hz` 37_500, at least 10 dB down without it). `nbfm.rs` (fixtures `nbfm_modulate`, `awgn`, 51 200 Hz): `a_strong_neighbour_holds_the_gate_closed` (centre empty, a 5 kHz-deviation FM carrier at +12.5 kHz 20 dB over the noise: zero audio frames, one event `open: false`, later events `guarded: true`); `a_centre_signal_opens_with_a_quiet_neighbour`; `equal_neighbours_do_not_guard` (signals on both, centre 3 dB weaker: audio flows); `the_guard_only_closes` (over 5 s of mixed fixtures, every block whose gate is open has the unguarded squelch open); `guard_params_out_of_range_fail_the_stage`.
- [ ] **Step 2: Run to fail**: `cargo test -p zeta-core --test ops guard`, `cargo test -p zeta-dsp --features test-fixtures channel nbfm`.
- [ ] **Step 3: Implement.** Guide note under the demodulators: "`fm_demod` on a channel of a plan (`channel_plans`) carries an adjacent-channel guard: its squelch stays closed while a channel `guard_hz` away is more than `guard_db` (12) dB stronger, so a transmission on ch20 does not open ch59 or ch60. The `squelch` event carries `adjacent_db` and `guarded`. `graph put` with `guard_hz: null` on the demod node turns it off."
- [ ] **Step 4: Run to pass**: the targets, then `cargo test -p zeta-dsp --features test-fixtures`, `-p zeta-core`, `-p zeta --test agents_md --test transports`. Note in the commit body the per-channel time of `nbfm_demod` with and without the guard on 10 s of 51 200 Hz noise (release build).
- [ ] **Step 5: Gate and commit**: `git commit -m "Adjacent-channel guard: fm_demod on a plan channel widens its channel, filters the centre and stays closed under a stronger neighbour"`.

---

### Task 3: Transcribe: `min_utterance_ms` and the daemon queue (Sonnet)

**Files:**
- Modify: `crates/zeta-speech/src/transcribe.rs`, `src/socket.rs`, `src/backend.rs`, `src/node.rs`, `src/testing.rs`, `crates/zeta/src/agents_md.rs`, `docs/agents/field-craft.md:44` ("Transcribing a channel"), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-speech/src/transcribe.rs` (unit), `crates/zeta-speech/tests/` (the existing socket/stage tests' file, by the builder's grep)

**Interfaces:**
- Produces: `TranscribeParams { ..., #[serde(default = "d700")] min_utterance_ms: u32 }`; `Accumulator::new(gap_ms, max_utterance_s, min_utterance_ms)`; a short utterance is counted, not offered; the stage notes `{"transcribe": {"short": N}}` through `ctx.note` at most once a second while `N` grows; `Transcript { text, language, queued_ms: u64 }`; `SpeechError::Busy(Duration)` displayed `the speech daemon is busy: no slot within <s> s`; `SocketBackend` holds `Arc<Semaphore>` of `STT_CONCURRENCY`, `acquire` under `STT_QUEUE_WAIT` (else `Busy`), `queued_ms` = the wait, the call timeout applied to `exchange` only; `exchange`'s connect retried once after `STT_RETRY_DELAY` on `ErrorKind::WouldBlock` or `ConnectionRefused`; the worker's outer bound `timeout + STT_QUEUE_WAIT`; `transcript` events gain `"queued_ms"`; `Busy` → `{"protocol": "transcript", "dropped": true, "reason": "busy", ...}`, not counted in `MAX_ERRORS`; a full per-job queue → `dropped` with `"reason": "queue"`; `QUEUE = 4`; `FakeDaemon::with_delay(path, reply, Duration)` and `max_concurrent() -> usize`.
- Consumes: plan 12's `ctx.note`; `transcripts.log` tee (unchanged; `reason` is not logged).

- [ ] **Step 1: Write the failing tests**:

```rust
#[test]
fn utterances_under_min_utterance_ms_are_discarded() {
    // Accumulator::new(300, 15, 700): a 600 ms burst closes to nothing, 800 ms to one utterance;
    // default params deserialise min_utterance_ms 700.
}
#[tokio::test]
async fn concurrency_is_bounded_and_busy_is_a_drop() {
    // FakeDaemon::with_delay 300 ms; 6 transcribe stages share one SocketBackend, each fed one
    // 1 s utterance at once: max_concurrent() <= 4; all 6 transcripts arrive; the 5th and 6th
    // carry queued_ms >= 250; with STT_QUEUE_WAIT shortened (a test constructor) a wait past it
    // is a dropped event with reason "busy" and the stage keeps running.
}
#[tokio::test]
async fn eagain_on_connect_is_retried_once() { /* a listener that refuses the first connect */ }
#[tokio::test]
async fn short_utterances_are_noted_in_the_job_context() { /* note {"transcribe":{"short":2}} */ }
```
- [ ] **Step 2: Run to fail**: `cargo test -p zeta-speech`.
- [ ] **Step 3: Implement.** Guide: `transcribe`'s `min_utterance_ms` ("utterances shorter than this, ms (700), are not sent: noise bursts behind an opening squelch") and the `dropped` reasons; field craft: "`queued_ms` on a transcript is how long it waited for the speech daemon (at most 4 at once); `dropped` with `reason: busy` means the daemon had no slot for 30 s."
- [ ] **Step 4: Run to pass**: `cargo test -p zeta-speech`, `-p zeta --test agents_md --test transports`, `-p zeta-core --test transcripts_log` (the log line is unchanged).
- [ ] **Step 5: Gate and commit**: `git commit -m "Transcribe: min_utterance_ms (700), a bounded speech-daemon queue with queued_ms, busy and queue drops named"`.

---

### Task 4: The page names channels (Sonnet)

**Files:**
- Create: `crates/zeta-ui/web/plans.js`
- Modify: `view.js:37-58`, `agent.js:329-362`, `overlays.js:255-275`, `waterfall.js:901-917`, `app.js` (mount: `loadPlans(state.api)` before the first job render), `style.css`, `crates/zeta/tests/ui.rs:233-260` (fixture `cb`: `tone_at(dir, "cb", 476_900_000)`, appended last to the serve flags and the doc comment)
- Test: `crates/zeta-ui/tests/e2e/ui.py`

**Interfaces:**
- Produces: `plans.js` `loadPlans(api) -> Promise<void>` (`channel_plans` once; an error leaves the list empty), `planIds() -> string[]`, `channelLabel(hz, policy) -> {text: "ch20", title: "uhf_cb_au ch20 · <note>"} | null` (`auto` any plan, `<id>` that plan, `none` null; within 1 000 Hz), `channelsIn(lo, hi, policy) -> [{number, center_hz}]`; `view.js` entry `{ key: "plan", parse, default_: "auto", describe }` (`auto`, `none` or a loaded id; `describe` lists them); job rows: `<span class="tag ch">ch20</span>` in the kind cell; outline text `ch20 <kind>`; axis: one `.tick.ch` per channel in view when the shown band's channels are at least 14 px apart; all redrawn on `view.onChange` for `plan`.
- Consumes: Task 1's op.

- [ ] **Step 1: Write the failing scenario** `scenario_channel_plan_labels` (on `cb`): select `cb`; `job_start` (API) `fm_demod` at 476 900 000 / 12 500 → the row's `.tag.ch` reads `ch20` within 3 s; `.outline-label` for the job contains `ch20 fm_demod`; zoom the band to 476.88-476.92 MHz (the band control) → `.tick.ch` texts include `20`, `59`, `60`; `view set plan none` → no `.tag.ch`, no `.tick.ch`, the outline reads `fm_demod`; `view set plan bogus` → an error line naming `auto, none, uhf_cb_au`; `view reset` → `ch20` back. Under 8 s.
- [ ] **Step 2: Run to fail**: `ZETA_UI_ONLY=channel_plan_labels cargo test -p zeta --test ui`.
- [ ] **Step 3: Implement.** **Step 4: Run to pass**: the scenario, then the whole suite (total time in the commit body; under 300 s).
- [ ] **Step 5: Gate and commit**: `git commit -m "UI: channel numbers on job rows, outlines and the axis; view set plan auto|<id>|none"`.

---

### Task 5: `cb_watch.py`, docs, plan series, final gate (Sonnet)

**Files:**
- Create: `examples/python/cb_watch.py`
- Modify: `README.md` (routes: `zeta plans`, the guard, `min_utterance_ms`, the example), `docs/agents/field-craft.md` (new "Channel plans": read `channel_plans`, name channels by number, the CB interleave and why ch59/60 echoed ch20; a pointer to `cb_watch.py`), `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (row 13's status and "Plan 13 status"), `AGENTS.md`, `skills/zetasdr/SKILL.md` if field craft is embedded
- Test: `crates/zeta/tests/` a test that runs `python3 -I examples/python/cb_watch.py --help` (exit 0) and `--dry-run` against a `ServeProc` with the `cb` fixture (prints the plan channels it would watch, starts nothing)

**Interfaces:**
- Produces: `cb_watch.py --node URL --secret-env VAR --source ID [--plan uhf_cb_au] [--open-db 10] [--idle-s 60] [--max-tuners 8] [--dry-run]`, stdlib only (`urllib.request`, `json`, `argparse`); `POST /op` with the secret header the CLI sends; loop of spec 3.4 (one elastic `spectrum` job, `stream_tail` once a second, per-channel peak over its band against the row's 15th-percentile floor, `fm_demod` + `graph_put` fixed `squelch_db` at floor + 6 + `job_attach` `transcribe`, stop after `--idle-s`, adopt running `fm_demod` jobs on plan channels at start, stop what it started on SIGINT/SIGTERM).
- Consumes: Task 1's op, existing ops.

- [ ] **Step 1: Write the failing test** (`cb_watch_dry_run_lists_the_channels_in_the_capture`). **Step 2: Run to fail.** **Step 3: Implement and write the docs.**
- [ ] **Step 4: Final gate**: `cargo fmt -p <each member> -- --check`, clippy, `cargo test --workspace --all-features --no-fail-fast` (no `FAILED`, no `ui e2e skipped`; record the UI suite's time), `agents-md --check`, `--skill --check`.
- [ ] **Step 5: Commit**: `git commit -m "Plan 13: cb_watch example, docs, field-craft, status"`.

---

## Live acceptance (controller, after the final review)

On the field host, rebuilt from a bundle of the branch with the B210 pinned at 476 918 750 / 1 000 000 Hz and the
CB jobs resumed (not re-created): `zeta plans` lists `uhf_cb_au`; `zeta graph get --job <ch59>` shows
`guard_hz` 12 500 and the channel's `wide_hz` 37 500; ch1's `job context` reads `wide: off: ...`. During
a ch20 transmission ch59 and ch60 stay `squelch` on the page and their `squelch` events read
`guarded: true`; `transcripts tail` shows one line per ch20 transmission, none on 59/60, no
one-token lines from sub-second bursts, and `queued_ms` on events when channels overlap; over an
hour the `dropped` count is near zero and no `transcribe` job fails. CPU with 80 guarded channels
stays within one core of plan 12's figure (`channel`/`nbfm_demod` lag counters zero). The page shows
`ch20` on the row and outline and channel ticks when zoomed; `view set plan none` clears them. Then
`cb_watch.py` with all CB jobs stopped: a transmission on ch27 starts a demodulator and a transcript
line follows; 60 s after it ends the jobs are gone.
