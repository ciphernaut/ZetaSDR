# Plan 28.1: the source transport, made rate-proof — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Soapy worker child hands its IQ to the node through a shared-memory ring instead of the stdout pipe, each source measures and reports the rate it can deliver, and the lease refuses a wider request naming the cap, so a 30.72 Msps B210 capture (and every wider front end to come) reaches the stages without loss.

**Architecture:** Worker protocol 2 keeps the one ordered stdout stream for control and adds a 48-byte *block descriptor* frame; the samples go into a memfd ring the node creates and the child inherits as fd 3 (`cf32` slots of a fixed size, the child dropping and counting when the ring is full, the node releasing slots as it copies them out). A protocol-1 child (blocks on the pipe) is still accepted, which is also the fallback when the ring cannot be made. At open the worker probes the device's rate ladder from the top and reports the first rung that streams clean as `max_bandwidth_hz: measured`, cached with the capabilities so a respawn skips the probe; `source_list` shows the transport and its drops, and `OpError::OutOfRange` says `insufficient bandwidth` when a measured cap refuses.

**Tech Stack:** Rust 1.97 workspace; `libc` (already a dependency of `zeta-source-worker`) for `memfd_create`, `mmap`, `dup2`; tokio process with `pre_exec`; no new crates.

**Spec:** `docs/superpowers/specs/2026-11-06-video-decode-design.md`, sections 3 (constraints), 5.6 (insufficient bandwidth) and 5.7 (the source transport). Plan 28.2 (the video stages and page) follows this plan.

## Global Constraints

- `zeta-core` takes no new dependency (`crates/zeta-core/tests/no_privileged_deps.rs`); the ring and the fd passing live in `zeta-source-worker`.
- The worker is exec'd from `/proc/self/exe`, so node and child are always the same build; protocol 1 stays accepted on the node side for the fake worker's `ZETA_FAKE_PROTOCOL=1` and as the pipe fallback, never because an older child could appear.
- Sample format in the ring and on the pipe is `cf32` (`Complex<f32>`, little endian). The hello declares it (`format: "cf32"`) so `sc16` can follow; this plan does not implement `sc16` (a shared-memory copy at 246 MB/s is a memcpy, the pipe was the cost).
- Every struct on the wire is `#[serde(deny_unknown_fields)]` except where this plan says `#[serde(default)]` for compatibility (`WorkerInfo.transport`, `Status.transport_dropped`).
- Repository rules: roles, never host names, in tracked files (`the field host`); `bash scripts/public-check.sh` and `bash scripts/check-doc-links.sh` clean before `main`; `AGENTS.md` and `skills/zetasdr/SKILL.md` regenerated, never edited.
- Gates: `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`, `cargo run -q -p zeta -- agents-md --check`, `cargo run -q -p zeta -- agents-md --skill --check`. Use a private `CARGO_TARGET_DIR`.
- Field-host commands go through `deploy/field-host/z.sh` (node calls) and `deploy/field-host/deploy-sdr.sh <name>` (deploy); the radio must be free for a sweep, which is the operator's call (never `jobs_stop` unasked).

## Review Focus

1. **A descriptor naming a slot the ring does not have** (a corrupt frame, a child of another build): the node must treat it as a bad frame (end the reader, fault the worker as today), never index past the mapping. Pinned in Task 4 (`a_descriptor_outside_the_ring_is_a_bad_frame`).
2. **A node reader that stalls** (the broadcast's receivers slow, the thread descheduled): the child must keep streaming, drop into `transport_dropped`, and never block on the ring; the drops must show in `source_list.worker.transport.dropped`. Pinned in Task 3 (`a_full_ring_drops_and_counts`) and Task 4 (`drops_reach_source_list`).
3. **A respawn while the old child still runs** (a hang killed at the hello timeout): the new child must get a fresh ring; a late write by the old child into its old ring must reach nothing. Pinned in Task 4 (`each_spawn_gets_its_own_ring`).
4. **A platform or sandbox without `memfd_create`**: the node falls back to the pipe (protocol 1 blocks) and says so in `transport.path`, rather than refusing the source. Pinned in Task 4 (`without_a_ring_the_child_streams_on_the_pipe`).
5. **A device whose probe cannot stream clean at any rung** (a loose cable at open): the probe must end within its bound, the cap must fall to the lowest rung tried, and the source must still attach (the operator sees a small `measured` cap, not a refused flag). Pinned in Task 5 (`a_probe_that_never_streams_clean_caps_at_the_lowest_rung`).

---

### Task 0: The sweep before (the field host, the B210)

**Files:**
- Create: `scripts/rate-sweep.sh`
- Modify: `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (a "Plan 28.1 status" section with the table)

**Interfaces:**
- Produces: `scripts/rate-sweep.sh <zeta-invocation> <source> <center_hz> <rate_hz>...` — for each rate: `source pin --source <s> --center-hz <c> --bandwidth-hz <rate>`, sleep 60, `source list` and print one line `rate=<r> measured=<stats.measured_rate_hz> overflows=+<delta> timeouts=+<delta> restarts=+<delta> fault=<fault>`; `source unpin` at the end. `<zeta-invocation>` is the client command (`deploy/field-host/z.sh` on the field host, `zeta --node URL` elsewhere); needs `jq`.

- [ ] **Step 1: Write the script** with `set -euo pipefail`, the deltas taken against the `source list` before each pin, and a trap that unpins on exit.

- [ ] **Step 2: Run it dry** against a file-source node: `cargo run -q -p zeta -- serve --source-file tone=crates/zeta-dsp/tests/data/ook-ev1527-synth-1.sigmf-meta --listen 127.0.0.1:7979 &` then `bash scripts/rate-sweep.sh "cargo run -q -p zeta -- --node http://127.0.0.1:7979" tone <centre> 1000000`.
Expected: one line with `measured=` a number and no error.

- [ ] **Step 3: Ask the operator for a window with the B210 free**, then run on the field host: `bash scripts/rate-sweep.sh deploy/field-host/z.sh b210 1090000000 15360000 20480000 30720000 61440000`. Watch `top` on the sdr side for the node's and the worker's CPU during each minute.

- [ ] **Step 4: Record the table** in the plan-series status section ("before": rate, delivered, overflows/min, worker CPU, node CPU, fault if any). `61.44` is expected to fault with an overflow storm and heal; that is a row.

- [ ] **Step 5: Commit**

```bash
git add scripts/rate-sweep.sh docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md
git commit -m "Plan 28.1: the rate sweep script and the B210's numbers before the transport change"
```

### Task 1: The ring

**Files:**
- Create: `crates/zeta-source-worker/src/ring.rs`
- Modify: `crates/zeta-source-worker/src/lib.rs` (`pub mod ring`)
- Test: `crates/zeta-source-worker/src/ring.rs` (unit tests)

**Interfaces:**
- Produces:
  - `pub const SLOT_SAMPLES: usize = 262_144` (2 MiB of `cf32` per slot); `pub const DEFAULT_RING_BYTES: usize = 64 << 20`; `pub const MIN_SLOTS: usize = 4`.
  - `pub struct Ring` (a mapped memfd; `Send + Sync`): `pub fn create(bytes: usize) -> io::Result<Ring>` (memfd_create `zeta-ring`, ftruncate, mmap shared, header written: magic `b"ZRNG"`, version 1, `slot_samples`, `slots`, `head: AtomicU64`, `tail: AtomicU64`, 64 bytes); `pub fn open(fd: OwnedFd) -> io::Result<Ring>` (fstat for the size, mmap, header checked: magic, version, `slots >= MIN_SLOTS`); `pub fn fd(&self) -> BorrowedFd<'_>`; `pub fn slots(&self) -> usize`; `pub fn bytes(&self) -> usize`.
  - Producer side: `pub fn try_write(&self, seq: u64, samples: &[Complex<f32>]) -> Result<u32, RingFull>` — `samples.len() <= SLOT_SAMPLES`; refuses with `RingFull` when `seq - tail >= slots` (Acquire load of `tail`); copies into slot `(seq % slots)`, stores `head = seq + 1` (Release), returns the slot index.
  - Consumer side: `pub fn read(&self, seq: u64, slot: u32, n: u32) -> Result<Arc<[Complex<f32>]>, RingError>` — `slot < slots`, `n <= SLOT_SAMPLES`, `slot == (seq % slots) as u32`, else `RingError::BadDescriptor(String)`; copies the slot out; `pub fn release(&self, seq: u64)` stores `tail = seq + 1` (Release).
  - `pub struct RingFull; pub enum RingError { BadDescriptor(String) }`.
  - `pub fn supported() -> bool`: `memfd_create` works here (a 4 KiB create and close).

- [ ] **Step 1: Write the failing tests** in `ring.rs`:
  - `a_block_round_trips_through_a_slot`: create 4 slots, `try_write(0, 1000 samples)` → `Ok(0)`, `read(0, 0, 1000)` equals, `release(0)`.
  - `a_full_ring_refuses_until_released`: write seqs 0..4 fine, seq 4 → `Err(RingFull)`, `release(0)`, seq 4 → `Ok(0)`.
  - `a_descriptor_that_does_not_match_the_sequence_is_refused`: `read(5, 0, 10)` → `BadDescriptor` (seq 5 maps to slot 1), `read(0, 9, 10)` → `BadDescriptor`, `read(0, 0, SLOT_SAMPLES as u32 + 1)` → `BadDescriptor`.
  - `open_on_the_duplicated_fd_sees_the_producers_writes`: `create`, `open(fd.try_clone_to_owned())`, write on one, read on the other.
  - `a_ring_too_small_for_min_slots_is_refused`: `create(1 << 20)` → `Err` (fewer than 4 slots of 2 MiB).

- [ ] **Step 2: Run them to see them fail**: `cargo test -p zeta-source-worker --lib ring`
Expected: compile error, `ring` not found.

- [ ] **Step 3: Implement `ring.rs`** with the signatures above. `libc::memfd_create` with `MFD_CLOEXEC`; `mmap(PROT_READ|PROT_WRITE, MAP_SHARED)`; the header's atomics through `AtomicU64::from_ptr` on the mapping; `Drop` unmaps. `slots = (bytes - 64) / (SLOT_SAMPLES * 8)`.

- [ ] **Step 4: Run the tests**: `cargo test -p zeta-source-worker --lib ring`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/zeta-source-worker/src/ring.rs crates/zeta-source-worker/src/lib.rs
git commit -m "Worker: a shared-memory ring of cf32 slots (memfd), producer and consumer halves"
```

### Task 2: Wire protocol 2

**Files:**
- Modify: `crates/zeta-source-worker/src/wire.rs` (module docs; `PROTOCOL = 2`; `Hello.transport`; `Status.transport_dropped`; `Frame::Descriptor`; `KIND_DESCRIPTOR = 2`)
- Test: `crates/zeta-source-worker/tests/wire.rs`

**Interfaces:**
- Produces:
  - In `zeta-core/src/port.rs`: `pub struct TransportInfo { pub path: TransportPath, pub format: String /* "cf32" */, pub ring_bytes: u64 }`, `pub enum TransportPath { Shm, Pipe }` (snake_case, `JsonSchema`, `deny_unknown_fields`; used by `WorkerInfo` in Task 4); `wire.rs` uses them by that name.
  - `Hello { protocol, capabilities, gain, driver, #[serde(default)] transport: Option<TransportInfo> }`: `None` from a protocol-1 child.
  - `Status { fault, current, gain, stats, #[serde(default)] transport_dropped: u64 }`: blocks the child dropped for a full ring (or a full pipe queue), cumulative.
  - `pub struct Descriptor { pub seq: u64, pub slot: u32, pub t0_ns: u64, pub sample_rate_hz: u64, pub center_hz: u64, pub n: u32 }` — 40 bytes little endian in that order; `Frame::Descriptor(Descriptor)` with kind byte 2.
  - `write_frame`/`read_frame` carry it; a kind-2 payload of the wrong length is `InvalidData`.

- [ ] **Step 1: Write the failing tests** in `tests/wire.rs`:
  - `a_descriptor_round_trips_byte_for_byte`: write `Frame::Descriptor{seq: 7, slot: 3, t0_ns: 10, sample_rate_hz: 30_720_000, center_hz: 5_845_000_000, n: 262_144}`, read back equal; the payload is 40 bytes.
  - `hello_carries_the_transport_and_reads_without_it`: a protocol-2 hello with `transport: Some(shm, "cf32", 67108864)` round-trips; the JSON `{"hello":{"protocol":1,"capabilities":…,"gain":null,"driver":"x"}}` (no `transport`) parses with `transport: None`.
  - `status_reads_without_transport_dropped`: a status JSON without the field parses with `0`.
  - extend `a_truncated_frame_is_an_error_and_a_clean_eof_is_none`: a kind-2 payload of 39 bytes is `InvalidData`.

- [ ] **Step 2: Run them to see them fail**: `cargo test -p zeta-source-worker --test wire`
Expected: compile errors on `Frame::Descriptor`, `transport`.

- [ ] **Step 3: Implement** the types in `zeta-core/src/port.rs` (`TransportInfo`, `TransportPath`, `deny_unknown_fields`, `JsonSchema`) and the wire changes in `wire.rs`; update the module docs to describe protocol 2 (descriptor frames, the ring, the hello's transport, `transport_dropped`).

- [ ] **Step 4: Run the tests**: `cargo test -p zeta-source-worker --test wire`
Expected: all pass. Then `cargo test -p zeta-source-worker` to see what the protocol bump broke downstream (the fake worker's `ZETA_FAKE_PROTOCOL` test expects `expected 1` text; fix in Task 4).

- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core/src/port.rs crates/zeta-source-worker/src/wire.rs crates/zeta-source-worker/tests/wire.rs
git commit -m "Worker wire protocol 2: block descriptors for a shared-memory ring, the hello's transport, dropped blocks in the status"
```

### Task 3: The child writes the ring

**Files:**
- Modify: `crates/zeta-source-worker/src/serve.rs` (`serve` gains a `ring: Option<Ring>` parameter; the pump; `Writer::block` → `Writer::descriptor`; the status loop reports drops), `crates/zeta-source-worker/src/bin/zeta-fake-worker.rs` (`--ring-fd N`; `ZETA_FAKE_NO_RING=1` ignores it), `crates/zeta/src/cli.rs` (`SoapyWorkerArgs.ring_fd: Option<i32>`), `crates/zeta/src/soapy_worker.rs` (opens the ring from the fd, passes it to `serve`)
- Test: `crates/zeta-source-worker/tests/serve.rs`

**Interfaces:**
- Consumes: `Ring::open`, `Ring::try_write` (Task 1); `Frame::Descriptor`, `Hello.transport`, `Status.transport_dropped` (Task 2).
- Produces: `pub async fn serve(port, driver, stdin, stdout, ring: Option<Ring>) -> ExitCode`. With a ring: the hello says `protocol: 2, transport: {shm, cf32, ring.bytes()}`; the pump splits each `IqBlock` into pieces of at most `SLOT_SAMPLES` (piece `t0 = t0 + offset / rate`, in ns), writes each with the next `seq`, and queues a `Descriptor` on the writer; a `RingFull` drops the piece and counts it. Without a ring the child still says `protocol: 2`, with `transport: Some({pipe, cf32, 0})`, and sends kind-1 blocks on the pipe as today (a hello with no `transport` at all is what a protocol-1 child sends). The status's `transport_dropped` is the drop count (ring full, or the pipe queue past `MAX_QUEUED_BLOCKS`).
- The test harness in `tests/serve.rs`: `spawn_with` gains a `ring: Option<&Ring>` argument that passes `--ring-fd 3` and `pre_exec`-dups the ring's fd to 3 (the same helper Task 4's supervisor uses, so put it in `crates/zeta-source-worker/src/spawn.rs`: `pub fn inherit_ring(cmd: &mut tokio::process::Command, ring: &Ring)` — `unsafe { cmd.pre_exec(dup2 to 3, clear CLOEXEC) }`).

- [ ] **Step 1: Write the failing tests** in `tests/serve.rs`:
  - `with_a_ring_the_hello_says_shm_and_blocks_arrive_as_descriptors`: create a ring, spawn `stream`, read the hello (`protocol 2`, `transport.path == Shm`, `ring_bytes == ring.bytes()`), tune, read three `Frame::Descriptor`s with rising `seq` 0,1,2 and `t0_ns` rising; `ring.read(seq, slot, n)` gives `n` samples; `release` each.
  - `a_full_ring_drops_and_counts`: do not release any slot; after `slots + 8` descriptors' worth of time, the next `Status` has `transport_dropped >= 8`, and the child is still alive (a tune is still answered).
  - `without_a_ring_blocks_come_on_the_pipe`: spawn with no `--ring-fd`: hello `transport.path == Pipe`, frames are `Frame::Block`.
  - `no_ring_env_makes_the_fake_ignore_the_fd`: `ZETA_FAKE_NO_RING=1` with a ring passed: `transport.path == Pipe`.
  - `a_block_wider_than_a_slot_is_split`: a mock scripted to read 600_000 samples per read; the descriptors have `n <= SLOT_SAMPLES` and consecutive `t0_ns` differing by `n / rate` in ns.

- [ ] **Step 2: Run them to see them fail**: `cargo test -p zeta-source-worker --test serve`
Expected: compile errors (`serve` arity, `--ring-fd` unknown).

- [ ] **Step 3: Implement** `serve`'s ring path, the pump's split and drop count, `spawn.rs::inherit_ring`, the fake worker's `--ring-fd` and `ZETA_FAKE_NO_RING`, `SoapyWorkerArgs.ring_fd` and `soapy_worker.rs` opening `OwnedFd::from_raw_fd(ring_fd)` into `Ring::open` (a failure to open logs one stderr line and serves on the pipe).

- [ ] **Step 4: Run the tests**: `cargo test -p zeta-source-worker --test serve`
Expected: all pass, the pre-existing ones included.

- [ ] **Step 5: Commit**

```bash
git add crates/zeta-source-worker/src/serve.rs crates/zeta-source-worker/src/spawn.rs crates/zeta-source-worker/src/lib.rs crates/zeta-source-worker/src/bin/zeta-fake-worker.rs crates/zeta-source-worker/tests/serve.rs crates/zeta/src/cli.rs crates/zeta/src/soapy_worker.rs
git commit -m "Worker: the child writes blocks into the inherited ring and sends descriptors; drops are counted in the status; --ring-fd"
```

### Task 4: The node reads the ring

**Files:**
- Modify: `crates/zeta-source-worker/src/bin/zeta-fake-worker.rs` (`ZETA_FAKE_BAD_SLOT=1`: the first descriptor names `slot: 9999`; `ZETA_FAKE_PROTOCOL=1` serves protocol 1 for real, pipe blocks and a hello without `transport`; `ZETA_FAKE_PROTOCOL=3` says hello and waits, as `=N` did), `crates/zeta-source-worker/src/supervisor.rs` (`spawn` creates a ring per spawn when `cfg.ring_bytes > 0` and `ring::supported()`, passes it with `inherit_ring`, keeps it in `Worker`; `read_stdout` handles `Frame::Descriptor` → `ring.read` → `shared.tx.send` → `ring.release`, a `RingError` is a bad frame; accepts `hello.protocol` 1 or 2; `Snapshot` keeps `transport: Option<TransportInfo>` and `transport_dropped`), `crates/zeta-source-worker/src/port.rs` (`WorkerConfig.ring_bytes: usize`; `Port::worker` fills `WorkerInfo.transport`), `crates/zeta-core/src/port.rs` (`WorkerInfo { pid, restarts, since_ns, #[serde(default)] transport: Option<TransportInfo>, #[serde(default)] transport_dropped: u64 }`), `crates/zeta/src/node_setup.rs` (`ring_bytes: DEFAULT_RING_BYTES`), `crates/zeta-core/src/ops.rs` (`require_worker` unchanged; the schema picks the new fields up)
- Test: `crates/zeta-source-worker/tests/supervisor.rs`, `crates/zeta-source-worker/tests/node.rs`

**Interfaces:**
- Consumes: `Ring::create/read/release/supported` (Task 1), `inherit_ring` (Task 3).
- Produces: `source_list[].worker.transport: {path, format, ring_bytes} | null`, `source_list[].worker.transport_dropped: N`. A protocol-1 hello is accepted (`transport` stays `null`, blocks on the pipe). The "protocol N, expected 1" refusal text becomes `protocol N, expected 1 or 2`.

- [ ] **Step 1: Write the failing tests**:
  - `tests/supervisor.rs`: `blocks_arrive_through_the_ring_and_the_worker_info_says_shm` (open with `ring_bytes: DEFAULT_RING_BYTES`, tune, receive blocks on `port.subscribe()`, `port.worker().transport.path == Shm`); `each_spawn_gets_its_own_ring` (`crash-after-ms 200` script; after the respawn, the ring fd number in the new child's `--ring-fd` is the same but `worker.since_ns` moved and blocks still arrive; assert the supervisor's `Worker.ring` is a different `Ring` by its memfd inode, exposed through a `#[cfg(feature = "test-bin")] pub fn ring_ino(&self)`); `a_descriptor_outside_the_ring_is_a_bad_frame` (the fake's `ZETA_FAKE_BAD_SLOT=1` sends a descriptor with `slot: 9999`: the worker is faulted `bad frame` and respawned, the port heals); `without_a_ring_the_child_streams_on_the_pipe` (`ring_bytes: 0`: `transport.path == Pipe`, blocks arrive); `a_protocol_1_child_is_still_accepted` (`ZETA_FAKE_PROTOCOL=1` currently means "say hello then wait": change the fake so `ZETA_FAKE_PROTOCOL=1` serves protocol 1 for real, and `ZETA_FAKE_PROTOCOL=3` is the refused one: `expected 1 or 2`).
  - `tests/node.rs`: `drops_reach_source_list` (a node over the fake with `ring_bytes` of the minimum 4 slots, a `stream` script at 20 Msps, the node's broadcast receiver held back by a subscriber that sleeps: `source_list` shows `worker.transport_dropped > 0` within 5 s, and the source has no fault).

- [ ] **Step 2: Run them to see them fail**: `cargo test -p zeta-source-worker --test supervisor --test node`
Expected: compile errors on `ring_bytes`, `transport`.

- [ ] **Step 3: Implement** the supervisor's ring per spawn, the reader's descriptor path, the hello acceptance of 1 or 2, `WorkerInfo`'s new fields and `node_setup`'s default. The ring is dropped (unmapped) when the `Worker` is reaped.

- [ ] **Step 4: Run the tests**: `cargo test -p zeta-source-worker` and `cargo test -p zeta-core --test ops -- source_list` (the schema), then `cargo run -q -p zeta -- agents-md > AGENTS.md && cargo run -q -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md`.
Expected: all pass; the guide shows `worker.transport`.

- [ ] **Step 5: Commit**

```bash
git add crates/zeta-source-worker crates/zeta-core/src/port.rs crates/zeta/src/node_setup.rs AGENTS.md skills/zetasdr/SKILL.md
git commit -m "Worker: the node creates a ring per spawn, reads descriptors out of it and shows the transport and its drops in source_list; protocol 1 still accepted"
```

### Task 5: The rate probe and the measured cap

**Files:**
- Create: `crates/zeta-source-soapy/src/probe.rs`
- Modify: `crates/zeta-source-soapy/src/source.rs` (`pub fn rate_model(&self) -> &RateModel`; `pub fn set_max_bandwidth(&mut self, claim: Claim<Hz>)`), `crates/zeta-source-soapy/src/lib.rs`, `crates/zeta/src/soapy_worker.rs` (run the probe before `serve` unless `--rate-cap-hz` or `max_rate_hz=` says otherwise), `crates/zeta/src/cli.rs` (`SoapyWorkerArgs.rate_cap_hz: Option<u64>`), `crates/zeta/src/node_setup.rs` (pass `--rate-cap-hz` from the cached capabilities when their `max_bandwidth_hz` is `Measured`), `crates/zeta/src/sources.rs` (the `max_rate_hz=N` item in `--source-soapy` args is zeta's own, stripped before the driver sees it, like `gain=`)
- Test: `crates/zeta-source-soapy/tests/probe.rs`, `crates/zeta/tests/cli.rs`

**Interfaces:**
- Produces:
  - `pub const PROBE_DWELL: Duration = Duration::from_millis(1500)`; `pub const PROBE_MIN_DELIVERED: f64 = 0.98`; `pub const PROBE_MAX_RUNGS: usize = 6`.
  - `pub fn probe_candidates(model: &RateModel) -> Vec<u64>`: a `Ladder` → its rungs from the top down, at most `PROBE_MAX_RUNGS`; a `Range` → `max, max/2, max/4`.
  - `pub async fn probe_rate_cap(src: &SoapySource, center_hz: u64) -> Result<u64, String>`: for each candidate: `tune(center, rate)`, sleep `PROBE_DWELL`, read `overflows()` delta and `measured_rate_hz()`; accept the first with zero new overflows and `measured >= PROBE_MIN_DELIVERED * rate`; none accepted → the last candidate (the lowest). A tune error on a candidate skips it; every candidate failing is `Err`.
  - The worker binary: `max_rate_hz=N` in the args → `set_max_bandwidth(Asserted(N))`, no probe; `--rate-cap-hz N` → `Measured(N)`, no probe; otherwise probe at the centre of the tune range and `set_max_bandwidth(Measured(cap))`, one stderr line `zeta-soapy-worker[<id>]: rate cap <cap> Hz measured (<n> rungs tried)`. The hello then carries the claim; the supervisor caches it as today.
  - A later retune never changes the claim; to re-probe, delete `sources/<id>.json` (documented in Task 7).

- [ ] **Step 1: Write the failing tests** in `tests/probe.rs` (the mock device, `MockDevice::with_uhd_decimation(61_440_000.0)` for a ladder; `MockRead::Overflow` on reads at the top rung via `push_reads` keyed by the rate the mock saw, or `with_rate_shortfall(0.9)` at the top rung):
  - `a_ladder_is_probed_from_the_top_and_the_first_clean_rung_wins`: the mock overflows at 61.44 and 30.72, clean at 20.48 → `Ok(20_480_000)`; the mock's `calls()` show exactly three `set_sample_rate`.
  - `a_rung_short_of_the_delivered_floor_is_rejected`: shortfall 0.9 at the top, clean below → the rung below.
  - `a_probe_that_never_streams_clean_caps_at_the_lowest_rung`: overflows at every candidate → `Ok(lowest candidate)`, within `PROBE_MAX_RUNGS * (PROBE_DWELL + 500 ms)`.
  - `a_range_device_probes_max_half_quarter`: `probe_candidates(&Range{max 20 M})` == `[20_000_000, 10_000_000, 5_000_000]`.
  - `crates/zeta-source-soapy/tests/probe.rs` too: `a_max_rate_item_asserts_the_cap_and_is_stripped_from_the_driver_args` (`split_own_args("driver=hackrf,max_rate_hz=8000000,gain=32")` → the driver string `driver=hackrf,gain=32` and `max_rate_hz: Some(8_000_000)`, the same helper that strips `gain=` and `ppm=` today), and in `crates/zeta-source-worker/tests/supervisor.rs`: `a_cached_measured_cap_is_passed_to_the_respawn` (cached capabilities with `max_bandwidth_hz: Measured(Hz(20_480_000))`: the fake worker's `--record` file shows `arg --rate-cap-hz 20480000` on the spawn; `WorkerConfig.args` is built by `node_setup`, so the supervisor test asserts through a `WorkerConfig` built the same way by a `pub fn worker_args(id, args, cached: Option<&Capabilities>) -> Vec<String>` moved into `zeta-source-worker/src/port.rs`).

- [ ] **Step 2: Run them to see them fail**: `cargo test -p zeta-source-soapy --features test-mock --test probe`
Expected: compile error, `probe` not found.

- [ ] **Step 3: Implement** `probe.rs`, the two `SoapySource` methods, the `max_rate_hz=` item, the worker args and `node_setup`'s pass-through.

- [ ] **Step 4: Run the tests**: `cargo test -p zeta-source-soapy --features test-mock` and `cargo test -p zeta --test cli -- rate`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/zeta-source-soapy crates/zeta/src
git commit -m "Soapy: the worker probes the rate ladder at open and reports the first clean rung as a measured cap; max_rate_hz= asserts one; a cached cap skips the probe"
```

### Task 6: "Insufficient bandwidth"

**Files:**
- Modify: `crates/zeta-core/src/ops.rs` (`OpError::OutOfRange` gains `#[serde(default, skip_serializing_if = "Option::is_none")] why: Option<String>`; `checked_port_up_to` and `checked_port` set it when the refusing bound is the port's `max_bandwidth_hz` and the claim is `Measured`: `insufficient bandwidth: <source> delivers at most <max> Hz (measured at open)`; the `Display` of `OutOfRange` appends `: <why>` when set), `crates/zeta-ui/web/api.js` or wherever the page formats `out_of_range` (the `why` shown after the range), `crates/zeta/src/agents_md.rs` (the error's prose)
- Test: `crates/zeta-core/tests/ops.rs`, `crates/zeta-ui` e2e not needed (text only)

**Interfaces:**
- Consumes: `Claim::Measured` on `Capabilities.max_bandwidth_hz`.
- Produces: `{"error":"out_of_range","field":"bandwidth_hz","value":24000000,"min":1,"max":20480000,"suggested":20480000,"why":"insufficient bandwidth: b210 delivers at most 20480000 Hz (measured at open)"}`; without a measured cap, no `why` key.

- [ ] **Step 1: Write the failing tests** in `tests/ops.rs`: `a_request_wider_than_a_measured_cap_is_refused_as_insufficient_bandwidth` (a test port with `max_bandwidth_hz: Measured(Hz(20_480_000))`, `job_start` with `bandwidth_hz 24_000_000` → `OutOfRange { why: Some(…) }` with the exact text above); `an_asserted_cap_refuses_without_a_why` (`Asserted`: `why: None`, JSON has no `why`).

- [ ] **Step 2: Run them to see them fail**: `cargo test -p zeta-core --test ops -- insufficient`
Expected: compile error, no field `why`.

- [ ] **Step 3: Implement**, then regenerate `AGENTS.md` and the skill (the schema changed).

- [ ] **Step 4: Run**: `cargo test -p zeta-core --test ops` and `cargo run -q -p zeta -- agents-md --check`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core/src/ops.rs crates/zeta-core/tests/ops.rs crates/zeta-ui/web crates/zeta/src/agents_md.rs AGENTS.md skills/zetasdr/SKILL.md
git commit -m "Ops: a band wider than a source's measured cap is refused as insufficient bandwidth, naming the source and the cap"
```

### Task 7: Docs and field craft

**Files:**
- Modify: `docs/agents/field-craft.md` (in "A radio that comes back": the ring, `worker.transport`, drops; a new paragraph "A radio's ceiling": the probe at open, `max_bandwidth_hz: measured`, `max_rate_hz=` to assert one, delete `sources/<id>.json` to re-probe, `insufficient bandwidth`), `crates/zeta/src/agents_md.rs` (the worker paragraph; `source_list` notes for `worker.transport`), `README.md` (the SoapySDR source section: the ring and the probe, one sentence each), `docs/guide.md` if it describes the worker, `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (plan 28.1 row in the 2026-11-06 sprint table and its status section), `TODO.md` (close "Lease: cap a Soapy source's capture at the rate the device can stream"; note `sc16` on the wire as a later gist under the transport)
- Test: `cargo run -q -p zeta -- agents-md --check`, `bash scripts/check-doc-links.sh`

- [ ] **Step 1: Write the prose**, regenerate `AGENTS.md` and the skill.
- [ ] **Step 2: Run** `cargo run -q -p zeta -- agents-md --check && cargo run -q -p zeta -- agents-md --skill --check && bash scripts/check-doc-links.sh && bash scripts/public-check.sh`
Expected: all clean.
- [ ] **Step 3: Commit**

```bash
git add docs README.md TODO.md crates/zeta/src/agents_md.rs AGENTS.md skills/zetasdr/SKILL.md
git commit -m "Docs: the worker's ring and the measured rate cap in the field craft, the guide and the README; the lease-cap gist closed"
```

### Task 8: The sweep after, and the fault paths (the field host)

**Files:**
- Modify: `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` ("Plan 28.1 status": the "after" table and the fault-path outcomes), `UAT.md` (a "Plan 28.1" section with the checks below, ticked as run)

- [ ] **Step 1: Build and deploy**: `RUSTFLAGS="--cfg soapy" cargo build -p zeta --release` from the main checkout after the branch is merged (or from the worktree for a trial deploy under a trial name), `ssh sdr@<field-host> 'bash /Projects/ZetaSDR/deploy/field-host/deploy-sdr.sh plan28.1-zeta'`.
- [ ] **Step 2: Check the open**: the node's journal shows `rate cap <N> Hz measured (<k> rungs tried)` for the B210 and the HackRF; `source list` shows `max_bandwidth_hz: {measured: N}` and `worker.transport.path: shm`.
- [ ] **Step 3: Run the sweep** again (Task 0, step 3) and record the "after" table beside the "before".
- [ ] **Step 4: Fault paths** (plan 20's acceptance 2 and 4, shortened): `kill -ABRT` the B210's worker with a job running: the fault shows, the worker is back with a fresh ring (`worker.restarts` +1, `transport.path shm`), the job relaunched; a cable pull if the operator is at the machine.
- [ ] **Step 5: A wider request is refused**: `source pin --source b210 --bandwidth-hz 61440000` → `out_of_range … insufficient bandwidth: b210 delivers at most <N> Hz (measured at open)`; the page's zoom stops at the cap.
- [ ] **Step 6: Record and commit**

```bash
git add docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md UAT.md
git commit -m "Plan 28.1 status: the B210's rate sweep after the ring, the fault paths on protocol 2, the measured cap"
```

---

## Self-review notes

- Spec coverage: 5.7's five bullets map to Tasks 1-5 (format declared: Task 2; shared memory with pipe fallback: Tasks 1, 3, 4; control not behind data: the pipe carries only descriptors after Task 3, so no second pipe is needed, recorded as the builder's call; the source says what it can deliver and the lease caps: Tasks 5 and 6; protocol 1 accepted: Task 4). 5.6's "insufficient bandwidth" is Task 6. Section 3's measurement before and after is Tasks 0 and 8. The budget "two seconds at the device's top rate" in 5.7 is replaced by a 64 MiB default (`DEFAULT_RING_BYTES`, 32 slots, 273 ms at 30.72 Msps): two seconds at 61.44 Msps is 983 MB per source, and the node copies a slot out at memcpy speed, so a quarter second is the whole buffer the pipe ever gave.
- Deviation from the spec, recorded: `sc16` on the wire is declared but not implemented (Global Constraints); the second control pipe is not built (above).
- The probe runs on every first open and on a respawn with no cached measured cap; a respawn after a crash passes `--rate-cap-hz` from the cache, so healing costs nothing extra.
