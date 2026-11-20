# Alternative Handlers, Plan 42: Handler Files, the Policy, Packets from Programs

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A mode can be decoded by any of several handlers (its row's own decoder, or a program described by a handler file), a policy picks one, and a program's frames reach plan 41's packet capture; accepted by capturing live aircraft from dump1090 and from OnAir.

**Architecture:** A handler file's entry for a mode is a program block, so the existing `external_decode` stage runs it; the mode registry holds each mode's handlers and one pure function picks among them. The external stage gains a second, binary output read by a `pcap` or `beast` parser into `Payload::Packet`. `job_start` names a handler or leaves it to the policy; the job record, every event and every packet capture say which handler ran.

**Tech Stack:** Rust 1.97 (zeta-core, zeta-ext, zeta, zeta-ws, zeta-mcp, zeta-agent), the plain-JS page, Python fakes for program tests, C++ for the out-of-repo `onair-rx` wrapper.

**Spec:** `docs/superpowers/specs/2026-11-21-alternative-handlers-design.md` (sections "Handlers", "Packets from programs", "Choosing a handler", "OnAir" batch 1). Plans 43 to 45 take Send to, stored captures and OnAir's other modes.

## Global Constraints

- One operation set: every new verb or field is in `crates/zeta-core/src/ops.rs` and applied by `Node::apply`; transports only translate.
- `AGENTS.md` and `skills/zetasdr/SKILL.md` are generated: prose goes in `crates/zeta/src/agents_md.rs` or `docs/agents/field-craft.md`, then regenerate; never hand-edit.
- A node with no handler files and rows without `packets` behaves exactly as before: every existing test passes unchanged except where a task says otherwise.
- "Handler" names the new concept in ops (`handler`, `handlers`). The row key `decoder.handlers` keeps its spelling and meaning; prose calls those "helper programs".
- Handler ids: `builtin` for a graph or frame row's own decoder; a program row's own is its `programs[0]`. A handler file's `id` matches `^[a-z0-9][a-z0-9_-]*$` and may not be `builtin`.
- Private link types: `user0` = 147 (frame rows, unchanged), `mode_s` = 148. Registered numbers pass through as written.
- A packet from a program carries the provenance the program's events carry (`last_prov`, the latest input frame; wall-clock before the first frame).
- Packets are built only while `receiver_count > 0`; the packet stream's framing is still consumed.
- A failed or lost packet output never fails the job: note `{"external": {"packets": "<reason>"}}` in the job's context.
- Probing never runs a program: it looks names up (`modes/handlers.rs` `find_handler`).
- No GPL code or program enters the repository. `docs/wireshark/zetasdr.lua` is written from the Mode S format. `onair-rx` lives outside the repository.
- Public repository: roles not host names, no observed traffic, no real aircraft addresses; fixtures use synthesised frames. `scripts/public-check.sh` and `scripts/check-doc-links.sh` clean before the branch lands.
- Gates per `CLAUDE.md`: `cargo fmt` per crate (nested worktree), clippy `-D warnings`, a private `CARGO_TARGET_DIR`. Per task run only its tests and neighbours; crate suites once per batch; workspace and UI suites once at the end.
- Commit messages: `Handlers: <what changed>`.

## Decisions beyond the spec

1. `decoder: null` rows are plan 45's (the modes only OnAir decodes); not built here.
2. `needs` is a filter before the ranking (spec, "Choosing a handler").
3. The ranking is kept in `<state_dir>/handler-policy.json`: `{"modes": {"<mode>": ["<id>", ...]}, "all": ["<id>", ...]}`.
4. A same-kind, same-band start with a different handler is not a duplicate (`duplicate_of` compares the resolved handler). Grouping is plan 43's.
5. `modes check` checks the row's own handler by default and `--handler <id>` another; `checked` is kept per `mode` and per `mode@handler`.
6. The page changes only where it would otherwise mislead: the capture entry reads the job's own handler's outputs, and a job row names its handler when its mode has more than one.
7. `onair-rx` emits one JSON line per decoded Mode S frame carrying the aircraft's current state.

## Review Focus

- A handler file naming a mode the node lacks, or whose `output.fields` omit a field the row's event schema requires: that entry is broken with a reason naming it; the file's other entries and the node still load. (Task 4)
- A pcap stream that ends mid-record, or whose header is not pcap: packets up to the last whole record are kept, the context notes the reason, events keep flowing. (Tasks 2, 3)
- The handler a job was started with is uninstalled before a restart: a named handler fails the job with the probe's reason; a chosen one is re-chosen and the context says so. (Task 6)
- A ranking that names an id no mode has, or an `unavailable` handler first: unknown ids are an error at `handler_policy_set`; an unavailable one is passed over at start, never an error. (Task 5)
- A capture attached to a job whose handler outputs no packets although the mode's `outputs` lists `packet`: refused naming the handler and the handlers that would give them. (Task 6)

---

### Task 1: `LinkType` for any link-layer number

**Files:**
- Modify: `crates/zeta-core/src/stream.rs` (LinkType), `crates/zeta-core/src/pcapng.rs`, `crates/zeta-core/src/packets.rs`, `crates/zeta-core/src/packet_capture.rs`, `crates/zeta-core/src/stream_in.rs`, `crates/zeta-plugin/src/wire.rs`
- Test: unit tests in `stream.rs`; `crates/zeta-core/tests/attach.rs`

**Interfaces:**
- Produces: `LinkType` stays `Copy + Eq`, gains variants `Ax25` (3), `ModeS` (148), `Other(u16)`; `LinkType::number(self) -> u16`; `LinkType::from_number(n: u16) -> LinkType` (a named variant when `n` is one, else `Other(n)`; `Other` never holds a named number). JSON: the named ones as today (`"ax25_kiss"`, `"user0"`, `"raw_ip"`, `"ax25"`, `"mode_s"`), any other as `{"other": <n>}`.

- [ ] **Step 1: Write the failing tests** in `stream.rs`:

```rust
#[test]
fn link_types_round_trip_by_number_and_json() {
    for (lt, n, json) in [
        (LinkType::Ax25Kiss, 202, r#""ax25_kiss""#),
        (LinkType::User0, 147, r#""user0""#),
        (LinkType::RawIp, 101, r#""raw_ip""#),
        (LinkType::Ax25, 3, r#""ax25""#),
        (LinkType::ModeS, 148, r#""mode_s""#),
        (LinkType::Other(195), 195, r#"{"other":195}"#),
    ] {
        assert_eq!(lt.number(), n);
        assert_eq!(LinkType::from_number(n), lt);
        assert_eq!(serde_json::to_string(&lt).unwrap(), json);
        assert_eq!(serde_json::from_str::<LinkType>(json).unwrap(), lt);
    }
    // A named number never deserialises into `other`.
    assert!(serde_json::from_str::<LinkType>(r#"{"other":148}"#).is_err());
}
```

- [ ] **Step 2: Run** `cargo test -p zeta-core --lib link_types_round_trip` — FAIL (no `Ax25`).
- [ ] **Step 3: Implement** the variants, `from_number`, and a `Deserialize` that rejects `other` holding a named number. Fix every exhaustive match (`pcapng.rs`, `packet_capture.rs`, `stream_in.rs`, `wire.rs`); a capture file still holds one link type.
- [ ] **Step 4: Add to `attach.rs`** `a_capture_of_another_link_type_opens_in_its_own_file`: publish two `Packet` frames of `Other(195)` then one of `ModeS` on a packet stream under capture; assert two files, link types `Other(195)` and `ModeS`, and each file's IDB link type number (bytes 8..10 of the IDB body) 195 and 148.
- [ ] **Step 5: Run** `cargo test -p zeta-core --lib stream:: && cargo test -p zeta-core --test attach` — PASS.
- [ ] **Step 6: Commit** `Handlers: a packet's link type is any link-layer number`.

### Task 2: The `pcap` and `beast` packet parsers

**Files:**
- Create: `crates/zeta-ext/src/packet_parsers/mod.rs`, `pcap.rs`, `beast.rs`
- Modify: `crates/zeta-ext/src/lib.rs`, `crates/zeta-core/src/modes/row.rs` (`PacketParserKind` only)

**Interfaces:**
- Produces:

```rust
/// One packet cut from a program's binary output.
pub struct Parsed { pub link_type: LinkType, pub data: Vec<u8> }
/// Why a stream stopped being readable.
pub struct StreamError(pub String);
/// An incremental parser: feed bytes as they arrive, take whole packets out.
pub trait PacketParser: Send {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<Parsed>, StreamError>;
}
pub fn parser(kind: PacketParserKind, link_types: &BTreeMap<u16, LinkType>) -> Box<dyn PacketParser>;
```

  `PacketParserKind { Pcap, Beast }` (serde `snake_case`, `JsonSchema`) is created by this task in `crates/zeta-core/src/modes/row.rs` and re-exported from `zeta_core::modes`; Task 3 uses it in the row schema.

- [ ] **Step 1: Write the failing tests.** In `pcap.rs`:
  - `a_pcap_stream_fed_byte_by_byte_gives_each_record`: a little-endian classic header (magic `0xa1b2c3d4`, link type 147) and two records of 14 and 7 bytes, pushed one byte at a time, give exactly two `Parsed` with those bytes, link type `User0`.
  - `link_types_map_what_the_program_writes`: the same with map `{147: ModeS}` gives `ModeS`.
  - `a_big_endian_and_a_nanosecond_header_are_read`: magics `0xd4c3b2a1` (swapped) and `0xa1b23c4d`.
  - `a_stream_that_is_not_pcap_is_an_error`: first four bytes `b"OFF\n"` → `Err`, message `not a pcap stream (magic 4f46460a)`.
  - `a_record_longer_than_the_snap_length_is_an_error`: `incl_len` 300000 with header snaplen 262144 → `Err`.
  In `beast.rs`:
  - `beast_frames_become_mode_s_packets`: `1a 33 <6 ts> <1 sig> <14 bytes>` and `1a 32 <6 ts> <1 sig> <7 bytes>` give two `ModeS` packets of 14 and 7 bytes; a `1a 31` (Mode A/C, 2 bytes) frame between them is consumed and gives none.
  - `an_escaped_1a_in_a_frame_is_unescaped`: a data byte `1a` written `1a 1a` is one byte out.
  - `garbage_before_a_frame_is_skipped`: bytes with no `1a 3x` are dropped without error; the next whole frame is returned.
- [ ] **Step 2: Run** `cargo test -p zeta-ext --lib packet_parsers` — FAIL.
- [ ] **Step 3: Implement** both parsers (each keeps a `Vec<u8>` of unconsumed bytes; `beast` never errors).
- [ ] **Step 4: Run** the same command — PASS.
- [ ] **Step 5: Commit** `Handlers: pcap and Beast streams parse into packets`.

### Task 3: A program block's `packets` output

**Files:**
- Modify: `crates/zeta-core/src/modes/row.rs` (`ProgramBlock`, validation), `crates/zeta-core/src/modes/template.rs` (`outputs`), `crates/zeta-ext/src/external.rs`, `crates/zeta-ext/tests/fake_lines.py`
- Test: `crates/zeta-core/src/modes/row.rs` unit tests, `crates/zeta-ext/tests/external.rs`

**Interfaces:**
- Consumes: Task 1 `LinkType`, Task 2 `packet_parsers::parser`.
- Produces: `ProgramBlock.packets: Option<ProgramPackets>` (`#[serde(default)]`, `null` in a row file when absent):

```rust
pub struct ProgramPackets {
    pub via: OutputVia,              // tcp or stdout; not the via `output` uses when that is stdout
    pub port: Option<String>,        // "$packet_port" with via tcp
    pub parser: PacketParserKind,    // pcap | beast
    #[serde(default)]
    pub link_types: BTreeMap<String, LinkType>,   // keys are decimal link-type numbers
}
```

  `args` substitute `$packet_port` (a second free loopback port, picked when `packets.via` is tcp or an arg names it). The `external_decode` stage's spec outputs become `[Event, Packet]`.

- [ ] **Step 1: Write the failing row tests** in `row.rs`:
  - `a_packets_block_round_trips_and_defaults_to_none`.
  - `packets_on_stdout_beside_events_on_stdout_is_refused`: error field `decoder.packets.via`, message `stdout already carries the events`.
  - `packets_via_tcp_needs_the_packet_port`: `port` not `"$packet_port"` → field `decoder.packets.port`.
  - `a_link_types_key_that_is_not_a_number_is_refused`: key `"usb"` → field `decoder.packets.link_types.usb`.
  And in `template.rs`: `a_program_row_with_packets_outputs_events_and_packets` (`outputs` gives `[Event, Packet]`; without the block, `[Event]` as today).
- [ ] **Step 2: Run** `cargo test -p zeta-core --lib modes::` — FAIL.
- [ ] **Step 3: Implement** the schema, validation and `outputs`.
- [ ] **Step 4: Extend `fake_lines.py`** with `--pcap-port PORT --pcap-file PATH`: listen on the port, on connect write the file's bytes in 5-byte chunks, 1 ms apart, and keep the socket open; and `--pcap-refuse` (never listen).
- [ ] **Step 5: Write the failing stage tests** in `tests/external.rs` (rows built in the test from the existing fake-lines row helper, with a `packets` block):
  - `a_programs_packets_reach_its_packet_output`: with a subscriber on the job's packet stream, three records in the file arrive as three `Payload::Packet` frames with the mapped link type, each frame's `provenance.span` equal to the span of an input frame the stage was fed.
  - `packets_are_not_built_with_nobody_reading`: no subscriber for 300 ms, then subscribe: only records written after the subscription arrive (the fake writes one record every 100 ms with `--pcap-interval-ms 100`; add that flag).
  - `a_packet_port_that_never_opens_is_noted_and_events_go_on`: `--pcap-refuse`; the job stays running, events arrive, and its context reads `external.packets` = `onair-fake did not open its packet port` within 6 s.
  - `a_stream_that_is_not_pcap_is_noted_once`: the file holds `OFF\n`; context `external.packets` starts `not a pcap stream`.
- [ ] **Step 6: Implement** in `external.rs`: pick `$packet_port`; after the event output is connected, a task connects the packet output (same retry as `connect`, 5 s cap, not fatal), reads chunks, pushes them through the parser, and publishes each `Parsed` as `Payload::Packet` on the packet output with `last_prov` when `receiver_count > 0`; it ends with the run.
- [ ] **Step 7: Run** `cargo test -p zeta-ext --test external` and `cargo test -p zeta-core --lib modes::` — PASS.
- [ ] **Step 8: Commit** `Handlers: a program block can give packets beside its events`.

### Task 4: Handler files and each mode's handlers

**Files:**
- Create: `crates/zeta-core/src/modes/handler_file.rs`, `handlers/` (repo root, empty but for a `README.md` stating the schema in ten lines)
- Modify: `crates/zeta-core/src/modes/mod.rs` (registry), `crates/zeta-core/src/modes/handlers.rs` (probe lookups), `crates/zeta/src/node_setup.rs` (probing, `--handlers-dir`), `crates/zeta/src/cli.rs`, `crates/zeta-core/src/node/mod.rs` (`NodeConfig`)
- Test: `crates/zeta-core/tests/modes.rs`, unit tests in `handler_file.rs`

**Interfaces:**
- Produces:

```rust
// handler_file.rs
pub struct HandlerFile { pub id: String, pub name: String, pub programs: Vec<String>,
                         pub install: Install, pub modes: BTreeMap<String, HandlerEntry> }
pub struct HandlerEntry { #[serde(flatten)] pub block: HandlerBlockSpec, pub extra: BTreeMap<String, FieldPath> }
// HandlerBlockSpec: args, input, output, packets, demod — ProgramBlock without programs/install.
impl HandlerEntry { pub fn program_block(&self, file: &HandlerFile) -> ProgramBlock }

// mod.rs
pub struct ModeHandler {
    pub id: String, pub name: String, pub in_process: bool,
    pub outputs: Vec<StreamKind>,
    pub state: ModeState,                 // Ready | Unavailable | Broken
    pub reason: Option<String>, pub program: Option<PathBuf>,
    pub missing: Vec<String>, pub checked: Option<u64>,
    pub file: String,
}
impl LoadedMode { pub handlers: Vec<ModeHandler> }   // [0] is the row's own
impl ModeRegistry {
    pub fn load_with(stages: &StageRegistry, modes_dir: Option<&Path>, handlers_dir: Option<&Path>) -> ModeRegistry;
    pub fn handler(&self, mode: &str, handler: &str) -> Option<&ModeHandler>;
    /// The program block that runs `handler` for `mode`: the row's own, or a file's entry.
    pub fn program_block(&self, mode: &str, handler: &str) -> Option<ProgramBlock>;
    pub fn set_handler_program(&mut self, mode: &str, handler: &str, program: Option<PathBuf>, reason: Option<String>);
    pub fn handler_files(&self) -> &[LoadedHandlerFile];   // each with file, state, reason, per-mode reasons
}
```

  `ModeRegistry::load` keeps its signature and calls `load_with(.., None)`. Embedded files come from an `EMBEDDED_HANDLERS` array beside `EMBEDDED` (empty until Task 8). `LoadedMode.state`/`program`/`reason`/`missing`/`checked` keep meaning the row's own handler in the registry; the union semantics are `ModeInfo`'s, Task 5.

- [ ] **Step 1: Write the failing tests** in `tests/modes.rs` (a temp `--handlers-dir`; the mode is the embedded `adsb_decode`; the entry copies the row's own `input`/`output` so validation has a known-good block):
  - `a_handler_file_adds_a_handler_to_each_mode_it_lists`: `adsb_decode`'s handlers are `["dump1090", "alt"]`, `alt.in_process == false`, `alt.outputs == [Event]`, and with a `packets` block `[Event, Packet]`; `aprs_decode`'s are `["builtin"]` with `in_process == true`.
  - `an_entry_for_an_unknown_mode_breaks_only_that_entry`: `handler_files()[0]` lists `no_such_mode: no mode of that id`, the `adsb_decode` entry still loads.
  - `an_entry_missing_a_required_event_field_is_broken`: `output.fields` without the row's subject key `icao` → that handler's `state == Broken`, reason `output.fields: no mapping for icao (the row's subject key)`.
  - `an_extra_field_may_not_shadow_a_row_field`: `extra: {"callsign": "/x"}` → reason `extra.callsign: the row's event schema already has it`.
  - `an_id_of_builtin_or_of_the_rows_own_program_is_refused`: file id `builtin` → the file is broken; file id `dump1090` → its `adsb_decode` entry is broken, reason `id dump1090 is the row's own handler`.
  - `an_operator_file_replaces_the_embedded_one_of_its_id` (uses a registry built with two in-memory files of one id via a test-only `load_handler_texts`).
  - `a_broken_handler_file_never_fails_the_load`: a file of `{` is listed `Broken` with the parse error; rows load.
- [ ] **Step 2: Run** `cargo test -p zeta-core --test modes` — FAIL.
- [ ] **Step 3: Implement** `handler_file.rs`, the registry's loading and validation (reuse `ProgramBlock`'s validation through `program_block`), and `ModeHandler` for the row's own decoder (`builtin`, or `programs[0]`).
- [ ] **Step 4: Probing.** In `node_setup.rs` probe each handler file's `programs` once (existing `find_handler`, so `ZETA_HANDLER_<PROGRAM>` works) and call `set_handler_program` for each of its modes; a missing program makes those handlers `Unavailable` with the existing reason wording and prints one `zeta: handlers:` line per file. Add `--handlers-dir <PATH>` (default: `handlers` beside the default modes directory) to the commands that take `--modes-dir`. Test in `crates/zeta/tests/cli.rs`: `a_handler_files_missing_program_is_one_startup_line` (file names program `zeta-no-such-handler`; `serve`'s startup lines hold exactly one line containing `zeta-no-such-handler: missing`).
- [ ] **Step 5: Run** `cargo test -p zeta-core --test modes && cargo test -p zeta --test cli a_handler_files_missing_program` — PASS.
- [ ] **Step 6: Commit** `Handlers: a handler file adds a program to the modes it lists`.

### Task 5: The policy, its ops, and `modes_list`

**Files:**
- Create: `crates/zeta-core/src/modes/policy.rs`
- Modify: `crates/zeta-core/src/ops.rs` (`Op`, `OpResult`, `ModeInfo`), `crates/zeta-core/src/node/mod.rs`, `crates/zeta-core/src/node/persist.rs`
- Test: unit tests in `policy.rs`; `crates/zeta-core/tests/modes.rs`

**Interfaces:**
- Consumes: Task 4 `ModeHandler`.
- Produces:

```rust
// policy.rs
#[derive(Default, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct HandlerPolicy { pub modes: BTreeMap<String, Vec<String>>, pub all: Vec<String> }
pub struct Passed { pub id: String, pub why: String }
/// The handler a start of `mode` gets: among `handlers`, those ready and giving every
/// kind of `needs`, ordered by the mode's ranking, the node-wide ranking, in-process
/// first, then `handlers`' own order.
pub fn pick<'a>(mode: &str, handlers: &'a [ModeHandler], needs: &[StreamKind], policy: &HandlerPolicy)
    -> Result<&'a ModeHandler, Vec<Passed>>;

// ops.rs
Op::HandlerPolicySet { mode: Option<ModeId>, order: Vec<String> }   // empty order clears
Op::HandlerPolicyGet {}
OpResult::HandlerPolicy(HandlerPolicy)
ModeInfo { handlers: Vec<ModeHandlerInfo>, handler: Option<String>, .. }
OpError::NoHandler { mode: String, passed: Vec<{id, why}> }
```

  `Node::handler_policy() -> HandlerPolicy`; the file is `handler-policy.json`, written before memory changes (as `station.json`), read at `Node::open`, a malformed file read as empty with one warning line.

- [ ] **Step 1: Write the failing unit tests** in `policy.rs` (handlers built by a `h(id, in_process, ready, outputs)` helper):
  - `the_rows_own_handler_wins_with_no_ranking`: `[dump1090, onair]`, both program, ready → `dump1090`.
  - `an_in_process_handler_beats_a_program`: `[extprog, builtin]` → `builtin`.
  - `the_modes_ranking_leads_then_the_node_wide_one`: `modes: {adsb_decode: [onair]}` → `onair`; `all: [onair]` alone → `onair`; mode ranking `[dump1090]` with `all: [onair]` → `dump1090`.
  - `an_unavailable_ranked_handler_is_passed_over`: ranking `[onair]`, `onair` not ready → `dump1090`.
  - `needs_filters_before_the_ranking`: ranking `[dump1090]`, `needs: [Packet]`, only `onair` gives packets → `onair`.
  - `nothing_left_names_each_handler_and_why`: `needs: [Packet]`, none gives it → `Err` with `why` `outputs no packet` for a ready one and the handler's `reason` for an unavailable one.
- [ ] **Step 2: Run** `cargo test -p zeta-core --lib modes::policy` — FAIL. **Step 3: Implement `pick`.** **Step 4:** PASS.
- [ ] **Step 5: Write the failing node tests** in `tests/modes.rs`:
  - `handler_policy_set_persists_and_get_returns_it`: set `{mode: adsb_decode, order: [alt]}`, reopen the node on the state directory, `handler_policy_get` returns it; `order: []` clears the entry.
  - `handler_policy_set_refuses_an_id_no_mode_has`: `order: ["nope"]` → `OpError::Unknown { field: "order", value: "nope", known: <every handler id> }`.
  - `modes_list_gives_each_modes_handlers_and_the_policys_pick`: `adsb_decode` with the row's program unavailable and `alt` ready reads `state: ready`, `handler: "alt"`, `program` = alt's path, `outputs` = alt's; `handlers[0].state == unavailable` with dump1090's reason. With no handler ready: `state: unavailable`, `handler: null`, `program`/`reason` the row's own.
  - `a_node_with_no_handler_files_lists_modes_as_before`: for every embedded row, `ModeInfo` minus `handlers`/`handler` equals a snapshot taken through the pre-change code path (compare `state`, `reason`, `program`, `missing`, `outputs`, `checked` against `LoadedMode`'s).
- [ ] **Step 6: Implement** the ops, persistence and `ModeInfo` (the union rule of the spec's "Handlers" section; `modes_show` gives the same plus each handler file entry's `extra` names).
- [ ] **Step 7: Run** `cargo test -p zeta-core --test modes --lib modes::` — PASS.
- [ ] **Step 8: Commit** `Handlers: a policy picks a mode's handler, and modes_list shows them`.

### Task 6: Starting a job with a handler

**Files:**
- Modify: `crates/zeta-core/src/ops.rs` (`JobStart`, `expand`), `crates/zeta-core/src/job.rs` (`JobRecord`), `crates/zeta-core/src/modes/template.rs` (`expand`), `crates/zeta-core/src/modes/summary.rs` (`envelope`), `crates/zeta-core/src/node/{mod,launch,recover,retune}.rs`, `crates/zeta-ext/src/external.rs` (`ExternalParams`), `crates/zeta-core/src/packets.rs`, `crates/zeta-core/src/packet_capture.rs`
- Test: `crates/zeta-ext/tests/external.rs`, `crates/zeta-core/tests/{jobs,resume,attach,events_log}.rs`

**Interfaces:**
- Consumes: Task 4 `program_block`, Task 5 `pick`.
- Produces:
  - `Op::JobStart { handler: Option<String>, needs: Vec<StreamKind>, .. }` (both `#[serde(default)]`; `needs` may hold `event`, `packet`, `audio` only).
  - `JobRecord { handler: Option<String>, handler_named: bool, .. }` (`#[serde(default)]`; `None` for kinds that are not mode rows).
  - `template::expand(row, handler: &ModeHandler, block: Option<&ProgramBlock>, tuning, out_rate_hz, audio_policy)`; a non-own handler expands as a program row does, the `external_decode` node's params `{"mode": <id>, "handler": <id>}`.
  - `ExternalParams { mode: String, #[serde(default)] handler: Option<String> }`; the stage takes its block from `ModeRegistry::program_block`.
  - `summary::envelope(row, handler: &str, start_ns, fields)` inserts `"handler"` in every event (in-process rows' stages pass `"builtin"`).
  - `PacketCaptureRecord.handler: Option<String>`, `NewPacketCapture.handler`; the packet comment reads `job=<id> handler=<id> source=<id> freq_hz=<hz>` (no `handler=` when `None`).

- [ ] **Step 1: Write the failing tests.**
  - `external.rs` `a_handler_files_program_runs_for_the_rows_mode`: a registry with the fake-lines row and a handler file `alt` whose entry runs `fake_lines.py` with other lines; a job expanded with handler `alt` emits the alt lines' events, each with `handler == "alt"`; with the row's own, `handler == <programs[0]>`.
  - `jobs.rs` `job_start_takes_the_policys_handler_and_records_it`: no `handler` → record `handler: Some(<own>)`, `handler_named: false`; `handler: "alt"` → `Some("alt")`, `true`.
  - `jobs.rs` `a_named_handler_that_is_unavailable_is_refused_with_its_reason`: `OpError::Other` whose message is `adsb_decode: handler alt is unavailable: <reason>`; `an_unknown_handler_names_the_known_ones`: `OpError::Unknown { field: "handler", known: [...] }`.
  - `jobs.rs` `needs_packet_with_no_such_handler_is_no_handler`: `OpError::NoHandler` listing each with `outputs no packet`.
  - `jobs.rs` `handler_and_needs_are_refused_on_a_kind_that_is_no_mode_row`: `fm_demod` with `handler: "x"` → `OpError::Other`, message `fm_demod has no handlers`.
  - `jobs.rs` `the_same_mode_on_the_same_band_with_another_handler_is_not_a_duplicate`: second start with `handler: "alt"` gives a second job id; a second start with the same handler gives the first's duplicate answer as today.
  - `resume.rs` `a_chosen_handler_gone_at_restart_is_chosen_again_and_noted`: start by policy with `alt` ranked first, reopen with `alt` unavailable: the job runs on the row's own, `context.handler` = `{"was": "alt", "now": "<own>", "why": "<reason>"}`; `a_named_handler_gone_at_restart_fails_the_job`: state `failed`, reason `handler alt is unavailable: <reason>`.
  - `attach.rs` `a_capture_of_a_job_whose_handler_gives_no_packets_names_who_would`: mode outputs list `packet` through `alt` only; `job_attach packets` to a job on the own handler → message `job <id> has no packet stream: handler <own> outputs none (alt does)`.
  - `attach.rs` `a_capture_records_its_jobs_handler`: `packets_list` entry `handler == "alt"`; the first packet's `opt_comment` contains `handler=alt`.
  - `events_log.rs` `an_events_line_carries_its_handler`: an `aprs_decode` job's logged event has `event.handler == "builtin"`.
- [ ] **Step 2: Run** the named tests — FAIL.
- [ ] **Step 3: Implement** start (resolve handler under the source's gate with the registry in force; `duplicate_of` gains the handler), expansion, the record, relaunch (`recover.rs`, `retune.rs` reuse one `resolve_for_relaunch(record) -> Result<String, String>`), the envelope and the capture record.
- [ ] **Step 4: Run** `cargo test -p zeta-core --test jobs --test resume --test attach --test events_log && cargo test -p zeta-ext --test external` — PASS. Run `cargo test -p zeta-core --features test-fixtures` once for the batch (Tasks 4 to 6).
- [ ] **Step 5: Commit** `Handlers: a job runs the handler it names or the policy's, and says which`.

### Task 7: Transports, the page's two fixes, and the agent's prose

**Files:**
- Modify: `crates/zeta/src/cli.rs`, `crates/zeta/src/main.rs`, `crates/zeta/src/agents_md.rs`, `crates/zeta/src/modes_check.rs`, `crates/zeta-agent/src/agent.rs` (tool list, if enumerated), `docs/agents/field-craft.md`, `crates/zeta-ui/web/actions.js`, `crates/zeta-ui/web/agent.js`, `crates/zeta-ui/tests/e2e/ui.py`, `crates/zeta/tests/ui.rs` (a handler file in the suite's node)
- Regenerate: `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta/tests/cli.rs`, `crates/zeta/tests/modes_check.rs`, the UI suite

**Interfaces:**
- Consumes: Tasks 5 and 6 ops.
- Produces: `zeta job start --handler <ID> --needs <KIND>...`; `zeta handlers list` (one line per mode's handler: mode, id, state, outputs; local, reads the registry), `zeta handlers policy get`, `zeta handlers policy set [--mode <ID>] <HANDLER>...` (no handlers clears); `zeta modes check [--handler <ID>]`.

- [ ] **Step 1: Write the failing CLI tests** in `cli.rs`: `job_start_handler_and_needs_reach_the_op` (`--handler alt --needs packet` → the op JSON has `"handler":"alt","needs":["packet"]`); `handlers_policy_set_and_get_round_trip` against a `serve` node; `handlers_list_names_each_modes_handlers`.
- [ ] **Step 2: Write the failing `modes_check.rs` test** `modes_check_checks_a_named_handler_and_records_it_apart`: with the fake handler, `modes check adsb_decode --handler alt` passes and `modes-checked.json` gains `adsb_decode@alt`; `adsb_decode`'s own `checked` is untouched.
- [ ] **Step 3: Implement** the CLI and `modes check`.
- [ ] **Step 4: Page.** `actions.js` `packets` entry: `applies` reads the job's own handler's outputs (`rowOf(kind).handlers.find(h => h.id === job.handler).outputs`), falling back to the mode's `outputs` when the job has no `handler`. `agent.js` job row: when the job's mode has more than one handler, the row's title line appends the handler's `name`. Add to `ui.py` `scenario_handler_named_on_job_row`: the suite's node (`ui.rs`) gets a `--handlers-dir` holding a file `alt` that serves `aprs_decode` with the fake-lines program and no `packets` (no new source: scenarios count the sources); on the `aprs` source start one job per handler; assert each row's text contains its handler's name, that the `builtin` job offers `Capture packets`, and that the `alt` job does not although the mode's `outputs` lists `packet`.
- [ ] **Step 5: Prose.** `agents_md.rs`: notes for `job_start.handler`/`needs`, `handler_policy_set`/`get`, `modes_list`'s `handlers`/`handler`, the `handler` on events and captures. `field-craft.md`: a short section "More than one decoder for a mode" (when to name a handler, that `needs: ["packet"]` is how a capture is asked for, that the row's `decoder.handlers` are helper programs).
- [ ] **Step 6: Run** `cargo test -p zeta --test cli --test modes_check`, regenerate both generated files, `cargo run -q -p zeta -- agents-md --check && cargo run -q -p zeta -- agents-md --skill --check`, and `ZETA_UI_ONLY=handler_named_on_job_row,packet_capture cargo test -p zeta --test ui -- --nocapture` (check for no `ui e2e skipped`).
- [ ] **Step 7: Commit** `Handlers: CLI, the page's capture entry and the agent's prose`.

### Task 8: ADS-B by Beast, `handlers/onair.json`, the Mode S dissector

**Files:**
- Modify: `modes/adsb_decode.json`, `crates/zeta-ext/tests/fake_dump1090.py`, `crates/zeta-core/src/modes/mod.rs` (`EMBEDDED_HANDLERS`), `RECIPES.md`, `UAT.md`, `CHANGELOG.md`, `TODO.md`, `docs/licensing.md`
- Create: `handlers/onair.json`, `docs/wireshark/zetasdr.lua`
- Test: `crates/zeta-ext/tests/adsb.rs`, `crates/zeta-core/tests/modes.rs`, `crates/zeta/tests/modes_check.rs`

**Interfaces:**
- Consumes: everything above.
- Produces: the ADS-B row's `packets`: `{"via": "tcp", "port": "$packet_port", "parser": "beast", "link_types": {}}` with `--net-bo-port $packet_port` in place of `--net-bo-port 0`. `handlers/onair.json`: id `onair`, name `OnAir`, programs `["onair-rx"]`, one mode `adsb_decode` with args `["--mode", "adsb", "--rate", "$rate_hz", "--format", "cs16", "--pcap-port", "$packet_port"]`, input `iq`/`cs16`/2400000/`stdin`, output `stdout`/`json_lines` mapping the row's fields from same-named members (`/icao`, `/callsign`, `/altitude_ft`, `/lat`, `/lon`, `/ground_speed_kt`, `/track_deg`, `/squawk`, `/on_ground`, `/msg`), packets `tcp`/`$packet_port`/`pcap`/`{"147": "mode_s"}`, extra `df` ← `/df`, `tc` ← `/tc`, `level_dbfs` ← `/level_dbfs`.

- [ ] **Step 1: Extend `fake_dump1090.py`** to serve Beast on `--net-bo-port`: for each SBS line it sends, one `1a 33` frame carrying a fixed 14-byte DF17 frame for that line's address (synthesised addresses only).
- [ ] **Step 2: Write the failing tests.** `adsb.rs` `dump1090s_beast_frames_are_mode_s_packets`: a subscriber on the job's packet stream gets `ModeS` packets of 14 bytes whose bytes 1..4 are the fake's address. `modes.rs` `the_embedded_onair_handler_serves_adsb`: handlers of `adsb_decode` are `["dump1090", "onair"]`, `onair.outputs == [Event, Packet]`, state `Unavailable` with `onair-rx: missing` when not installed, never `Broken`. `modes_check.rs`: the existing ADS-B check still passes.
- [ ] **Step 3: Implement** the row change, `handlers/onair.json`, `EMBEDDED_HANDLERS`.
- [ ] **Step 4: Write `docs/wireshark/zetasdr.lua`**: a `zetasdr_modes` protocol registered on `wtap.USER1`, fields downlink format (top 5 bits of byte 0), capability (low 3 bits, DF11/17/18), address (bytes 1..3 for DF11/17/18), ME (bytes 4..10) and type code (top 5 bits of byte 4) for DF17/18, parity (last 3 bytes); the info column reads `DF<n> <address or "(address in parity)">`. Check it with tshark on a capture written by the Step 2 test (keep the file under `$CARGO_TARGET_DIR`): `tshark -X lua_script:docs/wireshark/zetasdr.lua -r <file> -T fields -e zetasdr_modes.df -e zetasdr_modes.aa` prints `17` and the fake's address for each packet. Record the command and its output in the task report; skip with a note if `tshark` is absent.
- [ ] **Step 5: Docs.** `RECIPES.md`: capturing aircraft frames (dump1090 or OnAir, `--needs packet`, the Lua file's install line, `DLT_USER 148`). `UAT.md` "Plan 42": 42.1 `handlers list` on the field host shows dump1090 `ready` and onair `ready` once installed; 42.2 an ADS-B job by policy with a capture attached: the pcapng opens as Mode S with the dissector, times match the events; 42.3 the same with `--handler onair`, both jobs running at once on one band; 42.4 `handlers policy set --mode adsb_decode onair`, then a plain start runs OnAir; 42.5 with `onair-rx` moved away and the node restarted, the policy-started job runs on dump1090 and its context says why. `CHANGELOG.md` Unreleased (including: events gain `handler`; `ModeInfo` gains `handlers`/`handler`; peers older than this refuse link types they do not know). `docs/licensing.md`: OnAir beside the other external handlers. `TODO.md`: gists for plans 43 to 45 and for Beast sample-time stamping.
- [ ] **Step 6: Run** `cargo test -p zeta-ext --test adsb && cargo test -p zeta-core --test modes && cargo test -p zeta --test modes_check`, `bash scripts/check-doc-links.sh` — PASS.
- [ ] **Step 7: Commit** `Handlers: aircraft frames from dump1090's Beast output, and the OnAir handler file`.

### Task 9: `onair-rx` for ADS-B (outside the repository)

**Files (in the wrapper's own repository, GPL-3.0-or-later; nothing under this repository changes):**
- Create: `src/main.cpp` (from `spike/main.cpp`), `src/adsb.cpp`, `test/run.sh`, `LICENSE`
- Modify: `build.sh`, `README.md`

**Interfaces:**
- Consumes: `handlers/onair.json`'s contract (Task 8).
- Produces: `onair-rx --mode adsb --rate <hz> --format cs16|cf32 [--pcap-port <p>]`: IQ on stdin; one JSON object per decoded Mode S frame on stdout, line-buffered, members `icao` (6 upper-case hex digits), `df`, `tc` (absent when none), `level_dbfs`, `msg` (the SBS message type the frame corresponds to, 1 to 8, absent when none), and the aircraft's current `callsign`, `altitude_ft`, `lat`, `lon`, `ground_speed_kt`, `track_deg`, `squawk`, `on_ground` where known; with `--pcap-port`, listens on 127.0.0.1:`p` and serves one client a classic pcap stream (link type 147) of the frames; an unknown `--mode` exits 2 with `onair-rx: no mode <m>` on stderr; EOF on stdin drains and exits 0. Its `--version` prints the wrapper's and OnAir's versions and the line `Based on OnAir by Kristijonas Adomaitis - https://on-air.dev`.

- [ ] **Step 1: Write `test/run.sh`** (fails first): feed the recorded 1090 MHz capture (`iq/adsb1090.sigmf-data`, cf32, 2457600 Hz; skip with a message when absent) with `--pcap-port`; assert at least 150 JSON lines, every line parses and has a 6-hex-digit `icao`, at least one line has `altitude_ft`; a client on the pcap port receives a valid pcap header with link type 147 and at least 150 records; `--mode nope` exits 2.
- [ ] **Step 2: Implement.** Events from `AdsbReceiver::setFrameCallback` joined with the aircraft table (`telemetry`); the pcap port bridges the hub's live stream as the spike does. Keep the spike's `--lua`.
- [ ] **Step 3: Run** `ONAIR_SRC=<checkout> ./build.sh && test/run.sh` — PASS.
- [ ] **Step 4: Check against the node.** With `onair-rx` on `PATH` and a private state directory: `zeta modes check adsb_decode --handler onair` passes against the row's synthesised capture. If OnAir does not decode the synthesised capture, record why in the report and check instead with a `--source-file` node on the recorded capture: `job start --kind adsb_decode --handler onair --needs packet`, attach `packets`, and confirm events with `handler: "onair"` and a `mode_s` capture that `zetasdr.lua` dissects.
- [ ] **Step 5: Commit** in the wrapper's repository: `onair-rx: ADS-B events and a pcap port`.

---

## After the tasks

Whole-branch review, then: workspace gates (`cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`, the UI suite once), `public-check` and `check-doc-links` clean, rebase onto `main` if it moved. Landing, deploying and UAT 42 are the operator's word.
