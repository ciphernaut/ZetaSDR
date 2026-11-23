# Modes Only OnAir Decodes (Plan 47) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Zigbee, Thread, Wi-SUN, Bluetooth LE, LoRaWAN, Z-Wave, Wi-Fi, TETRA, GSM-R, LTE and DVB-S IP become rows with no decoder of their own, decoded by OnAir through `onair-rx` with events and Wireshark packets, each proven by `zeta modes check --handler onair` on a signal OnAir's generator made, and each with a Decode entry on the page.

**Architecture:** The node learns a row whose `decoder` is `null`: it keeps a stand-in own handler that is never ready, so a handler file's entry is the only thing that can run it, and it carries a `content` field that travels to `modes_list`, the job's context and the packet capture's record. `onair-rx` (a separate GPL repository) gains eleven modes on its two existing paths: the hub tap is written once and locked, the bus path gains an end-of-input flush, and the polled path gains packets and sessions that wait for a busy receiver.

**Tech Stack:** C++20 against OnAir `a63c3a8+patched` (`onair-rx`: `build.sh`, `test/*.sh`); Rust 1.97 (`zeta-core`, `zeta-ext`, `zeta`); JSON rows and handler files; the page's JavaScript and its Playwright suite.

**Spec:** `docs/superpowers/specs/2026-11-23-onair-only-modes-design.md` ("the note" below), on `2026-11-21-alternative-handlers-design.md`, `2026-11-22-onair-bus-modes-design.md` and `2026-11-22-onair-callback-modes-design.md`. Read the note first; it lists the operator's rulings, what is left to the builder and the risks.

**Run:** subagent-driven, the controller on Opus. Model per task: Opus for tasks 1, 3, 4, 5 (threads, receivers read for the first time), 6 (the registry) and 11 (the page's menus); Sonnet for tasks 2, 7, 8, 9, 10 and 12, whose briefs are complete. Task reviews on Sonnet, on Opus for tasks 3, 4 and 6. The whole-branch review at the end on the most capable model available. Order: tasks 1 to 5 in sequence in `onair-rx`; tasks 6, 7, 8 in sequence here, beside them; task 9 after 3 and 8; task 10 after 5 and 9 (resume task 9's implementer); task 11 after 9; task 12 last. Estimate: 4 to 5 million subagent tokens.

## Global Constraints

- **Two repositories.** Tasks 1 to 5 work in `/Projects/onair-rx` (branch `only-onair` from `master`); tasks 6 to 12 in this repository's worktree. `onair-rx` and OnAir are never copied, linked or vendored into this repository: only `handlers/onair.json` names them. A node task that needs the wrapper uses a frozen copy of its binary and of `_build/onair-gen` taken when the wrapper task it depends on was reviewed.
- **No OnAir rebuild and no new patch.** All eleven receivers are in the built `_onair-build/libdect2core.a`. Where a receiver has no switch for something wanted, live with it and write it in the README and `UPSTREAM.md`.
- **`onair-rx` licence:** GPL-3.0-or-later; every new source file starts with the SPDX line and `Based on OnAir by Kristijonas Adomaitis - https://on-air.dev`.
- **The wrapper's contract is unchanged for the sixteen existing modes:** their lines and packets do not change by a byte (`test/run.sh`, `test/bus.sh`, `test/poll.sh` pass untouched).
- **Mode names.** `onair-rx`: `zigbee`, `thread`, `wisun`, `ble`, `lorawan`, `zwave`, `wifi`, `tetra`, `gsmr`, `lte`, `dvbs`. Rows: `zigbee_decode`, `thread_decode`, `wisun_decode`, `ble_decode`, `lorawan_decode`, `zwave_decode`, `wifi_decode`, `tetra_decode`, `gsmr_decode`, `lte_decode`, `dvbs_ip_decode`.
- **ASTERIX is not built, listed or mentioned as planned** (the operator, 2026-11-23).
- **The field is `content`,** value `network_traffic`; the word "policy" is not used for it anywhere.
- **No keys.** Nothing loads, passes or stores a key; frames are given as received.
- **Every line** has `event` and `t_sample`, a member only when its value is known, whole numbers without a decimal point, non-finite numbers left out, text through `Json::utf8`. Addresses are lower-case hex without separators unless the table below says otherwise.
- **The receivers were read, not run.** Rates and setters in this plan come from a read of OnAir's headers and tests (file and line in the table). The first step of each wrapper task is to run the generator into the receiver; where the source differs from this plan, the source wins and the README says what was found.
- **The listing bar** (note, Anti-goal and Risks): a mode gets its row and entry only if its check passes and its job keeps real time on the build host, on a file source, with the node dropping no samples. A mode that passes only with `--drop` is listed with `--drop` in its entry and says so in the row's notes. A mode that misses stays in `onair-rx`; its row and entry stay out, with one line under plan 47 in `TODO.md`.
- **The node is unchanged for a node with no `onair-rx`:** every existing test passes; the eleven rows are `unavailable` and name the missing program.
- **Ranges and bands are the standards' own,** across regions, sourced in the row's notes; none from one station, one country or one radio's limits.
- Public repository: roles not host names, no observed traffic; captures of real traffic never enter git. Test signals are generated, and the generated pairs are not committed (the smallest is megabytes).
- Gates per `CLAUDE.md`: `cargo fmt -p <crate> -- --check` per crate (nested worktree), clippy `-D warnings`, a private `CARGO_TARGET_DIR` with `CARGO_INCREMENTAL=0` and `CARGO_PROFILE_DEV_DEBUG=line-tables-only`, deleted when the gates are done. Per task only its tests and neighbours; crate suites once per batch; the workspace suite and the UI suite once at the end. `zeta-core` tests take `--features test-fixtures`.
- Commit messages: `onair-rx: <what>` in `onair-rx`; `OnAir-only modes: <what>` here.
- Nothing is pushed, deployed or offered upstream by this plan. Landing on `main`, the field host and any offer to OnAir wait for the operator's word.

## The modes

Start values; Tasks 2 to 5 settle `rate` by measurement and tasks 9 and 10 the row's band.

| mode | receiver (under `_onair-src/core/`) | path | rate, Hz | needs | line (`event`) |
|---|---|---|---|---|---|
| `ble` | `BleReceiver`, `include/dect2/ble/ble_rx.h:29` | bus | 4000000 | | `ble.device` |
| `lorawan` | `LorawanReceiver`, `lorawan/lorawan_rx.h:19` | bus | 2000000 | `--freq`; a plan | `lora.packet` |
| `zigbee` | `ZigbeeReceiver`, `zigbee/zigbee_rx.h:29` | bus, lines from packets | 4000000 | `--freq` | `zigbee.frame` |
| `thread` | the same, `setThreadMode(true)` | bus, lines from packets | 4000000 | `--freq` | `thread.frame` |
| `zwave` | `ZwaveReceiver`, `zwave/zwave_rx.h:19` | bus, lines from packets | 2000000 | `--freq` | `zwave.frame` |
| `wifi` | `WifiReceiver`, `wifi/wifi_rx.h:29` | bus, lines from packets | 20000000 | `--freq` | `wifi.frame` |
| `wisun` | `WisunReceiver`, `wisun/wisun_rx.h:32` | bus, lines from packets, flushed | 2000000 | `--freq` | `wisun.frame` |
| `tetra` | `TetraReceiver`, `tetra/tetra_rx.h:33` | polled, packets | 2000000 | `--freq` | `tetra.cell` |
| `gsmr` | `GsmrReceiver`, `gsmr/gsmr_rx.h:27` | polled, packets | 2000000 | `--freq` | `gsmr.cell` |
| `lte` | `CellReceiver`, `cell/cell_rx.h:27` | polled, packets | 20000000 | `--freq` | `lte.cell` |
| `dvbs` | `DvbsReceiver`, `dvbs/dvbs_rx.h:16`, with `dvbsip::IpSink` | polled, packets | 10000000 | `--width` | `dvbs.status` |

Hub channels and link types are `wire_chan.h`'s: wifi 127, zigbee and thread 195, wisun 230, ble 251, zwave 148, lorawan 270, dvbip 101; tetra, gsmr and lte GSMTAP in raw IP (101), which `runBus` already wraps in IPv4 and UDP.

## Review Focus

- **A row with no decoder and no ready handler, started every way a job can start** (the page, `job_start` with and without `handler`, a set, a plan, a watch, identify, `capture_decode`, a resume of a stored job after the wrapper was removed): each refuses or passes over it with the row's reason; none panics, and none launches the stand-in. (Task 6)
- **An operator's handler file that replaces the embedded `onair.json`** and leaves a decoder-less row with a different handler, or with two: the policy picks among them, and `modes check <id>` checks the one it picks. (Task 6)
- **Input that ends inside a receiver's stretch** (a `capture_decode` replay 1 s long into a receiver that decodes 2.3 s at a time): the wrapper flushes, writes what was decoded, and exits 0 within a few seconds; it never waits on a worker that will not finish. (Tasks 3 to 5)
- **Noise and the wrong signal** (a 20 MHz capture of noise into `wifi`, a Zigbee signal into `ble`, a DVB-S2X carrier into `dvbs`): no line that claims a decode, status modes say `locked` false, CPU stays bounded, exit 0. (Tasks 2 to 5)
- **A capture job attached to a decoder-less row's job, across a relaunch:** the record keeps `content` and `handler`, and an index written before this plan still loads. (Task 7)

---

### Task 1: One locked hub tap, the new options, and the two bus rows (`onair-rx`, Opus)

**Files:**
- Create: `src/hub_tap.h`, `src/hub_tap.cpp`, `test/only.sh`
- Modify: `src/bus.cpp` (`runBus` uses the tap; two table rows; the end flush), `src/bus.h` (`BusMode::flushAtEnd`), `src/rx.h` (`Options`), `src/main.cpp` (options, usage), `test/gen.cpp` (`--mode ble|lorawan`, `--sigmf`), `README.md`

**Interfaces:**
- Produces, in `src/hub_tap.h`:

```cpp
// The Wireshark hub's packets of one channel, collected from whatever thread publishes them. One per run.
class HubTap {
public:
    explicit HubTap(int chan);              // configures the hub (no file, no live capture, no GSMTAP), sets the sink, enables chan
    ~HubTap();                              // clears the sink and shuts the hub down
    void drain(std::vector<std::vector<uint8_t>>& out);   // moves out what has arrived since the last drain, in order
};
// One packet as a pcap record body: as it is for a Direct channel, in IPv4 + UDP to the channel's port otherwise.
std::vector<uint8_t> hubRecord(int chan, const uint8_t* p, size_t n);
```

- `BusMode` gains a last member `bool flushAtEnd = false;`: when true, `runBus` calls the mode's feed once with `(nullptr, 0)` at the end of input and then drains events and packets one more time; the mode's own feed treats that call as "flush". The ten existing rows are not edited.
- `Options` gains `double width = 0;` (`--width HZ`), `std::string plan;` (`--plan NAME`), `std::string region;` (`--region NAME`), `bool drop = false;` (`--drop`). Each is refused (exit 2, one line) with a mode that does not take it.
- `ble` (`ble.device`): the bus event's members as they are (`mac`, `name`, `rssi_db`), `mac` lower-case hex without separators. Minimum rate the receiver's own.
- `lorawan` (`lora.packet`): the bus event's members (`kind`, `hex`, `snr_db`, `sf`, `modulation`, `devaddr`) plus `freq_hz` from `Event::freqHz` (by `shape`). `--freq` required. The plan: `--plan` when given; otherwise the plan of OnAir's table with the most channels inside the input around `--freq`, a tie to the table's order; no channel of any plan inside the input is exit 2 with `onair-rx: lorawan: no channel plan has a channel within <lo>-<hi> MHz`.
- `onair-gen --mode ble|lorawan --rate HZ [--freq HZ]`: OnAir's `makeBleSynth` / `makeLorawanSynth` scene, a second of noise either side, `message from A s to B s` on stderr as the other modes. `onair-gen ... --sigmf STEM` writes `STEM.sigmf-data` (ci16_le) and `STEM.sigmf-meta` (sample rate, the centre frequency, datatype) instead of stdout, in the shape `iq/adsb1090.sigmf-meta` has.

- [ ] **Step 1: Write `test/only.sh`** in `test/bus.sh`'s style. `ble` at 4000000: at least one line, every line valid JSON with `event` `ble.device`, an integer non-decreasing `t_sample`, a `mac` of 12 lower-case hex digits equal to the generator's device; with `--pcap-port`, a client reads a pcap header of link type 251 and at least one record. `lorawan` at 2000000 with the scene's `--freq`: at least one line with `event` `lora.packet`, `sf` and `freq_hz` integers, `freq_hz` within the input; pcap link type 270. `lorawan` without `--freq` and with a `--freq` no plan covers: each exits 2 with its line. Every existing mode with `--drop`, `--plan` or `--region`: exit 2.
- [ ] **Step 2: Run it.** `bash test/only.sh`. Expected: FAIL, `onair-rx: no mode ble`.
- [ ] **Step 3: Add the generator modes and `--sigmf`** to `test/gen.cpp`; build (`./build.sh`).
- [ ] **Step 4: Write `HubTap` and `hubRecord`,** move `runBus` onto them, add `flushAtEnd`.
- [ ] **Step 5: Run `test/bus.sh` and `test/run.sh`.** Expected: pass with `master`'s counts (the tap changed nothing for them).
- [ ] **Step 6: Add the options and the two rows;** run `test/only.sh`. Expected: pass.
- [ ] **Step 7: Update `README.md`** (the mode table, the options, how LoRaWAN's plan is chosen) and commit. `onair-rx: a locked hub tap, and Bluetooth LE and LoRaWAN on the bus path`

### Task 2: Zigbee, Thread, Z-Wave and Wi-Fi, lines from their packets (`onair-rx`, Sonnet)

**Files:**
- Create: `src/frames.cpp` (the four `packetLines` functions), `src/frames.h`
- Modify: `src/bus.cpp` (four rows and their `open`), `test/gen.cpp` (`--mode zigbee|thread|zwave|wifi`), `test/only.sh`, `README.md`

**Interfaces:**
- Consumes: `BusMode::packetLines`, `HubTap` (Task 1).
- Lines, one per packet OnAir's own parser reads (a packet it cannot read gives no line and is still a pcap record):
  - `zigbee.frame` (`zigbee::parse`): `frame_type` (`"beacon"`, `"data"`, `"ack"`, `"command"`), `seq`, `pan` (4 hex), `src`, `dst` (4 hex short or 16 hex extended), `nwk_src`, `nwk_dst` (4 hex, when the network header is in the clear), `secured` (boolean), `len`.
  - `thread.frame` (`thread::analyse`): the Zigbee members that apply to an 802.15.4 frame (`frame_type`, `seq`, `pan`, `src`, `dst`, `secured`, `len`) and `kind`, OnAir's word for what the frame is (an MLE advertisement, a data frame).
  - `zwave.frame`: `home_id` (8 hex), `src`, `dst` (node ids, integers), `frame_type`, `rate_bps`, `freq_hz`, `secured`, `len`.
  - `wifi.frame`: for a beacon, probe response or FILS discovery frame only: `frame_type` (`"beacon"`, `"probe_response"`, `"fils_discovery"`), `bssid` (12 hex), `ssid` (absent when hidden), `channel`, `freq_hz`, `rate_mbps`, `security` (OnAir's word), `len`. Every FCS-good frame is a pcap record whether or not it gives a line.
- `zigbee`, `thread`: `setCenterHz(o.freq)`; `--freq` required (it picks the PHY). `zwave`: `--freq` required; `--region` or the receiver's own pick. `wifi`: `--freq` required (it picks the band).
- Generators: `makeZigbeeSynth`; Thread frames built as `tests/thread/test_thread_rx.cpp:16-45` builds them; `makeZwaveSynth`; `makeWifiSynth` (the 2.4 GHz scene).

- [ ] **Step 1: Extend `test/only.sh`** for the four: at least one line each with the generator's own values (the scene's PAN and addresses; the Z-Wave home id; the Wi-Fi scene's SSID and BSSID); every line's `t_sample` inside the message's time; pcap link types 195, 195, 148, 127 with at least as many records as lines; `--freq` missing is exit 2; 8 s of `/dev/urandom` as `--format cs16` gives no line and exit 0 for each (Wi-Fi: 2 s). The script prints each mode's user CPU over its seconds of input.
- [ ] **Step 2: Run it.** Expected: FAIL, `no mode zigbee`.
- [ ] **Step 3: Add the generator modes;** build.
- [ ] **Step 4: Write the four `open` functions and `packetLines`;** add the rows.
- [ ] **Step 5: Run every test script.** Expected: pass.
- [ ] **Step 6: Find each mode's lowest rate** that still decodes its generated signal (try the receiver's minimum, then the table's start value) and record rate and CPU in the README.
- [ ] **Step 7: Commit.** `onair-rx: Zigbee, Thread, Z-Wave and Wi-Fi frames from the Wireshark hub`

### Task 3: Wi-SUN, whose frames come from a decoder thread (`onair-rx`, Opus)

**Files:**
- Modify: `src/bus.cpp` (one row, `flushAtEnd` true), `src/frames.cpp`, `test/gen.cpp` (`--mode wisun`), `test/only.sh`, `README.md`

**Interfaces:**
- Consumes: `HubTap`, `BusMode::flushAtEnd` (Task 1).
- `wisun`: `setCenterHz`/`setDialHz` from `--freq` (required), `setRegion(o.region)` (empty: the receiver picks by frequency), `setOffline(!o.drop)`. Its feed, called with `(nullptr, 0)`, calls the receiver's `flush()` and returns only when the worker has handed over its last frame.
- `wisun.frame`: `frame_type`, `seq`, `pan`, `src`, `dst` (16 hex), `freq_hz` (the channel's), `rate_bps`, `len`.

- [ ] **Step 1: Extend `test/only.sh`:** `wisun` at 2000000 on `makeWisunSynth` (generated at 2000000 around one of its channels): at least one line with the scene's addresses; pcap link type 230; the same input cut 0.2 s after the message's end still gives the line (the flush); run twice, the two outputs are byte-identical; run under `taskset -c 0` and under `taskset -c 0,1`, the lines are the same (no dependence on thread timing); input cut in the middle of the message exits 0 within 5 s.
- [ ] **Step 2: Run it.** Expected: FAIL.
- [ ] **Step 3: Add the generator mode, the row and its `packetLines`.**
- [ ] **Step 4: Run every test script,** `test/only.sh` ten times in a loop. Expected: pass every time.
- [ ] **Step 5: Record in the README** how far a line's `t_sample` trails its frame, measured against the generator's message time.
- [ ] **Step 6: Commit.** `onair-rx: Wi-SUN, flushed at the end of input`

### Task 4: The polled path gains packets; TETRA and GSM-R cells (`onair-rx`, Opus)

**Files:**
- Create: `src/poll_cell.cpp`
- Modify: `src/poll.h` (`PollMode::chan`, `PollMode::needsFreq`), `src/poll.cpp` (`runPolled` with a `HubTap`; two rows), `src/main.cpp` (the `--pcap-port` refusal is now for a polled mode with `chan` -1), `test/gen.cpp` (`--mode tetra|gsmr`), `test/only.sh`, `README.md`

**Interfaces:**
- Consumes: `PollSession`, `PollMode` (plan 46), `HubTap`, `hubRecord` (Task 1).
- `PollMode` gains last members `int chan = -1;` (a `dect2::wire::Chan`, or -1: no packets) and `bool needsFreq = false;`. With `chan` >= 0 and a pcap port, `runPolled` makes a `HubTap`, drains it after every block and after the last poll, and writes one record per packet at the drained block's first sample (`RecordClock`, as `runBus`).
- **A session never lets its receiver skip input** unless `o.drop`: its `feed` returns only when the receiver is no longer busy (`busy()`, or the receiver's blocking switch). `poll(last = true)` flushes: it waits for the worker's last stretch, bounded at 5 s, then reports.
- Lines, one when a cell is first read and one when a member changes, keyed by carrier:
  - `tetra.cell`: `freq_hz`, `mcc`, `mnc`, `colour_code`, `la` (location area), `network_time` (when the broadcast carries it), `neighbours` (count), `snr_db`.
  - `gsmr.cell`: `freq_hz`, `arfcn`, `bsic`, `mcc`, `mnc`, `lac`, `cell_id`, `snr_db`.
- `pollSec` 0.25. `--freq` required for both. TETRA searches the whole input for 25 kHz carriers (the receiver's default); `setSingleCarrier` is not set.
- Generators: `makeTetraSynth` scene 0, `makeGsmrSynth` scene 0, each for long enough to hold three of the receiver's stretches.

- [ ] **Step 1: Extend `test/only.sh`:** each mode at 2000000: one line per generated cell with the scene's own identity (TETRA's MCC, MNC and colour code; GSM-R's ARFCN, BSIC, MCC, MNC, LAC); no second line for a cell whose members did not change; pcap link type 101 with at least one record whose UDP destination port is 4729; run twice, byte-identical lines; the input cut to 1 s exits 0 within 8 s; noise gives no line; `--drop` runs and exits 0 (its lines are not compared).
- [ ] **Step 2: Run it.** Expected: FAIL.
- [ ] **Step 3: Add the generator modes;** extend `runPolled`; run `test/poll.sh`. Expected: pass untouched.
- [ ] **Step 4: Write `src/poll_cell.cpp`** (the two sessions; the cell keying shared).
- [ ] **Step 5: Run every test script,** `test/only.sh` ten times. Expected: pass every time.
- [ ] **Step 6: Measure** each mode's wall time and user CPU per second of input with and without `--drop`, and write both in the README with whether it keeps real time.
- [ ] **Step 7: Commit.** `onair-rx: packets on the polled path, and TETRA and GSM-R cells`

### Task 5: LTE cells, DVB-S IP, and 0.4.0 (`onair-rx`, Opus)

**Files:**
- Create: `src/poll_dvbs.cpp`
- Modify: `src/poll_cell.cpp` (LTE), `src/poll.cpp` (two rows), `src/main.cpp` (`kVersion` `0.4.0`), `test/gen.cpp` (`--mode lte|dvbs`), `test/only.sh`, `README.md`, `UPSTREAM.md`

**Interfaces:**
- Consumes: Task 4's polled path with packets.
- `lte.cell`: `freq_hz`, `pci`, `bandwidth_hz` (from the MIB), `mcc`, `mnc`, `tac`, `cell_id` (from SIB1, absent until read), `sibs` (the SIB numbers read so far, ascending), `snr_db`. Packets: the hub's LTE channel (GSMTAP, the SI messages). `--freq` required.
- `dvbs.status`: `locked`, `standard` (`"dvbs"`, `"dvbs2"`), `symbol_rate`, `modcod` (OnAir's word), `snr_db`, `datagrams` (IP datagrams so far). Cadence as plan 46's status modes: the first line at first lock or after 2 s of input, then on a change of any member but `datagrams` and `snr_db`, and at least every 2 s. Packets: the hub's `dvbip` channel (link type 101, whole datagrams). `--width HZ` bounds the symbol rate search (at most the width); without it the receiver's own search. GSE always; MPE and ULE when the receiver's transport stream can be passed back to `feedTs` without a patch, and the README says which.
- Generators: `makeCellSynth` (the 5 MHz scene, to keep the test short); `DvbsSignal` with a GSE field carrying one known UDP datagram, as `tests/dvbs/test_dvbs_gse.cpp:19-60`.

- [ ] **Step 1: Extend `test/only.sh`:** `lte` at the lowest rate that holds the 5 MHz scene: a line with the scene's `pci` and, later, one with `mcc`, `mnc` and a non-empty `sibs`; pcap records to port 4729. `dvbs` at 10000000 with `--width`: a `locked` true line, a later line with `datagrams` at least 1, a pcap record whose bytes are the known datagram; on noise at least three lines, all `locked` false; gaps between lines never above 2 s of samples. Cut inputs and repeat runs as Task 4.
- [ ] **Step 2: Run it.** Expected: FAIL.
- [ ] **Step 3: Add the generator modes and the two sessions.**
- [ ] **Step 4: Run every test script,** `test/only.sh` ten times. Expected: pass every time.
- [ ] **Step 5: Measure** as Task 4, and LTE also on the 10 and 20 MHz scenes at 20000000; write the figures in the README.
- [ ] **Step 6: Set `kVersion` to `0.4.0`;** `README.md`'s table holds all eleven with rate, options, what is and is not decoded, and how far packet time trails; `UPSTREAM.md` gains what a receiver lacked a switch for. Commit. `onair-rx: LTE cells and IP over DVB-S; 0.4.0`

### Task 6: A row with no decoder of its own (`zeta-core`, `zeta`; Opus)

**Files:**
- Modify: `crates/zeta-core/src/modes/row.rs` (`ModeRow::decoder`, `validate`), `crates/zeta-core/src/modes/mod.rs` (`add`, `own_handler`, `ModeHandler::of_row`, `attach_handlers`, `carry_programs`, the hand-built row), `crates/zeta-core/src/modes/handler_file.rs` (`own_handler_id`, `validate_entry`, the reserved id), `crates/zeta-core/src/modes/template.rs` (`outputs`, `voices`, `expand`, `check_band`), `crates/zeta-core/src/modes/policy.rs` (`reported`), `crates/zeta-core/src/modes/signatures.rs:279`, `crates/zeta-core/src/ops.rs` (`ModeInfo::decoder_kind`, `ModeHandlerInfo` listing, `mode_row`, `start_handler`), `crates/zeta/src/modes_check.rs` (`check_row`), `crates/zeta/src/node_setup.rs` (the probe; `every_job_kind_expands_to_a_valid_graph`), `crates/zeta-dsp/src/identify.rs` (`classify_candidate`, if it reads the decoder), `crates/zeta/src/agents_md.rs`, `handlers/README.md`
- Test: `crates/zeta-core/tests/modes.rs`, `crates/zeta-core/tests/jobs.rs`, `crates/zeta-core/tests/resume.rs`, `crates/zeta/tests/modes_check.rs`, the tests modules of `policy.rs` and `template.rs`
- Regenerate: `AGENTS.md`, `skills/zetasdr/SKILL.md`

**Interfaces:**
- Produces:
  - `ModeRow::decoder: Option<Decoder>`; a row file writes `"decoder": null` (the key is required).
  - `pub const NO_DECODER: &str = "none";` in `handler_file.rs`: `own_handler_id` gives it for a row with no decoder, and a handler file whose `id` is `none` is broken (`id: none is reserved`).
  - `LoadedMode::has_decoder(&self) -> bool`.
  - The stand-in: `handlers[0]` of such a row is `ModeHandler { id: "none", name: "none", in_process: false, outputs: [], state: Unavailable, reason: Some("the row has no decoder of its own"), .. }`. The row's own `state` is `Unavailable`, its `reason` `no decoder of its own, and no handler for it is ready`, its `outputs` and `voices` empty.
  - `ModeInfo::decoder_kind: Option<DecoderKind>` (`null` for such a row); `modes_list`'s and `modes_show`'s `handlers` leave the stand-in out.
  - `validate`: a row with no decoder is refused with `identify.quick` other than `null` (`identify.quick: a row with no decoder has no quick test`) and with a `test` of kind `synth` (`test.kind: a row with no decoder is checked on a fixture`).
  - `validate_entry`: for a row with no decoder, the entry's `fields` and `extra` together must name every property of the row's schema bar `ENVELOPE_FIELDS` (`fields: <name> is not mapped, and the row has no decoder of its own to fill it`).
  - `modes check <id>` on such a row: the policy's pick is checked and recorded as `<id>@<handler>`, printing `<id>@<handler>: ok`; with no ready handler it is a skip, `<id>: skipped, no decoder of its own and no handler ready`.

- [ ] **Step 1: Write the failing tests.** A test row `nodec_decode` (a copy of a small embedded row with `"decoder": null`, no `quick`, no `test`) and a test handler file `alt` serving it with a fake program, by the suite's existing helpers for handler files:
  - `modes.rs`, `a_row_with_no_decoder_loads_unavailable`: no handler file: state `unavailable`, the reason above, `decoder_kind` null, `handlers` empty, `handler` null, `outputs` empty, `startable()` false.
  - `modes.rs`, `a_row_with_no_decoder_is_ready_through_a_handler`: with `alt` ready: state `ready`, `handlers` is `[alt]`, `handler` `alt`, `outputs` are `alt`'s, `program` `alt`'s.
  - `modes.rs`, `a_handler_file_named_none_is_broken`; `a_row_with_no_decoder_and_a_quick_test_is_broken`; `..._and_a_synth_test_is_broken`; `an_entry_for_a_row_with_no_decoder_must_map_every_field`.
  - `policy.rs`, `a_row_with_no_decoder_is_ranked_node_wide`: handlers `none` (unavailable) and `alt`, `onair` (Event + Packet), own outputs empty; `ranked(&[], &["onair"])` picks `onair`; with neither ready, `pick` is `Err` listing both and never `none` as picked.
  - `jobs.rs`, `a_row_with_no_decoder_starts_through_its_handler`: `job_start` naming no handler runs `alt`, and the job's record has `handler` `alt`; `handler: "none"` is refused as not ready with the stand-in's reason; with no handler file the start is refused with `mode nodec_decode is unavailable: no decoder of its own, and no handler for it is ready`.
  - `jobs.rs`, `nothing_starts_a_row_with_no_decoder_and_no_handler`: a set naming it, a plan, a watch over its band, an identify on its band and a `capture_decode` each refuse or pass over it with that reason (Review Focus 1).
  - `resume.rs`, `a_stored_job_of_a_row_whose_handler_is_gone_fails_with_the_reason`: a job started through `alt`, the node restarted without `alt`'s file: the job is failed with the row's reason, and the node starts.
  - `modes_check.rs`, `a_row_with_no_decoder_is_checked_through_the_policys_pick` and `..._is_a_skip_with_no_handler` (and a fail under `--strict`).
- [ ] **Step 2: Run them.** `cargo test -p zeta-core --features test-fixtures --test modes -- a_row_with_no_decoder` (and the others by name). Expected: FAIL, the row is broken (`row: invalid type: null`).
- [ ] **Step 3: Make `decoder` optional** and follow the compiler: `None` arms give no outputs, no voices, no handler blocks, no audio policy; `expand` and `check_band` without a block on such a row are an error naming the row, never a panic.
- [ ] **Step 4: The stand-in, the state and reason at load, `reported`, the listing, the refusals' words, `modes check`.**
- [ ] **Step 5: Run the new tests, then `cargo test -p zeta-core --features test-fixtures` and `cargo test -p zeta --test modes_check`.** Expected: pass; `a_node_with_no_handlers_beyond_the_rows_lists_modes_as_before` unchanged (no embedded row has no decoder yet).
- [ ] **Step 6: The prose** (`agents_md.rs`: `modes_list`'s `decoder_kind` and `handlers`, `modes_show`, the handlers paragraph; `handlers/README.md`: a row with no decoder, the reserved id, the every-field rule); regenerate the two generated files; run both `agents-md --check` gates.
- [ ] **Step 7: Commit.** `OnAir-only modes: a row may have no decoder of its own, and is run only by a handler`

### Task 7: The `content` field (`zeta-core`, `zeta-ws`; Sonnet)

**Files:**
- Modify: `crates/zeta-core/src/modes/row.rs` (`ModeRow::content`, `Content`), `crates/zeta-core/src/ops.rs` (`ModeInfo::content`, `AttachTo`, `packets_attach`, `expand_attached`), `crates/zeta-core/src/node/launch.rs` (the context note; the relaunch refresh at `:1249`), `crates/zeta-core/src/packet_capture.rs` (`PacketCaptureParams`), `crates/zeta-core/src/packets.rs` (`NewPacketCapture`, `PacketCaptureRecord`), `crates/zeta/src/agents_md.rs`
- Test: `crates/zeta-core/tests/modes.rs`, `crates/zeta-core/tests/attach.rs`, `crates/zeta-core/tests/jobs.rs`
- Regenerate: `AGENTS.md`, `skills/zetasdr/SKILL.md`

**Interfaces:**
- Produces: `pub enum Content { NetworkTraffic }` (`snake_case`, `word()` as `Access` has); `ModeRow::content: Option<Content>` (`#[serde(default)]`); `ModeInfo::content` beside `access`, nullable; a job's context holds `content: "network_traffic"` from launch (every start and relaunch, at the one merge in `launch.rs`); `PacketCaptureRecord::content: Option<Content>` with a serde default. It follows `access`'s chain link for link, and is not put on events, in the packet comment, or in anything that gates.

- [ ] **Step 1: Write the failing tests:** `modes.rs`: a row with `"content": "network_traffic"` reports it in `modes_list` and `modes_show`; `"content": "other"` is a broken row (`content: unknown variant`); every embedded row today reports `null`. `jobs.rs`: a job of such a row has the context note after launch (poll with the suite's `until`, the note is written when the stage starts). `attach.rs`: a capture attached to such a job writes a record with `content`, still there after the input job is relaunched; an `index.json` without the member loads with `content` null; a restricted row's capture is still refused, a `content` row's is not.
- [ ] **Step 2: Run them.** Expected: FAIL (`unknown field content`).
- [ ] **Step 3: Add the field and thread it.**
- [ ] **Step 4: Run them and `attach.rs` whole.** Expected: pass.
- [ ] **Step 5: Prose** (`modes_list`'s note: what `content` is, that nothing reads it yet); regenerate; both `--check` gates.
- [ ] **Step 6: Commit.** `OnAir-only modes: a row says what it carries (content), on modes_list, the job's context and its packet captures`

### Task 8: `$bandwidth_hz` for a program's arguments (`zeta-core`, `zeta-ext`; Sonnet)

**Files:**
- Modify: `crates/zeta-core/src/modes/row.rs` (`PROGRAM_PLACEHOLDERS`), `crates/zeta-core/src/modes/template.rs` (where a program block's arguments are filled), `crates/zeta-ext/src/external.rs` if the fill is there, `handlers/README.md`, `TODO.md` (plan 46's follow-up line: the placeholder exists; DVB-T's entry does not use it yet)
- Test: the tests module of `template.rs`, `crates/zeta-ext/tests/external.rs`

**Interfaces:**
- Produces: `$bandwidth_hz` in a program row's or handler entry's `args` is the job's band width in Hz, a whole number, filled where `$center_hz` is; an unknown placeholder's error text lists it.

- [ ] **Step 1: Write the failing test** `a_programs_arguments_take_the_jobs_bandwidth`: a handler entry with `["--width", "$bandwidth_hz"]` on a job 1500000 Hz wide launches the program with `--width 1500000`; after a retune to 2000000, with `--width 2000000`.
- [ ] **Step 2: Run it.** Expected: FAIL, the entry is broken (`args: unknown placeholder $bandwidth_hz`).
- [ ] **Step 3: Add the placeholder.**
- [ ] **Step 4: Run it and the neighbours.** Expected: pass.
- [ ] **Step 5: Commit.** `OnAir-only modes: a program's arguments may take the job's bandwidth`

### Task 9: Seven rows and their OnAir entries (Sonnet)

Bluetooth LE, LoRaWAN, Zigbee, Thread, Z-Wave, Wi-Fi, Wi-SUN.

**Files:**
- Create: `modes/ble_decode.json`, `modes/lorawan_decode.json`, `modes/zigbee_decode.json`, `modes/thread_decode.json`, `modes/zwave_decode.json`, `modes/wifi_decode.json`, `modes/wisun_decode.json`, `scripts/onair-fixtures.sh`
- Modify: `handlers/onair.json`, `crates/zeta-core/src/modes/mod.rs` (`EMBEDDED`), `crates/zeta-core/tests/modes.rs` (its copy of the count; the OnAir handler test; `a_node_with_no_handlers_beyond_the_rows_lists_modes_as_before`), `crates/zeta/tests/modes_check.rs` (the wrapper-backed test), `crates/zeta/src/node_setup.rs` (the expand test, if Task 6 left the new ids to it)

**Interfaces:**
- Consumes: Task 6 (`decoder: null`), Task 7 (`content`), Task 8 (`$bandwidth_hz`); `onair-rx` 0.4.0's lines for the seven (Tasks 1 to 3; its README is the reference) and `onair-gen --sigmf`.
- Produces, per row: `"decoder": null`, `"content": "network_traffic"`, `family` (`iot` for all seven), `label` (Bluetooth LE, LoRaWAN, Zigbee, Thread, Z-Wave, Wi-Fi, Wi-SUN), `protocol` (`ble`, `lorawan`, `zigbee`, `thread`, `zwave`, `wifi`, `wisun`), `band`, an `event.schema` holding exactly the wrapper's members for the mode with a `summary`, an `identify` block with `"quick": null` and `"proof": "event"`, `notes` (what is decoded and what is not, the rate the handler takes, that one job reads one channel or every channel in its band, where the ranges come from), and a `test` `{"kind": "fixture", "path": "onair/<mode>.sigmf-meta", "expect": {...}, "within_ms": ...}` whose `expect` is the generator's own values.
  - Subjects: `ble_decode` key `mac` (name `devices`); `lorawan_decode` key `devaddr` (`devices`); `zigbee_decode` and `thread_decode` key `src` (`devices`); `zwave_decode` key `home_id` (`networks`); `wifi_decode` key `bssid` (`networks`, label `ssid`); `wisun_decode` key `src` (`devices`).
  - Bands, start values: `ble_decode` 2 MHz at the three advertising channels (2402, 2426, 2480 MHz); `zigbee_decode` 2 MHz at channels 11 to 26 (2405 MHz + 5 MHz steps) with the sub-GHz ranges the receiver's BPSK PHY takes; `thread_decode` the 2.4 GHz channels only; `wifi_decode` 20 MHz, narrowest 20 MHz, ranges the 2.4, 5 and 6 GHz bands; `lorawan_decode`, `zwave_decode`, `wisun_decode` 2 MHz, narrowest one channel's width, ranges the union of the receiver's own plan or region table.
- Produces, per entry in `handlers/onair.json`: `["--mode", <m>, "--rate", "$rate_hz", "--format", "cs16", "--freq", "$center_hz", "--pcap-port", "$packet_port"]`, `input` cs16 on stdin at the rate Tasks 1 to 3 settled, `output` `json_lines` with `fields` naming every member, and `packets` on tcp `$packet_port`, parser `pcap`, with `link_types` naming the mode's number where the node has a name for it.
- `scripts/onair-fixtures.sh <onair-gen> <dir>`: makes `<dir>/onair/<mode>.sigmf-data` and `.sigmf-meta` for every row whose `test` names one, at the row's entry rate, by `onair-gen --sigmf`. Prints one line per pair; exits non-zero if a generator fails.

- [ ] **Step 1: Extend the tests.** `modes.rs`: the embedded count (17 plus seven); the OnAir handler test gains the seven with `&[Event, Packet]` and their rates; a new `the_embedded_rows_with_no_decoder_carry_content_and_a_fixture_test` (every embedded row with no decoder has `content`, a fixture `test`, no quick test, a `notes` text that names its ranges' source); `a_node_with_no_handlers_beyond_the_rows_lists_modes_as_before` reads "one handler" as "the row's own alone, or none listed for a row with no decoder". `modes_check.rs`, in `onair_passes_its_modes_checks`: when `ZETA_TEST_ONAIR_GEN` is also set, run `scripts/onair-fixtures.sh` into a temporary directory, link the repository's VDL2 fixture into it, pass it as `ZETA_FIXTURES_DIR`, and check the seven ids under `--handler onair --strict`; unset, print `<id>@onair: not run, ZETA_TEST_ONAIR_GEN is not set` for each.
- [ ] **Step 2: Run them.** Expected: FAIL (the count; no such mode).
- [ ] **Step 3: Write the script, then the seven rows and entries,** one mode at a time: row, entry, `zeta modes check <id> --handler onair --strict` by hand with the generated pair.
- [ ] **Step 4: Run the tests** with `ZETA_TEST_ONAIR_RX` and `ZETA_TEST_ONAIR_GEN` set to the frozen binaries. Expected: pass, `<id>@onair: ok` for the seven.
- [ ] **Step 5: Measure against the listing bar.** A scratch node on a file source of each generated pair, the job run for 30 s: no dropped samples reported for the job; the wrapper's CPU read (`ps` of the child). The figures go in each row's notes. A mode that misses comes out (row, entry, tests, count) with its line in `TODO.md`.
- [ ] **Step 6: Commit.** `OnAir-only modes: Bluetooth LE, LoRaWAN, Zigbee, Thread, Z-Wave, Wi-Fi and Wi-SUN as rows OnAir decodes`

### Task 10: Four rows and their OnAir entries (Sonnet; resume Task 9's implementer)

TETRA, GSM-R, LTE, DVB-S IP.

**Files:**
- Create: `modes/tetra_decode.json`, `modes/gsmr_decode.json`, `modes/lte_decode.json`, `modes/dvbs_ip_decode.json`
- Modify: as Task 9 (`handlers/onair.json`, the count, the three tests)

**Interfaces:**
- Consumes: as Task 9; `onair-rx` 0.4.0's lines for the four (Tasks 4 and 5) and its README's figures with and without `--drop`.
- Produces: rows as Task 9's. `family`: `two_way` (TETRA), `cellular_rail` (GSM-R, LTE), `satellite` (DVB-S IP). `label`: TETRA, GSM-R, LTE, DVB-S IP. `protocol`: `tetra`, `gsmr`, `lte`, `dvbs_ip`. Subjects: `tetra_decode` and `gsmr_decode` key `freq_hz` (name `cells`); `lte_decode` key `pci` (`cells`); `dvbs_ip_decode` none (a status row, as DVB-T's). `identify`: `tetra_decode`, `gsmr_decode`, `lte_decode` proof `event`; `dvbs_ip_decode` proof `field` on `locked` with `as_asked` true, as the DVB-T row. Bands, start values: `tetra_decode` 2 MHz, narrowest 25 kHz; `gsmr_decode` 2 MHz, narrowest 200 kHz; `lte_decode` 20 MHz, narrowest 1.4 MHz; `dvbs_ip_decode` 10 MHz, narrowest 1 MHz, no ranges (a dish's intermediate frequency is the operator's).
- Entries as Task 9's; `dvbs_ip_decode` adds `"--width", "$bandwidth_hz"`; an entry carries `"--drop"` only if the mode met the listing bar no other way.

- [ ] **Step 1: Extend the tests** as Task 9, Step 1, for the four (the count; the handler test; the wrapper-backed list).
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Write the four rows and entries,** one mode at a time, each checked by hand.
- [ ] **Step 4: Run the tests** with both variables set. Expected: pass.
- [ ] **Step 5: Measure against the listing bar** as Task 9, Step 5; LTE also on a 20 MHz carrier. Figures into the notes; a mode that misses comes out with its `TODO.md` line.
- [ ] **Step 6: Commit.** `OnAir-only modes: TETRA, GSM-R and LTE cells and IP over DVB-S as rows OnAir decodes`

### Task 11: The page's Decode entries (`zeta-ui`; Opus)

**Files:**
- Modify: `crates/zeta-ui/web/actions.js` (the entries; the header comment's list), `crates/zeta-ui/web/subjects.js` only if Step 4 finds a wrong read, `crates/zeta/tests/ui.rs` (`fake_handlers`: the suite's `onair` file also serves `ble_decode` with a fake program that writes two recorded lines), `crates/zeta/tests/ui.py` or wherever the scenarios live (`grep -rn "decode_across" crates/zeta/tests`)

**Interfaces:**
- Consumes: the rows of Tasks 9 and 10 as `modes_list` gives them (`state`, `reason`, `handlers` without the stand-in, `band`).
- Produces: one `decoder("<row id>", "<the row's label>")` entry per listed row, after ACARS and before Video, in the order of the plan's table. An entry whose row is not `ready` is disabled with the row's `reason` as its title, by the mechanism the Video entry's `disabled` uses; the six existing entries are not changed.

- [ ] **Step 1: Write the failing scenarios:** `a_row_with_no_decoder_has_a_decode_entry`: on a box at 2426 MHz, 2 MHz wide, the Decode group shows "Bluetooth LE"; choosing it starts a `ble_decode` job whose record names handler `onair`, and its two events reach the events pane. `a_decode_entry_for_a_row_with_no_ready_handler_is_greyed`: the same box shows "Zigbee" disabled, its title the row's reason. `a_decode_entry_falls_elsewhere_off_its_band`: on a box at 100 MHz, "Bluetooth LE" is under the elsewhere submenu, as RDS is on a box at 1090 MHz.
- [ ] **Step 2: Run them.** `ZETA_UI_ONLY=<the three> cargo test -p zeta --test ui -- --nocapture`. Expected: FAIL (no such entry), and not `ui e2e skipped`.
- [ ] **Step 3: Add the entries and the disabled rule;** extend `fake_handlers`.
- [ ] **Step 4: Read the two first-handler reads** (`actions.js` `groupHandlers`, `subjects.js` `handlerOf`) against a row whose first listed handler is a file's: a job of such a row always names its handler, so neither fallback is reached; if either is, fix it and add the scenario that shows it.
- [ ] **Step 5: Run the three scenarios, then the whole UI suite once.** Expected: pass; scenarios that count menu entries or list modes are updated only where the new rows are the cause.
- [ ] **Step 6: Commit.** `OnAir-only modes: a Decode entry for each row OnAir alone decodes`

### Task 12: Documents and the gates (Sonnet)

**Files:**
- Modify: `RECIPES.md` (a recipe: the eleven modes, what each gives, the capture for Wireshark, the widths and costs measured, Z-Wave's user link type and how to read it), `docs/agents/field-craft.md` (what a row with no decoder is; one job per channel for the one-channel receivers; the wide ones need a source that holds the rate), `handlers/README.md`, `docs/licensing.md` if it lists what OnAir serves, `UAT.md` (a "Plan 47" section), `TODO.md`, `CHANGELOG.md` (Unreleased)
- Regenerate: `AGENTS.md`, `skills/zetasdr/SKILL.md`

- [ ] **Step 1: `UAT.md`, plan 47**, unticked, saying what was and was not run when written: 47.1 `handlers list` on the field host shows OnAir ready for the listed modes once 0.4.0 is installed, and the rows read `ready`; 47.2 a Bluetooth LE job from the page's Decode entry lists nearby advertisers, and its capture opens in Wireshark as Bluetooth LE; 47.3 the same for one of Zigbee, Z-Wave, LoRaWAN or Wi-SUN, whichever is on the air; 47.4 Wi-Fi beacons at 20 MHz on a radio that holds the rate, with the node's dropped-sample count read; 47.5 an LTE cell's identity and system information, the capture opening as LTE RRC; 47.6 TETRA or GSM-R where a carrier is in reach; 47.7 `modes_list`, a job's context and a capture's record each show `content`; 47.8 the CPU of each job, written down; 47.9 with `onair-rx` moved aside, the rows read `unavailable` with the reason and their Decode entries are greyed.
- [ ] **Step 2: `TODO.md`:** plan 47's line rewritten as built with what is left, one line each: the note's non-goals; any mode that missed the bar; Decode entries made from the rows themselves; a richer Bluetooth LE line (PDU type, channel, advertising data); nothing reads `content` yet (pointing at "Policies"); what a receiver lacked a switch for. No line for ASTERIX.
- [ ] **Step 3: The prose** in the files above; regenerate the two generated files.
- [ ] **Step 4: The gates,** each with its output read: `cargo fmt` per touched crate, clippy, `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium` with `ZETA_TEST_ONAIR_RX` and `ZETA_TEST_ONAIR_GEN` set, `cargo test -p zeta --test ui -- --nocapture` (check it did not print `ui e2e skipped`), both `agents-md --check`, `scripts/public-check.sh`, `scripts/check-doc-links.sh`; in `onair-rx`: `test/run.sh`, `test/bus.sh`, `test/poll.sh`, `test/only.sh`, `test/patch.sh`.
- [ ] **Step 5: Commit.** `OnAir-only modes: the recipe, the acceptance checks and what is left`
