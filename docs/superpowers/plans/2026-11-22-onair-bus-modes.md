# OnAir's Modes on the Bus (Plan 45) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** ACARS, POCSAG, FLEX, APRS, AIS, VDL2, HFDL and the radiosondes each gain OnAir as a second handler, AIS and APRS with packets, every one proven by `zeta modes check --handler onair`.

**Architecture:** `onair-rx` (a separate GPL repository) gains one generic mode: any OnAir receiver with `configure/ready/feed`, its decodes taken from OnAir's automation bus and its frames from OnAir's Wireshark hub through a small carried patch. A mode is a table row there and a handler-file entry in the node. The node changes in one place only: a handler entry may give a constant for a row field.

**Tech Stack:** C++20 against OnAir `a63c3a8` (`onair-rx`: `build.sh`, `test/run.sh`); Rust 1.97 (`zeta-core`, `zeta-ext`, `zeta`); JSON handler files.

**Spec:** `docs/superpowers/specs/2026-11-22-onair-bus-modes-design.md` (the design note; "the note" below), on `docs/superpowers/specs/2026-11-21-alternative-handlers-design.md` sections "Handlers" and "OnAir". Read both. The spike the note reports is `onair-rx` branch `spike-generic` (`src/bus.cpp`): read it for the shape, do not merge it.

## Global Constraints

- **Two repositories.** Tasks 1 to 4 work in `/Projects/onair-rx` (branch `bus-modes` from `master`); tasks 5 to 8 in this repository's worktree. `onair-rx` and OnAir are never copied, linked or vendored into this repository: only `handlers/onair.json` names them.
- **OnAir checkout:** `scratch/upstream/OnAir` of the main checkout (`ONAIR_SRC`), at `a63c3a8`. It is never edited in place: `build.sh` applies `patches/*.patch` to a copy it builds from. The patched build is a full OnAir build (about 20 minutes, `nice -n 19`, `JOBS` at most 8); do it once, in Task 1.
- **`onair-rx` licence:** GPL-3.0-or-later; every new source file starts with the SPDX line and `Based on OnAir by Kristijonas Adomaitis - https://on-air.dev`.
- **The wrapper's contract:** `onair-rx --mode <m> --rate <hz> --format cs16|cf32 [--freq <hz>] [--pcap-port <p>]`, IQ on stdin, one JSON object per line on stdout, a mode it does not have ends it with exit 2 and `onair-rx: no mode <m>` on stderr. ADS-B's lines and packets do not change by a byte.
- **Mode names in `onair-rx`:** `adsb`, `acars`, `pocsag`, `flex`, `aprs`, `ais`, `vdl2`, `hfdl`, `rs41`, `dfm`, `m20`.
- **Every line of a bus mode** has `event` (the bus type, for example `pager.message`) and `t_sample` (index of the first sample of the block whose `feed()` produced it); whole numbers are written without a decimal point; non-finite numbers are left out.
- **Input rates** (the receivers' own minimums): ACARS, POCSAG, FLEX 1000000; APRS 240000; AIS, VDL2 250000; HFDL 24000; sondes 2000000.
- **The node is unchanged for a node with no `onair-rx`:** every existing test passes unchanged, and OnAir's handlers are `unavailable` with the missing program named.
- **No row changes, no new operation, no page change.** `decoder: null`, `policy`, RDS, DMR, P25, M17, DVB-T and analog video are plans 46 and 47.
- **A mode that does not fit the generic path is not forced:** it is left out of `handlers/onair.json`, and `TODO.md`'s plan 46 gist gains one line saying why.
- Public repository: roles not host names, no observed traffic; captures of real traffic never enter git. Test signals are synthesised. `scripts/public-check.sh` and `scripts/check-doc-links.sh` clean before the branch lands.
- Gates per `CLAUDE.md`: `cargo fmt -p <crate> -- --check` per crate (nested worktree), clippy `-D warnings`, a private `CARGO_TARGET_DIR`. Per task only its tests and neighbours; crate suites once per batch; the workspace suite once at the end. No UI suite: the page is untouched. `zeta-core` tests take `--features test-fixtures`.
- Commit messages: `onair-rx: <what>` in `onair-rx`; `OnAir modes: <what>` here.
- Nothing is pushed, deployed or offered upstream by this plan. Landing on `main`, the field host and the pull requests to OnAir wait for the operator's word.

## Review Focus

- **No packet client ever connects** (a job without `needs: packet` still passes `--pcap-port` only when asked, but a client may also hang up mid-run): the decode loop never blocks on the hub sink, and events keep their rate. (Task 2)
- **A frequency-placed mode started without `--freq`, or with a centre the mode's channels do not fall in:** exit 2 with one line naming the option, before any sample is read; never a silent run that decodes nothing. (Task 2)
- **Text the bus cuts or that is not ASCII** (a pager message of 2000 characters, an APRS comment with bytes above 0x7e, a control character): the line is still valid JSON and one line. (Task 2)
- **A multi-sentence AIS message whose second sentence never arrives, or arrives after another message's:** no event built from half a message, no sentence of one message joined to another's, and the lone sentence is dropped after its channel's next complete message. (Task 3)
- **A handler constant for a field the program also gives, or of a type the row's schema refuses:** the entry fails validation and breaks alone, the file's other modes load. (Task 5)

---

### Task 1: The hub sink patch, applied by the build (`onair-rx`)

**Files:**
- Create: `patches/0001-wirecap-hub-sink.patch`, `test/patch.sh`
- Modify: `build.sh`, `README.md` (the build section; and correct the sentence saying OnAir's file source "sizes, seeks and loops": that is true only of its SigMF, WAV and BLUE readers)

**Interfaces:**
- Produces, in OnAir's `dect2/wirecap/wire.h`:
  `using Sink = std::function<void(Chan c, int64_t unixUsec, const uint8_t* p, size_t n)>;`
  `void Hub::setSink(Sink s);` An empty function removes it. The sink is called from `Hub::publish`, on the publisher's thread, for an enabled channel, before and whether or not the packet is queued, with `unixUsec` exactly as the caller passed it (0 when it passed none). With a sink set and `Config{files=false, live=false, gsmtap=false}` the hub opens no file and no socket.
- Produces: `build.sh` builds from `_onair-src`, a copy of `$ONAIR_SRC` with every `patches/*.patch` applied (`git apply`), remade when the checkout's commit or a patch's content changes (a stamp file holding both hashes); `ONAIR_VERSION` is the checkout's short commit followed by `+patched`.

- [ ] **Step 1: Write `test/patch.sh`.** It applies each patch to a clean copy of `$ONAIR_SRC` with `git apply --check` and fails naming the patch that does not apply; it fails when `_onair-src` exists and its stamp does not match.
- [ ] **Step 2: Run it.** `ONAIR_SRC=/Projects/ZetaSDR/scratch/upstream/OnAir bash test/patch.sh`. Expected: FAIL, no `patches/` directory.
- [ ] **Step 3: Write the patch** against `core/include/dect2/wirecap/wire.h` and `core/src/wirecap/wire.cpp` to the interface above (the sink under its own mutex, copied out before the call, as `automation::Engine::submit` does for its tap), plus a test in OnAir's style, `tests/wirecap/test_wire_sink.cpp`: a sink set, a channel enabled, `publish(c, p, n, 42)` reaches the sink with 42 and the same bytes; a disabled channel reaches nothing; an empty sink removes it.
- [ ] **Step 4: Teach `build.sh` the patched copy** and rebuild (`ONAIR_REBUILD=1`). Expected: `built .../onair-rx`, and `./onair-rx --version` prints `OnAir a63c3a8+patched`.
- [ ] **Step 5: Run `test/patch.sh` and `test/run.sh`.** Expected: both pass; the ADS-B counts of `test/run.sh` are the ones `master` gives.
- [ ] **Step 6: Commit.** `onair-rx: OnAir's Wireshark hub gains a sink, as a patch the build applies`

### Task 2: The generic bus mode, with POCSAG, FLEX, ACARS and APRS (`onair-rx`)

**Files:**
- Create: `src/bus.cpp` (the generic run and the mode rows), `src/bus.h`, `test/gen.cpp` (test signals), `test/bus.sh`
- Modify: `src/main.cpp` (`--freq`, the mode table, usage, version `0.2.0`), `src/rx.h` (`Options.freq`, `Json` arrays and raw members), `build.sh` (also builds `_build/onair-gen`), `README.md`

**Interfaces:**
- Consumes: `Hub::setSink` (Task 1).
- Produces: `Options.freq` (`double`, 0 when not given).
- Produces, in `bus.h`:
  ```cpp
  struct BusMode {
      const char* name;                 // "pocsag"
      int chan;                         // a dect2::wire::Chan, or -1: no packets
      bool needsFreq;
      // makes the receiver, sets it up for o, and returns its feed; false with *err set when o does not suit the mode
      bool (*open)(const Options& o, std::function<void(const cf32*, size_t)>* feed, std::string* err);
      // adds the members the row needs and the bus lacks; false drops the event
      bool (*shape)(const dect2::automation::Event& e, Json& j);
  };
  int runBus(const BusMode& m, const Options& o, IqInput& in, Output& out);
  ```
  `runBus` taps the bus with `kAllFamilies`, registers the hub sink for `m.chan` when there is a pcap port, feeds blocks of 16384 samples, and after each `feed()` writes the block's events then its packets. A packet's time is the ADS-B mode's rule (`adsb.cpp`): signal time of the block's first sample on the wall clock of the first sample, moved on when the input falls behind real time.
- Produces: `onair-gen --mode <m> --rate <hz> [--freq <hz>]`: cf32 IQ on stdout of one known message from OnAir's own test-signal generator for the mode (`core/src/<mode>/*_gen.cpp`), with a second of noise each side. The known messages: POCSAG address 1234567 function 3 text `HELLO ZETA` at 1200 baud; FLEX capcode 1234567 text `HELLO ZETA`; ACARS tail `VH-ZET` label `H1` flight `ZET001`; APRS `VK3ABC>APRS:!2723.05S/15307.05E>ZetaSDR`.
- The rows and what `shape` adds:
  - `pocsag`, `flex`: `PagerReceiver`, `setProtocols(1)` and `(2)`; adds `baud` (integer, from the bus's `speed`) and for FLEX `levels` (2 or 4); drops events of the other protocol.
  - `acars`: `AcarsReceiver`, `needsFreq`, `setCenterHz(freq)` and `setChannels({freq})`.
  - `aprs`: `PacketReceiver`, `chan` `wire::Aprs` (link type 3); adds `fcs_ok` (true unless OnAir marks the frame repaired: read `packet_rx.cpp` near line 316 for which frames it publishes, and say in a comment what was found) and `path` as an array of strings.

- [ ] **Step 1: Write `test/bus.sh`.** For each of the four modes: `onair-gen | onair-rx` gives at least one line; every line parses as JSON with a string `event` and an integer `t_sample` below the sample count; the known message's members are there with the values above (`address` is the integer `1234567`, not `1234567.0`); `t_sample / rate` lies within 0.5 s of where the generator put the message. APRS with `--pcap-port`: the pcap client of `test/run.sh` receives a header with link type 3 and one record whose bytes hold `VK3ABC` shifted as AX.25 addresses. From Review Focus: `--mode acars` without `--freq` exits 2 with `onair-rx: acars needs --freq HZ` and reads no input; a POCSAG message of 2000 characters and an APRS comment holding bytes 0x01, 0x22, 0x5c and 0xe9 each give exactly one line that parses; `--pcap-port` with no client and with a client that hangs up after the header: the line count and the run time (within 20 %) are those of a run without the option.
- [ ] **Step 2: Run it.** Expected: FAIL, `onair-rx: no mode pocsag`.
- [ ] **Step 3: Implement `test/gen.cpp`, `bus.h`, `bus.cpp` and the `main.cpp` changes** to the interfaces above. `Json` gains `Json& arr(const char* k, const std::vector<std::string>& v)`.
- [ ] **Step 4: Run `test/bus.sh` and `test/run.sh`.** Expected: both pass.
- [ ] **Step 5: Commit.** `onair-rx: receivers on OnAir's bus and hub, with pagers, ACARS and APRS`

### Task 3: AIS, an event per message from the hub's sentences (`onair-rx`)

**Files:**
- Create: `src/ais.cpp`
- Modify: `src/bus.cpp` or `src/main.cpp` (the `ais` row), `test/gen.cpp`, `test/bus.sh`, `README.md`

**Interfaces:**
- Consumes: `runBus`'s loop, the hub sink (`wire::Ais`: each packet is one NMEA sentence ending `\r\n`; the pcap record wraps it as OnAir's file writer does, raw IP with UDP port `kNmeaPort`, link type 101).
- Produces: mode `ais`, `needsFreq`, `AisReceiver::setCenterHz(freq)`. One line per complete AIS message with `event: "ais.message"`, `mmsi`, `msg_type`, `channel` (`"A"` or `"B"`), `repeat`, `nmea` (array of the message's sentences without line ends) and, where the message type carries them, `lat`, `lon`, `sog_kt`, `cog_deg`, `heading_deg`, `nav_status`, `name`, `callsign`, `destination`, `imo`, `ship_type`. The message is parsed by OnAir's own AIS parser (`dect2/ais/ais_proto.h`), not by new code. The bus's `ais.vessel` events are dropped.
- Produces: `onair-gen --mode ais`: a type 1 position report, MMSI 503000001, -27.3842, 153.1175, 12.3 kt, 45.0 degrees, on AIS 1, and a type 5 static report (two sentences) for the same MMSI named `ZETA TEST`.

- [ ] **Step 1: Extend `test/bus.sh`.** The generated capture at 250000 Hz with `--freq 162000000` gives a line with `msg_type` 1, `mmsi` 503000001, `lat` and `lon` within 0.0001, `sog_kt` 12.3, one `nmea` sentence starting `!AIVDM`; and a line with `msg_type` 5, `name` `ZETA TEST`, two `nmea` sentences. The pcap client gets link type 101 and three records. From Review Focus, as a unit test of the joiner built into `onair-gen --selftest ais-join` (sentences in, messages out): a two-sentence message whose second part never comes gives no event and is forgotten when the channel's next complete message arrives; part 2 of sequence id 3 is never joined to part 1 of sequence id 4.
- [ ] **Step 2: Run it.** Expected: FAIL, `onair-rx: no mode ais`.
- [ ] **Step 3: Implement `ais.cpp`:** `class AisJoiner { public: std::optional<std::vector<std::string>> add(const std::string& sentence); };` keyed by channel and sequence id, and `bool aisLine(const std::vector<std::string>& sentences, Json& j);`.
- [ ] **Step 4: Run `test/bus.sh`.** Expected: PASS.
- [ ] **Step 5: Commit.** `onair-rx: AIS, an event per message from the sentences`

### Task 4: VDL2, HFDL and the radiosondes on the generic path (`onair-rx`)

**Files:**
- Modify: `src/bus.cpp`, `test/gen.cpp`, `test/bus.sh`, `README.md`

**Interfaces:**
- Produces: modes `vdl2` (`Vdl2Receiver`, `needsFreq`, `setCenterHz` and `setChannels({freq})`), `hfdl` (`HfdlReceiver`, `needsFreq`, the same two calls), `rs41`, `dfm`, `m20` (`SondeReceiver`; `shape` drops `sonde.update` events whose `type` is another sonde's).
- `shape` adds for `vdl2`: `icao` (the bus's `src` when `from_air`, else `dst`, six upper-case hex digits) and `addr_type`; for the sondes: `id` (the bus's `serial`).
- Produces: a table in `README.md`, one row per mode: the receiver, the minimum rate, whether `--freq` is needed, the bus event and what `shape` adds. A mode that could not be made to decode its own generator's signal on this path has no row in the mode table of `main.cpp`, and a line in `README.md` under "Not on the generic path" saying what it needed.

- [ ] **Step 1: Extend `test/gen.cpp` and `test/bus.sh`** for each of the five from OnAir's generators (`vdl2_gen`, `hfdl_gen`, the sonde generator per type): one known message each, the line's key member checked (`icao` for VDL2 and HFDL, `id` and a position within 0.001 for a sonde).
- [ ] **Step 2: Run it.** Expected: FAIL, `onair-rx: no mode vdl2`.
- [ ] **Step 3: Add the rows.** Give each mode at most what the spike's four needed (a channel, a centre, a type filter). When that is not enough, stop on that mode, write its README line and remove its checks.
- [ ] **Step 4: Run `test/bus.sh`, `test/run.sh`, `test/patch.sh`.** Expected: all pass.
- [ ] **Step 5: Commit.** `onair-rx: VDL2, HFDL and the radiosondes` and tag nothing.

### Task 5: A handler entry may give constants (`zeta-core`, `zeta-ext`)

**Files:**
- Modify: `crates/zeta-core/src/modes/handler_file.rs` (`HandlerEntry`, `EntryFields`, `validate_entry`, `program_block`), `crates/zeta-core/src/modes/row.rs` (`ProgramOutput`), `crates/zeta-ext/src/external.rs` (beside `apply_fields`, line 704, and its caller at 910), `crates/zeta/src/agents_md.rs` only if it describes handler files
- Test: unit tests in `handler_file.rs` and `external.rs`; `crates/zeta-core/tests/modes.rs`

**Interfaces:**
- Produces: a handler entry key `"const"`, an object of row event field to JSON value: `pub consts: BTreeMap<String, serde_json::Value>` on `HandlerEntry` (`#[serde(default, rename = "const")]`), carried to the stage as `ProgramOutput.consts` (`#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]`, so a row without it serialises as before). The stage sets each constant on every event after the field map.
- Validation, each breaking the entry alone, with these `field` and `error` strings:
  - a key not in the row's event schema: `const.<key>`, `not in the row's event schema`;
  - a key also in `output.fields` or `extra`: `const.<key>`, `also read from the program`;
  - a value the schema's `type` for that property refuses, or an object or array: `const.<key>`, `not a <type>` (`<type>` as the schema spells it, joined by ` or `);
  - the row's subject key given as a constant: `const.<key>`, `the row's subject key is read from the program`.
- A program row's own `decoder.output` does not accept `const` from a file (it has no need: its fields are its own): `row.rs` validation refuses it there with `decoder.output.const`, `only a handler file's entry gives constants`.

- [ ] **Step 1: Write the failing tests.** In `handler_file.rs`: `an_entry_gives_constants` (parse `{"const": {"protocol": "pocsag"}}` on an entry for `pocsag_decode`, `validate_entry` is `Ok`, `program_block(..).output.consts["protocol"] == json!("pocsag")`); `a_constant_must_fit_the_row` (the four refusals above, asserting `field` and `error`; then that a file with one bad and one good entry loads the good one). In `external.rs`: `constants_are_set_after_the_fields` (`fields` `{"address": "/a"}`, `consts` `{"protocol": "pocsag"}`, line `{"a": 7, "protocol": "x"}` gives `{"address": 7, "protocol": "pocsag"}`). In `tests/modes.rs`: `a_row_refuses_constants_of_its_own`.
- [ ] **Step 2: Run them.** `cargo test -p zeta-core --features test-fixtures an_entry_gives_constants a_constant_must_fit_the_row` and `cargo test -p zeta-ext constants_are_set_after_the_fields`. Expected: FAIL to compile, no `consts`.
- [ ] **Step 3: Implement** to the interfaces above.
- [ ] **Step 4: Run the three tests, then `cargo test -p zeta-core --features test-fixtures --test modes` and `cargo test -p zeta-ext`.** Expected: PASS; no existing test changed.
- [ ] **Step 5: Regenerate if prose changed** (`cargo run -q -p zeta -- agents-md > AGENTS.md`, and `--skill`), then commit. `OnAir modes: a handler entry may give a row field a constant`

### Task 6: OnAir's entries for the pagers, ACARS, APRS and AIS (`handlers/onair.json`)

**Files:**
- Modify: `handlers/onair.json`, `crates/zeta-core/tests/modes.rs` (beside `the_embedded_onair_handler_serves_adsb`), `crates/zeta/tests/modes_check.rs`, `docs/wireshark/zetasdr.lua` (its header notes only)

**Interfaces:**
- Consumes: `const` (Task 5); `onair-rx` modes (Tasks 2 and 3).
- Produces: entries for `pocsag_decode`, `flex_decode`, `acars_decode`, `aprs_decode`, `ais_decode`. Each: `args` `["--mode", "<m>", "--rate", "$rate_hz", "--format", "cs16", "--freq", "$center_hz"]` (plus `"--pcap-port", "$packet_port"` for APRS and AIS); `input` `{"stream": "iq", "format": "cs16", "rate_hz": <the mode's rate from Global Constraints>, "via": "stdin"}`; `output` `json_lines` on stdout.
- The maps (row field from wrapper member), anything not listed unmapped:
  - POCSAG: `address` `/address`, `function` `/function`, `baud` `/baud`, `kind` `/type`, `text` `/text`; `const` `protocol` as the row's own decoder writes it; `extra` `damaged` `/damaged`.
  - FLEX: `capcode` `/address`, `baud` `/baud`, `levels` `/levels`, `kind` `/type`, `text` `/text`; `const` `protocol`; `extra` `damaged`.
  - ACARS: `reg` `/tail`, `flight` `/flight`, `label` `/label`, `msg_no` `/msg_num`, `text` `/text`, `downlink` `/downlink`, `lat` `/lat`, `lon` `/lon`; `const` `protocol`.
  - APRS: `src` `/from`, `dst` `/to`, `path` `/path`, `info` `/info`, `fcs_ok` `/fcs_ok`, `lat` `/lat`, `lon` `/lon`; `const` `protocol` `"aprs"`; `extra` `summary` `/summary`; `packets` `{"via": "tcp", "port": "$packet_port", "parser": "pcap", "link_types": {"3": "ax25"}}`.
  - AIS: every row field of the same name from Task 3's line; `const` `protocol`; `packets` with `link_types` `{"101": "raw_ip"}`.
- Where a row's `kind` enum or `protocol` value is spelled differently from OnAir's, the entry is where it is reconciled if a pointer or constant can do it; otherwise the wrapper's `shape` is (and Task 2's test gains the check). The row is not changed.

- [ ] **Step 1: Write the failing tests.** `tests/modes.rs`: `the_embedded_onair_handler_serves_the_bus_modes`: for each of the five ids `handler_ids(&reg, id)` ends with `"onair"`, the handler validates (`reason` is only ever the missing program), `outputs` is `["event"]` for the pagers and ACARS and `["event", "packet"]` for APRS and AIS. `tests/modes_check.rs`: `onair_passes_the_bus_modes_checks`: when the test-only `ZETA_TEST_ONAIR_RX` names an executable (passed to the node as `ZETA_HANDLER_ONAIR_RX`, which the suite otherwise pins missing), `zeta modes check <id> --handler onair --strict` exits 0 and prints `<id>@onair: ok` for each of the five; otherwise it prints `onair check skipped: ZETA_TEST_ONAIR_RX is not set` and passes.
- [ ] **Step 2: Run them.** Expected: the first FAILS (`["multimon-ng"]`-style lists without `onair`); the second, run with `ZETA_TEST_ONAIR_RX=/Projects/onair-rx/onair-rx`, FAILS with `no handler onair`.
- [ ] **Step 3: Write the five entries.**
- [ ] **Step 4: Run both tests with `ZETA_TEST_ONAIR_RX` set.** Expected: PASS, with no `skipped` line. ACARS's check expects `alt_ft` 16500 from the row's `derive` on `text`: if OnAir's `text` (it strips the message number and flight) defeats the derivation, give the wrapper's `acars` `shape` a `text` rebuilt as the row's own decoder gives it, and say so in the entry's test.
- [ ] **Step 5: Note the CPU cost.** With `zeta modes check pocsag_decode --handler onair` and again without `--handler`, record user CPU seconds of the decoder process over the 10 s check (`/usr/bin/time -v`); put both numbers in the commit message. No code follows from it.
- [ ] **Step 6: Commit.** `OnAir modes: the pagers, ACARS, APRS and AIS through onair-rx`

### Task 7: OnAir's entries for VDL2, HFDL and the radiosondes

**Files:**
- Modify: `handlers/onair.json`, `crates/zeta-core/tests/modes.rs`, `crates/zeta/tests/modes_check.rs`, `TODO.md`

**Interfaces:**
- Consumes: Task 4's README table: only the modes with a row there get an entry.
- Produces: entries for `vdl2_decode` (`icao`, `addr_type`, `dst`, `frame_type` `/frame`; `extra` `summary`, `proto`), `hfdl_decode` (`icao`, `pdu` `/kind`; `extra` `summary`, `snr_db`, `uplink`), `rs41_decode`, `dfm_decode`, `m20_decode` (`id`, `lat`, `lon`, `alt` `/alt_m`, `vel_v` `/vspeed_ms`, `vel_h` `/hspeed_ms`, `sats`, `batt` `/batt_v`; `extra` `temp_c`), each with its rate and `--freq $center_hz`.
- Rows with a check that can pass today: `vdl2_decode` (a recorded fixture), `rs41_decode`, `m20_decode`. `hfdl_decode`'s fixture is pending and `dfm_decode` has no test: their entries are validated and listed but not run, and the test says so by name.

- [ ] **Step 1: Extend the two tests of Task 6** with these ids: handler lists and outputs for all with an entry; `modes check --handler onair --strict` for `vdl2_decode`, `rs41_decode`, `m20_decode`.
- [ ] **Step 2: Run them.** Expected: FAIL, no such handler.
- [ ] **Step 3: Write the entries.** A mode whose check OnAir cannot pass on the row's own capture (the capture is 12.5 to 25 kHz wide, resampled to the receiver's rate) is taken out again, and `TODO.md`'s plan 46 gist gains its line.
- [ ] **Step 4: Run both tests, then `cargo test -p zeta-core --features test-fixtures` and `cargo test -p zeta --test modes_check --test cli`.** Expected: PASS.
- [ ] **Step 5: Commit.** `OnAir modes: VDL2, HFDL and the radiosondes through onair-rx`

### Task 8: Documents and the gates

**Files:**
- Modify: `RECIPES.md`, `UAT.md`, `CHANGELOG.md`, `docs/licensing.md`, `TODO.md`, `docs/superpowers/specs/2026-11-21-alternative-handlers-design.md` (one line under "OnAir" pointing at the note for what differs), `docs/superpowers/specs/2026-11-22-onair-bus-modes-design.md` (status line: approved 2026-11-22)
- Create in `onair-rx`: `UPSTREAM.md`

- [ ] **Step 1: `RECIPES.md`:** a recipe "A second opinion on a decoder": `modes_show <id>` for the handlers, `job_start` with `handler: "onair"` beside a running job of the same mode, what the wide input costs (Task 6's numbers), and capturing AIS or APRS frames with `needs: ["packet"]` (Wireshark reads AIS as NMEA over UDP port 10110 and APRS as AX.25 with no dissector of ours).
- [ ] **Step 2: `UAT.md` "Plan 45":** 45.1 `handlers list` on the field host shows OnAir `ready` for every mode of this plan after the new `onair-rx` is installed; 45.2 an AIS job with `--handler onair --needs packet` beside the node's own AIS job on live traffic: both give events for the same vessels, the pcapng opens in Wireshark as NMEA; 45.3 the same for APRS, as AX.25; 45.4 a POCSAG or ACARS job by OnAir on live traffic gives events with the row's fields; 45.5 the node's CPU with and without the OnAir sibling, written down.
- [ ] **Step 3: `CHANGELOG.md` Unreleased** (the modes OnAir now serves; the `const` key; `onair-rx` 0.2.0 needed, an older wrapper fails those jobs at launch with `no mode`); `docs/licensing.md` (the carried patch is the operator's build of OnAir, nothing of it is in this repository); `TODO.md`: replace the "Plan 45" gist with gists for plan 46 (RDS, DMR, P25, M17, DVB-T, analog video, plus any mode Tasks 4 and 7 moved) and plan 47 (`decoder: null` rows and the `policy` placeholder), and one for the two offers to OnAir.
- [ ] **Step 4: `UPSTREAM.md` in `onair-rx`:** the two offers as the operator would send them: the sink (the patch of Task 1 with its test) and receivers passing sample time to `publish` (described, with the call sites listed; not written here). OnAir takes focused pull requests with a test and a DCO sign-off, from the operator's account.
- [ ] **Step 5: The gates,** with a private `CARGO_TARGET_DIR`: `cargo fmt -p zeta-core -p zeta-ext -p zeta -- --check`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`, once with `ZETA_TEST_ONAIR_RX` set and once without (the second prints the `skipped` line); `cargo run -q -p zeta -- agents-md --check` and `--skill --check`; `bash scripts/public-check.sh`; `bash scripts/check-doc-links.sh`. In `onair-rx`: `test/patch.sh`, `test/run.sh`, `test/bus.sh`. Expected: all clean. Delete the private target directory.
- [ ] **Step 6: Commit** here (`OnAir modes: recipes, UAT 45, changelog and the plans that follow`) and in `onair-rx` (`onair-rx: what to offer upstream`).
