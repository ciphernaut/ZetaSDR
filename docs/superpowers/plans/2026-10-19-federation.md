# Federation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One ZetaSDR node uses another node's source over the LAN: IQ crosses as VITA 49.2, everything else as the typed envelope, with credit-based back-pressure, the plan-3 shared secret, and a Pi 5 build that serves at the antenna.

**Architecture:** A new `zeta-vrt` crate encodes and decodes VITA 49.2 signal-data and context packets (no I/O). `zeta-ws` gains a binary mode on the existing stream websocket: IQ streams send VRT packets, other kinds send the typed envelope, and the client meters the flow with credits. `zeta-client` gains the matching websocket subscriber. A new `zeta-source-node` crate is a push `Port` that tunes by starting a `passthrough` job on the remote node (so the remote lease is honest), subscribes to its IQ stream and rebuilds `IqBlock`s. A `zeta-source-vrt` UDP ingest port makes any DIFI-style sender a source. `deploy/pi/` cross-builds and installs a headless Pi 5 node.

**Tech Stack:** Rust 2024, tokio, axum 0.8 websockets (server), tokio-tungstenite 0.30 (client), num-complex, `cross` + docker for aarch64, systemd on the Pi.

**Spec:** `docs/superpowers/specs/2026-10-18-zetasdr-design.md` (sections 3 Constraints, 4.1 Sources, 4.3 Streams, 4.7 Federation, 5.2 Transports, 8 item 7).

## Global Constraints

- `zeta-core` gains no dependency: `no_privileged_deps` keeps passing; VRT, websockets and UDP live in their own crates.
- "IQ streams between nodes are carried as VITA 49.2"; audio, text, events ride ZetaSDR's own typed envelope on the same transport.
- "A node ships narrowband, channelised streams by default; shipping wideband IQ across the wire is allowed but explicit, with flow control and back-pressure from v1." Default remote-source cap `NODE_DEFAULT_MAX_BANDWIDTH_HZ = 250_000`; wider needs `max_bandwidth_hz=` on the flag.
- "LAN only for now; v1 uses a shared secret between nodes and callers on every transport." The node source presents the plan-3 bearer secret; nothing listens off-LAN (`zeta-ws::addr::is_lan` already enforces it).
- Port contract (`zeta-core/src/port.rs`): `tune`/`current` return the capture band whose `bandwidth_hz` equals the block sample rate; producer exits without subscribers; `t0` rises monotonically and jumps forward on loss; failures via `fault()`, never a panic or a silent close.
- Secrets never appear in `source list`, logs, errors, `Debug` or `Display`.
- Strict op schemas (`deny_unknown_fields`, `_hz`/`_ns`/`_ms` units) unchanged; no new op is added in this plan.
- CI runs with no hardware and no network beyond loopback.
- Gate: `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features`; `zeta agents-md --check` and `--skill --check` pass.
- Build the minimum: no UDP VRT *sender*, no TLS, no discovery, no multi-channel VRT, no class-id word.

## Review Focus

1. A slow consumer on WiFi: when its credit is exhausted the server skips frames for that socket only, counts them, and reports `lagged` when credit returns; other subscribers never stall. Test: Task 3 `credit_zero_skips_and_reports_lagged`.
2. The remote node goes away mid-job: the node port faults with the node URL and the reason, the local job fails with that reason, and no thread or task spins. Test: Task 5 `server_stop_faults_port_and_names_url`.
3. A wideband request through federation: a `tune` wider than the cap is `OutOfRange` with `suggested` = cap and no remote job is started. Test: Task 5 `bandwidth_over_cap_is_out_of_range_without_remote_job`.
4. Hostile or truncated VRT bytes: the decoder returns an error, never panics, and the next good packet decodes. Test: Task 1 `random_bytes_never_panic` and Task 2 `decoder_recovers_after_bad_packet`.
5. A remote `passthrough` job left behind when the federating node dies is a known limitation (no op can tag ownership); dropping or retuning the port stops it. Test: Task 5 `drop_stops_remote_job`.

---

## File structure

```
crates/zeta-vrt/                 VITA 49.2 codec, no I/O
  src/lib.rs                     re-exports
  src/packet.rs                  header word, data packets, timestamps, ci16 conversion
  src/context.rs                 context packets, CIF0 fields, payload format words
  src/stream.rs                  VrtEncoder (context cadence) and VrtDecoder (context-before-data, loss)
crates/zeta-ws/src/stream.rs     binary mode + credits on GET /stream/{id}
crates/zeta-ws/src/wire.rs       Credit message, envelope framing for non-IQ binary frames
crates/zeta-client/src/ws.rs     NodeClient::subscribe: websocket client, credits, decoded Payloads
crates/zeta-source-node/         RemoteNodePort
crates/zeta-source-vrt/          UDP VRT ingest port
crates/zeta/src/sources.rs       --source-node, --source-vrt grammars
crates/zeta/src/node_setup.rs    wiring
crates/zeta/src/agents_md.rs     RUNNING section
deploy/pi/                       build-pi.sh, install.sh, zetasdr.service, README.md
```

Wire conventions used by every task (stated once here):

- VRT is big-endian throughout. Timestamps: TSI=1 (UTC seconds, u32), TSF=2 (real-time picoseconds, u64). `Timestamp(ns)` maps to `secs = ns / 1_000_000_000`, `ps = (ns % 1_000_000_000) * 1000`.
- Signal data packet: type `0b0001` (with stream id), C=0, T=0; words: header, stream id, TSI, TSF hi, TSF lo, then complex int16 samples (I then Q). Packet size field is the whole packet in 32-bit words.
- Context packet: type `0b0100`, C=0; words: header, stream id, TSI, TSF hi, TSF lo, CIF0, then fields in CIF0 bit order (high bit first): bandwidth (bit 29, u64 fixed-point, radix after bit 20, i.e. `hz << 20`), RF reference frequency (bit 27, same encoding, carries `center_hz`), sample rate (bit 21, same encoding), signal data payload format (bit 15, two words). Bit 31 (change indicator) is set on the first context packet of a stream and whenever a field changed.
- Payload format words for complex int16: word 1 = `0b0 01 00000 0 000 0000 0000 001111 001111` (packing method processing-efficient, complex cartesian, signed fixed-point, item packing field size 15, data item size 15), word 2 = 0.
- Stream id: FNV-1a 32-bit of the `StreamId` string. Packet count: 4-bit, per stream, independently for data and context packets.
- f32 to i16: `round(x * 32767)` saturating; i16 to f32: `v / 32767`.
- Typed envelope for non-IQ binary frames (the plugin wire's data frame, reused): `u32LE header_len | FrameSummary JSON | u32LE body_len | body`; body is f32 LE PCM for audio, UTF-8 for text, empty for event and sink (payload is in the header for events). Limits as `zeta-plugin`: header ≤ 64 KiB, body ≤ 16 MiB.
- Credits: a text message `{"type":"credit","packets":N}` (N in 1..=4096) adds N to the socket's credit (cap 65 536). Every binary message consumes one credit. Credit starts at 0 in binary mode.

### Task 1: VRT signal data packets

**Files:**
- Create: `crates/zeta-vrt/Cargo.toml` (deps: `num-complex = "0.4.6"`, `thiserror = "2.0.21"`; no tokio), `crates/zeta-vrt/src/lib.rs`, `crates/zeta-vrt/src/packet.rs`
- Modify: root `Cargo.toml` is `members = ["crates/*"]`, nothing to add
- Test: `crates/zeta-vrt/tests/packet.rs`

**Interfaces:**
- Produces:
  - `pub fn stream_id_of(id: &str) -> u32` (FNV-1a 32)
  - `pub struct Header { pub kind: PacketKind, pub count: u8, pub size_words: u16 }`, `pub enum PacketKind { Data, Context }`; `Header::encode(&self) -> u32`, `Header::decode(word: u32) -> Result<Header, VrtError>` (TSI must be 1 and TSF 2; C and T bits must be 0; anything else `VrtError::Unsupported(String)`)
  - `pub struct DataPacket { pub stream_id: u32, pub count: u8, pub t0_ns: u64, pub samples: Vec<Complex<f32>> }`
  - `DataPacket::encode(&self) -> Vec<u8>`; `DataPacket::decode(bytes: &[u8]) -> Result<DataPacket, VrtError>`
  - `pub const MAX_SAMPLES_PER_PACKET: usize = 16_384`
  - `pub enum VrtError { Truncated { need: usize, have: usize }, SizeMismatch { header_words: u16, actual_words: usize }, Unsupported(String), TooLong(usize) }` (`thiserror`, `Display` names every value)
  - `pub fn f32_to_i16(x: f32) -> i16`, `pub fn i16_to_f32(v: i16) -> f32`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn header_roundtrips_and_has_known_bits() {
    let h = Header { kind: PacketKind::Data, count: 5, size_words: 7 };
    // type 0001, C=0, T=0, rsvd 00, TSI 01, TSF 10, count 0101, size 0x0007
    assert_eq!(h.encode(), 0x1065_0007);
    assert_eq!(Header::decode(0x1065_0007).unwrap(), h);
}

#[test]
fn data_packet_bytes_are_big_endian_ci16() {
    let p = DataPacket { stream_id: 0xDEAD_BEEF, count: 1, t0_ns: 1_000_000_001_000,
        samples: vec![Complex::new(1.0, -1.0), Complex::new(0.5, 0.0)] };
    let b = p.encode();
    assert_eq!(b.len(), 7 * 4);
    assert_eq!(&b[0..4], &0x1061_0007u32.to_be_bytes());
    assert_eq!(&b[4..8], &[0xDE, 0xAD, 0xBE, 0xEF]);
    assert_eq!(&b[8..12], &1000u32.to_be_bytes());          // TSI seconds
    assert_eq!(&b[12..20], &1_000_000u64.to_be_bytes());    // TSF ps = 1000 ns
    assert_eq!(&b[20..24], &[0x7F, 0xFF, 0x80, 0x01]);     // 32767, -32767
    assert_eq!(&b[24..28], &[0x40, 0x00, 0x00, 0x00]);     // 16384, 0
    let back = DataPacket::decode(&b).unwrap();
    assert_eq!(back.stream_id, 0xDEAD_BEEF);
    assert_eq!(back.t0_ns, 1_000_000_001_000);
    assert!((back.samples[0].re - 1.0).abs() < 1e-4 && (back.samples[1].re - 0.5).abs() < 1e-4);
}

#[test]
fn f32_conversion_saturates() {
    assert_eq!(f32_to_i16(2.0), 32767);
    assert_eq!(f32_to_i16(-2.0), -32767);
    assert_eq!(f32_to_i16(0.0), 0);
}

#[test]
fn truncated_and_mismatched_sizes_are_errors() {
    let b = DataPacket { stream_id: 1, count: 0, t0_ns: 0, samples: vec![Complex::new(0.0, 0.0); 4] }.encode();
    assert!(matches!(DataPacket::decode(&b[..b.len() - 1]), Err(VrtError::SizeMismatch { .. }) | Err(VrtError::Truncated { .. })));
    assert!(matches!(DataPacket::decode(&b[..3]), Err(VrtError::Truncated { .. })));
    let mut wrong = b.clone();
    wrong[3] = 0x02; // size field says 2 words
    assert!(matches!(DataPacket::decode(&wrong), Err(VrtError::SizeMismatch { .. })));
}

#[test]
fn too_many_samples_is_rejected() {
    let p = DataPacket { stream_id: 1, count: 0, t0_ns: 0, samples: vec![Complex::new(0.0, 0.0); MAX_SAMPLES_PER_PACKET + 1] };
    assert!(matches!(p.try_encode(), Err(VrtError::TooLong(_))));
}

#[test]
fn random_bytes_never_panic() {
    // xorshift, 10_000 buffers of 0..=96 bytes; only the Result matters
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..10_000 {
        s ^= s << 13; s ^= s >> 7; s ^= s << 17;
        let len = (s % 97) as usize;
        let bytes: Vec<u8> = (0..len).map(|i| (s.rotate_left(i as u32 % 64) & 0xFF) as u8).collect();
        let _ = DataPacket::decode(&bytes);
        if bytes.len() >= 4 { let _ = Header::decode(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])); }
    }
}

#[test]
fn stream_id_is_fnv1a32() {
    assert_eq!(stream_id_of(""), 0x811C_9DC5);
    assert_eq!(stream_id_of("a"), 0xE40C_292C);
}
```

- [ ] **Step 2: Run** `cargo test -p zeta-vrt` — FAIL (crate missing).
- [ ] **Step 3: Implement `packet.rs`**: `DataPacket::encode` panics only via `try_encode(&self) -> Result<Vec<u8>, VrtError>` (`encode` unwraps it and is documented for lengths ≤ `MAX_SAMPLES_PER_PACKET`). `decode` checks: ≥ 4 bytes for the header, header kind `Data`, `size_words * 4 == bytes.len()` else `SizeMismatch`, ≥ 20 bytes else `Truncated`, payload length even in words.
- [ ] **Step 4: Run** `cargo test -p zeta-vrt` — PASS.
- [ ] **Step 5: Commit** `git add crates/zeta-vrt && git commit -m "feat(vrt): vita 49.2 signal data packets"`

### Task 2: VRT context packets and the stream codec

**Files:**
- Create: `crates/zeta-vrt/src/context.rs`, `crates/zeta-vrt/src/stream.rs`
- Modify: `crates/zeta-vrt/src/lib.rs`
- Test: `crates/zeta-vrt/tests/context.rs`, `crates/zeta-vrt/tests/stream.rs`

**Interfaces:**
- Consumes: Task 1 types.
- Produces:
  - `pub struct ContextPacket { pub stream_id: u32, pub count: u8, pub t0_ns: u64, pub changed: bool, pub center_hz: u64, pub bandwidth_hz: u64, pub sample_rate_hz: u64 }`, `encode`/`decode` as Task 1 (payload format words must be the ci16 words above, else `Unsupported("payload format …")`).
  - `pub const CONTEXT_INTERVAL_NS: u64 = 1_000_000_000`
  - `pub struct IqMeta { pub center_hz: u64, pub sample_rate_hz: u64, pub bandwidth_hz: u64 }`
  - `pub struct VrtEncoder { … }`; `VrtEncoder::new(stream_id: u32) -> Self`; `fn encode(&mut self, t0_ns: u64, meta: IqMeta, samples: &[Complex<f32>]) -> Vec<Vec<u8>>`: a context packet first when none was sent, when `meta` changed, or when `t0_ns - last_context_ns >= CONTEXT_INTERVAL_NS`; then data packets of at most `MAX_SAMPLES_PER_PACKET` samples each with `t0_ns` advanced by `n * 1e9 / sample_rate_hz` per chunk; counts wrap at 16.
  - `pub struct Decoded { pub t0_ns: u64, pub meta: IqMeta, pub samples: Vec<Complex<f32>>, pub missed_packets: u32 }`
  - `pub struct VrtDecoder { … }`; `VrtDecoder::new() -> Self`; `fn feed(&mut self, packet: &[u8]) -> Result<Option<Decoded>, VrtError>`: a context packet updates `meta` and returns `Ok(None)`; a data packet before any context is `Err(VrtError::Unsupported("data before context"))`; a data packet's `count` gap from the previous data packet is reported in `missed_packets` (mod 16); `fn meta(&self) -> Option<IqMeta>`.

- [ ] **Step 1: Write the failing tests**

```rust
// tests/context.rs
#[test]
fn context_packet_fields_are_fixed_point_hz() {
    let c = ContextPacket { stream_id: 7, count: 0, t0_ns: 0, changed: true,
        center_hz: 100_000_000, bandwidth_hz: 48_000, sample_rate_hz: 48_000 };
    let b = c.encode();
    assert_eq!(b.len(), (5 + 1 + 2 + 2 + 2 + 2) * 4);
    assert_eq!(&b[0..4], &0x4060_000Eu32.to_be_bytes());         // type 0100, TSI 01, TSF 10, count 0, 14 words
    assert_eq!(&b[20..24], &0xA820_8000u32.to_be_bytes());       // CIF0: bits 31,29,27,21,15
    assert_eq!(&b[24..32], &(48_000u64 << 20).to_be_bytes());    // bandwidth
    assert_eq!(&b[32..40], &(100_000_000u64 << 20).to_be_bytes()); // rf ref freq
    assert_eq!(&b[40..48], &(48_000u64 << 20).to_be_bytes());    // sample rate
    assert_eq!(&b[48..52], &0x2000_03CFu32.to_be_bytes());       // payload format word 1
    assert_eq!(&b[52..56], &[0, 0, 0, 0]);
    assert_eq!(ContextPacket::decode(&b).unwrap(), c);
}

#[test]
fn context_with_other_payload_format_is_unsupported() {
    let mut b = ContextPacket { /* as above */ }.encode();
    b[48] = 0x2E; // data item format 01110: IEEE-754 single
    assert!(matches!(ContextPacket::decode(&b), Err(VrtError::Unsupported(_))));
}

// tests/stream.rs
#[test]
fn encoder_sends_context_first_then_every_second_and_on_change() {
    let meta = IqMeta { center_hz: 1_000_000, sample_rate_hz: 48_000, bandwidth_hz: 48_000 };
    let mut e = VrtEncoder::new(1);
    let s = vec![Complex::new(0.0, 0.0); 480];
    assert_eq!(e.encode(0, meta, &s).len(), 2);                 // context + data
    assert_eq!(e.encode(10_000_000, meta, &s).len(), 1);        // data only
    assert_eq!(e.encode(1_000_000_000, meta, &s).len(), 2);     // 1 s: context again
    let changed = IqMeta { center_hz: 2_000_000, ..meta };
    assert_eq!(e.encode(1_010_000_000, changed, &s).len(), 2);  // change: context again
}

#[test]
fn encoder_splits_long_blocks_and_advances_t0() {
    let meta = IqMeta { center_hz: 0, sample_rate_hz: 1_000_000, bandwidth_hz: 1_000_000 };
    let mut e = VrtEncoder::new(1);
    let s = vec![Complex::new(0.0, 0.0); MAX_SAMPLES_PER_PACKET + 10];
    let packets = e.encode(5_000, meta, &s);
    assert_eq!(packets.len(), 3);
    let d2 = DataPacket::decode(&packets[2]).unwrap();
    assert_eq!(d2.samples.len(), 10);
    assert_eq!(d2.t0_ns, 5_000 + MAX_SAMPLES_PER_PACKET as u64 * 1_000); // 1 µs per sample
}

#[test]
fn decoder_needs_context_counts_loss_and_recovers_after_bad_packet() {
    let meta = IqMeta { center_hz: 1, sample_rate_hz: 48_000, bandwidth_hz: 48_000 };
    let mut e = VrtEncoder::new(9);
    let s = vec![Complex::new(0.25, 0.0); 48];
    let first = e.encode(0, meta, &s);           // [context, data#0]
    let mut d = VrtDecoder::new();
    assert!(matches!(d.feed(&first[1]), Err(VrtError::Unsupported(_))));
    assert!(d.feed(&first[0]).unwrap().is_none());
    assert_eq!(d.meta().unwrap().sample_rate_hz, 48_000);
    let got = d.feed(&first[1]).unwrap().unwrap();
    assert_eq!(got.samples.len(), 48);
    assert_eq!(got.missed_packets, 0);
    let _skipped = e.encode(1_000_000, meta, &s);  // data#1 never fed
    let third = e.encode(2_000_000, meta, &s);     // data#2
    assert!(d.feed(b"\x10\x60\x00\x05junk").is_err());
    let got = d.feed(&third[0]).unwrap().unwrap();
    assert_eq!(got.missed_packets, 1);
    assert_eq!(got.t0_ns, 2_000_000);
}
```

- [ ] **Step 2: Run** `cargo test -p zeta-vrt` — FAIL (`ContextPacket` missing).
- [ ] **Step 3: Implement `context.rs` and `stream.rs`** per the Interfaces block. CIF0 bit positions and the payload format word are the plan's values; if the implementer has access to the VITA 49.2 or DIFI 1.1 field tables and finds a discrepancy, they follow the standard and record the correction in their report (the encoder and decoder must still agree and the tests' expected words are then updated in the same commit).
- [ ] **Step 4: Run** `cargo test -p zeta-vrt` — PASS.
- [ ] **Step 5: Commit** `git commit -am "feat(vrt): context packets and the stream encoder/decoder"`

### Task 3: Binary stream websocket with credits

**Files:**
- Modify: `crates/zeta-ws/Cargo.toml` (add `zeta-vrt`), `crates/zeta-ws/src/wire.rs`, `crates/zeta-ws/src/stream.rs`
- Test: `crates/zeta-ws/tests/binary.rs`

**Interfaces:**
- Consumes: `VrtEncoder`, `IqMeta` (Task 2); `WsMessage`, `ws_frame`, `connection` loop (`stream.rs:137`); `Limits` (`http.rs`).
- Produces:
  - `GET /stream/{id}?format=binary` (`format=text` or absent keeps today's behaviour; any other value is 400 with `OpError::Other`).
  - `WsMessage` gains `Credit { packets: u32 }` (inbound only; sent by the server never).
  - `pub fn envelope(f: &Frame) -> Result<Vec<u8>, String>` in `wire.rs`: the typed envelope for non-IQ frames (audio body f32 LE, text UTF-8, event/sink empty); `pub fn parse_envelope(bytes: &[u8]) -> Result<(WsFrame, Vec<u8>), String>` with the 64 KiB / 16 MiB limits.
  - `pub const CREDIT_CAP: u32 = 65_536`, `pub const CREDIT_MAX_GRANT: u32 = 4_096`.
  - Binary-mode loop: IQ frames go through one `VrtEncoder` per socket (stream id `stream_id_of(&id.0)`, meta from the `IqBlock`: `bandwidth_hz = sample_rate_hz`); each VRT packet is one binary message; non-IQ frames are one envelope message each. A frame arriving with credit 0 is skipped and counted; the next time credit is positive and a frame is sent, `{"type":"lagged","missed":K}` precedes it (K includes broadcast lag). Credit messages that are not valid JSON, have `packets` outside 1..=4096, or arrive in text mode are ignored. `ServerStats` gains `ws_credit_skipped: u64`.

- [ ] **Step 1: Write the failing tests** (use `zeta_ws::testing::spawn`, `tokio_tungstenite::connect_async` with the bearer header as the existing `tests/` do; start a `passthrough` job on `tone` at 48 kHz, find its iq stream via `node.bus().list()`):

```rust
#[tokio::test]
async fn binary_iq_stream_is_vrt_after_credit() {
    // connect ?format=binary; no credit → nothing within 300 ms
    // send {"type":"credit","packets":4}; expect: first binary message decodes as ContextPacket with sample_rate 48_000, then DataPackets
    // feed all through VrtDecoder and get ≥ 1 Decoded with samples.len() == 4800
}

#[tokio::test]
async fn credit_zero_skips_and_reports_lagged() {
    // credit 2 → receive 2 binary messages; wait 600 ms (≥ 6 frames published); credit 4
    // first text message is {"type":"lagged","missed":K} with K ≥ 4, then binary resumes
    // stats.get().ws_credit_skipped ≥ 4; a second text-mode socket on the same stream kept receiving frames throughout
}

#[tokio::test]
async fn audio_binary_frames_are_envelopes() {
    // publish an Audio frame on a registered stream via node.bus(); binary subscriber with credit
    // parse_envelope → header.kind == Audio, body.len() == samples*4, f32 LE roundtrip exact
}

#[tokio::test]
async fn bad_format_is_400_and_bad_credit_is_ignored() {
    // ?format=vrt → HTTP 400; in binary mode send {"type":"credit","packets":0} then {"type":"credit","packets":1} → exactly one binary message
}
```

- [ ] **Step 2: Run** `cargo test -p zeta-ws --test binary` — FAIL.
- [ ] **Step 3: Implement**: parse `format` with `Query<FormatQuery>` in `ws`; split `connection` into the text loop (unchanged) and `connection_binary`; `tokio::select!` with `biased` as today, inbound text parsed as `WsMessage::Credit`.
- [ ] **Step 4: Run** `cargo test -p zeta-ws` — PASS; existing text-mode tests unchanged.
- [ ] **Step 5: Commit** `git commit -am "feat(ws): binary stream mode: vrt for iq, envelope for the rest, credits"`

### Task 4: Websocket subscriber in `zeta-client`

**Files:**
- Modify: `crates/zeta-client/Cargo.toml` (add `tokio-tungstenite = "0.30.0"`, `futures-util = "0.3"`, `tokio` with `net`, `zeta-vrt`, `zeta-ws` is NOT a dependency: copy the two wire constants or move `envelope`/`parse_envelope` into `zeta-vrt::envelope` — move them; `zeta-ws` then uses them from `zeta-vrt`), `crates/zeta-client/src/lib.rs`
- Create: `crates/zeta-client/src/ws.rs`
- Test: `crates/zeta-client/tests/ws.rs`

**Interfaces:**
- Consumes: Task 3 wire; `NodeClient { url, secret }`.
- Produces:
  - `pub struct Subscription { … }` with `pub async fn next(&mut self) -> Option<Result<Received, ClientError>>`; `pub enum Received { Iq { t0_ns: u64, meta: IqMeta, samples: Vec<Complex<f32>>, missed_packets: u32 }, Frame { header: FrameSummary, body: Vec<u8> }, Lagged { missed: u64 } }`; `None` after `closed` or socket end; `pub fn close_reason(&self) -> Option<&str>`.
  - `NodeClient::subscribe(&self, stream: &StreamId) -> Result<Subscription, ClientError>`: opens `ws://HOST:PORT/stream/{id}?format=binary` with the bearer header; grants `CLIENT_INITIAL_CREDIT = 256` at open and `CLIENT_TOPUP = 128` more every time 128 messages have been consumed since the last grant.
  - `ClientError` gains `Stream { url: String, detail: String }`.

- [ ] **Step 1: Write the failing tests** against `zeta_ws::testing::spawn` (dev-dep, already present):

```rust
#[tokio::test]
async fn subscribe_yields_decoded_iq_blocks() {
    // passthrough job on tone; subscribe; collect until 3 Iq received: meta.sample_rate_hz == 48_000, samples.len() == 4800, t0 strictly rising by 100_000_000 ns
}

#[tokio::test]
async fn subscribe_keeps_credit_topped_up() {
    // receive 1000 Iq messages within 15 s with Limits::default() (the server would stall at 256 without top-ups)
}

#[tokio::test]
async fn unauthorized_subscribe_is_a_client_error() {
    // wrong secret → Err(ClientError::Unauthorized { .. })
}

#[tokio::test]
async fn server_stop_ends_subscription_with_reason() {
    // stop the TestServer; next() → Some(Ok(Received::…)) until the close, then None; close_reason() == Some("node shutting down")
}
```

- [ ] **Step 2: Run** `cargo test -p zeta-client --test ws` — FAIL.
- [ ] **Step 3: Implement `ws.rs`**; `VrtDecoder` per subscription; a decoder error on one message is returned as `Err(ClientError::Stream {..})` and the subscription continues.
- [ ] **Step 4: Run** `cargo test -p zeta-client` and `cargo test -p zeta-ws` — PASS.
- [ ] **Step 5: Commit** `git commit -am "feat(client): binary stream subscription with credits"`

### Task 5: The remote node port

**Files:**
- Create: `crates/zeta-source-node/Cargo.toml` (deps: `zeta-core`, `zeta-client`, `zeta-vrt`, `tokio`, `async-trait`, `thiserror`, `num-complex`), `src/lib.rs`, `src/port.rs`
- Test: `crates/zeta-source-node/tests/port.rs` (dev-dep `zeta-ws` with `test-server`)

**Interfaces:**
- Consumes: `NodeClient::{apply, streams, subscribe}`, `Op::{SourceList, JobStart, JobStop}`, `OpResult::{Sources, Job, Streams}`; `Port` trait; `IqBlock`.
- Produces:
  - `pub const NODE_DEFAULT_MAX_BANDWIDTH_HZ: u64 = 250_000`
  - `pub struct NodeSourceConfig { pub id: SourceId, pub url: String, pub secret: String, pub remote_source: SourceId, pub max_bandwidth_hz: u64 }` (`Debug` prints `secret: "***"`)
  - `pub struct NodePort { … }`; `pub async fn open(cfg: NodeSourceConfig) -> Result<NodePort, PortError>`: one `source_list` round-trip; the remote source must exist (`Unsupported("remote node URL has no source X; known: …")`); capabilities copied with `remote: true`, `push: true`, `max_bandwidth_hz = Asserted(min(remote.max_bandwidth_hz, cfg.max_bandwidth_hz))`, formats `[Cf32]`.
  - `Port::tune(t)`: `bandwidth_hz > cap` → `OutOfRange { field: "bandwidth_hz", value, min: 1, max: cap, suggested: Some(cap) }` with no remote call; otherwise stops the previous remote job if any, `job_start { kind: passthrough, source: remote_source, center_hz, bandwidth_hz, deviation_hz: null }`, `stream_list` to find the stream with `provenance.job == id` and `kind == Iq`, subscribes, spawns the pump task; returns the capture band from the first context packet (`center_hz`, `bandwidth_hz = sample_rate_hz`), waiting at most `TUNE_TIMEOUT = 10 s` (`Io("remote node URL: no iq within 10 s")`). A remote `OpError` becomes `PortError::Unsupported(format!("remote node {url}: {err}"))` (secret never in the text).
  - Pump: `Received::Iq` → `IqBlock { t0: Timestamp(t0_ns), sample_rate_hz, center_hz, samples }` on a broadcast channel of capacity 64; `missed_packets > 0` is just a `t0` jump (the Port contract); `Received::Lagged` likewise; a `ClientError::Stream` is logged to stderr and skipped; subscription end sets `fault = Some(format!("remote node {url}: stream closed: {reason}"))` and the pump exits; the pump also exits when the sender has no receivers.
  - `Port::current` returns the last capture band; `fault()` as above; `Drop` for `NodePort` spawns `job_stop` on the remote job best-effort (same on retune, awaited).

- [ ] **Step 1: Write the failing tests** (each spawns a `TestServer` as the remote with its `tone` source; `NodeSourceConfig { url: srv.url, secret: "s3cret", remote_source: "tone", max_bandwidth_hz: 250_000 }`):

```rust
#[tokio::test]
async fn open_copies_capabilities_as_remote_push() { /* remote: true, push: true, formats == [Cf32], tune range == tone's */ }

#[tokio::test]
async fn tune_starts_remote_passthrough_and_streams_blocks() {
    // tune 48 kHz at the tone centre → returned bandwidth_hz == 48_000; remote job_list has one passthrough job holding tone;
    // subscribe(), 3 blocks: 4800 samples each, t0 rising by 100_000_000, |mean power - tone power| small
}

#[tokio::test]
async fn bandwidth_over_cap_is_out_of_range_without_remote_job() {
    // tune 300_000 → OutOfRange { max: 250_000, suggested: Some(250_000) }; remote job_list empty
}

#[tokio::test]
async fn retune_stops_previous_remote_job() { /* tune twice; remote job_list: first Stopped, second Running */ }

#[tokio::test]
async fn drop_stops_remote_job() { /* drop port; within 2 s remote job_list shows Stopped */ }

#[tokio::test]
async fn server_stop_faults_port_and_names_url() {
    // tune, read one block, srv.stop(); within 3 s port.fault() == Some(s) with s containing the url and "stream closed"; no secret in s
}

#[tokio::test]
async fn open_against_unknown_remote_source_names_known() { /* Unsupported containing "tone" */ }
```

- [ ] **Step 2: Run** `cargo test -p zeta-source-node` — FAIL.
- [ ] **Step 3: Implement**. Note the remote's `stream_list` ordering: pick by provenance, not position.
- [ ] **Step 4: Run** `cargo test -p zeta-source-node` — PASS.
- [ ] **Step 5: Commit** `git add crates/zeta-source-node && git commit -m "feat(source-node): another zetasdr node as a push source"`

### Task 6: CLI `--source-node`, guide, two-node decode test

**Files:**
- Modify: `crates/zeta/Cargo.toml` (add `zeta-source-node`), `crates/zeta/src/sources.rs`, `crates/zeta/src/cli.rs`, `crates/zeta/src/node_setup.rs`, `crates/zeta/src/agents_md.rs`, `docs/agents/field-craft.md`, regenerate `AGENTS.md` and `skills/zetasdr/SKILL.md`
- Test: `crates/zeta/tests/cli.rs`, unit tests in `sources.rs`

**Interfaces:**
- Consumes: `NodeSourceConfig`, `NodePort::open` (Task 5); `SourceSpec`, `split_id`, `kiwi_password_env` pattern (plan 4 Task 15).
- Produces:
  - `SourceSpec::Node { id: String, url: String, remote_source: String, max_bandwidth_hz: u64 }` parsed from `--source-node ID=SOURCE@URL[,max_bandwidth_hz=N]` (`URL` is `http://host:port`; `max_bandwidth_hz` 1..=20_000_000; anything else is a usage error naming the part).
  - `pub fn node_secret_env(id: &str) -> String` = `ZETA_NODE_SECRET_<ID>` (same mapping as the Kiwi one); missing or empty → usage error "`--source-node ID`: set ZETA_NODE_SECRET_<ID>" (exit 1). The secret is never in `Debug`/`Display` of the spec.
  - `--source-node` is rejected with `--node` like the other source flags.
  - `agents_md.rs` RUNNING section: the flag, the env variable, the 250 kHz default cap and that a remote source holds a `passthrough` job on the other node; `field-craft.md` gains a paragraph on federation (Pi ships narrowband; widen explicitly; the remote `source list` shows the holder).

- [ ] **Step 1: Write the failing tests**

```rust
// sources.rs unit tests
#[test] fn node_spec_parses_and_defaults_cap() { /* "pi=hack@http://pi.local:7979" → max_bandwidth_hz 250_000 */ }
#[test] fn node_spec_accepts_cap_and_rejects_bad_parts() { /* ",max_bandwidth_hz=2000000" ok; "pi=http://x" (no @) err names "SOURCE@URL"; cap 0 err */ }
#[test] fn node_secret_env_name() { assert_eq!(node_secret_env("pi-5"), "ZETA_NODE_SECRET_PI_5"); }

// tests/cli.rs (two real processes: node A = `zeta serve --listen 127.0.0.1:0`? the CLI has no port-0 echo, so use a free port from `std::net::TcpListener::bind("127.0.0.1:0")` released before spawning)
#[test]
fn measure_over_federation_labels_remote_tone_as_carrier() {
    // A: serve --listen 127.0.0.1:P --source-file tone=<tone.sigmf-meta> with ZETA_SECRET=s; wait for /health
    // B: ZETA_NODE_SECRET_PI=s zeta --state-dir B --source-node pi=tone@http://127.0.0.1:P job start --kind measure --source pi --center-hz <tone c> --bandwidth-hz 48000 --then-tail 1,8000
    // stdout: one event with label.name == "carrier"; A's `job list` (via --node) shows a passthrough job on tone; after B exits, within 3 s A's job list shows it Stopped
    // stdout+stderr never contain "s" as a bearer (assert !contains("ZETA_NODE_SECRET") and !contains("Bearer"))
}

#[test]
fn aprs_decode_over_federation_yields_the_packet() {
    // same shape with the APRS fixture (write_aprs from zeta-source-file test-fixtures): --kind aprs_decode --then-tail 1,8000 → event with src "VK3ABC"
}

#[test]
fn source_node_without_secret_is_usage_error() { /* exit 1, stderr contains "ZETA_NODE_SECRET_PI" */ }

#[test]
fn source_node_is_rejected_with_node() { /* existing rejection message */ }
```

- [ ] **Step 2: Run** `cargo test -p zeta --all-features` — FAIL (unknown flag).
- [ ] **Step 3: Implement**, regenerate the guide and skill (`zeta agents-md` with its `--check`/`--skill` flags as plan 4 did).
- [ ] **Step 4: Run** `cargo test -p zeta --all-features`; `zeta agents-md --check`; `--skill --check` — PASS.
- [ ] **Step 5: Commit** `git commit -am "feat(cli): --source-node federates another node's source; guide updated"`

### Task 7: VRT UDP ingest port

**Files:**
- Create: `crates/zeta-source-vrt/Cargo.toml` (deps: `zeta-core`, `zeta-vrt`, `tokio` with `net`, `async-trait`, `num-complex`), `src/lib.rs`
- Modify: `crates/zeta/src/sources.rs`, `cli.rs`, `node_setup.rs`, `agents_md.rs` (one bullet), regenerate guide/skill
- Test: `crates/zeta-source-vrt/tests/udp.rs`, `crates/zeta/tests/cli.rs` (one test)

**Interfaces:**
- Produces:
  - `pub struct VrtSourceConfig { pub id: SourceId, pub bind: SocketAddr }`; `pub async fn open(cfg) -> Result<VrtPort, PortError>` binds the UDP socket (`Io` on failure); capabilities: `tune_min_hz: Asserted(0)`, `tune_max_hz: Asserted(u64::MAX / 2)`, `max_bandwidth_hz: Asserted(20_000_000)`, formats `[Cf32]`, `remote: true`, `push: true`.
  - Receive loop (spawned at open, exits on drop): 65 536-byte buffer; each datagram is one VRT packet through a `VrtDecoder`; errors are counted (`pub fn errors(&self) -> u64`) and the loop continues; `Decoded` → `IqBlock` on a broadcast of capacity 64; `current()` is the meta of the last context packet as a capture band (`bandwidth_hz = sample_rate_hz`).
  - `tune(t)`: a VRT sender is not tunable; succeeds iff `current()` is `Some(band)` and `t` lies inside it (centre ± sample_rate/2 contains `t.center ± t.bandwidth/2`) returning `band`; else `Unsupported("vrt source ID: sender is at CENTER ± RATE/2 (or: no context packet received yet); retune the sender")`.
  - CLI: `--source-vrt ID=udp://ADDR:PORT` (`ADDR` must be LAN per `is_lan`, which the CLI reimplements minimally: loopback, private, link-local, unspecified).

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn udp_packets_become_blocks_and_tune_follows_context() {
    // open on 127.0.0.1:0 (read bound addr: `pub fn local_addr(&self)`); encode 48 kHz blocks with VrtEncoder and send over a UdpSocket;
    // current() == Some(Tuning { center 1_000_000, bandwidth 48_000 }); tune(1_000_000, 24_000) → Ok(band); tune(2_000_000, 24_000) → Unsupported;
    // subscribe() yields blocks with 480 samples and rising t0
}

#[tokio::test]
async fn garbage_datagrams_are_counted_not_fatal() { /* send b"nope" ×3 then good packets; errors() == 3; blocks still arrive; fault() is None */ }

#[tokio::test]
async fn tune_before_any_context_is_unsupported_and_says_so() { /* "no context packet received yet" */ }

// cli.rs
#[test]
fn source_vrt_measure_labels_sent_tone_as_carrier() {
    // spawn zeta --source-vrt t=udp://127.0.0.1:P job start --kind measure --source t --center-hz 1000000 --bandwidth-hz 48000 --then-tail 1,8000
    // meanwhile the test sends 2 s of a 1 kHz tone at 48 kHz via VrtEncoder+UdpSocket (10 ms blocks, real time);
    // stdout has one event labelled "carrier"
}
```

- [ ] **Step 2: Run** `cargo test -p zeta-source-vrt` — FAIL.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run** `cargo test -p zeta-source-vrt` and `cargo test -p zeta --all-features` — PASS; guide drift checks pass.
- [ ] **Step 5: Commit** `git add crates/zeta-source-vrt && git commit -am "feat(source-vrt): vita 49.2 over udp as a push source"`

### Task 8: Pi 5 build and deployment

**Files:**
- Create: `deploy/pi/build-pi.sh`, `deploy/pi/install.sh`, `deploy/pi/zetasdr.service`, `deploy/pi/zetasdr.env.example`, `deploy/pi/README.md`
- Modify: `.gitignore` (add `/deploy/pi/out/`), `Cross.toml` at the repo root
- Test: `deploy/pi/smoke.sh` run by hand in this task and by Task 9's gate

**Interfaces:**
- Produces:
  - `build-pi.sh`: `cargo install cross --locked` if absent (needs docker, which the dev box has), then `cross build --release --target aarch64-unknown-linux-gnu -p zeta -p zeta-mcp`, copies the two binaries to `deploy/pi/out/`, prints their sizes and sha256. Soapy is not built (nothing is attached to the Pi yet); the README says how to add it later (`RUSTFLAGS="--cfg soapy"` on the Pi itself after `apt install libsoapysdr-dev`).
  - `smoke.sh`: runs `qemu-aarch64-static deploy/pi/out/zeta schema` (the box has `/usr/bin/qemu-aarch64-static`) and checks the output parses as JSON with an `op` key; runs `… zeta --help` and checks `--source-node` appears. Exit 0 on success.
  - `install.sh` (runs on the Pi as root): installs the binaries to `/usr/local/bin`, creates user `zetasdr` with `/var/lib/zetasdr` as state dir, copies `zetasdr.env.example` to `/etc/zetasdr/env` (mode 0600) if absent, installs and enables the unit.
  - `zetasdr.service`: `User=zetasdr`, `EnvironmentFile=/etc/zetasdr/env`, `ExecStart=/usr/local/bin/zeta --state-dir /var/lib/zetasdr $ZETA_SOURCES serve --listen 0.0.0.0:7979`, `Restart=on-failure`, `MemoryMax=1G`, `Nice=-5`. `zetasdr.env.example`: `ZETA_SECRET=change-me-to-16-plus-chars`, `ZETA_SOURCES=` with a commented Kiwi example and its `ZETA_KIWI_PASSWORD_*` line. Note that `serve` binds LAN addresses only, and `0.0.0.0` is accepted by `is_lan` (unspecified).
  - `README.md`: build on the dev box, copy with `scp`, `sudo ./install.sh`, set the secret, `systemctl status zetasdr`, then on the laptop `ZETA_NODE_SECRET_PI=… zeta --source-node pi=kiwi@http://pi.local:7979 source list`. States the WiFi budget: 250 kHz ci16 ≈ 8 Mbit/s.

- [ ] **Step 1: Write `smoke.sh` first** and run it: FAIL (no `out/`).
- [ ] **Step 2: Write `Cross.toml`** (`[build] default-target = "aarch64-unknown-linux-gnu"`) and `build-pi.sh`; run it. Expected: two binaries in `deploy/pi/out/`, `file deploy/pi/out/zeta` says `ELF 64-bit LSB … ARM aarch64`.
- [ ] **Step 3: Run `smoke.sh`** — PASS (prints the schema's first line and the help line).
- [ ] **Step 4: Write `install.sh`, the unit, the env example, the README.** `bash -n` both scripts; `systemd-analyze verify deploy/pi/zetasdr.service` if available (it may complain about the missing user; that is fine).
- [ ] **Step 5: Commit** `git add Cross.toml deploy .gitignore && git commit -m "build(pi): cross-build, smoke test and systemd deployment for a pi 5 node"`

### Task 9: Whole-workspace verification, README, plan series

**Files:**
- Modify: `README.md` (new section "Federation": the node source, the VRT source, the binary websocket and credits, the Pi deployment pointer), `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (row 5 **Delivered**, a "Plan 5 status" paragraph)

- [ ] **Step 1: Gate**: `cargo fmt --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features` PASS; `cargo test -p zeta-core --test no_privileged_deps` PASS; `zeta agents-md --check` and `--skill --check` PASS; `deploy/pi/smoke.sh` PASS.
- [ ] **Step 2: By hand, two nodes on this box**: terminal A `ZETA_SECRET=correct-horse-battery zeta --state-dir scratch/A --source-file tone=scratch/fx/tone.sigmf-meta serve --listen 127.0.0.1:7979`; terminal B `ZETA_NODE_SECRET_A=correct-horse-battery zeta --state-dir scratch/B --source-node a=tone@http://127.0.0.1:7979 serve --listen 127.0.0.1:7980`; then with `--node http://127.0.0.1:7980`: `source list` (a: remote true, push true, max_bandwidth_hz 250000), `job start --kind measure --source a …` and `stream tail` showing carrier events; on A `source list` shows tone held by a passthrough job; `job stop` on B releases it on A within 3 s. Paste outputs into the report.
- [ ] **Step 3: By hand, if a HackRF is attached**: A serves `--source-soapy hack=driver=hackrf` (release build with `--cfg soapy`), B federates `--source-node hack=hack@http://127.0.0.1:7979`, `fm_demod` at 104.9 MHz with 200 kHz on B tails audio frames. Record whether it ran.
- [ ] **Step 4: Commit** `git commit -am "docs: federation section; plan series updated"`

---

## Self-review notes

- Spec coverage: 4.1 "another ZetaSDR node" as a v1 source and "remote is the normal case" (Task 5, 6); 4.3 IQ as VITA 49.2 between nodes, other kinds on the typed envelope on the same transport (Tasks 1–4); 4.7 narrowband by default, wideband explicit, flow control and back-pressure from v1 (cap in Task 5, credits in Task 3), layer 1 runs where the samples are (unchanged: `measure` on the Pi is a job like any other); 3 Constraints: Pi 5 on WiFi ships narrowband (default cap), LAN only with a shared secret (Task 6 env secret, `is_lan` retained), permissive licence (no new GPL deps); 5.2 "HTTP/websocket + JSON: … remote nodes" (Task 3 is an extension of that websocket, not a new transport); 8 item 7 "runs on the Pi (headless, source and cheap stages) and the laptop, federated" (Tasks 8, 9). Discovery is deferred (the spec says "nodes discover and talk"; v1 is talk by URL: ruling recorded here, discovery is a later plan). 
- Names used across tasks checked: `stream_id_of`, `Header`, `PacketKind`, `DataPacket`, `ContextPacket`, `IqMeta`, `VrtEncoder`, `VrtDecoder`, `Decoded`, `VrtError`, `MAX_SAMPLES_PER_PACKET`, `CONTEXT_INTERVAL_NS`, `envelope`, `parse_envelope`, `WsMessage::Credit`, `CREDIT_CAP`, `CREDIT_MAX_GRANT`, `Subscription`, `Received`, `NodeClient::subscribe`, `NodeSourceConfig`, `NodePort`, `NODE_DEFAULT_MAX_BANDWIDTH_HZ`, `TUNE_TIMEOUT`, `SourceSpec::Node`, `node_secret_env`, `VrtSourceConfig`, `VrtPort`.
- Review Focus items each have a named test: 1 → Task 3, 2 → Task 5, 3 → Task 5, 4 → Tasks 1 and 2, 5 → Task 5.
- Known limitation stated for the executor: a remote `passthrough` job is orphaned if the federating node dies without dropping the port; the remote's `job list` shows it and `job stop` clears it. Tagging ownership needs a context-write op, which this plan does not add.
- Values that are the author's call, not the spec's: FNV-1a stream ids; RF reference frequency (not IF) carries the centre; ci16 only on the wire; credits of 256/128; 16 384 samples per packet; 1 s context cadence; 250 kHz default cap; UDP ingest with no sender.
