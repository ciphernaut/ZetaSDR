# Plan 41: Packet capture for Wireshark (phase A, the stored capture) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `job_attach` of the new kind `packets` to a running APRS or frame-row job writes that job's decoded frames to a pcapng file that opens and dissects in Wireshark, downloadable by id while it is still being written.

**Architecture:** A frame stage (`aprs_decode`, `frame_decode`) gains a second output of the new kind `StreamKind::Packet` carrying `Payload::Packet { link_type, data }`. A `packets` job is an attached job like `transcribe`: `stream_in {kind: packet} -> packet_capture`, where `packet_capture` is a sink stage in `zeta-core` that writes pcapng blocks through a new `PacketStore` (`state_dir/packets`, its own index, a rolling size cap). The store's records are listed and deleted by two new ops and served at `GET /captures/<id>.pcapng`.

**Tech Stack:** Rust (tokio, serde, schemars, `base64` 0.23 already used by `zeta-ws`), a hand-rolled little-endian pcapng writer (no new crate beyond `base64` in `zeta-core`), axum in `zeta-ws`, the page's plain JS and the Playwright suite.

**Spec:** `docs/superpowers/specs/2026-11-19-packet-capture-wireshark-design.md` (all of it; phase B and the `.ts` egress are out of this plan). Executors read both.

## Global Constraints

- A capture holds exactly the frames the decoder under it produced. No new decoder, no decapsulation stage, nothing that widens what a row decodes.
- Phase A does not start Wireshark, watch it or own a socket. It writes a file.
- One operation set: everything operator-facing is an `Op` applied by `Node::apply`; transports only translate.
- A packet is a typed `Payload::Packet`, never hex taken back out of an event.
- The packet carries no timestamp of its own: the pcapng time is the frame's `provenance.span.start_ns` (ns since the UNIX epoch), `if_tsresol = 9`.
- Link types, exact numbers: `LINKTYPE_AX25_KISS = 202` (APRS; each record is `0x00` then the AX.25 frame, no FCS), `LINKTYPE_USER0 = 147` (frame rows), `LINKTYPE_RAW = 101` (reserved, emitted by nothing in this plan). The stage sets the link type; no egress infers it.
- The per-packet comment is exactly `job=<id> source=<id> freq_hz=<centre>`; `job` is the decoder job's id, not the `packets` job's.
- One pcapng is capped at `PACKET_CAPTURE_MAX_BYTES = 256 * 1024 * 1024`; a full file is closed and a fresh artefact id opened.
- A `packets` attachment does not resume across a node restart: the file is kept and served, the job is not relaunched.
- A row with `access: restricted` cannot be packet-captured; a capture record carries its row's `access`.
- A Packet stream is not rendered: no tab or pane shows packets in this plan.
- Fixtures are synthetic, use invented callsigns (`N0CALL`-style, never a real one) and stay under a few MB. Roles, never private host names, addresses, positions or observed frequencies, in code, tests, docs and commit messages. The demo station is Brisbane Airport.
- `AGENTS.md` and `skills/zetasdr/SKILL.md` are generated: after any op, field or note change regenerate both (`cargo run -q -p zeta -- agents-md > AGENTS.md`, `... --skill > skills/zetasdr/SKILL.md`) and commit them in the same commit.
- Tests that read asynchronous state poll for the end state (the suites' `until` helpers); none reads once after an operation.
- Gates per task: `cargo fmt -p <crate> -- --check` for each touched crate (the nested worktree breaks `cargo fmt --all`), `cargo clippy --workspace --all-targets --all-features -- -D warnings`, the task's tests and their neighbours. Use a private `CARGO_TARGET_DIR`. The full gate list in `CLAUDE.md` runs once, in task 9.
- One commit per task on this branch, message in the repository's style (`Area: what changed`), no trailer.

## Decisions this plan makes beyond the spec

The operator reads these with the plan; each is the smallest thing that satisfies the spec.

1. **The Packet output is always registered** on `aprs_decode` and `frame_decode`, not created at attach time. An attach must find the stream on a job that is already running, and the bus drops a frame with no subscriber. To keep "a job with no packet consumer pays nothing", the stage builds a packet only when `bus.receiver_count(stream) > Some(0)`.
2. **A `packets` attachment is an attached job** (`JobKind::Packets`), so it appears in `job_list` with its `input`, follows its input through a retune and ends with it, with no new mechanism. `job_stop` on it is the detach.
3. **No resume = the record is kept, the job is not relaunched.** After a restart the `packets` job is listed `stopped` with the context note `packet capture does not resume: attach again`.
4. **Two new ops**, `packets_list` and `packets_delete`, rather than widening `capture_list`: its `CaptureRecord` is SigMF IQ (rate, bandwidth, classify), and the spec keeps the two models apart.
5. **Retention uses the burst store's policy values** (`BurstPolicy.keep_bytes`, `keep_days`) applied to the packet store on its own: the two stores do not share one byte budget. A file being written is never expired.
6. **`language` is ignored and `stream` must be `null`** for `packets`. One `packets` attachment per input job.
7. **Plugin stages and VRT do not carry `packet`** in this plan (refused by name, as `sink` is).

## Review Focus

1. A node killed mid-write leaves a pcapng cut inside a block: the capture still opens in Wireshark and keeps every whole packet (task 4, `open_trims_a_capture_cut_inside_a_block`).
2. The input job is retuned while it is being captured: the capture goes on, the old file is closed whole, and packets after the retune carry the new `freq_hz` (task 6, `a_retuned_input_is_captured_into_a_fresh_file_with_its_new_frequency`).
3. The state directory cannot be written (full or read-only): the `packets` job ends `failed` naming the file error, and the decoder job keeps running and decoding (task 5, `a_write_error_fails_the_capture_and_not_its_input`).
4. The operator deletes a capture that is still being written: refused naming the job, the file intact (task 6, `deleting_a_capture_being_written_is_refused`).
5. A download names something that is not an id (`../../state.pcapng`, an IQ capture's id, an unknown id): 404, nothing outside the store read (task 7, `only_a_packet_capture_id_is_served`).

---

### Task 1: The packet payload and stream kind

**Files:**
- Modify: `crates/zeta-core/src/types.rs` (`StreamKind`), `crates/zeta-core/src/stream.rs` (`Payload`, `LinkType`, `publish`), `crates/zeta-core/src/stage.rs` (`ALL_KINDS`), `crates/zeta-core/src/ops.rs` (`FrameSummary::from_frame`), `crates/zeta-core/Cargo.toml` (`base64 = "0.23"`)
- Modify: `crates/zeta-plugin/src/wire.rs`, `crates/zeta-plugin/src/host.rs`, `crates/zeta-vrt/src/envelope.rs`, `crates/zeta-client/src/ws.rs`, and every other exhaustive match the compiler names (`zeta-ws`, `zeta-mcp`, `crates/zeta`)
- Test: `crates/zeta-core/src/stream.rs` (unit), `crates/zeta-core/tests/ops_stream.rs`, `crates/zeta-plugin/src/wire.rs` (unit)

**Interfaces:**
- Produces:
  - `StreamKind::Packet`, wire name `packet`.
  - `pub enum LinkType { Ax25Kiss, User0, RawIp }` in `stream.rs`: `Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema`, `#[serde(rename_all = "snake_case")]` (`ax25_kiss`, `user0`, `raw_ip`); `pub fn number(self) -> u16` returning 202, 147, 101.
  - `Payload::Packet { link_type: LinkType, data: Arc<[u8]> }`, `Payload::kind()` → `StreamKind::Packet`.
  - `FrameSummary.payload` for a packet: `{"link_type": "ax25_kiss", "data": "<standard base64, padded>"}`. This is what `stream tail` and the ws stream push send, and what phase B reads.

- [ ] **Step 1: Write the failing tests**

In `stream.rs` tests:

```rust
#[test]
fn link_types_carry_their_registered_numbers() {
    assert_eq!(LinkType::Ax25Kiss.number(), 202);
    assert_eq!(LinkType::User0.number(), 147);
    assert_eq!(LinkType::RawIp.number(), 101);
    assert_eq!(serde_json::to_value(LinkType::Ax25Kiss).unwrap(), "ax25_kiss");
}

#[tokio::test]
async fn the_bus_refuses_an_empty_packet() {
    // register a Packet stream, subscribe, publish data: [] then data: [0, 1]
    // dropped() rises by exactly 1; the subscriber receives only the 2-byte packet
}
```

In `tests/ops_stream.rs`, beside the video summary test:

```rust
#[test]
fn a_packet_frame_summarises_as_link_type_and_base64() {
    // Frame with Payload::Packet { link_type: LinkType::User0, data: [0xde, 0xad, 0xbe, 0xef] }
    assert_eq!(s.kind, StreamKind::Packet);
    assert_eq!(s.payload, serde_json::json!({"link_type": "user0", "data": "3q2+7w=="}));
}
```

In `zeta-plugin/src/wire.rs` tests: writing a `Payload::Packet` frame is `WireError::KindNotCarried(StreamKind::Packet)` at protocol 2; a hello declaring a `packet` input or output is refused by `host.rs` with the existing "it declares a ... protocol N carries ..." message.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p zeta-core --lib stream:: && cargo test -p zeta-core --test ops_stream && cargo test -p zeta-plugin --lib wire::`
Expected: does not compile (`LinkType`, `StreamKind::Packet` unknown).

- [ ] **Step 3: Add the variants and fix every match**

- `publish` drops and counts a `Payload::Packet` whose `data` is empty, beside the audio and video checks.
- `ALL_KINDS` becomes 7 entries, `Packet` last (`passthrough` then carries it).
- `zeta-plugin`: `kind_name` → `"packet"`; reading and writing a packet frame is `KindNotCarried`; the host refuses a hello that declares it, at every protocol.
- `zeta-vrt/src/envelope.rs`: `Payload::Packet { .. } => Err("packet frames are not sent over vrt".to_string())`.
- `zeta-client/src/ws.rs` kind name → `"packet"`.
- `zeta-ws`: a Packet frame goes out as the JSON `FrameSummary` text message (the path events take), never a binary frame.
- Build the workspace with `--all-targets --all-features` and fix each non-exhaustive match the compiler reports the same way: serialise through `FrameSummary`, or skip the stream. Nothing renders it.

- [ ] **Step 4: Run the tests and the touched crates' suites**

Run: `cargo test -p zeta-core --lib && cargo test -p zeta-core --test ops_stream && cargo test -p zeta-plugin && cargo test -p zeta-vrt && cargo test -p zeta-ws && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 5: Commit**

`Streams: a packet payload and stream kind, with its link type`

---

### Task 2: The pcapng writer

**Files:**
- Create: `crates/zeta-core/src/pcapng.rs`
- Modify: `crates/zeta-core/src/lib.rs` (`pub mod pcapng;`)
- Test: unit tests in `pcapng.rs`

**Interfaces:**
- Consumes: `LinkType::number()` (task 1).
- Produces (all little-endian, pure functions that append to a `Vec<u8>`, no I/O):
  - `pub fn header(out: &mut Vec<u8>, link_type: LinkType)`: one Section Header Block (type `0x0A0D0D0A`, byte-order magic `0x1A2B3C4D`, version 1.0, section length `-1`, option `shb_userappl` (code 4) = `zeta`) then one Interface Description Block (type `1`, the link type as `u16`, reserved `0`, snaplen `0`, option `if_tsresol` (code 9, length 1) = `9`). Every option list ends with `opt_endofopt`.
  - `pub fn packet(out: &mut Vec<u8>, t_ns: u64, data: &[u8], comment: &str)`: one Enhanced Packet Block (type `6`, interface `0`, timestamp high then low 32 bits of `t_ns`, captured and original length = `data.len()`, data padded to 4 bytes, option `opt_comment` (code 1) = `comment` as UTF-8 padded to 4).
  - `pub fn whole_blocks(bytes: &[u8]) -> usize`: the length of the longest prefix that is whole blocks (each block's leading and trailing total length agree and fit); 0 when the first block is not a whole Section Header Block.
  - `pub struct Packet { pub t_ns: u64, pub data: Vec<u8>, pub comment: Option<String> }` and `pub fn read(bytes: &[u8]) -> Result<(u16, Vec<Packet>), String>`: the link type number and the packets of a one-section, one-interface file as this writer makes them. Used by the tests of tasks 4 to 7 and by nothing at run time; it is `pub` so the `zeta` and `zeta-ws` test crates can call it.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_capture_reads_back_what_was_written() {
    let mut f = Vec::new();
    header(&mut f, LinkType::Ax25Kiss);
    packet(&mut f, 1_700_000_000_123_456_789, &[0, 1, 2, 3, 4], "job=J source=S freq_hz=144800000");
    packet(&mut f, 1_700_000_001_000_000_000, &[9], "");
    let (lt, p) = read(&f).unwrap();
    assert_eq!(lt, 202);
    assert_eq!(p.len(), 2);
    assert_eq!(p[0].t_ns, 1_700_000_000_123_456_789);
    assert_eq!(p[0].data, [0, 1, 2, 3, 4]);
    assert_eq!(p[0].comment.as_deref(), Some("job=J source=S freq_hz=144800000"));
    assert_eq!(p[1].data, [9]);
}

#[test]
fn the_section_header_is_byte_exact() {
    let mut f = Vec::new();
    header(&mut f, LinkType::User0);
    assert_eq!(&f[..12], [0x0a, 0x0d, 0x0d, 0x0a, 0x20, 0, 0, 0, 0x4d, 0x3c, 0x2b, 0x1a]);
    assert_eq!(f.len() % 4, 0);
}

#[test]
fn every_block_is_padded_and_its_two_lengths_agree() { /* walk the blocks of a 3-packet file with data lengths 1, 2, 3 */ }

#[test]
fn whole_blocks_stops_before_a_cut_block() {
    // header + 2 packets; for every cut point inside the second packet,
    // whole_blocks(&f[..cut]) == the offset where the second packet starts
}
```

- [ ] **Step 2: Run to see them fail** — `cargo test -p zeta-core --lib pcapng::` → does not compile.
- [ ] **Step 3: Implement the four functions and `Packet`.**
- [ ] **Step 4: Run** — `cargo test -p zeta-core --lib pcapng::` → PASS. When `tshark` is on `PATH`, also check by hand that a file written by the first test's bytes lists two packets (`tshark -r <file>`); record the result in the commit body, do not make a test depend on it.
- [ ] **Step 5: Commit** — `Packets: a pcapng writer (section, interface and packet blocks)`

---

### Task 3: The frame stages emit packets

**Files:**
- Modify: `crates/zeta-dsp/src/aprs.rs`, `crates/zeta-dsp/src/frame_decode.rs`, `crates/zeta-core/src/modes/template.rs` (`outputs`), `crates/zeta/src/agents_md.rs` (the `outputs` prose), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: unit tests in `aprs.rs` and `frame_decode.rs`, `crates/zeta-core/tests/modes.rs`, `crates/zeta-core/tests/frame_rows.rs`

**Interfaces:**
- Consumes: `Payload::Packet`, `LinkType`, `StreamBus::receiver_count` (existing).
- Produces:
  - `aprs_decode`: `spec().outputs == [Event, Packet]`. For each CRC-good frame the event as today, and, when the packet stream has a subscriber, `Payload::Packet { link_type: Ax25Kiss, data }` with `data = [0x00] ++ the AX.25 frame without its two FCS bytes`, published with the same `Provenance` as the event.
  - `frame_decode`: `spec().outputs == [Event, Packet]`. For each frame, `Payload::Packet { link_type: User0, data: fr.bytes }` under the same rule and provenance (`fd.prov`).
  - `modes::template::outputs(row, stages)`: a frame row is `[Event, Packet]`; a graph row is as today plus `Packet` when any of its nodes' stages lists `Packet` in `spec().outputs` and no edge of kind `packet` leaves that node.

- [ ] **Step 1: Write the failing tests**

`aprs.rs`: extend the existing decode test's setup with a second registered output and a subscriber on it:

```rust
#[tokio::test]
async fn a_decoded_frame_is_also_a_kiss_packet() {
    // same fixture as the event test (info ">hi", no path, invented callsigns)
    let Payload::Packet { link_type, data } = &f.payload else { panic!() };
    assert_eq!(*link_type, LinkType::Ax25Kiss);
    assert_eq!(data[0], 0x00);
    assert_eq!(data.len(), 1 + 7 + 7 + 1 + 1 + 3); // KISS, dst, src, control, PID, ">hi"
    assert_eq!(&data[data.len() - 3..], b">hi");
    assert_eq!(f.provenance.span.start_ns, event_frame.provenance.span.start_ns);
}

#[tokio::test]
async fn no_packet_is_built_without_a_subscriber() {
    // packet output registered, nobody subscribed: the event still arrives and bus.dropped() stays 0
}
```

`frame_decode.rs`: `a_cut_frame_is_also_a_user0_packet` — on the fixture of `frames_are_decoded_across_split_bits_events`, each packet's `data` equals the bytes whose hex is the matching event's `bytes`, `link_type == User0`, one packet per frame event, in order.

`tests/modes.rs`: the per-stage expectation table gains `"aprs_decode" => &[StreamKind::Event, StreamKind::Packet]`, and a new test:

```rust
#[test]
fn rows_that_cut_frames_list_packet() {
    // modes_list: aprs_decode's outputs == [event, packet]; every embedded frame row's
    // outputs == [event, packet]; pocsag_decode, rds_decode and video_decode do not list packet
}
```

- [ ] **Step 2: Run to see them fail** — `cargo test -p zeta-dsp --lib aprs:: frame_decode:: ; cargo test -p zeta-core --test modes`.
- [ ] **Step 3: Implement.** The HDLC deframer already drops flags; confirm with the length assertion above whether it returns the FCS and strip it in the stage if so. Both stages now take two output stream ids; the launch (`node_outputs`) gives the second one a job output stream with no further change. Fix every test and helper that passes these stages one output (`modes_check.rs`, `bits_graph`/`bits_plan` paths, `frame_rows.rs`).
- [ ] **Step 4: Update the prose** in `agents_md.rs` where `outputs` is described: `packet` is the frames a `job_attach` of `packets` captures. Regenerate both generated files.
- [ ] **Step 5: Run** — `cargo test -p zeta-dsp && cargo test -p zeta-core --test modes --test frame_rows && cargo run -q -p zeta -- modes check && cargo run -q -p zeta -- agents-md --check && cargo run -q -p zeta -- agents-md --skill --check` → PASS.
- [ ] **Step 6: Commit** — `Frames: aprs_decode and frame_decode publish each frame as a packet`

---

### Task 4: The packet capture store

**Files:**
- Create: `crates/zeta-core/src/packets.rs`
- Modify: `crates/zeta-core/src/lib.rs`, `crates/zeta-core/src/node/mod.rs` (open the store beside the burst store; `Node::packets()`), `crates/zeta-core/src/stage.rs` (`StageContext.packets`), `crates/zeta-core/src/node/launch.rs` (fill it where `bursts` is filled)
- Test: unit tests in `packets.rs`

**Interfaces:**
- Consumes: `pcapng::{header, packet, whole_blocks}`, `LinkType`, `bursts::BurstPolicy`, `modes::row::Access`.
- Produces:

```rust
pub const PACKET_CAPTURE_MAX_BYTES: u64 = 256 * 1024 * 1024;
pub const PACKETS_DIR: &str = "packets";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PacketCaptureRecord {
    pub id: String,            // a fresh ULID; the file is <dir>/<id>.pcapng
    pub job: JobId,            // the decoder job captured
    pub source: SourceId,
    pub center_hz: Hz,
    pub link_type: LinkType,
    pub mode: Option<String>,  // the decoder job's row id, nullable in the schema
    pub access: Option<Access>,// the row's, nullable in the schema
    pub created_ns: Timestamp,
    pub bytes: u64,
    pub packets: u64,
    pub writing: bool,         // a job in this node is appending to it
    #[serde(skip)] pub path: PathBuf,
}

pub struct NewPacketCapture { pub job: JobId, pub source: SourceId, pub center_hz: Hz,
    pub link_type: LinkType, pub mode: Option<String>, pub access: Option<Access> }

impl PacketStore {
    pub fn open(state_dir: &Path, policy: BurstPolicy) -> std::io::Result<Arc<Self>>;
    pub fn open_with_cap(state_dir: &Path, policy: BurstPolicy, max_bytes: u64) -> std::io::Result<Arc<Self>>; // tests
    pub fn begin(self: &Arc<Self>, new: NewPacketCapture) -> std::io::Result<PacketWriter>;
    pub fn list(&self) -> Vec<PacketCaptureRecord>;   // newest first
    pub fn get(&self, id: &str) -> Option<PacketCaptureRecord>;
    pub fn delete(&self, id: &str) -> Result<PacketCaptureRecord, PacketStoreError>;
    pub fn expire(&self) -> Vec<String>;
}
pub enum PacketStoreError { Unknown(String), Writing { id: String, job: JobId }, Io(String) }

impl PacketWriter {
    pub fn id(&self) -> &str;                          // the file now being written
    pub fn write(&mut self, t_ns: u64, data: &[u8]) -> std::io::Result<Option<String>>;
}
// Drop for PacketWriter marks its record writing: false.
```

  `write` appends one Enhanced Packet Block with the comment `job=<job> source=<source> freq_hz=<center_hz>`, flushes it to the OS (`write_all` on an unbuffered `File`; no `fsync` per packet), and updates `bytes` and `packets` in the in-memory record. When the block would take the file past the cap, it first closes the file, begins a fresh record (new id, new header, same `NewPacketCapture`) and returns `Ok(Some(new_id))`; otherwise `Ok(None)`. The index (`<dir>/index.json`, written atomically like the burst store's) is rewritten on `begin`, on roll, on drop, on `delete`/`expire`, and at most once a second from `write`.
  - `StageContext.packets: Option<Arc<PacketStore>>`, `Node::packets() -> Arc<PacketStore>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn a_written_capture_is_listed_and_reads_back() {
    // begin, write 3 packets; list()[0] has packets == 3, writing == true,
    // bytes == file length; pcapng::read gives the 3 packets, each comment
    // "job=<job> source=<source> freq_hz=144800000"; drop the writer -> writing == false
}
#[test] fn a_full_capture_rolls_to_a_fresh_id() {
    // open_with_cap(.., 4096); write 100 packets of 200 bytes; several records,
    // each file <= 4096 bytes and each parsing on its own; the packet counts sum to 100;
    // write returned Some(id) exactly at each roll and writer.id() follows it
}
#[test] fn a_packet_larger_than_the_cap_is_still_written_whole() { /* cap 4096, one 8000-byte packet: in a file of its own */ }
#[test] fn open_trims_a_capture_cut_inside_a_block() {
    // write 3 packets, forget the writer (std::mem::forget) and truncate the file by 5 bytes;
    // reopen the store: the record has writing == false, packets == 2, bytes == file length,
    // and pcapng::read parses 2 packets
}
#[test] fn open_drops_a_record_whose_file_is_gone_and_adopts_none_it_does_not_know() {}
#[test] fn deleting_a_capture_being_written_is_refused() {
    // delete(id) with the writer alive -> Err(Writing { id, job }); the file is intact
}
#[test] fn expiry_takes_the_oldest_closed_captures_and_never_one_being_written() {
    // policy keep_bytes smaller than two closed captures plus one open: the oldest closed goes, the open stays
}
```

- [ ] **Step 2: Run to see them fail** — `cargo test -p zeta-core --lib packets::`.
- [ ] **Step 3: Implement the store.** At `open`: read the index; for each record resolve the path, drop it if the file is gone, set `writing: false`, truncate the file to `pcapng::whole_blocks` and recount `bytes`/`packets` when its length disagrees with the record. The node opens it right after the burst store with `cfg.burst_policy` and calls `expire()` once, as it does for bursts.
- [ ] **Step 4: Run** — `cargo test -p zeta-core --lib packets::` → PASS; `cargo test -p zeta-core --lib` → PASS.
- [ ] **Step 5: Commit** — `Packets: a store of pcapng captures with a rolling size cap`

---

### Task 5: `stream_in` for packets and the `packet_capture` sink

**Files:**
- Create: `crates/zeta-core/src/packet_capture.rs`
- Modify: `crates/zeta-core/src/stream_in.rs`, `crates/zeta-core/src/lib.rs`, `crates/zeta-core/src/node/mod.rs` (`NodeError::NoPacketStream`, `Node::job_packet_stream`, the stream an attached record's `stream_in` is pointed at), `crates/zeta-core/src/node/launch.rs`, `crates/zeta/src/node_setup.rs` (register the stage)
- Test: unit tests in `stream_in.rs` and `packet_capture.rs`

**Interfaces:**
- Consumes: `PacketStore`, `PacketWriter`, `StageContext.packets`, `Payload::Packet`.
- Produces:
  - `stream_in` accepts `"kind": "packet"` as well as `"audio"`: `spec().outputs == [Audio, Packet]`, `outputs_for(params) == [params.kind]`. Any other kind is refused as today.
  - `pub const PACKET_CAPTURE_KIND: &str = "packet_capture";` and `pub struct PacketCaptureStage` (`new()`), `spec()`: inputs `[Packet]`, outputs `[]`, params (deny unknown fields) `{ "job": <the decoder job id>, "center_hz": <u64> }`. It opens its file at the first packet (the link type is the packet's), writes each packet with `provenance.span.start_ns`, and notes `{"capture": "<id>", "packets": <n>, "bytes": <n>}` in the job's context through `ctx.note` at the first packet, at each roll and at most once a second. A packet whose link type differs from the open file's closes it and begins a fresh one. A write error ends the stage `Err(StageError::Failed(..))` naming the path and the error. With `ctx.packets == None` it fails at once: `packet_capture needs a node's state directory`.
  - `Node::job_packet_stream(&self, job: &JobId) -> Result<StreamId, NodeError>`: the last `Packet` output stream of `job`'s current run; `InputNotRunning` as for audio; else `NodeError::NoPacketStream(JobId)`, message `job <id> has no packet stream: its kind's outputs list no packet (modes_list)`.
  - The launch of an attached record points each `stream_in` node at the input's stream of the node's own `kind` param (`audio`: as today, with the voice; `packet`: `job_packet_stream`), and sets each `packet_capture` node's `job` and `center_hz` params from the input's record (`id`, `tuning.center_hz`) every time it launches, so a relaunch after the input's retune carries the new frequency. The record's `mode` and `access` for `NewPacketCapture` come from the input job's mode row (`ctx.mode` is the attached job's own and is `None`): pass them the same way, as launch-filled params `mode` (nullable string) and `access` (nullable `Access`).

- [ ] **Step 1: Write the failing tests**

`stream_in.rs`: `forwards_packets_when_its_kind_is_packet` (a packet published on the input arrives on the output with its provenance span unchanged); `refuses_a_kind_other_than_audio` is renamed `refuses_a_kind_other_than_audio_or_packet` and keeps asserting `iq` is refused.

`packet_capture.rs` (a `PacketStore` in a temp dir, the stage run directly on a bus as `stream_in`'s tests do):

```rust
#[tokio::test] async fn packets_are_written_with_their_own_times_and_the_job_comment() {
    // params {"job": "DECODER", "center_hz": 144800000, "mode": "aprs_decode", "access": null}
    // publish 2 Ax25Kiss packets with span.start_ns 1_700_000_000_000_000_000 and +1 s;
    // until store.list()[0].packets == 2; pcapng::read: link type 202, the two t_ns, the data,
    // comment "job=DECODER source=<ctx.source> freq_hz=144800000";
    // the record: job DECODER, mode Some("aprs_decode"), writing true
}
#[tokio::test] async fn the_capture_closes_when_the_input_closes() { /* unregister the input: the stage ends Ok, the record has writing == false */ }
#[tokio::test] async fn no_file_is_made_before_the_first_packet() {}
#[tokio::test] async fn the_job_context_names_the_capture() { /* a StageNote collects {"capture": id, "packets": 1, ..} */ }
#[tokio::test] async fn a_write_error_fails_the_capture_and_not_its_input() {
    // after the first packet, replace the store's directory with a read-only one (or remove it
    // and put a file in its place) and shrink the cap so the next write rolls: the stage returns
    // Failed naming the path; the input stream is still registered and publishing to it does not panic
}
```

- [ ] **Step 2: Run to see them fail** — `cargo test -p zeta-core --lib stream_in:: packet_capture::`.
- [ ] **Step 3: Implement** the stage, the `stream_in` kind, `job_packet_stream` and the launch's param fill. File writes are blocking: do them with `tokio::task::block_in_place` or keep the writer on a `spawn_blocking` thread fed by a channel; do not block the runtime on a slow disk.
- [ ] **Step 4: Run** — the two modules, then `cargo test -p zeta-core --features test-fixtures --test attach --test voices --test mix` (the suites that launch `stream_in`) → PASS.
- [ ] **Step 5: Commit** — `Packets: stream_in carries packets and packet_capture writes them`

---

### Task 6: `job_attach` of `packets`, and the capture ops

**Files:**
- Modify: `crates/zeta-core/src/ops.rs` (`JobKind::Packets`, `attachable`, `expand_attached`, `job_attach`, `Op::PacketsList`, `Op::PacketsDelete`, `OpResult::PacketCaptures`, the op-name table, the delete allowance beside `capture_delete`), `crates/zeta-core/src/node/mod.rs` (resume), `crates/zeta/src/cli.rs` and `crates/zeta/src/main.rs` (`zeta job attach packets <job>`, `zeta packets list`, `zeta packets delete <id>`), `crates/zeta-agent/src/agent.rs` (the tool list and the delete allowance), `crates/zeta/src/agents_md.rs`, `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta/tests/attach.rs` (the real chain), `crates/zeta-core/tests/attach.rs`, `crates/zeta-core/tests/resume.rs`, `crates/zeta/tests/cli.rs`

**Interfaces:**
- Consumes: tasks 1 to 5.
- Produces:
  - `JobKind::Packets`, name `packets`, `attachable()` true. `expand_attached` for it: `stream_in {"job", "kind": "packet", "stream"} -> packet_capture {}` joined by one `packet` edge (the launch fills the sink's params, task 5).
  - `job_attach {kind: "packets", job, language: null, stream: null}` → `OpResult::Job(id)`. Refusals, all `other`, nothing started:
    - `stream` not null: `packets reads the job's packet stream: pass stream null`;
    - the job's kind lists no `packet` output: the `NoPacketStream` message of task 5;
    - the job's row is `access: restricted`: `packets: <row id> is restricted: its capture waits on access control`;
    - the job already has a running `packets` attachment: `job <id> is already captured by <packets job id>`.
    `language` is not checked for `packets`.
  - `Op::PacketsList {}` → `OpResult::PacketCaptures(Vec<PacketCaptureRecord>)`, newest first.
  - `Op::PacketsDelete { capture: String }` → `OpResult::Ok`; unknown id → the `unknown` error shape `capture_delete` gives, with the known ids; a capture being written → `other("capture <id> is being written by job <job>: stop the packets job first")`. It is gated wherever `capture_delete` is gated (`allow.delete_captures` for the agent, the read-only one-shot rule).
  - On node start a persisted `packets` record that was `running` is set `stopped` with context `{"note": "packet capture does not resume: attach again"}` and is not launched; its input resumes as it would have.
  - The op doc comments are the agent prose: `job_attach`'s names both kinds and what `packets` writes and where to fetch it (`GET /captures/<id>.pcapng`, the id from the job's context `capture` or `packets_list`); a note in `agents_md.rs` says the capture rolls at 256 MiB, is downloadable while written, and does not resume.

- [ ] **Step 1: Write the failing tests**

`crates/zeta/tests/attach.rs` (full registry, a file source playing a synthesised APRS capture; build it with the framing/AFSK synthesiser the `aprs_decode` row's `test` block already uses, looping, invented callsigns):

```rust
#[tokio::test] async fn a_packets_attachment_writes_a_capture_wireshark_reads() {
    // job_start aprs_decode; job_attach packets; until packets_list()[0].packets >= 2
    // record: job == the aprs job, link_type ax25_kiss, mode "aprs_decode", access null, writing true
    // file: pcapng::read -> link type 202; packet 0: data[0] == 0, the data after it parses with
    // zeta_dsp::ax25::parse_ax25_ui to the src/dst/info the job's own event carries;
    // comment == format!("job={aprs} source={source} freq_hz={centre}");
    // t_ns == the matching event's frame provenance start_ns
    // job_list: the packets job has input == the aprs job; its context "capture" == the record id
    // if `tshark` is on PATH: `tshark -r <file> -T fields -e ax25.src` prints the callsign for
    // every packet; else eprintln!("tshark check skipped: tshark not on PATH")
}
#[tokio::test] async fn stopping_the_packets_job_closes_the_capture_and_leaves_the_decoder_running() {}
#[tokio::test] async fn stopping_the_decoder_stops_its_packets_job() { /* state stopped, writing false */ }
#[tokio::test] async fn a_retuned_input_is_captured_into_a_fresh_file_with_its_new_frequency() {
    // job_retune the aprs job to a centre 25 kHz up, inside the capture, so it relaunches;
    // until two records exist for the job; the first has writing == false and parses whole;
    // the second's first comment carries the new freq_hz
}
#[tokio::test] async fn deleting_a_capture_being_written_is_refused() { /* packets_delete -> other(".. is being written by job ..") */ }
#[tokio::test] async fn a_frame_row_is_captured_as_user0() {
    // a frame row from tests/frame_rows.rs's fixtures: link type 147, data == the event's `bytes` unhexed
}
```

`crates/zeta-core/tests/attach.rs`: the four refusals above, each asserting the exact message and that `job_list` gained no job (`wfm_demod` is the job with no packet stream; a test row with `access: restricted` for the restricted one).

`crates/zeta-core/tests/resume.rs`: `a_packets_attachment_does_not_resume` — attach, drop the node, reopen: the input job runs again, the `packets` record is `stopped` with the note, `packets_list` still lists the capture with `writing == false`, and a fresh `job_attach` starts a new one.

`crates/zeta/tests/cli.rs`: `zeta job attach packets <job>` builds `{"op":"job_attach","kind":"packets","job":..,"language":null,"stream":null}`; `zeta packets list` and `zeta packets delete <id>` build their ops.

- [ ] **Step 2: Run to see them fail.**
- [ ] **Step 3: Implement** the kind, the ops, the resume rule and the CLI; add the two ops to the agent's tool list and to the page's read-op allow-list only where `capture_list` and `capture_delete` appear.
- [ ] **Step 4: Regenerate** `AGENTS.md` and `skills/zetasdr/SKILL.md`.
- [ ] **Step 5: Run** — `cargo test -p zeta --test attach --test cli && cargo test -p zeta-core --features test-fixtures --test attach --test resume --test ops && cargo test -p zeta-agent --features test-fake && cargo run -q -p zeta -- agents-md --check && cargo run -q -p zeta -- agents-md --skill --check` → PASS.
- [ ] **Step 6: Commit** — `Ops: job_attach of packets, packets_list and packets_delete`

---

### Task 7: Serving a capture over HTTP

**Files:**
- Modify: `crates/zeta-ws/src/captures.rs`
- Test: `crates/zeta-ws/tests/` (the file that tests `/captures/{file}` today; else a new `captures.rs` beside `binary.rs`)

**Interfaces:**
- Consumes: `Node::packets()`, `PacketStore::get`.
- Produces: `GET /captures/<id>.pcapng` → 200, `Content-Type: application/vnd.tcpdump.pcap`, `Content-Disposition: attachment; filename="<id>.pcapng"`, the file's bytes cut to `pcapng::whole_blocks` (a reader racing the writer never gets half a block). The id is looked up in the packet store only; the SigMF arms are unchanged. Anything else is the existing 404.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test] async fn a_capture_is_served_while_it_is_being_written() {
    // a PacketWriter held open with 2 packets written: GET -> 200, the headers above,
    // pcapng::read(body) gives 2 packets; write a third, GET again -> 3
}
#[tokio::test] async fn only_a_packet_capture_id_is_served() {
    // 404 for: an unknown ULID, "<iq capture id>.pcapng", "<packet id>.sigmf-data",
    // "..%2F..%2Fstate.pcapng", ".pcapng"
}
```

- [ ] **Step 2: Run to see them fail.**
- [ ] **Step 3: Implement the `pcapng` arm.**
- [ ] **Step 4: Run** — `cargo test -p zeta-ws` → PASS.
- [ ] **Step 5: Commit** — `HTTP: serve a packet capture at /captures/<id>.pcapng`

---

### Task 8: The page lists packet captures

**Files:**
- Modify: the Explore pane's module in `crates/zeta-ui/web/` (the one that polls `capture_list`), `crates/zeta-ui/web/actions.js` / `menu.js` (a job's menu), `crates/zeta-ui/web/agent.js` (its read-op list), `crates/zeta-ui/tests/e2e/ui.py`
- Test: one scenario in `ui.py`

**Interfaces:**
- Consumes: `packets_list`, `packets_delete`, `job_attach {kind: "packets"}`, `GET /captures/<id>.pcapng`.
- Produces:
  - The Explore pane's capture list shows packet captures beside the IQ ones, each row: its row label (`APRS`), the frequency, the packet count, the size, `recording` while `writing`, and a download link to `/captures/<id>.pcapng` (a plain `<a download>`); delete goes through `packets_delete` and is disabled while `writing`.
  - A running job whose row's `outputs` lists `packet` has a menu item `Capture packets` (sends the attach) which reads `Stop packet capture` while its `packets` job runs (sends `job_stop` on that job). No other job shows it.
  - Nothing else changes: a `packet` stream gets no tab, pane or player.

- [ ] **Step 1: Write the failing scenario** `packet_capture`: on a node whose file source plays the synthesised APRS capture of task 6 (add it to the suite's fixtures the way the `cb` fixture is; keep it small), start the APRS decoder from the page, choose `Capture packets` from its menu, wait for the Explore row to read `recording` with a packet count of at least 1, fetch the download link's `href` with the page's own `fetch` and assert the first four bytes are `0a 0d 0d 0a`, choose `Stop packet capture`, wait for the row to lose `recording`, delete it, wait for it to go. Use function-form `wait_for_function` (the page's CSP refuses string forms).
- [ ] **Step 2: Run it to see it fail** — `ZETA_UI_ONLY=packet_capture cargo test -p zeta --test ui -- --nocapture`; check the output has no `ui e2e skipped:` line.
- [ ] **Step 3: Implement** the list rows and the menu item, in the existing components' style and theme tokens.
- [ ] **Step 4: Run** the scenario, then the scenarios that touch the Explore pane and the job menu (`ZETA_UI_ONLY=<those names>`) → PASS.
- [ ] **Step 5: Commit** — `Page: capture a job's packets and download the capture`

---

### Task 9: Docs, acceptance checks and the full gates

**Files:**
- Modify: `docs/agents/field-craft.md` (a short "Open decoded frames in Wireshark" recipe: attach, find the id, `curl -O http://<node>/captures/<id>.pcapng`, open; for a frame row, how to bind a user dissector to `USER0` under Wireshark's DLT_USER preferences), `RECIPES.md` if it lists such recipes, `TODO.md` (the "Feature: packet capture output for Wireshark" gist: phase A is built by plan 41, what remains is phase B (GSMTAP over UDP, then extcap), the `.ts` egress, and capture of restricted rows behind access control), `UAT.md` (a "Plan 41" section), `CHANGELOG.md`, `AGENTS.md`, `skills/zetasdr/SKILL.md`

- [ ] **Step 1: Write the docs.** `UAT.md` plan 41 checks, each against a live radio and written for the operator to tick:
  1. Attach `packets` to the running APRS job; after traffic, download the capture while it records and open it in Wireshark: frames dissect as AX.25 and APRS, times match the page's events, the packet comment names the job, source and frequency.
  2. Stop the capture from the page; the file is whole and still listed.
  3. Restart the node mid-capture: the file opens, the `packets` job is `stopped` with its note, a fresh attach works.
  4. A frame row's capture opens as `USER0` with the frame bytes visible.
  5. A day-long capture: the node's CPU and the decoder's event rate are unchanged by it.
- [ ] **Step 2: Regenerate** the two generated files.
- [ ] **Step 3: Run every gate in `CLAUDE.md`** with a private `CARGO_TARGET_DIR`: fmt per touched crate, clippy, `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`, the feature-gated suites of `zeta-core` (`--features test-fixtures`), `zeta-dsp` and `zeta-agent`, the UI suite once in full, both `agents-md --check` runs, `bash scripts/public-check.sh`, `bash scripts/check-doc-links.sh`. Expected: all clean; the UI run prints no `ui e2e skipped:` line.
- [ ] **Step 4: Commit** — `Docs: packet capture for Wireshark (plan 41), its recipe and acceptance checks`
- [ ] **Step 5: Delete the private target directory.**
