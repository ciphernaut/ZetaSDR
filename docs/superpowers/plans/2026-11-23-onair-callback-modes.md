# OnAir's Modes with No Bus Emitter (Plan 46) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** RDS, DMR, P25, M17, DVB-T and analog video each gain OnAir as a second handler giving events only, proven by `zeta modes check --handler onair` where the row has a capture, and the node-wide handler ranking never takes speech or picture away.

**Architecture:** `onair-rx` (a separate GPL repository) gains one polled path: a receiver is fed, its state snapshot is read at a fixed interval of samples, and a small session per mode writes a line for what is new or changed. A mode is a table row there and a handler-file entry in the node. The node changes in one place: the node-wide ranking passes over a handler that gives fewer kinds of output than the row's own decoder.

**Tech Stack:** C++20 against OnAir `a63c3a8+patched` (`onair-rx`: `build.sh`, `test/*.sh`); Rust 1.97 (`zeta-core`, `zeta`); JSON handler files.

**Spec:** `docs/superpowers/specs/2026-11-22-onair-callback-modes-design.md` ("the note" below), on `2026-11-22-onair-bus-modes-design.md` and `2026-11-21-alternative-handlers-design.md` section "OnAir". Read the note first; it lists what is left to the builder and the risks.

## Global Constraints

- **Two repositories.** Tasks 1 to 3 work in `/Projects/onair-rx` (branch `callback-modes` from `master`); tasks 4 to 7 in this repository's worktree. `onair-rx` and OnAir are never copied, linked or vendored into this repository: only `handlers/onair.json` names them.
- **No OnAir rebuild and no new patch.** All six receivers are in the built `_onair-build/libdect2core.a`; `build.sh` then only compiles and links `src/*.cpp` (about a minute). A second carried patch is a non-goal: where a receiver has no switch for something wanted off, live with it and write it down.
- **`onair-rx` licence:** GPL-3.0-or-later; every new source file starts with the SPDX line and `Based on OnAir by Kristijonas Adomaitis - https://on-air.dev`.
- **The wrapper's contract is unchanged for the eleven existing modes:** their lines and packets do not change by a byte (`test/run.sh` and `test/bus.sh` pass untouched).
- **New mode names in `onair-rx`:** `rds`, `dmr`, `p25`, `m17`, `dvbt`, `video`. None has packets.
- **Every line of a polled mode** has `event` and `t_sample` (index of the first sample of the block after which the poll ran), a member only when its value is known, whole numbers without a decimal point, non-finite numbers left out. Values are spelled as the node's row spells them (its field names and enumerations are in `modes/<id>.json` of this repository).
- **Input rates** (`rate_hz` in the handler entry; the receiver's minimum in `onair-rx`): `dmr` 48000, `p25` and `m17` 250000, `rds` 500000, `dvbt` 10000000, `video` the lowest rate Task 3 finds that locks, confirmed against the listing bar in Task 6.
- **Events only.** No audio tap, no frame poll, no transport stream out. Events must appear with mbelib and Codec 2 hidden from the wrapper.
- **The listing bar** (note, Risks): a mode goes into `handlers/onair.json` only if its check passes (DVB-T: its `onair-rx` test) and its job keeps real time on the build host with the node dropping no samples. A mode that misses stays in `onair-rx`, out of the handler file, with one line under plan 46 in `TODO.md` saying what it needed.
- **The node is unchanged for a node with no `onair-rx`:** every existing test passes, and OnAir's handlers are `unavailable` with the missing program named.
- **No row changes, no new operation, no page change.**
- Public repository: roles not host names, no observed traffic; captures of real traffic never enter git. Test signals are synthesised. `scripts/public-check.sh` and `scripts/check-doc-links.sh` clean before the branch lands.
- Gates per `CLAUDE.md`: `cargo fmt -p <crate> -- --check` per crate (nested worktree), clippy `-D warnings`, a private `CARGO_TARGET_DIR`. Per task only its tests and neighbours; crate suites once per batch; the workspace suite and **the UI suite once at the end** (the handler file changes; plan 45 skipped it and CI went red). `zeta-core` tests take `--features test-fixtures`.
- Commit messages: `onair-rx: <what>` in `onair-rx`; `OnAir callback modes: <what>` here.
- Nothing is pushed, deployed or offered upstream by this plan. Landing on `main`, the field host and any offer to OnAir wait for the operator's word.

## Review Focus

- **A check capture that repeats without end** (the node's check source loops it in real time): the same call comes round every few seconds. Each pass gives one first line and one last line; a call that simply stays up gives no line per poll. (Task 2)
- **A call that leaves the snapshot's log without ever being marked ended** (the log ages entries out): its last line is written once, when it is gone, and never a second time. (Task 2)
- **A signal that never locks** (noise, the wrong bandwidth, a DVB-T2 multiplex): `dvbt` keeps cycling its bandwidths and writes `locked: false` every two seconds; `video` likewise; neither spins the CPU or holds stdin's end back by more than a block. (Task 3)
- **Text that is cut or not ASCII** (a programme service name half received, radiotext with bytes above 0x7e, a DVB service name in another alphabet): `ps` and `rt` are absent until complete, and every line is valid one-line JSON. (Tasks 1 and 3)
- **`onair` in the node-wide ranking on a node whose in-process decoder is not ready** (a broken row, or a mode whose row is itself a program): the OnAir handler is still picked when it is the best one left; the rule orders, it does not exclude. (Task 4)

---

### Task 1: The polled path, with RDS (`onair-rx`)

**Files:**
- Create: `src/poll.h`, `src/poll.cpp`, `src/poll_rds.cpp`, `test/poll.sh`
- Modify: `src/main.cpp` (mode lookup, usage, the `--pcap-port` refusal), `test/gen.cpp` (`--mode rds`), `README.md` (the mode table and a "polled modes" paragraph)

**Interfaces:**
- Produces, in `src/poll.h`:

```cpp
// One run of a polled mode: a receiver and what has already been written about its state.
struct PollSession {
    virtual ~PollSession() = default;
    virtual void feed(const cf32* x, size_t n) = 0;
    // The lines for what is new or changed since the last poll. last: the input has ended.
    // A line has neither "event" nor "t_sample": runPolled adds both.
    virtual void poll(bool last, std::vector<Json>& lines) = 0;
};
struct PollMode {
    const char* name;      // "rds"
    const char* event;     // "rds.group"
    double pollSec;        // seconds of input between polls; 0: after every block
    // makes the session for o; nullptr with *err set when o does not suit the mode (the rate is too low)
    std::unique_ptr<PollSession> (*open)(const Options& o, std::string* err);
};
extern const PollMode kPollModes[];
extern const size_t kPollModeCount;
int runPolled(const PollMode& m, const Options& o, IqInput& in, Output& out);   // the exit status
```

- `runPolled` reads blocks of 16384 as `runAdsb` does, polls when `in.samples()` has advanced `pollSec * o.rate` since the last poll, and polls once more with `last` true at end of input. An `open` that fails ends with exit 2 and `onair-rx: <err>` before any sample is read.
- `main.cpp`: a polled mode with `--pcap-port` exits 2 with `onair-rx: <mode> has no packets (no --pcap-port)`, the bus modes' line.
- RDS line (`rds.group`): `pi` (4 upper-case hex digits), `group` (`"0A"`, `"2B"`), `pty` (integer), `tp` (boolean), from the group tap's blocks; `ps` and `rt` from the telemetry snapshot at that group, each absent until OnAir has it complete. The station finder is off; `setSilent(true)` is called before `configure`.
- `onair-gen --mode rds --rate HZ`: OnAir's `FmGenerator` with PI `0x2AB1`, PS `ZETA FM `, RT `HELLO FROM ZETASDR`, for 6 s.

- [ ] **Step 1: Write `test/poll.sh`** in `test/bus.sh`'s style (its `fail`, temp directory and Python line checks). For `rds` at 500000: at least 20 lines; every line valid JSON with `event` `rds.group`, an integer `t_sample` that never decreases, `pi` `2AB1`, `group` matching `[0-9]{1,2}[AB]`; some line has `ps` equal to `ZETA FM ` and some line `rt` equal to `HELLO FROM ZETASDR`; no line has a `ps` shorter than 8 characters. Also: `--mode rds --rate 100000` exits 2 with one stderr line naming the minimum rate and no stdout; `--mode rds --pcap-port 5000` exits 2 with the refusal above.
- [ ] **Step 2: Run it.** `bash test/poll.sh`. Expected: FAIL, `onair-rx: no mode rds`.
- [ ] **Step 3: Add `--mode rds` to `test/gen.cpp`** and build (`./build.sh`).
- [ ] **Step 4: Write `src/poll.h` and `src/poll.cpp`** to the interface above, and wire `main.cpp`.
- [ ] **Step 5: Write `src/poll_rds.cpp`**: the session queues one line per group inside `feed` (the tap runs there) and `poll` hands them over; `pollSec` 0.
- [ ] **Step 6: Run `test/poll.sh`, `test/bus.sh` and `test/run.sh`.** Expected: all pass, the last two with the counts `master` gives.
- [ ] **Step 7: Update `README.md`** and commit. `onair-rx: a polled path for receivers that offer state, and RDS on it`

### Task 2: DMR, P25 and M17 calls (`onair-rx`)

**Files:**
- Create: `src/poll_voice.cpp`, `src/calls.h` (the call keying, header-only so `onair-gen --selftest calls` can drive it)
- Modify: `src/poll.cpp` (three table rows), `test/gen.cpp` (`--mode dmr|p25|m17`, `--selftest calls`), `test/poll.sh`, `README.md`

**Interfaces:**
- Consumes: `PollSession`, `PollMode` (Task 1).
- Produces, in `src/calls.h`:

```cpp
// What a snapshot says about one call. key: what makes it the same call between polls (the mode's own choice, for
// example slot, source and destination); members: the line's members as written; ended: the snapshot marks it over.
struct CallSeen { std::string key; std::string members; bool ended; };
// Turns successive snapshots into lines: a call first seen gives its members; a call whose members changed gives them
// again; a call marked ended, or no longer in the snapshot, gives its last members with "eot": true, once. A call seen
// again after its end is a new call. finish() ends every call still open (the last poll).
class CallLog {
public:
    void see(const std::vector<CallSeen>& now, std::vector<std::string>& lines);
    void finish(std::vector<std::string>& lines);
};
```

- Lines (`voice.call`), members as the rows spell them:
  - `dmr`: `type` `"call"`, `src`, `dst` (integers), `call` (`"group"` or `"private"`), `color_code`, `slot` (1 or 2), `encrypted`, `emergency`, `eot` (true on the last line only), `silent` `"encrypted"` on an encrypted call and absent otherwise, and `alias` when OnAir has the talker alias.
  - `p25`: `type` `"call"`, `src`, `dst`, `call`, `nac`, `encrypted`, `emergency`, `eot`, `silent` as for DMR, and `mfid`, `algid`, `kid` when the snapshot has them.
  - `m17`: `src`, `dst` (callsigns), `can`, `type` (`"voice"`, `"voice_data"`, `"data"`, `"packet"`), `encryption` (the row's value, absent when none), `eot`, and `lat`, `lon` when the transmission carries a position.
- **Calls only, unless a channel is honest.** The DMR and P25 rows also have `type` `"channel"` events (a channel seen with no call). Write one only if the snapshot states a colour code or NAC with no call in progress; otherwise write calls alone and say so in the README.
- **A call's first line waits** until its source, destination and colour code or NAC (M17: source, destination and type) are all in hand.
- Rates: `dmr` through `configureChannel` at 48000 and above (if that path does not decode the generated signal, the 1 MHz `configure` path, with the README and the handler entry's `rate_hz` saying so); `p25`, `m17` at 250000 and above with a signal offset of 0. `pollSec` 0.25.
- Generated signals, the node's own check values: DMR colour code 5, source 9990001, group 9990, three superframes; P25 NAC 0x123, source 9990001, group 65000; M17 `VK4ZET` to `ALL`, CAN 0, voice, 25 frames. Each with a second of noise either side, then the same call a second time after 2 s of noise.

- [ ] **Step 1: Write `onair-gen --selftest calls`** against `CallLog`: a call seen in three polls with the same members gives one line; changed members give a second; `ended` gives one `eot` line and a later poll with the entry still present gives none; a call that vanishes without `ended` gives one `eot` line; the same key reappearing after its end gives a new first line; `finish()` ends an open call once.
- [ ] **Step 2: Extend `test/poll.sh`** for the three modes: exactly two calls, each with a first line carrying the values above and a last line with `eot` true, no line between them when nothing changed; `t_sample` of each first line inside its call's time (the generator's `message from A s to B s`). Then the same run with the codec libraries hidden (`DECT2_MBELIB_PATH=/nonexistent DECT2_CODEC2_PATH=/nonexistent` and, if the wrapper still finds a system copy, the README's stated way to hide it): the same lines.
- [ ] **Step 3: Run both.** Expected: FAIL (no selftest, `no mode dmr`).
- [ ] **Step 4: Write `src/calls.h`**, then the generator modes, then `src/poll_voice.cpp` (one session per mode: its receiver, its snapshot to `CallSeen`s, a `CallLog`).
- [ ] **Step 5: Settle speech decoding.** Read `voice_monitor.h` and each receiver's voice path: if a stock monitor (or any switch that needs no patch) makes the receiver skip speech decoding, set it. Otherwise leave it, measure the wrapper's user CPU per second of input for each mode with and without the codec found, and put both figures in the README and in `UPSTREAM.md` (a new section: "3. Bus emitters and a no-speech switch for the voice and broadcast receivers", described only).
- [ ] **Step 6: Run `test/poll.sh` and the selftest.** Expected: pass.
- [ ] **Step 7: Update `README.md`** and commit. `onair-rx: DMR, P25 and M17 calls from the receivers' call logs`

### Task 3: DVB-T and analog video status, and 0.3.0 (`onair-rx`)

**Files:**
- Create: `src/poll_tv.cpp`
- Modify: `src/poll.cpp`, `src/main.cpp` (`--bandwidth`, `kVersion` `0.3.0`), `src/rx.h` (`Options::bandwidthMhz`, 0 when not given), `test/gen.cpp` (`--mode dvbt|video`), `test/poll.sh`, `README.md`, `UPSTREAM.md`

**Interfaces:**
- Consumes: `PollSession`, `PollMode` (Task 1).
- Lines:
  - `dvbt` (`dvbt.status`): `locked` (boolean), `lock` (the row's words for the stage reached), `bandwidth_hz`, `mode`, `guard`, `constellation`, `code_rate`, `hierarchy`, `lp_code_rate`, `cell_id`, `snr_db`, each spelled as the row's own program writes it (`modes/dvbt_decode.json` notes; `dvbt-rx`'s README if cloned beside this repository); and, once OnAir's `TsDemux` has read the tables, `services`: an array of objects `{"id": <service id>, "name": "<name>", "kind": "tv"|"radio"|"data"}`.
  - `video` (`video.status`): `system`, `standard` (`"pal"`, `"ntsc"`, `"secam"`), `locked`, `lock`, `line_hz`, `snr_db`, as `modes/video_decode.json` spells them.
- **Cadence, both:** the first line at first lock or after 2 s of input, whichever is first; then a line when any member changes, and one at least every 2 s. `pollSec` 0.25.
- `dvbt` bandwidth: `--bandwidth 8|7|6|5` fixes it; without it the session tries 8, 7, 6 (and 5 if `DvbtReceiver::configure` takes it) for 2 s of input each, in turn, until the TPS locks, and stays there while locked. The transport stream callback feeds `TsDemux` only; nothing else takes the packets.
- `video`: `AtvReceiver` with `setModulation(1)` (FM video); the minimum rate is the receiver's own.
- Generated signals: DVB-T, 8 MHz, 8k, one service named `ZETA TEST` (`dvbt::Generator` with `testTsSourceNamed`), 6 s at the given rate; video, a PAL test card on FM (`AtvGenerator` or `FpvGenerator`, whichever makes FM video), 3 s.

- [ ] **Step 1: Extend `test/poll.sh`.** `dvbt` at 10000000: the first line within 2 s of input; some line `locked` true with `bandwidth_hz` 8000000; some line with `services` holding a name `ZETA TEST`; the gap between successive lines' `t_sample` never more than 2 s of samples. The same for noise only (8 s of `/dev/urandom` read as `--format cs16`): at least three lines, all `locked` false, exit 0. `video` at 16000000: some line `locked` true with `standard` `pal`. The script prints, for each, the wrapper's user CPU over the seconds of input (`/usr/bin/time -f %U`).
- [ ] **Step 2: Run it.** Expected: FAIL, `no mode dvbt`.
- [ ] **Step 3: Add the generator modes;** build.
- [ ] **Step 4: Write `src/poll_tv.cpp`** and the two table rows; add `--bandwidth`.
- [ ] **Step 5: Run every test script.** Expected: pass. If `dvbt` does not keep real time on one core (user CPU above the seconds of input), try `setPipelined` and keep whichever is cheaper in wall time; write both figures in the README.
- [ ] **Step 6: Find `video`'s lowest working rate:** rerun its check at 8, 10, 12 and 16 MHz and record in the README which lock and at what CPU.
- [ ] **Step 7: Set `kVersion` to `0.3.0`, update `README.md` and `UPSTREAM.md`** and commit. `onair-rx: DVB-T and analog video status; 0.3.0`

### Task 4: The node-wide ranking passes over a handler that gives less (`zeta-core`)

**Files:**
- Modify: `crates/zeta-core/src/modes/policy.rs` (`pick`, its doc comment, the tests module), `crates/zeta/src/agents_md.rs` (the note on `handler_policy_set`), `handlers/README.md` (the paragraph on ranking)
- Regenerate: `AGENTS.md`, `skills/zetasdr/SKILL.md`

**Interfaces:**
- Produces: in `pick`, a handler's place in `policy.all` counts only when its `outputs` hold every kind the mode's first handler (the row's own decoder, `handlers[0]`) gives; otherwise it is ordered as if `all` did not name it. The mode's own ranking and a named handler (`choose`) are unchanged. No handler is excluded by this rule.

- [ ] **Step 1: Write the failing tests** in `policy.rs`'s tests module, with its `h`, `picked` and `ranked` helpers:
  - `the_node_wide_ranking_passes_over_a_handler_that_gives_less`: handlers `builtin` (in process, Event + Audio) and `onair` (Event); `ranked(&[], &["onair"])` picks `builtin`.
  - `the_modes_own_ranking_still_picks_it`: the same handlers; `ranked(&["onair"], &[])` picks `onair`.
  - `a_handler_that_gives_as_much_is_still_ranked_node_wide`: `builtin` (Event) and `onair` (Event + Packet); `ranked(&[], &["onair"])` picks `onair`.
  - `a_lesser_handler_is_picked_when_nothing_else_is_ready`: `builtin` not ready, `onair` (Event) ready, `ranked(&[], &["onair"])` picks `onair`.
- [ ] **Step 2: Run them.** `cargo test -p zeta-core --features test-fixtures --lib modes::policy`. Expected: the first FAILS (picks `onair`); the others pass.
- [ ] **Step 3: Change `pick`** and its doc comment.
- [ ] **Step 4: Run the module's tests.** Expected: all pass, the existing ones unchanged.
- [ ] **Step 5: Say it in the prose** (`agents_md.rs`'s `handler_policy_set` note and `handlers/README.md`: the node-wide ranking does not prefer a handler that lacks an output the row's own decoder gives; rank it for the mode, or name it, to run it), regenerate `AGENTS.md` and the skill, run both `agents-md --check` gates.
- [ ] **Step 6: Commit.** `OnAir callback modes: the node-wide ranking passes over a handler that gives less than the row's own decoder`

### Task 5: OnAir's entries for RDS, DMR, P25 and M17 (`handlers/onair.json`)

**Files:**
- Modify: `handlers/onair.json`, `crates/zeta-core/tests/modes.rs` (`the_embedded_onair_handler_serves_the_bus_modes`: four more rows, or a second test beside it in the same shape), `crates/zeta/src/modes_check.rs` (the `ZETA_TEST_ONAIR_RX` test's list)

**Interfaces:**
- Consumes: `onair-rx` 0.3.0's lines (Tasks 1 and 2; its README is the reference), built at `/Projects/onair-rx/onair-rx`.
- Produces: entries `rds_decode`, `dmr_decode`, `p25_decode`, `m17_decode`, each `["--mode", <m>, "--rate", "$rate_hz", "--format", "cs16", "--freq", "$center_hz"]`, `input` cs16 on stdin at the rate in Global Constraints, `output` `json_lines` on stdout with a `fields` map onto the row's fields (it must map the subject key: `pi`; `src`), members with no row field in `extra` (`alias`). `dmr_decode` and `p25_decode` carry `"const": {"audio": false}` and map `silent` (the wrapper writes it only on an encrypted call). `m17_decode` has neither: its row has no such fields. No `packets`.

- [ ] **Step 1: Extend the two tests:** in `modes.rs`, the four modes with `&events` and rates 500_000, 48_000, 250_000, 250_000, and `handler_consts` for DMR and P25 equal to `{"audio": false}`; in `modes_check.rs`, the four ids added to the list the wrapper-backed test checks under `--strict`.
- [ ] **Step 2: Run them.** `cargo test -p zeta-core --features test-fixtures --test modes -- the_embedded_onair_handler` and `ZETA_TEST_ONAIR_RX=/Projects/onair-rx/onair-rx cargo test -p zeta --test modes_check -- onair`. Expected: FAIL, no such handler.
- [ ] **Step 3: Write the four entries.**
- [ ] **Step 4: Run both again,** then by hand `zeta modes check <id> --handler onair --strict` for each. Expected: pass; each first event carries the row's `expect` (RDS `pi` `2AB1`; DMR `src` 9990001, `dst` 9990, `color_code` 5; P25 `src` 9990001, `dst` 65000, `nac` 291; M17 `src` `VK4ZET`, `dst` `ALL`, `type` `voice`). A mode that cannot pass goes back to the wrapper (its first line, its values' spelling), not around the check.
- [ ] **Step 5: Commit.** `OnAir callback modes: OnAir decodes RDS, DMR, P25 and M17, events only`

### Task 6: OnAir's entries for DVB-T and analog video

**Files:**
- Modify: `handlers/onair.json`, `crates/zeta-core/tests/modes.rs`, `crates/zeta/src/modes_check.rs`, `TODO.md` (only if a mode misses the listing bar)
- Create: `crates/zeta-core/tests/fixtures/onair-dvbt-status.jsonl` (two lines of the wrapper's own output on its generated signal: one unlocked, one locked with `services`)

**Interfaces:**
- Consumes: `onair-rx` 0.3.0's `dvbt` and `video` lines (Task 3) and the CPU figures and rates its README records.
- Produces: entries `dvbt_decode` (`rate_hz` 10000000) and `video_decode` (`rate_hz` the lowest rate Task 3 found that locks, if it also meets the listing bar here), `fields` onto the rows' fields, `services` in `dvbt_decode`'s `extra`.

- [ ] **Step 1: Write the failing tests.** In `modes.rs`: both modes present with `&events`; and `the_onair_dvbt_entry_maps_a_status_line`: each line of the fixture, put through the entry's `fields` and `extra` as the node's json_lines parser does, gives an event whose `locked`, `bandwidth_hz`, `constellation` and `code_rate` pass the row's schema, and the locked line's `services` holds `ZETA TEST`. In `modes_check.rs`: `video_decode` added to the wrapper-backed list (`dvbt_decode` has no `test` and is not).
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Write the two entries;** run the tests and `zeta modes check video_decode --handler onair --strict`.
- [ ] **Step 4: Measure against the listing bar.** With a scratch node on a file source of each generated capture (the check's own setup), run the job for 30 s: the node reports no dropped samples for the job, and the wrapper's CPU is read (`ps` of the child). Write the figures into the entry's notes text for Task 7.
- [ ] **Step 5: A mode that misses the bar comes out of the handler file** (its tests then assert it absent), with one line under plan 46 in `TODO.md`: the mode, the rate, the figure and what it needed.
- [ ] **Step 6: Commit.** `OnAir callback modes: OnAir reads DVB-T signalling and its service list, and analog video's lock`

### Task 7: Documents and the gates

**Files:**
- Modify: `RECIPES.md` (the OnAir recipe: the six modes, what each gives, no speech and no picture, the measured costs, the ranking rule), `handlers/README.md`, `UAT.md` (a "Plan 46" section), `TODO.md` (plan 46's line: built, what is left), `docs/agents/field-craft.md` if it lists OnAir's modes
- Regenerate: `AGENTS.md`, `skills/zetasdr/SKILL.md`

- [ ] **Step 1: `UAT.md`, plan 46**, unticked, saying what was and was not run when written: 46.1 `handlers list` on the field host shows OnAir ready for the listed modes once 0.3.0 is installed; 46.2 a DMR, P25 or M17 job by OnAir beside the node's own on live traffic gives the same calls, and attaching a transcriber to it is refused with the audio line; 46.3 RDS by OnAir beside the node's own on a broadcast station; 46.4 DVB-T by OnAir on a real multiplex gives lock and a service list; 46.5 with `onair` in the node-wide ranking, a voice job started from the page still runs the node's own decoder and can be heard; 46.6 the CPU of each OnAir job, written down.
- [ ] **Step 2: `TODO.md`:** plan 46's line rewritten as built with what is left, one line each: the note's non-goals (DMR data and locations, P25 trunking, phase 2, DVB-T2, teletext), no node check for DVB-T, speech decoded and discarded if Task 2 found no switch, anything Task 6 left out.
- [ ] **Step 3: The prose** in `RECIPES.md` and `handlers/README.md`; regenerate the two generated files.
- [ ] **Step 4: The gates,** each with its output read: `cargo fmt` per touched crate, clippy, `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`, `cargo test -p zeta --test ui -- --nocapture` (check it did not print `ui e2e skipped`), both `agents-md --check`, `scripts/public-check.sh`, `scripts/check-doc-links.sh`; in `onair-rx`: `test/run.sh`, `test/bus.sh`, `test/poll.sh`, `test/patch.sh`.
- [ ] **Step 5: Commit.** `OnAir callback modes: the recipe, the acceptance checks and what is left`
