# Channel plans: labels by channel number, an adjacent-channel guard, cleaner transcripts

Date: 2026-10-21. Status: draft for builder review.

## 1. Intent and why

Plan 12 ran on the field host with the B210 pinned to the AU UHF CB band (`source_pin` at
476 918 750 Hz / 1 000 000 Hz, a 1 024 000 Hz capture) and two hours of `transcripts.log`. Four
faults and one wish came out of it, in the user's priority order.

**Numbers, not hertz.** The operator and the agent talk about "ch20"; the page shows hertz only:
the axis prints five ticks in MHz (`crates/zeta-ui/web/waterfall.js:901-917`), a rigid job's
outline is labelled with its kind (`overlays.js:255-275`), a job row shows kind, source, state and
id (`agent.js:329-362`). Nothing in the node or the page knows a channel plan; the band plan the
node has is `knowledge_lookup`'s allocations (`crates/zeta-core/src/knowledge.rs:11-23`), ranges
with a service name.

**Interleaved neighbours open their squelch.** Every ch20 (476.900 MHz) transmission also opened
ch59 (476.8875) and ch60 (476.9125), 12.5 kHz either side, and those produced duplicate or empty
transcripts. The cause is in the channel filter: a Hann-windowed sinc (`crates/zeta-dsp/src/util.rs:33-51`)
of `8 · decim + 1` taps, at least 63 (`crates/zeta-dsp/src/channel.rs:19-34`), cut at
`bandwidth_hz / 2` (`channel.rs:100`). On the 1 024 000 Hz capture the bank has 8 bins
(`crates/zeta-dsp/src/pfb.rs:13`) of 128 kHz, a 256 kHz bin rate, a channel rate
`channel_rate_hz(256 000, 12 500) = 51 200` (`crates/zeta-core/src/ops.rs:957-966`), so decimation
5 and 63 taps: a transition band of about 3.1 · 256 000 / 63 ≈ 12.6 kHz centred on the 6.25 kHz
cutoff. A neighbour 12.5 kHz away, whose FM sidebands reach to about 4.5 kHz of this channel's
centre with 5 kHz deviation, is barely attenuated; the 10 kHz filters of the current mitigation
(`~/zetasdr/scratch/cb-setup.sh`, tuners only on ch6, 9, 20, 27) move the cutoff, not the
transition. The squelch (`crates/zeta-dsp/src/squelch.rs:28-41`) then sees the neighbour's
energy 6 dB over its floor and opens. Overlap of interleaved channels is inherent to the plan
(25 kHz-era radios on a 12.5 kHz raster), so a sharper filter alone does not remove it.

**Drops.** 40 of 117 lines were `dropped`. Each `transcribe` job holds one call in flight and
`QUEUE = 2` waiting (`crates/zeta-speech/src/transcribe.rs:31, 275-299`); one `SocketBackend` is
shared by every job (`crates/zeta/src/transport.rs:216-221`) and opens one connection per call
(`crates/zeta-speech/src/socket.rs:115-117`), so three channels firing at once put three calls on a
daemon that answers one at a time, and a fourth utterance on any of them is dropped. Once the
connect answered EAGAIN (the daemon's listen backlog full), an `Unreachable` error, which counts
toward the two consecutive errors that fail the stage (`transcribe.rs:33, 348-352`).

**Noise words.** Sub-second bursts became one-token lines ("Uh", "Yeah."). The squelch's 300 ms hang
(`squelch.rs:32`) plus a 200 ms burst is just over the 500 ms minimum (`transcribe.rs:28-29,
191-193`).

**Only active channels** (optional): 80 tuners on mostly idle channels; the user wants the node to
run demodulators only where there is activity.

## 2. Goals, non-goals, anti-goals

Goals:

- One table-driven channel-plan registry in the node (one plan now, `uhf_cb_au`; a plan is one table
  entry), served by an op, so the page, the CLI and the agent use the same numbers.
- The page labels axis, outlines and job rows with channel numbers when the view or a job lies on
  a plan, under a view policy `plan auto | <plan id> | none`.
- A channel whose power is far below an adjacent channel's never opens its squelch.
- Utterances shorter than `min_utterance_ms` (default 700) never reach the daemon.
- The speech daemon sees a bounded number of concurrent connections; a busy daemon delays rather
  than drops or fails jobs; the delay is reported.
- Optionally: demodulators only on active channels, as a stdlib Python script on existing ops.

Non-goals: a node-side `channel_watch` job kind (8.4); CLI sugar `--channel 20` on `job start`;
plans other than `uhf_cb_au`; a guard for `am_demod`; repeater-pair semantics.

Anti-goals: no change to `job_start`, `job_list`, `JobRecord` or the lease rules; no job off a
plan changes graph, cost or behaviour; the guard never makes a channel open that would be closed
without it; the transcript log's line keeps plan 12's fields.

## 3. Node side

### 3.1 The channel-plan registry (`zeta-core`, new `src/plans.rs`)

```rust
pub struct Segment { pub first: u32, pub count: u32, pub first_hz: u64, pub step_hz: u64 }
pub struct ChannelPlan { pub id: &'static str, pub name: &'static str, pub region: &'static str,
    pub bandwidth_hz: u64, pub segments: &'static [Segment], pub notes: &'static [(u32, &'static str)] }
pub const PLANS: &[ChannelPlan] = &[UHF_CB_AU];
pub const CHANNEL_MATCH_HZ: u64 = 1_000;
pub fn channel_at(hz: u64) -> Option<(&'static ChannelPlan, u32)>;   // nearest within the match
pub fn adjacent_hz(plan: &ChannelPlan) -> u64;                       // smallest centre spacing
```

`UHF_CB_AU`: `region` `au`, `bandwidth_hz` 12 500, segments `{1, 40, 476 425 000, 25 000}` and
`{41, 40, 476 437 500, 25 000}`, notes for 5 and 35 (emergency), 11 (calling), 22 and 23 (data, no
voice), 40 (road); the builder checks the notes against the ACMA CB class licence. `adjacent_hz` is
12 500 (ch20 and ch59/ch60). Adding a plan is one `ChannelPlan` constant and one entry in `PLANS`.
A frequency on two plans takes the first in `PLANS`.

New op `channel_plans {}` → `OpResult::ChannelPlans { plans: [{id, name, region, bandwidth_hz,
adjacent_hz, channels: [{number, center_hz, note: string | null}]}] }`, channels expanded and sorted
by frequency (80 for `uhf_cb_au`, about 6 KiB). It is read-only and local; `knowledge_lookup` is
unchanged (its allocations are ranges with no raster; folding channels in would change its result
for every provider). CLI `zeta plans [--plan ID]` prints the op's result (one plan, or an
`other("unknown plan <id> (known: uhf_cb_au)")`).

Recommendation (8.1): the registry lives in the node. The page alone would be cheapest for labels,
but the guard (3.2) needs the raster at `expand`, the agent needs it through the schema, and the
optional watcher (3.4) needs it too; one table served by an op keeps them from drifting.

### 3.2 The adjacent-channel guard (option (a), in `nbfm_demod`)

Options weighed: (a) the demodulator takes a wider channel (three raster steps) and does the final
channel filtering itself, comparing centre and neighbour power before its squelch opens; (b) a
per-source power map from the bank or a spectrum job, consulted by every squelch, a cross-job
coupling with its own timing and no stage owning it; (c) sharper channel filters only, which do not
remove the inherent overlap (section 1) and cost every channel the longer filter at the bin rate.
Chosen: (a), it stays inside one stage and one job's graph.

**Expand.** `JobKind::FmDemod` (`ops.rs:579-610`) consults `channel_at(tuning.center_hz)`: on a
plan whose `adjacent_hz` `g` satisfies `bandwidth_hz + 2g <= out` (the channel rate), the channel
node gains `wide_hz: bandwidth_hz + 2g` and the demod node `guard_hz: g`, `guard_db: 12`, `channel_hz: bandwidth_hz`; off a plan
(or too narrow a channel rate) both are absent and the graph is today's. `reexpand` (`ops.rs:422-452`)
carries `guard_hz`, `guard_db` and `channel_hz` like `squelch_db`, so a `graph_put` that set `guard_hz: null`
survives a resume; `fill_squelch_params` (`ops.rs:927-950`) is unchanged (the guard is opt-in by
param). Resumed CB jobs are re-expanded at their rate and gain the guard without being re-created.

**Channel stage.** `ChannelParams` gains `wide_hz: Option<u64>` (`#[serde(default)]`): when set and
`center ± wide_hz/2` lies inside the block and `wide_hz <= out_rate_hz` (`channel.rs:91`), the
stage cuts at `wide_hz / 2` instead of `bandwidth_hz / 2`; otherwise (a channel at the capture's
edge: ch1 widened reaches 476.40625 MHz, below the pinned capture's 476.40675) it cuts as today and
notes `{"channel": {..., "wide": "off: <why>"}}` through `ctx.note` (`launch.rs:179`). Same taps,
same cost; the bank-path rule (a band no wider than a bin) is judged on `wide_hz`.

**Demod stage.** `NbfmParams` gains `guard_hz: Option<u64>`, `guard_db: Option<i64>` and `channel_hz:
Option<u64>` (default null: no guard, today's stage). With `guard_hz = g` at channel rate `r`:

- a centre filter, a 63-tap Hann low-pass at `channel_hz / 2` (a third param `expand` writes:
  the job's own `bandwidth_hz`), runs on every block; its output, not the raw block, feeds the `Squelch` and the discriminator, so the
  squelch and the audio see the channel, not its neighbours (transition ≈ 3.1 · 51 200 / 63 ≈ 2.5 kHz);
- on a block where the centre's squelch would be open, the same filter runs on the block shifted by
  `+g` and `-g` (two NCO mixes) and gives the neighbours' powers `P+`, `P-`; the guard holds the
  gate closed while `max(P+, P-) - P_centre > guard_db`. `Squelch` keeps its floor, hang and M2M4
  rules; the guard is an extra condition on its output (`open && !guarded`), so it can only close.
- the `squelch` event (`squelch.rs:228-236`) gains `adjacent_db` (the stronger neighbour, dBFS, or
  null without a guard) and `guarded: bool`; a guard transition is a transition. Additive fields:
  the page's tag reads only `open`.

**DSP cost**, per channel at 51 200 Hz: the channel stage today does 51 200 × 63 ≈ 3.2 M complex
MAC/s; the centre filter adds 3.2 M always (the idle tuner's cost doubles), the neighbour filters
6.4 M more only while the centre is over its threshold. 80 idle CB channels: about +260 M MAC/s,
under one core; `channel` and `nbfm_demod` lag counters (plan 12, by kind) show whether it holds.
`guard_db` 12 is a first guess for live acceptance (8.2).

### 3.3 Transcribe: minimum length and a daemon queue (`zeta-speech`)

**`min_utterance_ms`.** `TranscribeParams` gains `min_utterance_ms: u32` (`#[serde(default)]`
700). `Accumulator::new(gap_ms, max_utterance_s, min_utterance_ms)` replaces `MIN_SAMPLES`
(`transcribe.rs:28-29, 191-193`). A short utterance is not published (a line per noise burst is the
clutter being removed); the stage counts them and notes `{"transcribe": {"short": N}}` through
`ctx.note` at most once a second, so `zeta job context` explains a quiet channel. Recommendation
8.3.

**`SocketBackend` queue.** One `tokio::sync::Semaphore` of `STT_CONCURRENCY` (4) permits per
backend, shared by all jobs. `transcribe` waits at most `STT_QUEUE_WAIT` (30 s) for a permit, then
returns `SpeechError::Busy(<ms>)`; with a permit, the call timeout covers the exchange only. A
connect that fails with `WouldBlock` (EAGAIN) or `ConnectionRefused` while the socket file exists is
retried once after 200 ms. `Transcript` gains `queued_ms: u64` (the permit wait; `NodeBackend` and
the fakes answer 0). The worker's outer bound (`transcribe.rs:302-310`) becomes the call timeout plus
`STT_QUEUE_WAIT`. In the stage: `queued_ms` is added to the `transcript` event (additive);
`Busy` publishes a `dropped` event with `"reason": "busy"` and does not count toward `MAX_ERRORS`; a
full per-job queue keeps publishing `dropped` with `"reason": "queue"`. `QUEUE` rises from 2 to 4:
the daemon's wait is now bounded and explicit, and with the guard and the minimum length fewer
utterances arrive. `transcripts.log`'s line is unchanged (`dropped: true` stays the signal).

### 3.4 Active channels only (optional, minimum viable)

`examples/python/cb_watch.py`, standard library only, against `POST /op` with the shared secret
(`crates/zeta-ws/src/http.rs:320-329`): read `channel_plans`; start one elastic `spectrum` job on the
pinned source; every second `stream_tail` its last row, take each plan channel's peak over its band
from `q` against the row's floor; on a channel over `--open-db` (10) start `fm_demod` with
`squelch_db` fixed at floor + 6 through `graph_put` (a demod started mid-transmission would seed
its automatic floor on the signal, `squelch.rs:9-16`) and `job_attach` a `transcribe`; stop both
after `--idle-s` (60) without a row over the threshold; `--max-tuners` (8) caps them; it exits
stopping what it started. Jobs it starts are ordinary persisted jobs; a restart of the script
adopts running `fm_demod` jobs on plan channels. A node-side `channel_watch` kind is 8.4.

## 4. Page side

New `crates/zeta-ui/web/plans.js`: `loadPlans(api)` calls `channel_plans` once at mount (a node
without the op leaves the list empty and every label off), `channelLabel(hz, policy) -> "ch20" | null`
(`auto`: any plan; `<id>`: that plan; `none`: null).

`view.js`'s registry (`view.js:37-58`) gains `{ key: "plan", parse, default_: "auto" }` accepting
`auto`, `none` or a source-id-shaped word that is a loaded plan id; `describe` names the loaded ids
(`auto, none, uhf_cb_au`). `view set plan ...`, `view show` and `view reset` work as for every key.

- **Job rows** (`agent.js:329-362`): a job whose `tuning.center_hz` maps to a channel gets
  `<span class="tag ch">ch20</span>` in its kind cell, title `uhf_cb_au ch20 · <note>`.
- **Outlines** (`overlays.js:255-275`): a rigid job's outline text becomes `ch20 fm_demod`.
- **Axis** (`waterfall.js:901-917`): when the shown band overlaps a plan and its channels are at
  least 14 px apart, the axis adds a `.tick.ch` per channel in view (`20`, `59`, ...) beside the
  MHz ticks; otherwise only the MHz ticks. Redrawn on `onChange("plan")`.

Tests: one Playwright scenario, under 8 s, on a new fixture source `cb` (the `tone`
recording relabelled at 476 900 000 Hz, `tone_at`, `crates/zeta/tests/ui.rs:147`), added last so
other scenarios' source order is unchanged.

## 5. Agent and CLI

`zeta plans [--plan ID]`. Guide: the `channel_plans` op section from its doc comment; under the
demodulators, the guard ("`fm_demod` on a plan channel compares its power with the adjacent
channels', `guard_hz` and `guard_db` on the demod node, `graph_put` `guard_hz: null` turns it
off; the `squelch` event says `guarded`"); `transcribe`'s `min_utterance_ms` and the `dropped`
reasons; field craft "Channel plans" (read `channel_plans`, name channels by number to the user,
the CB interleave and why ch59/60 echo ch20) and a line in "Transcribing a channel"
(`docs/agents/field-craft.md:44`) on `queued_ms`.

## 6. Constraints

- `zeta-core` gains no dependency; `JobRecord`, `job_start`, `job_list` and the lease rules are
  unchanged. One new op, `channel_plans {}`; new params are optional (`#[serde(default)]`) so stored
  graphs load. Both ends of a `--source-node` link need not move together (no shared result changes).
- `transcripts.log` lines keep plan 12's fields; events gain only additive fields.
- UI suite: about 237 s of 300 s; one new scenario, under 8 s; one new fixture source.
- Gates as plan 12: `cargo fmt -p` per crate, clippy `-D warnings`, crate tests, the Playwright
  suite with no skip line, `agents-md --check` and `--skill --check`.

## 7. Left to the builder

The centre filter's tap count if 63 lags at 80 channels (a power of the decimation, as
`taps_for`); whether neighbour powers come from two FIRs or one 512-point FFT per 10 ms block
(similar cost; the FIRs reuse code); the axis tick's style and the 14 px rule; the `short` note's
rate limit; how `cb_watch.py` reads a row's floor (the 15th percentile of `q`, `measure`'s rule).

## 8. Open questions

1. **Where the mapping lives.** Recommendation: node registry plus `channel_plans` op (3.1). The
   page-only alternative is about half of Task 1's size but leaves the guard without a raster.
2. **The guard.** Recommendation: option (a) as in 3.2, on by default for plan channels with
   `guard_db` 12, tuned at live acceptance. A genuine weak signal on ch59 while ch20 is 12 dB stronger
   is suppressed; that is the trade.
3. **Short utterances.** Recommendation: `min_utterance_ms` 700, not published, counted in
   `job_context`. The alternative (a `dropped` line with `reason: "short"`) explains every gap but
   puts the noise back into `transcripts.log` as lines.
4. **Active channels.** Recommendation: the Python script (3.4) now; a node-side `channel_watch` kind
   (persisted, one measurement over the bank's bins, no websocket polling) only if the script proves
   the policy (open threshold, idle timeout, first-syllable loss of a start on activity) worth it.
5. **`STT_CONCURRENCY`.** Recommendation: a constant 4 now; a flag `--stt-concurrency` only if a
   daemon that answers in parallel shows up (`voiced` answers serially).

## 9. Calls made in this document

- One channel-plan table in `zeta-core`, served by `channel_plans {}`; `knowledge_lookup` untouched.
- The guard is option (a), opt-in by params that `expand` writes only for plan channels; the channel
  stage widens to three raster steps when it fits, the demod filters the centre itself and compares.
- The guard can only close a squelch; the event gains `adjacent_db` and `guarded`.
- `min_utterance_ms` replaces the 500 ms constant; short utterances are counted, not published.
- A per-backend semaphore of 4, a 30 s bounded wait, one retry on EAGAIN; `queued_ms` on the event;
  a busy daemon drops (`reason: "busy"`) rather than fails the job; `QUEUE` 4.
- Active-channel tracking ships as `examples/python/cb_watch.py`, not a job kind.
