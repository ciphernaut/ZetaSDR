# Plan 19.3: Frames and signatures Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A framing hypothesis that checks runs as a `frame_decode` stage, a mode row may be a frame decoder like any other, an exploration proposes a signature the operator accepts, and the next encounter of that signal is a layer-2 match or a CRC-proved decode through a generated row.

**Architecture:** One new stage in `zeta-dsp` (`frame_decode`, over a pure `framing` module the synthesiser and `identify`'s quick attempt share) and a third decoder kind on a mode row (`decoder.kind: frame`, expanded in `zeta-core` to `channel → resample → <demod> → clock_recover → slicer → frame_decode [→ frame_parse]`). In `zeta-core`: a `SignatureStore` under `state_dir/signatures/` with five `signature_*` ops, proposals made by the node at a capture job's end and by a per-job event watcher on bits, graph and frame-row jobs, a `LearnedKnowledge` provider and a `LearnedClassifier` over the accepted records, a hot-swappable mode registry so an accepted framed signature's generated row (`learned_<id>.json`) is live at once, and a session record per exploration job. The page lists proposals on the Explore pane. The parsers (`frame_parse`: DNP3, IEC 101, IEC 104; Modbus RTU as a parser only) are the last task group and droppable.

**Tech Stack:** Rust (tokio, serde, schemars; `zeta_dsp::synth` fsk/psk/ook generators and the 19.2 stages), vanilla JS page, the Playwright e2e suite in `crates/zeta-ui/tests/e2e/ui.py`.

**Spec:** `docs/superpowers/specs/2026-11-02-layer4-explore-design.md`, section 7 (all of it), section 8 (the 19.3 lines: `frame_decode` on each encoding and length rule, every CRC against a known vector, proposals, accept with the pin and the generated row, reject, supersede, the `ask` gate, `modes check` on the frame rows over the `framing` family, a proposal accepted from the page), sections 10 and 11 (calls 9, 10, 11). Sections 5 and 6 shipped as 19.1 and 19.2; this plan changes their code only where section 7 says.

## Global Constraints

- Every op field is required and unknown fields are rejected (`deny_unknown_fields` on `Op`); a nullable op field uses `#[serde(deserialize_with = "crate::modes::nullable")]` plus `#[schemars(required, schema_with = "crate::types::nullable::<T>")]` as `BitsStart.modulation` does (`crates/zeta-core/src/ops.rs`); a nullable enum uses `crate::modes::nullable_enum::<T>`.
- Stage params: a param that depends on the signal is required (`frame_decode`'s `sync` and `length`); every other param has a `#[serde(default = "...")]` so `stage_params_schemas_are_strict` (`crates/zeta-dsp/src/lib.rs`) passes and stored graphs keep loading. Every new stage is registered in `zeta_dsp::register_all`.
- New fields on persisted records (`Signature`, `ModeRow`'s decoder, `CaptureRecord`) are `#[serde(default)]` with a nullable schema, so the seed file, the embedded rows and an older store keep loading; the schema transform `require_all` is used where the record is always written whole.
- A `frame` event is at most 64 KiB (plan 16 ruling): a frame longer than `MAX_FRAME_BITS = 65_536` is dropped and counted.
- Numbers from the spec are named constants, never literals in logic: `CONFIDENCE_FEATURES = 0.3`, `CONFIDENCE_FRAMING = 0.6`, `CONFIDENCE_CRC = 0.9`, `PROPOSE_CRC_FRAMES = 5`, `LEARNED_CENTER_SCORE = 0.4`, `LEARNED_BANDWIDTH_SCORE = 0.3`, `LEARNED_MODULATION_SCORE = 0.2`, `LEARNED_RATE_SCORE = 0.1`, `LEARNED_PERIOD_SCORE = 0.1`, `LEARNED_BANDWIDTH_RANGE = (0.5, 1.5)`, `LEARNED_RATE_TOLERANCE = 0.05`, `LEARNED_PERIOD_TOLERANCE = 0.2`, `LEARNED_MIN_SCORE = 0.7`, `LEARNED_VERDICT_MIN = 0.5`, `SUPERSEDE_WIDTH_RANGE = (0.5, 2.0)`.
- Every stage resets its streaming state on a time discontinuity (`zeta_dsp::is_seam`) or a new `bits` segment (an `offset_bits` that is not the previous event's end), as `correlate` does.
- The node runs, and every test passes, with no captures, no signatures, no station, no operator modes directory and no network.
- `AGENTS.md` and `skills/zetasdr/SKILL.md` are generated: after any op, flag, stage or field-craft change run `cargo run -q -p zeta -- agents-md --check`, regenerate (`cargo run -q -p zeta -- agents-md > AGENTS.md`, `cargo run -q -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md`) and commit the result in the same commit. The prose for a new op goes in `crates/zeta/src/agents_md.rs` (the `match name` of per-op notes) and in `docs/agents/field-craft.md`.
- Gates (CLAUDE.md): `cargo fmt -p <crate> -- --check` per crate (the nested worktree breaks `cargo fmt --all`), `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`, `cargo test -p zeta --test ui -- --nocapture` (no `ui e2e skipped` line), `cargo run -q -p zeta -- agents-md --check`, `cargo run -q -p zeta -- agents-md --skill --check`, `bash scripts/public-check.sh`, `bash scripts/check-doc-links.sh`. Use a private `CARGO_TARGET_DIR`.
- The repository is public: roles, never private host names or LAN addresses, in code, tests, docs and commit messages ("the field host"). The demo station is Brisbane Airport (-27.3842, 153.1175, 4 m). No real licensee's traffic as a fixture: every frame fixture is synthesised.
- Commits go on this branch; one commit per task, message in the repo's style (`Area: what changed`), no trailer.

## Models per task

Opus where the task tunes behaviour against synthesised signals, changes the launch or classify paths, or defines an interface several later tasks consume; Sonnet where the brief is complete and the tests pin the behaviour.

| Task | Model | Why |
|---|---|---|
| 1 CRC additions | Sonnet | check values and a serde type |
| 2 `framing` rules and `frame_decode` | Opus | the decoder every later task (proposals, rows, quick attempt) relies on |
| 3 the `framing` synthesiser family and the offline chain runner | Sonnet | the inverse of Task 2 plus a move |
| 4 frame rows | Opus | the row schema, the expander, the async quick attempt, `modes check` |
| 5 the signature store and ops | Opus | the record every later task reads; the `ask` gate |
| 6 proposals and sessions | Opus | the capture end hook and a per-job event watcher in the launch path |
| 7 learned knowledge and the scorer | Opus | the layer-2 scorer and identify's verdict |
| 8 the learned row, the registry reload, superseding | Opus | a hot-swapped registry across four crates |
| 9 the page | Opus | the Explore pane's proposals, the allow switch, a new e2e scenario |
| 10 field craft and the guide | Sonnet | |
| 11a `frame_parse` and the parsers (droppable) | Sonnet | pure parsers with published vectors |
| 11b the parser rows (droppable) | Sonnet | rows and `modes check` |
| 12 TODO, plan series, UAT, gate | Sonnet | |

Ledger: `.superpowers/sdd/plan19.3/progress.md` in the worktree, one line per task with the implementer's and reviewer's verdicts; the rulings made on the operator's behalf go into the final report.

## Review Focus

Inputs the spec implies but no obvious test exercises; each gets a test in the task that owns the code:

1. A proposal must never duplicate: a looping bits job crosses the CRC threshold on every pass, and a watch's capture child ends once per signal; both propose once. (Task 6: `a_bits_job_proposes_once_however_many_passes`, `a_watch_capture_child_proposes_once`.)
2. A signal an accepted signature already labels must not be captured or proposed again by the watch, else the store fills with copies of one link. (Task 7: `the_watch_does_not_capture_a_learned_signal`; Task 6: `no_proposal_when_an_accepted_signature_holds_the_band`.)
3. `signature_accept` on a node whose operator modes directory is unwritable or absent must still accept (and pin) and say the row was not written, never fail half-way. (Task 8: `accept_without_a_writable_modes_directory_accepts_and_says_so`.)
4. A generated `learned_<id>.json` the operator has since edited (a parser, a subject) must survive a second accept of a signature with the same id and a node restart. (Task 8: `an_existing_learned_row_is_kept_as_is`.)
5. A `frame_decode` whose length rule names bits beyond the data (a damaged length field, a `to_next_sync` frame that never closes) must drop the frame and keep decoding, never buffer without bound. (Task 2: `a_bad_length_field_drops_the_frame_and_goes_on`, `an_unclosed_to_next_sync_frame_is_bounded`.)

---

### Task 1: The CRC table completed, and a CRC parameter type

**Files:**
- Modify: `crates/zeta-dsp/src/crc.rs`, `crates/zeta-dsp/src/bit_view.rs` (tests only)
- Test: `crates/zeta-dsp/src/crc.rs` tests module, `crates/zeta-dsp/src/bit_view.rs` tests module

**Interfaces:**
- Consumes: `CrcSpec`, `CRCS`, `crc`, `by_name` (plan 19.2 Task 1).
- Produces: `pub const CRC8: CrcSpec` (`"crc8"`, CRC-8/SMBUS: width 8, poly 0x07, init 0, no reflection, xorout 0, check 0xF4); `pub const CRC16_MODBUS: CrcSpec` (`"crc16-modbus"`, poly 0x8005, init 0xFFFF, refin and refout, xorout 0, check 0x4B37); `CRCS` in the order `CRC16_CCITT, CRC16_IBM, CRC16_DNP, CRC16_MODBUS, CRC32, CRC16_X25, CRC16_KERMIT, CRC8` (the 8-bit one last: it checks by chance most often). `#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)] #[serde(deny_unknown_fields)] pub struct CrcDef { pub width: u8, pub poly: u64, pub init: u64, pub refin: bool, pub refout: bool, pub xorout: u64 }` (the spec's general form); `impl From<&CrcSpec> for CrcDef`; `pub fn crc_def(def: &CrcDef, data: &[u8]) -> u64` (the evaluator; `crc(spec, data)` becomes `crc_def(&spec.into(), data)`); `#[serde(untagged)] pub enum CrcParam { Name(String), Def(CrcDef) }` with `pub fn resolve(&self) -> Result<(CrcDef, String), String>` giving the definition and its name (`"crc16-ccitt"`, or `"crc<width>/<poly:x>"` for a `Def`); errors `crc: unknown name "<x>" (one of crc16-ccitt, crc16-ibm, ...)` and `crc.width: 8..=64`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn the_two_new_specs_check() { assert_eq!(by_name("crc8").unwrap().check, 0xF4); assert_eq!(by_name("crc16-modbus").unwrap().check, 0x4B37); /* and every_spec_matches_its_check_value still passes over the eight */ }
#[test] fn a_def_evaluates_like_its_spec() { for s in CRCS { assert_eq!(crc_def(&CrcDef::from(s), b"123456789"), s.check, "{}", s.name); } }
#[test] fn crc_param_resolves_names_and_defs() { /* json "crc16-dnp" -> (CrcDef of CRC16_DNP, "crc16-dnp"); json {width:16, poly:0x1021, init:0xFFFF, refin:false, refout:false, xorout:0} -> name "crc16/1021"; "crc99" -> Err starting "crc: unknown name"; width 4 -> Err("crc.width: 8..=64") */ }
// bit_view.rs
#[test] fn a_modbus_crc_is_found_on_a_framed_stream() { /* framed_stream_with(14, 136, 1, &CRC16_MODBUS) (a `framed_stream` variant taking the spec): crc_candidates has crc "crc16-modbus" with frames >= 12 */ }
#[test] fn random_bits_give_no_sync_candidate_and_say_so() { /* unchanged, still passes with crc8 in the table */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-dsp --features test-fixtures crc:: bit_view::` — Expected: FAIL to compile (`CRC8`, `CrcDef`).
- [ ] **Step 3: Implement** the two specs, `CrcDef`, `crc_def`, `CrcParam::resolve`, and the `framed_stream_with` test helper.
- [ ] **Step 4: Run** — `cargo test -p zeta-dsp --features test-fixtures crc bit_view correlate` — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-dsp
git commit -m "DSP: the CRC table gains crc8 and crc16-modbus, a CrcDef general form and the CrcParam a stage takes by name or definition"
```

---

### Task 2: The `framing` rules and the `frame_decode` stage

**Files:**
- Create: `crates/zeta-dsp/src/framing.rs`, `crates/zeta-dsp/src/frame_decode.rs`
- Modify: `crates/zeta-dsp/src/lib.rs` (`pub mod framing; pub mod frame_decode;`, `register_all`), `crates/zeta-dsp/src/correlate.rs` (`BitsIn` and `BitsIn::parse` become `pub(crate)` for `frame_decode` to read `bits` events)
- Test: the two modules' tests

**Interfaces:**
- Consumes: `crc::{CrcParam, CrcDef, crc_def}` (Task 1), `correlate::BitsIn` (`bits`, `offset_bits`, `t_ns`, `bit_ns`, `prov`, `end()`, `time(i)`), `crate::is_seam`, `run_map_ending`, `StageContext.mode` (a frame-row job publishes in the row's envelope as `pocsag.rs` does: the row's `protocol` and `summary`).
- Produces in `framing.rs` (pure, no DSP, the types every later task names):
  - `#[serde(rename_all = "snake_case")] pub enum LineCode { Nrz, Nrzi, Manchester, Differential }`; `pub enum BitOrder { Msb, Lsb }`; `pub enum LengthUnit { Bits, Bytes }`.
  - `#[serde(deny_unknown_fields)] pub struct Word { pub hex: String, pub bits: u32 }` (`bits` 1..=64 and at most the hex's bits; the word is the hex's first `bits` bits, MSB first).
  - `Whitening`: `None`, `Pn9`, or `Lfsr { poly: u32, seed: u32 }`; on the wire `"none"`, `"pn9"` or `{"poly", "seed"}` (an `#[serde(untagged)]` enum of a name string or the object, checked in `check`). `pn9` is the 9-bit Fibonacci LFSR x^9 + x^5 + 1, seed 0x1FF, one output bit (the register's LSB) XORed with each frame bit in frame bit order, restarted at each frame's first bit after the sync; its first eight output bytes are TI's published PN9 sequence `FF E1 1D 9A ED 85 33 24` (the implementer confirms the vector against the CC1101 data sheet before pinning the test). `Lfsr { poly, seed }` is the same machine with `poly` the tap mask and `seed` the start state.
  - `Length`: `Fixed { fixed_bits: u32 }` (1..=65536; the bits after the sync), `Field { field: LengthField }` with `#[serde(deny_unknown_fields)] pub struct LengthField { pub offset_bits: u32, pub bits: u32 (1..=32), pub big_endian: bool, pub unit: LengthUnit, pub add: i64 }` (the field read from `offset_bits` after the sync; the frame's bits after the sync are `value × unit + add`, in bits), or `ToNextSync { to_next_sync: bool }` (the frame runs to the next sync hit, a seam or the input's end). An `#[serde(untagged)]` enum.
  - `#[serde(deny_unknown_fields)] pub struct Framing { #[serde(default)] pub encoding: LineCode, pub sync: Word, #[serde(default)] pub preamble: Option<Word>, #[serde(default)] pub whitening: Whitening, pub length: Length, #[serde(default)] pub crc: Option<CrcParam>, #[serde(default)] pub bit_order: BitOrder, #[serde(default)] pub max_errors: u32 }` (JsonSchema; `pub fn check(&self) -> Result<(), String>` naming the field: `sync.hex: hex`, `sync.bits: 1..=64 and at most the hex's bits`, `preamble.bits`, `max_errors: under sync.bits`, `length.fixed_bits: 1..=65536`, `length.field.bits: 1..=32`, `whitening: none, pn9 or {poly, seed}`, `whitening.poly: not 0`, and `crc`'s own errors).
  - `pub const MAX_FRAME_BITS: usize = 65_536`.
  - `pub struct DecodedFrame { pub at_bit: usize, pub sync_errors: u32, pub bytes: Vec<u8>, pub crc_ok: Option<bool> }`: `at_bit` the sync's first bit in the input, `bytes` the frame after the sync (the CRC field included), unwhitened and packed by `bit_order` (`lsb`: each byte's bits reversed), `crc_ok` `None` without a `crc`.
  - `pub fn decode_frames(bits: &[u8], f: &Framing) -> (Vec<DecodedFrame>, usize)`: over one segment; the line code decoded first (`nrzi`: a 1 where the raw bit differs from the previous, `differential`: the XOR with the previous raw bit, `manchester`: pairs `10` → 1 and `01` → 0 at the pair phase that gives a sync hit, `nrz` as is), the sync hunted at every bit (Hamming distance at most `max_errors`; with a `preamble` its bits must precede exactly), the frame cut by the length rule, a frame whose length is not whole bytes, under 1 byte, over `MAX_FRAME_BITS` or past the input dropped (counted, the hunt resumes after the sync), the CRC checked over the bytes but the last `width / 8` against the trailing field; the second return is how many input bits are settled (the decoder may be called again with the rest and new bits appended).
  - `pub fn encode_frames(bodies: &[Vec<u8>], f: &Framing) -> Vec<u8>`: the inverse, per frame: the preamble, the sync, the body with a `Field` length written into it (the body must be at least the field's end), the CRC appended when given, whitening and bit order applied to the bits after the sync, the line code encoded over everything (`manchester` doubles the bits).
- Produces in `frame_decode.rs`: `pub const FRAME_DECODE_KIND: &str = "frame_decode"`; `pub const BUFFER_BITS_MAX: usize = 1 << 20`; `pub type FrameDecodeParams = Framing` (the stage's params are the `Framing` struct; `check_params` is `Framing::check`); `pub struct FrameDecodeStage` (inputs `[Event]`, outputs `[Event]`). Behaviour: `bits` events of one segment are appended to a buffer (a new segment, a seam or a lag clears it; the buffer never exceeds `BUFFER_BITS_MAX`, the oldest bits dropped and counted as `dropped_bits`); `decode_frames` runs over the unsettled bits after each event and at the input's end (with `to_next_sync`, the end closes the last frame); one event per frame `{"protocol": "frame", "t_ns": <the sync's first bit's time>, "sync": "<hex>", "length_bytes": n, "bytes": "<hex>", "crc_ok": true|false|null, "payload": "opaque", "summary": "frame <n> B, crc ok|bad|none"}` with the frame's span as provenance, and after each a note `{"frame_decode": {"frames", "crc_ok", "crc_bad", "sync_hits", "bad_length", "dropped_bits", "stream": <the event stream id>}}`. Every other event (`segment`, `sync`, `constellation`, `timing`, `hypotheses`) passes through unchanged, so a `frame_decode` may sit before `bit_view` as `correlate` does.

- [ ] **Step 1: Write the failing tests** (framing.rs helpers: `fx(sync: &str, bits: u32, length: Length) -> Framing` with nrz, no whitening, `crc: Some(Name("crc16-ccitt"))`, msb; `bodies(n, len, seed) -> Vec<Vec<u8>>` pseudo-random)

```rust
// framing.rs
#[test] fn round_trip_on_each_encoding() { /* for each LineCode: encode 10 bodies of 32 B with fixed_bits 272 (32 B + 2 B crc), decode: 10 frames, bytes[..32] == body, crc_ok Some(true), at_bit ascending */ }
#[test] fn round_trip_on_each_length_rule() { /* Fixed; Field{offset_bits 0, bits 8, big_endian true, unit bytes, add 3} with bodies of varying length whose first byte the encoder fills; ToNextSync with bodies of varying length: every body back */ }
#[test] fn pn9_matches_the_published_sequence() { /* the first 8 whitening bytes over zero input == FF E1 1D 9A ED 85 33 24 */ }
#[test] fn lsb_bit_order_reverses_each_byte() { /* bit_order lsb: a body round-trips; the raw bits differ from msb's by per-byte reversal */ }
#[test] fn max_errors_admits_a_damaged_sync_and_a_preamble_is_required() { /* one sync bit flipped: 0 frames at max_errors 0, the frame at 1; preamble {hex "aaaa", bits 16}: a frame without it is not cut */ }
#[test] fn a_bad_length_field_drops_the_frame_and_goes_on() { /* Field rule; frame 3's length byte set to 0xFF (past the input): frames 1, 2, 4.. decoded, 3 missing; the settled count stops before frame 3's sync when the rest is not yet in */ }
#[test] fn check_names_the_bad_field() { /* "zz" hex -> "sync.hex: hex"; bits 70 -> "sync.bits: ..."; max_errors 16 on a 16-bit sync -> "max_errors: under sync.bits"; whitening "pn7" -> "whitening: none, pn9 or {poly, seed}"; crc "crc99" -> "crc: unknown name ..." */ }
// frame_decode.rs
#[tokio::test] async fn frames_are_decoded_across_split_bits_events() { /* bits of 8 frames in events split mid-sync and mid-frame (bits_frames helper as correlate's tests): 8 frame events, crc_ok true, t_ns of frame k == the sync bit's time; the note's frames == 8, crc_ok == 8 */ }
#[tokio::test] async fn a_seam_resets_the_buffer_and_other_events_pass_through() { /* two segments with a `segment` event between: frames from both, none spanning; the segment event is forwarded */ }
#[tokio::test] async fn to_next_sync_closes_on_the_next_sync_and_at_the_end() { /* 3 frames of varying length, ToNextSync: 2 events while streaming, the third at input end */ }
#[tokio::test] async fn an_unclosed_to_next_sync_frame_is_bounded() { /* one sync then 2 million zero bits without another: the buffer holds at most BUFFER_BITS_MAX; the note's dropped_bits > 0; no frame */ }
#[test] fn stage_params_are_the_framing_struct() { /* check_params(json!({"sync": {"hex": "2dd4", "bits": 16}, "length": {"fixed_bits": 272}})) is Ok; json!({}) names "sync" */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-dsp --features test-fixtures framing frame_decode` — Expected: FAIL to compile.
- [ ] **Step 3: Implement `framing.rs` and `frame_decode.rs`**, register the stage, open `BitsIn` to the crate.
- [ ] **Step 4: Run** — `cargo test -p zeta-dsp --features test-fixtures framing frame_decode correlate stage_params_schemas_are_strict` — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-dsp
git commit -m "DSP: framing rules (line code, sync and preamble, length rules, whitening, CRC, bit order) and the frame_decode stage emitting one frame event per cut frame"
```

---

### Task 3: The `framing` synthesiser family and the offline chain runner

**Files:**
- Create: `crates/zeta-dsp/src/offline.rs`
- Modify: `crates/zeta-dsp/src/synth.rs` (`SynthSpec::Framing`, `parse_params`, `rate_hz`, `validate`, `synthesise`), `crates/zeta-dsp/src/lib.rs` (`pub mod offline;`), `crates/zeta-dsp/tests/bits_chain.rs` (`run_chain`/`run_chain_at` moved out; the file uses `zeta_dsp::offline::*`)
- Test: `crates/zeta-dsp/src/synth.rs` tests, `crates/zeta-dsp/tests/bits_chain.rs`

**Interfaces:**
- Consumes: `framing::{Framing, encode_frames}` (Task 2), the `Fsk`/`Psk`/`Ook` generators and `repeat_bursts` in `synth.rs`, `generic_rate_hz`.
- Produces: `SynthSpec::Framing { modulation: FramingModulation, symbol_rate_baud: u32, #[serde(default = "default_order")] order: u8, #[serde(default)] deviation_hz: f64, frames: u32, body_bytes: u32, #[serde(default)] seed: u64, #[serde(default)] bodies: Vec<String>, framing: Framing, repeat: u32, #[serde(default = "default_gap_ms")] gap_ms: u64 }` with `#[serde(rename_all = "snake_case")] pub enum FramingModulation { Fsk, Psk, Ook }`; the family name `framing`; the bits are `encode_frames` of `frames` bodies of `body_bytes` pseudo-random bytes from `seed` (xorshift64), or of `bodies` (hex, one per frame) when given, generated as the `Fsk` (with `deviation_hz`, required non-zero for fsk), `Psk` or `Ook` arm would generate them; `rate_hz()` is `generic_rate_hz(symbol_rate_baud)`; `validate()` runs `framing.check()` (errors prefixed `framing.`) and refuses `frames: 0`, `body_bytes: 0` without `bodies`, and a `bodies` entry that is not hex. `pub fn framing_bits(spec: &SynthSpec) -> Option<Vec<u8>>`: the planted bits of a `Framing` spec, for tests.
- `offline.rs`: `pub async fn run_chain(stages: &[(&str, serde_json::Value)], iq: &[Complex<f32>], rate_hz: u64) -> Vec<serde_json::Value>` and `pub async fn run_chain_at(stages, iq, rate_hz, center_hz)`, exactly the harness `tests/bits_chain.rs` has today (a private bus, one stream per stage output, 50 ms blocks with contiguous spans, every event of the last stage collected), moved into the library so `identify`'s quick attempt (Task 4) can use it; `recovered_bits` and `align_errors` stay in the test file.

- [ ] **Step 1: Write the failing tests**

```rust
// synth.rs
#[test] fn the_framing_family_parses_and_plants_its_frames() { /* parse json {"family": "framing", "modulation": "fsk", "symbol_rate_baud": 4800, "deviation_hz": 2400.0, "frames": 8, "body_bytes": 32, "framing": {"sync": {"hex": "2dd4", "bits": 16}, "length": {"fixed_bits": 272}, "crc": "crc16-ccitt"}, "repeat": 1}: Ok; framing_bits(&spec).len() == 8 × (16 + 272); decode_frames over them gives 8 crc_ok frames */ }
#[test] fn the_framing_family_refuses_a_bad_framing_naming_the_field() { /* sync bits 70: Err starting "framing.sync.bits" */ }
// tests/bits_chain.rs
#[tokio::test] async fn framing_family_decodes_to_crc_clean_frames() { /* the spec above at 20 dB, offset 500, ppm 20: 48k -> resample 38400 -> fsk_demod -> clock_recover -> slicer -> frame_decode{same framing}: at least 7 of 8 frame events with crc_ok true, bytes[..32] equal to the planted bodies */ }
#[tokio::test] async fn framing_family_round_trips_manchester_and_pn9() { /* encoding manchester, whitening pn9, fsk order 2 at 4800 Bd (an fsk chain has no sign ambiguity; a BPSK inversion would invert the sync, which is slicer `differential`'s matter, not this test's): at least 7 frames crc_ok */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-dsp --features test-fixtures synth::the_framing bits_chain` — Expected: FAIL to compile.
- [ ] **Step 3: Implement** the family, the move to `offline.rs`.
- [ ] **Step 4: Run** — `cargo test -p zeta-dsp --features test-fixtures synth bits_chain` — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-dsp
git commit -m "DSP: the framing synthesiser family (bits framed by frame_decode's rules over fsk, psk or ook) and the offline chain runner moved into the library"
```

---

### Task 4: Frame rows: `decoder.kind: frame`, the expander, the quick attempt, `modes check`

**Files:**
- Modify: `crates/zeta-core/src/modes/row.rs` (`Decoder::Frame`, `DecoderKind::Frame`, `FrameBlock`, `FrameDemod`, `FRAME_PARSERS`, `validate`), `crates/zeta-core/src/modes/template.rs` (`expand`, `check_band`), `crates/zeta-core/src/modes/mod.rs` (re-exports), `crates/zeta-core/src/bits.rs` (`decision_nodes` factored out of `bits_graph`), `crates/zeta-dsp/src/identify.rs` (`Attempt`, `quick_attempt(row)`), `crates/zeta/src/agents_md.rs` (the `modes_list` note: `decoder_kind` `graph`, `program` or `frame`; a "frame rows" paragraph under the modes note)
- Create: `crates/zeta-dsp/src/frame_quick.rs`, `crates/zeta-core/tests/frame_rows.rs`
- Test: `crates/zeta-core/tests/frame_rows.rs`, `crates/zeta-core/src/modes/row.rs` tests, `crates/zeta/tests/modes_check.rs`

**Interfaces:**
- Consumes: `frame_decode`'s `check_params` through the `StageRegistry` (the row's `framing` is validated by the stage, as graph rows' node params are: `zeta-core` holds no `Framing` type), `bits::{Modulation, chain_rate_hz}`, `offline::run_chain` (Task 3), `channel_rate_hz` in `ops.rs`.
- Produces in `row.rs`: `Decoder::Frame(FrameBlock)` (tag `"frame"`); `#[serde(deny_unknown_fields)] #[schemars(transform = require_all)] pub struct FrameBlock { pub demod: FrameDemod, pub framing: serde_json::Value, #[serde(default = "default_parser")] pub parser: String }`; `#[serde(deny_unknown_fields)] pub struct FrameDemod { pub modulation: crate::bits::Modulation, pub symbol_rate_baud: u32, #[serde(default = "default_order")] pub order: u8, #[serde(default)] pub tone_spacing_hz: Option<u64> }`; `DecoderKind::Frame`; `pub const FRAME_PARSERS: &[&str] = &["opaque"]` (Task 11b adds the four); `QUICK_ATTEMPTS` gains `"frame"`. `validate`: `decoder.framing` must pass `stages`' `frame_decode` `check_params` (error field `decoder.framing.<field>`; a registry without the stage: `decoder.framing: frame_decode is not a registered stage`); `decoder.parser` in `FRAME_PARSERS`; `decoder.demod.order` in `modulation.orders()` (`decoder.demod.order: <n> is not 2 or 4 for fsk`); `decoder.demod.symbol_rate_baud: at least 1`; `identify.quick: frame` only on a frame row (`identify.quick: frame needs decoder.kind frame`) and a frame row's `quick`, when set, must be `frame`.
- `template::expand` for `Decoder::Frame(b)`: `channel {center_hz, bandwidth_hz, out_rate_hz: out}` → `resample {out_rate_hz: chain}` (chain = `bits::chain_rate_hz(out, baud, modulation, order)`; the node is left out when `chain == out`) → the three `bits::decision_nodes(modulation, baud, order, tone_spacing_hz) -> [(&'static str, &'static str, Value); 3]` (ids `demod`, `clock`, `slicer`; the demod, `clock_recover` and `slicer` params exactly as `bits_graph` builds them today, which now calls it) → `frame_decode {<b.framing>}` → `frame_parse {parser}` only when `parser != "opaque"` (its stage lands in Task 11a; until then a row with another parser is refused by `FRAME_PARSERS`); edges iq, iq, audio, audio, event, event; fresh ids as the other arms. `check_band` applies to a frame row as to an IQ program row (`band.min_hz`, else `band.bandwidth_hz`, is the least).
- `identify.rs`: `type Attempt = Arc<dyn Fn(Vec<Complex<f32>>, u64) -> BoxFuture<'static, Option<String>> + Send + Sync>` (the four existing attempts wrapped in `async move`); `fn quick_attempt(row: &ModeRow) -> Option<Attempt>` by `identify.quick`, `"frame"` giving a closure over the row's `FrameBlock` that runs `frame_quick::attempt`. `frame_quick.rs`: `pub async fn attempt(chan: Vec<Complex<f32>>, rate_hz: u64, block: &FrameBlock) -> Option<String>`: `offline::run_chain` over `resample {chain_rate_hz(rate, ..)}` → `decision_nodes` → `frame_decode {framing}`; `Some("frame <n> B, <crc name> ok (<ok>/<frames>)")` when at least one event has `crc_ok == true` (a framing without a `crc` never proves: `None`).
- `modes_list`'s `decoder_kind` reads `frame`; `job_start --kind <row>` runs it through `JobKind::Mode`.

- [ ] **Step 1: Write the failing tests** (`frame_rows.rs`: `open(dir, modes_dir)` as `tests/modes.rs`'s `cfg`; a row file `fsk_test_frames.json`: `family general`, `band {bandwidth_hz: 12500, centers_hz: [433920000]}`, `modulation ["fsk", "fm"]`, `label "FSK frames"`, `protocol "frame"`, `decoder {kind: frame, demod {modulation: fsk, symbol_rate_baud: 4800, order: 2, tone_spacing_hz: null}, framing {sync {hex "2dd4", bits 16}, length {fixed_bits: 272}, crc "crc16-ccitt"}, parser "opaque"}`, `event {schema with protocol, sync, length_bytes, bytes, crc_ok, payload; summary "frame {length_bytes} B"}`, `identify {proof: field, field: crc_ok, min_events: 1, dwell_ms: 10000, min_span_ms: 1000, quick: frame}`, `catalogue []`; a file source of the Task 3 framing spec at 48 kHz labelled 433.92 MHz)

```rust
#[test] fn a_frame_row_parses_validates_and_lists_its_kind() { /* parse + validate Ok; Decoder::kind() == Frame; modes_list value has decoder_kind "frame" */ }
#[test] fn a_bad_framing_or_parser_names_the_field() { /* sync bits 70: "decoder.framing.sync.bits: ..."; parser "modbus_rtu" (before Task 11b): "decoder.parser: ..."; quick "frame" on the pocsag row: "identify.quick: frame needs decoder.kind frame" */ }
#[test] fn a_frame_row_expands_to_the_chain() { /* expand(row, {433920000, 12500}, 48000): kinds in order channel, resample, fsk_demod, clock_recover, slicer, frame_decode; resample out_rate_hz 38400; frame_decode params == the row's framing; 5 edges iq,iq,audio,audio,event */ }
#[tokio::test] async fn a_frame_row_job_decodes_crc_clean_frames() { /* job_start kind fsk_test_frames on the file source: the first event within 10 s has protocol "frame", crc_ok true, length_bytes 34 */ }
#[tokio::test] async fn classify_proves_a_frame_row_through_the_quick_attempt() { /* Op::Classify over a 1 s span of the ring: tried has fsk_test_frames with outcome proved and a layer-2 label "FSK frames" */ }
#[tokio::test] async fn identify_proves_a_frame_row() { /* an identify on the band ends with verdict.mode == "fsk_test_frames" */ }
```

In `crates/zeta/tests/modes_check.rs`: `a_frame_row_is_checked_over_the_framing_family` (the row above with `test: {kind: synth, family: framing, modulation: fsk, symbol_rate_baud: 4800, deviation_hz: 2400.0, frames: 8, body_bytes: 32, framing: <the same>, repeat: 1, snr_db: 20, expect: {crc_ok: true}, within_ms: 10000}` written with `write_row`: stdout has `fsk_test_frames: ok`).

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-core --test frame_rows` — Expected: FAIL to compile.
- [ ] **Step 3: Implement** the row kind, the expander, `decision_nodes`, the async attempts, `frame_quick.rs`; regenerate `AGENTS.md` and the skill.
- [ ] **Step 4: Run** — `cargo test -p zeta-core --test frame_rows --test modes --test bits_start --test identify`, `cargo test -p zeta-dsp --features test-fixtures classify identify`, `cargo test -p zeta --test modes_check --test agents_md` — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core crates/zeta-dsp crates/zeta AGENTS.md skills
git commit -m "Modes: a frame row (decoder.kind frame: demod, framing, parser) expands to the demod-clock-slicer-frame_decode chain, proves by a CRC-clean frame in classify (the frame quick attempt) and identify, and is checked over the framing family"
```

---

### Task 5: The signature store, the `signature_*` ops, the CLI and the `ask` gate

**Files:**
- Create: `crates/zeta-core/src/signatures.rs`, `crates/zeta-core/tests/signatures.rs`
- Modify: `crates/zeta-core/src/lib.rs`, `crates/zeta-core/src/ops.rs` (five ops, two results, `name`, `apply`), `crates/zeta-core/src/node/mod.rs` (`Inner.signatures`, `Node::signatures()`, `Node::signature_propose/accept/reject`), `crates/zeta/src/cli.rs` (`Command::Signature(SignatureCmd)`, `--allow-signatures` on `ask`), `crates/zeta/src/main.rs` (the write-op list), `crates/zeta/src/ask.rs`, `crates/zeta-agent/src/agent.rs` (`Allow.accept_signatures`, `gate`), `crates/zeta/src/agents_md.rs` (per-op notes, the `RUNNING` text's `allow` object), `crates/zeta-ui/web/app.js` (`allow signatures on|off`, carried in `/ask`, the refusal note's retry button)
- Test: `crates/zeta-core/tests/signatures.rs`, `crates/zeta-agent/src/agent.rs` tests, `crates/zeta/src/cli.rs` tests

**Interfaces:**
- Consumes: `FeatureRecord`, `Allocation`, `Station`, `BurstStore::{get, pin, list}`, `classify_capture` (the layer-1 label of a capture), `Node::apply_from(op, transport)` (the transport name every op arrives with: `proposed_by`, `accepted_by`).
- Produces in `signatures.rs`: `pub const SIGNATURES_DIR: &str = "signatures"`; the confidence constants of the Global Constraints; `#[serde(rename_all = "snake_case")] pub enum SignatureState { Proposed, Accepted, Rejected, Superseded }`; `pub struct Burst { pub duty: Option<f32>, pub burst_ms: Option<f32>, pub period_ms: Option<f32> }`; `pub struct SignatureProvenance { pub source: SourceId, pub station: Option<Station>, pub captures: Vec<String>, pub jobs: Vec<JobId>, pub allocation: Option<Allocation>, pub proposed_ns: u64, pub proposed_by: String, pub accepted_ns: Option<u64>, pub accepted_by: Option<String>, pub superseded_by: Option<String>, pub superseded_ns: Option<u64> }`; `pub struct SignatureRecord { pub id: String (ULID), pub name: String, pub state: SignatureState, pub min_hz: Hz, pub max_hz: Hz, pub bandwidth_hz: Hz, pub modulation: String, pub order: Option<u8>, pub symbol_rate_baud: Option<Baud>, pub tone_spacing_hz: Option<Hz>, pub burst: Burst, pub framing: Option<serde_json::Value>, pub decoder: Option<String>, pub decoder_file: Option<String>, pub decoder_note: Option<String>, pub confidence: f32, pub evidence: String, pub provenance: SignatureProvenance, pub matches: u64, pub last_matched_ns: Option<u64> }` (every struct `deny_unknown_fields`, `require_all`, every `Option` `#[serde(default)]` with a nullable schema; `pub fn band_holds(&self, center_hz: Hz) -> bool` for `min_hz - bw/2 ..= max_hz + bw/2`); `pub struct NewSignature { pub name: Option<String>, pub features: FeatureRecord, pub modulation: String, pub framing: Option<Value>, pub crc_frames: u32, pub evidence: String, pub source: SourceId, pub station: Option<Station>, pub captures: Vec<String>, pub jobs: Vec<JobId>, pub allocation: Option<Allocation>, pub proposed_by: String }`; `pub fn confidence(framing: Option<&Value>, crc_frames: u32) -> f32` (none → 0.3; a framing → 0.6; a framing whose `crc` is not null with `crc_frames >= PROPOSE_CRC_FRAMES` → 0.9); `pub fn default_name(modulation: &str, bandwidth_hz: Hz, center_hz: Hz) -> String` = `"{modulation} {kHz:.1} kHz at {MHz:.3} MHz"` (`fsk 16.0 kHz at 404.025 MHz`); `pub enum StoreError { Unknown(String), State { id: String, state: SignatureState }, Io(String) }`; `pub struct SignatureStore` with `open(state_dir) -> io::Result<Arc<Self>>` (reads every `*.json`; unreadable files listed by `load_errors()`), `list(state: Option<SignatureState>) -> Vec<SignatureRecord>` (newest first), `get(id)`, `accepted()`, `propose(new, now_ns) -> Result<SignatureRecord, StoreError>` (the record's band is the feature's centre ± half its bandwidth, `name` the given or `default_name`), `accept(id, name: Option<String>, by, now_ns)` and `reject(id, by, now_ns)` (`State` unless proposed), `supersede(id, mode, now_ns)` (`State` unless accepted), `set_decoder(id, mode, file, note)`, `matched(id, now_ns)` (+1, `last_matched_ns`); files written atomically (tmp + rename, as `node/persist.rs`).
- `ops.rs`: `Op::SignatureList { state: Option<SignatureState> }` (`nullable_enum`), `SignatureShow { id: String }`, `SignaturePropose { capture: String, name: Option<String>, framing: Option<serde_json::Value> }`, `SignatureAccept { id: String, name: Option<String> }`, `SignatureReject { id: String }`; names `signature_list`, `signature_show`, `signature_propose`, `signature_accept`, `signature_reject`; `OpResult::Signatures(Vec<SignatureRecord>)`, `OpResult::Signature(SignatureRecord)`. Errors: an unknown id is `Unknown { field: "id", known: <the store's ids> }`; a wrong state `Other("signature <id> is <state>, not proposed")`; `signature_propose` refuses a `framing` the `frame_decode` stage's `check_params` refuses (`Other("framing: <reason>")`), an unknown capture (`Unknown { field: "capture" }`), a capture whose `features` is not a `FeatureRecord` (`Other("capture <id> has no feature record")`); its `modulation` is the layer-1 label of `classify_capture` over the capture (`noise` is refused: `Other("capture <id> is noise: nothing to propose")`), `allocation` the first `tx` assignment of the record's `allocations`, `station` the node's, `jobs` `[the capture's job]`, `captures` every capture of the record's `set`, `proposed_by` the transport. `signature_accept` pins every provenance capture still in the store (`bursts.pin`), then returns the record (Task 8 adds the row). Doc comments on the variants carry the AGENTS.md text: list/show/propose/accept/reject as spec 7.3 states them, with "Accepting or rejecting is the operator's decision: through `ask` the run needs `allow.accept_signatures` (`zeta ask --allow-signatures`; on the page, `allow signatures on`), else `not_allowed`: report the proposal, do not retry" on accept and reject, and "a proposal is harmless: it waits for the operator" on propose.
- `Inner.signatures: Arc<SignatureStore>` opened in `open_with_modes` (its load errors joined to the node's); `Node::signatures()`.
- CLI: `zeta signature list [--state proposed|accepted|rejected|superseded]`, `show --id ID`, `propose --capture ID [--name NAME] [--framing-file FILE]` (the file's JSON is the `framing`), `accept --id ID [--name NAME]`, `reject --id ID`; `zeta ask --allow-signatures`; `main.rs`'s write-op list gains propose, accept and reject (a one-shot beside a running `serve` refuses them as it refuses `capture delete`).
- Agent: `Allow.accept_signatures: bool` (default false); `gate` refuses `signature_accept` and `signature_reject` without it: `{"error": "not_allowed", "field": "op", "op": name, "message": "accepting or rejecting a signature is the operator's decision: report the proposal to the operator; they unlock it by allowing signatures for the run"}`. Page: `allow signatures on|off` (state `allowSignatures`, the note `allow signatures: on|off`), `/ask` carries `accept_signatures`, the refusal note offers `allow signatures and ask again` as the deletes one does (`app.js` ~703-714). `agents_md.rs`'s `RUNNING` text lists `accept_signatures` in the `allow` object.

- [ ] **Step 1: Write the failing tests** (`tests/signatures.rs`: `open(dir)` as `capture_ops.rs`; captures written through `node.bursts().write` from `synth::Fsk` IQ with `features` a `FeatureRecord` of `{center 433_920_000, bandwidth 20_000, symbol_rate_baud 4800, order 2, ...}`)

```rust
#[tokio::test] async fn propose_by_hand_from_a_capture_lists_a_proposal() { /* SignaturePropose{capture, name: None, framing: None} via apply_from(.., "cli"): Signature(rec) with state proposed, confidence 0.3, name "fsk 20.0 kHz at 433.920 MHz", modulation "fsk", provenance.captures == every id of the set, proposed_by "cli", station the node's (None here); SignatureList{state: Some(Proposed)} lists it; the file state_dir/signatures/<id>.json exists */ }
#[tokio::test] async fn a_framing_lifts_the_confidence_and_a_bad_one_is_refused() { /* framing {sync 2dd4/16, fixed_bits 272, crc crc16-ccitt}: 0.6; framing {sync bits 70}: Other starting "framing: sync.bits" */ }
#[tokio::test] async fn accept_pins_the_captures_and_reject_ends_a_proposal() { /* accept with name Some("test link"): state accepted, name, accepted_by "cli", accepted_ns set; capture_list shows every provenance capture pinned; accept again: Other "signature <id> is accepted, not proposed"; reject another: rejected; SignatureShow of an unknown id: Unknown{field: "id"} */ }
#[tokio::test] async fn the_store_reloads_its_records() { /* drop the node, reopen: list equals; a garbage file is in load_errors and skipped */ }
#[tokio::test] async fn a_noise_capture_proposes_nothing() { /* a capture of noise: Other "... is noise: nothing to propose" */ }
```

Agent tests: `signature_accept_and_reject_are_gated` (`gate` refuses both without the switch, admits with it; `signature_propose` and `signature_list` pass). CLI parse tests: `signature accept --id X --name "a b"` → `Op::SignatureAccept { id: "X", name: Some("a b") }`; `signature list` → `state: None`; `ask --allow-signatures` → `Allow.accept_signatures`.

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-core --test signatures` — Expected: FAIL to compile.
- [ ] **Step 3: Implement** the store, the ops, the node methods, the CLI, the gate, the page switch; regenerate `AGENTS.md` and the skill.
- [ ] **Step 4: Run** — `cargo test -p zeta-core --test signatures --test capture_ops`, `cargo test -p zeta-agent`, `cargo test -p zeta cli agents_md ask` — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core crates/zeta crates/zeta-agent crates/zeta-ui AGENTS.md skills
git commit -m "Signatures: the store under state_dir/signatures, signature_list/show/propose/accept/reject, zeta signature, and the agent's allow.accept_signatures gate (allow signatures on the page)"
```

---

### Task 6: Proposals from jobs, and the session record

**Files:**
- Create: `crates/zeta-core/src/node/propose.rs`, `crates/zeta-core/src/sessions.rs`, `crates/zeta-core/tests/proposals.rs`
- Modify: `crates/zeta-core/src/node/capture.rs` (`capture_finished` calls `propose::capture_ended`), `crates/zeta-core/src/node/launch.rs` (`launch_inner` starts the event watcher beside `tee_transcripts`; `supervise` writes the session record beside `capture_finished`), `crates/zeta-core/src/node/mod.rs` (`Inner.sessions`, the sweep after `capture_delete` and at open), `crates/zeta-core/src/ops.rs` (`capture_delete` sweeps), `crates/zeta-core/tests/watch.rs` (one test)
- Test: `crates/zeta-core/tests/proposals.rs`, `crates/zeta-core/tests/watch.rs`

**Interfaces:**
- Consumes: `SignatureStore`, `NewSignature`, `confidence`, `PROPOSE_CRC_FRAMES` (Task 5); `bit_view`'s `hypotheses` event (`crc_candidates[]: {crc, sync, period_bits, frames, checked}`, `sync_candidates[]: {word, bits, ...}`, `encoding.likely`); the `frame` event (Task 2); `ClassifierSet::measure` (layer 1 only); `Inner::job_outputs`-equivalent (`node_outputs` in `launch.rs`).
- Produces in `propose.rs`:
  - `pub(super) async fn capture_ended(inner: &Arc<Inner>, rec: &JobRecord, classified: Option<&Classification>)`, called from `capture_finished` for a hand run (after its classify) and for a watch's child (`classified` `None`): proposes with features only when the run ended on its own with at least one window written, the label is not `noise` (a hand run: the `classified` layer-1 label; a child: `ClassifierSet::measure` over the first window), nothing proved (`classified` has no `tried` with `Outcome::Proved` and no layer-2 label of confidence 1.0) and no accepted signature's band holds the first window's measured centre; `NewSignature { features: the first window's record, modulation: the label, framing: None, crc_frames: 0, evidence: "features only: <measured summary>", captures: the written ids, jobs: [rec.id] plus the owning watch, allocation: the first tx assignment of the window's allocations, proposed_by: "node" }`; publishes `{"protocol": "signature", "state": "proposed", "id", "confidence", "name", "summary": "proposed <name> (<confidence>)", "t_ns"}` on the job's `burst_capture` event stream (still registered: `capture_finished` runs before the streams are unregistered) and merges `{"capture": {"proposal": id}}` into the context.
  - `pub(super) fn watch_for_proposals(inner: &Arc<Inner>, rec: &JobRecord, outputs: &[(StreamKind, StreamId)], token: &CancellationToken)`, spawned from `launch_inner` for records of kind `bits`, `graph`, and `Mode(id)` whose row is a frame row (shaped as `tee_transcripts`: weak `inner`, drains on `token`): on a `hypotheses` event whose `crc_candidates` has an entry with `frames >= PROPOSE_CRC_FRAMES`, or on the `PROPOSE_CRC_FRAMES`th `frame` event with `crc_ok == true`, once per job (a flag in the task; a relaunch by `graph_put` starts a fresh watcher, but `job_context.proposal` set by the earlier one stops it), never for a `learned_*` row's job, and never when an accepted record's band already holds the centre: `NewSignature { features: the first capture's record (captures from `rec.context.bits.captures`, else the graph's first `capture_in`'s `captures`; a frame-row job on a radio has none, and its features are a `FeatureRecord` of the job's tuning and the row's demod: `center_hz`, `bandwidth_hz`, `symbol_rate_baud`, `order`, `snr_db` 0, `burst` false, the rest null), modulation: `rec.context.bits.modulation`, else the demod node's kind (`fsk_demod` → `fsk`), framing: `framing_from_hypothesis` or the row's framing, crc_frames: the candidate's `frames` or the `crc_ok` count, evidence: "crc16-ccitt on 12/14 frames, sync 0x2dd4 every 1120 bits" or "<n> CRC-clean frames (<row id>)", jobs: [rec.id], proposed_by: "node" }`; publishes the `signature` event on the stream the triggering event came from; merges `{"proposal": id}` into the context.
  - `pub fn framing_from_hypothesis(h: &Value, candidate: &Value) -> Value`: `{"encoding": h.encoding.likely, "sync": {"hex": candidate.sync, "bits": <the sync_candidates entry's bits>}, "preamble": null, "whitening": "none", "length": period_bits > 0 ? {"fixed_bits": period_bits - sync bits} : {"to_next_sync": true}, "crc": <candidate.crc without a " lsb-first" suffix>, "bit_order": suffix present ? "lsb" : "msb", "max_errors": 0}`.
- Produces in `sessions.rs`: `pub const SESSIONS_DIR: &str = "sessions"`; `#[serde(rename_all = "snake_case", tag = "kind")] pub enum Outcome { Proposal { id: String }, Proved { mode: String }, None }`; `pub struct Session { pub job: JobId, pub kind: String, pub source: SourceId, pub center_hz: Hz, pub bandwidth_hz: Hz, pub captures: Vec<String>, pub features: Value, pub graph: Graph, pub hypotheses: Value, pub frame_decode: Value, pub outcome: Outcome, pub started_ns: u64, pub ended_ns: u64 }`; `pub struct SessionStore` with `open(state_dir)`, `write(&Session) -> io::Result<()>` (`<job id>.json`, atomic), `list() -> Vec<Session>`, `sweep(live_captures: &HashSet<String>) -> Vec<JobId>` (a session with a non-empty `captures` none of which is live is removed). `pub(super) fn session_of(rec: &JobRecord, ended_ns) -> Session` in `propose.rs`: for kinds `capture`, `bits`, `graph` and frame rows; `captures` from the context (`capture.written`, `bits.captures`, the graph's `capture_in`); `hypotheses` = `context.bit_view` or null; `frame_decode` = `context.frame_decode` or null; outcome `Proposal` from `context.proposal` or `context.capture.proposal`, else `Proved` when `context.frame_decode.crc_ok >= 1` on a frame-row job, else `None`. Written in `supervise` where `capture_finished` is called, for every such kind. The sweep runs at `open`, after `capture_delete` and after every `capture` job's end (the store expires on writes).

- [ ] **Step 1: Write the failing tests** (`tests/proposals.rs`: a file source of the Task 3 framing spec (8 frames, CRC) and one of a plain `Fsk` burst train; `open(dir)` as `capture_ops.rs`; a capture job via `Op::JobStart { kind: Capture, .. }` with `graph_put` setting `bursts: 2, whole: true` as the e2e's `two_windows` does; `until(pred, 10 s)`)

```rust
#[tokio::test] async fn a_hand_capture_proposes_features_only_at_its_end() { /* subscribe to the capture job's event stream first; after the job ends Stopped: one proposed signature, confidence 0.3, modulation "fsk", provenance.captures == job_context.capture.written, jobs == [job], proposed_by "node"; a `signature` event with state proposed on the stream; job_context.capture.proposal == id; state_dir/sessions/<job>.json exists with outcome {kind: proposal, id} */ }
#[tokio::test] async fn a_proved_capture_proposes_nothing() { /* a capture of synthesised POCSAG (the classify proves pocsag_decode): no proposal; the session's outcome is none */ }
#[tokio::test] async fn no_proposal_when_an_accepted_signature_holds_the_band() { /* propose + accept by hand on the band first; a second capture job: still one signature */ }
#[tokio::test] async fn a_bits_job_proposes_once_however_many_passes() { /* bits_start on the framed capture (loop on): within 20 s one proposal at 0.9 with framing {sync 2dd4/16, fixed_bits 272, crc crc16-ccitt, encoding nrz}; wait two more passes (loop_pause 500 ms): still one; job_context.proposal == id; stop the job: the session has hypotheses and outcome proposal */ }
#[tokio::test] async fn a_frame_row_job_proposes_on_five_clean_frames() { /* the Task 4 row on the framing file source: a proposal with the row's framing, evidence "5 CRC-clean frames (fsk_test_frames)", confidence 0.9 */ }
#[tokio::test] async fn sessions_are_swept_with_their_captures() { /* after the first test's job: capture_delete both captures: the session file is gone; a session with no captures (a frame-row job on a radio) stays */ }
// tests/watch.rs
#[tokio::test] async fn a_watch_capture_child_proposes_once() { /* the existing on_unknown: capture fixture: after `captured`, one proposed signature whose jobs hold the child and the watch; the signal's retries add none */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-core --features test-fixtures --test proposals` — Expected: FAIL to compile.
- [ ] **Step 3: Implement** `propose.rs`, `sessions.rs`, the hooks.
- [ ] **Step 4: Run** — `cargo test -p zeta-core --features test-fixtures --test proposals --test watch --test capture_job --test bits_start --test graph_start` — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core
git commit -m "Signatures: the node proposes at a capture job's end (features only) and when a bits, graph or frame job reaches five CRC-clean frames (once per job), publishes the signature event, and writes a session record per exploration job, swept with its captures"
```

---

### Task 7: Learned knowledge and the layer-2 scorer; identify's `learned` verdict

**Files:**
- Create: `crates/zeta-core/src/learned.rs`, `crates/zeta-core/tests/learned.rs`
- Modify: `crates/zeta-core/src/knowledge.rs` (`Signature`'s optional fields), `crates/zeta-core/src/classify.rs` (`ClassifierSet::learned`), `crates/zeta-core/src/node/mod.rs` (register both at open), `crates/zeta-core/src/node/identify.rs` (the unknown verdict's `learned` label), `crates/zeta-core/src/node/watch.rs` and `node/watch/capture.rs` (the summary; no capture of a learned signal), `crates/zeta-core/src/node/propose.rs` (the rule is already "no accepted signature holds the band"), `crates/zeta/src/agents_md.rs` (`knowledge_lookup`'s note: `tier`, learned signatures)
- Test: `crates/zeta-core/tests/learned.rs`, `crates/zeta-core/tests/identify.rs`, `crates/zeta-core/tests/watch.rs`

**Interfaces:**
- Consumes: `SignatureStore::{accepted, matched}`, `SignatureRecord::band_holds`, `Measurement`, `Classifier` (`classify_measured`), `nearest_allocations`.
- Produces: `Signature` gains `#[serde(default)]` nullable `order: Option<u8>`, `tone_spacing_hz: Option<Hz>`, `burst: Option<crate::signatures::Burst>`, `sync: Option<String>`, `encoding: Option<String>`, `crc: Option<String>`, `confidence: Option<f32>`, `tier: Option<String>` (`require_all`; the seed file and `CatalogueKnowledge` leave them null; `tier` is `learned` only from the provider below). `learned.rs`: `pub struct LearnedKnowledge(Arc<SignatureStore>)` (`name()` `"learned"`; `allocations` empty; `signatures(center, bw)` the accepted records whose band overlaps, each as a `Signature` with `tier: Some("learned")`, `decoder` the record's, `confidence` the record's, `sync` the framing's sync hex, `encoding` and `crc` its names); `pub struct LearnedClassifier(Arc<SignatureStore>)` (`name()` `"learned"`, `layer()` 2, `classify` empty; `classify_measured` with `measured` `Some`: for each accepted record, the score of the Global Constraints' terms (centre: `band_holds(features.center_hz)`; bandwidth within `LEARNED_BANDWIDTH_RANGE` of the record's; modulation: the measured label equals the record's `modulation` and the orders are equal or either is null; rate within `LEARNED_RATE_TOLERANCE` when both have one; period within `LEARNED_PERIOD_TOLERANCE` when both have one), capped at 1.0; a record with the centre term and a score of at least `LEARNED_MIN_SCORE = 0.7` labels `learned <name>` at `confidence × score` with evidence `learned <id>: centre, bandwidth, fsk, 4800 Bd, period` naming the terms that hit, and `store.matched(id)`; the best record only). `ClassifierSet::learned(&self, measured: &Measurement) -> Vec<Label>`: the layer-2 classifiers named `learned` run with no IQ. `Node::open_with_modes` registers both (every node, tests included, scores accepted signatures).
- Identify: after its layer-1 measurement the job calls `classifiers.learned(&measurement)`; the unknown verdict's `label` is `learned <name>` when the top such label has confidence at least `LEARNED_VERDICT_MIN = 0.5` (else the layer-1 label as today), and the verdict gains `"learned": {"id", "name", "confidence"}` (null otherwise). The watch: an `unknown` whose verdict carries `learned` reads `learned <name> at <MHz>: <measured>` in its summary, is not captured (`on_unknown` returns first) and keeps its retry schedule (a real decoder may still supersede the signature). `knowledge_lookup` lists the learned signatures beside the seed and the catalogue through the provider.

- [ ] **Step 1: Write the failing tests** (`tests/learned.rs`: a capture of synthesised 2-FSK 4800 Bd in a 20 kHz band at 433.92 MHz with a measured `FeatureRecord`; propose by hand and accept)

```rust
#[tokio::test] async fn an_accepted_signature_labels_a_later_capture_learned() { /* capture_classify on a second window of the same signal: a layer-2 label "learned <name>" with confidence within 0.01 of 0.3 × score and score >= 0.7; evidence starts "learned <id>:"; the record's matches == 1 and last_matched_ns set */ }
#[tokio::test] async fn a_proposed_or_rejected_signature_is_not_scored() { /* no label before accept; none after reject */ }
#[tokio::test] async fn knowledge_lookup_lists_learned_signatures_with_their_tier() { /* signatures has one with tier "learned", decoder null, sync null for a features-only record; the seed APRS entry still loads with tier null */ }
#[tokio::test] async fn a_different_signal_on_the_band_scores_under_the_floor() { /* a 100 kHz psk capture on the same centre: no learned label */ }
// tests/identify.rs
#[tokio::test] async fn identify_reports_a_learned_signal_in_its_unknown_verdict() { /* on the FSK file source with an accepted signature: verdict.mode null, verdict.label "learned <name>", verdict.learned.id == id */ }
// tests/watch.rs
#[tokio::test] async fn the_watch_does_not_capture_a_learned_signal() { /* on_unknown: capture; an accepted signature on the signal's band: the unknown event's summary starts "learned <name> at", no capturing event within the retry window, no new proposal */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-core --test learned` — Expected: FAIL to compile.
- [ ] **Step 3: Implement** `learned.rs`, the `Signature` fields, the registration, identify's verdict, the watch's rule; regenerate `AGENTS.md` and the skill.
- [ ] **Step 4: Run** — `cargo test -p zeta-core --features test-fixtures --test learned --test identify --test watch --test ops`, `cargo test -p zeta-core knowledge` — Expected: PASS (the builtin-signatures tests still load the seed).
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core crates/zeta AGENTS.md skills
git commit -m "Signatures: LearnedKnowledge lists accepted signatures (tier learned) and LearnedClassifier scores them in layer 2; identify's unknown verdict names a learned match and the watch neither captures nor proposes it again"
```

---

### Task 8: The learned row, the registry reload, and superseding

**Files:**
- Modify: `crates/zeta-core/src/modes/mod.rs` (`ModesHandle`, `ModeRegistry::carry_programs`), `crates/zeta-ext/src/external.rs` (the `OnceLock` alias replaced by `zeta_core::modes::ModesHandle`), `crates/zeta-dsp/src/identify.rs` and `crates/zeta-dsp/src/catalogue.rs` (`new(handle)`), `crates/zeta-dsp/src/lib.rs` (`classifiers(handle)`), `crates/zeta-core/src/node/mod.rs` (`Inner.modes: ModesHandle`, `Inner::modes()`, `Node::modes_handle()`, `Node::reload_modes()`, `signature_accept`'s row), `crates/zeta-core/src/node/identify.rs` (`Inner::verdict` supersedes), `crates/zeta-core/src/ops.rs` (`classify` and `capture_classify` supersede on a `proved`), `crates/zeta-core/src/node/propose.rs` (the watcher's first-event supersede, for every mode-row job), `crates/zeta-core/src/signatures.rs` (`learned_row`), `crates/zeta/src/node_setup.rs` (one handle through), the tests that call `zeta_dsp::classifiers(node.modes())`
- Test: `crates/zeta-core/tests/learned.rs` (continued), `crates/zeta-core/tests/modes.rs`

**Interfaces:**
- Produces: `#[derive(Clone, Default)] pub struct ModesHandle(Arc<RwLock<Option<Arc<ModeRegistry>>>>)` in `zeta_core::modes` with `get() -> Option<Arc<ModeRegistry>>` and `set(Arc<ModeRegistry>)` (replacing; `ExternalDecodeStage`, `DecodeToIdentify` and `CatalogueKnowledge` read `get()` at each use); `ModeRegistry::carry_programs(&mut self, old: &ModeRegistry)` (a program row's `program` and `state` from `old` by id); `Node::modes_handle() -> ModesHandle`; `Node::reload_modes(&self) -> Result<Vec<String>, NodeError>`: `ModeRegistry::load(&inner.registry, modes_dir)` (the `NodeConfig.modes_dir`, else `default_operator_dir()`), `carry_programs`, `load_checked`, `set` on the handle; returns the broken rows' startup lines. `signatures::learned_row(rec: &SignatureRecord, mode_id: &str) -> ModeRow`: `id` `learned_<id lowercased>`, `name` the signature's, `family general`, `band {bandwidth_hz: the record's, centers_hz: [the record's centre], min_hz: null, range_hz: null}`, `modulation [the record's modulation]` (plus `fm` for `fsk`), `label` the name, `protocol "frame"`, `decoder {kind: frame, demod {modulation, symbol_rate_baud, order (2 when null), tone_spacing_hz}, framing: the record's, parser: "opaque"}`, `event` the frame event's schema and `summary "frame {length_bytes} B"`, `identify {proof: field, field: crc_ok, min_events: 1, dwell_ms: 10000, min_span_ms: 1000, quick: frame, priority: 0}`, `catalogue []`, `test null`, `notes "generated from signature <id>; edit freely (a parser, a subject); not overwritten"`. `Node::signature_accept` after the pin: when `framing.crc` is not null and `symbol_rate_baud` is set: the path `<modes_dir>/learned_<id>.json`; the file present → `decoder_note` `row learned_<id> already present: kept as is`; no modes directory (none configured and no `HOME`/`XDG_CONFIG_HOME`) → `decoder_note` `no modes directory: row not written`; else written (pretty JSON, the directory created) and `reload_modes()`; `decoder` set to the row id in every case but the last, `decoder_file` the path. `Inner::supersede_by(&self, mode: &str, tuning: Tuning, now_ns)`: for every accepted record, when `mode` does not start with `learned_`, the record's band holds `tuning.center_hz` and `tuning.bandwidth_hz` lies within 0.5-2× the record's bandwidth: `store.supersede(id, mode)` and a stderr line `zeta: signature <id> (<name>) superseded by <mode>`; called from `Inner::verdict` when the verdict's `mode` is a string (with the proved child's tuning), from `classify`/`capture_classify` for each `Tried` with `Outcome::Proved` (with the classified band), and from the Task 6 watcher on the first event of any mode-row job (the watcher now runs for every `Mode` job and exits after that event unless the row is a frame row).

- [ ] **Step 1: Write the failing tests**

```rust
// tests/learned.rs
#[tokio::test] async fn accepting_a_framed_signature_writes_the_learned_row_and_it_decodes() { /* a proposal with the Task 4 framing (propose by hand with framing, or Task 6's bits proposal): accept: decoder "learned_<id>", decoder_file under the test's modes_dir, decoder_note null; modes_list shows it ready with decoder_kind frame; job_start --kind learned_<id> on the framing file source gives a frame event with crc_ok true */ }
#[tokio::test] async fn an_existing_learned_row_is_kept_as_is() { /* write a row with that id and `name: "edited"` first: accept sets decoder and decoder_note "row learned_<id> already present: kept as is"; the file's name is still "edited"; after reopen the row still loads */ }
#[tokio::test] async fn accept_without_a_writable_modes_directory_accepts_and_says_so() { /* NodeConfig.modes_dir pointing under a plain file (the directory cannot be created): state accepted, captures pinned, decoder null, decoder_note starting "row not written:" with the io error; the no-directory case (`modes_dir` None and no HOME/XDG) follows the same path with the note "no modes directory: row not written" and is covered by the code, not an env-var test */ }
#[tokio::test] async fn a_features_only_signature_writes_no_row() { /* decoder null, decoder_note null */ }
#[tokio::test] async fn a_proved_classify_supersedes_an_overlapping_learned_signature() { /* accepted signature on 148.6375 MHz / 12.5 kHz; capture_classify of a POCSAG capture there: proved; the record is superseded, superseded_by "pocsag_decode", captures still pinned, no longer labelled by the scorer */ }
#[tokio::test] async fn an_identify_proof_supersedes_and_a_learned_row_never_does() { /* identify proving pocsag_decode supersedes; a learned_<id> job's first event leaves another accepted learned signature on the band accepted */ }
// tests/modes.rs
#[tokio::test] async fn reload_modes_picks_up_a_new_row_and_keeps_program_state() { /* write a row file after open; reload_modes: modes_list has it; a program row's state and program are as before the reload */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-core --test learned --test modes` — Expected: FAIL to compile.
- [ ] **Step 3: Implement** the handle across the four crates, the reload, the row writer, the three supersede hooks; regenerate `AGENTS.md` and the skill (the accept note gains the row).
- [ ] **Step 4: Run** — `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium` (the handle touches every node and classifier test) — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates AGENTS.md skills
git commit -m "Signatures: accepting a framed signature writes learned_<id>.json to the operator's modes directory and reloads the rows through a hot-swappable ModesHandle; a proved built-in or operator row supersedes an overlapping learned signature"
```

---

### Task 9: The page: proposals on the Explore pane, Accept, Rename, Reject

**Files:**
- Modify: `crates/zeta-ui/web/explore.js` (the proposals table), `crates/zeta-ui/web/style.css` (`tr.signature`, `.ribbon.signature`), `crates/zeta-ui/web/app.js` (the `allow signatures` switch landed in Task 5; the Explore pane's eligibility), `crates/zeta-ui/tests/e2e/ui.py`
- Test: `ui.py` (`scenario_proposal_accepted_from_the_page`, `scenario_allow_signatures_switch`)

**Interfaces:**
- Consumes: `signature_list`, `signature_accept`, `signature_reject` (Task 5); the pane's poll (`EXPLORE_POLL_MS`), `DELETE_CONFIRM_MS`; `overlays.js` already draws any event with a `summary` as a ribbon classed by its `protocol`, so the `signature` event is a ribbon with no page change but a `.ribbon.signature` style.
- Produces: the Explore pane polls `signature_list {state: null}` on the same timer as `capture_list` and renders, under the captures table, a `table.signatures` of the `proposed` and `accepted` records newest first, one `tr.signature[data-id]` each: `td.name` holding `input.sig-name` (prefilled with the record's name; `readonly` once accepted), `td.confidence` (`0.90`), `td.state` (`proposed`, or `accepted · learned` plus the row id `learned_<id>` when `decoder` is set), `td.evidence` (the record's `evidence`), `td.actions` with `button[data-action="accept"]` (`Accept`; proposed only: `signature_accept {id, name: <the input when changed, else null>}`; the transcript notes `accepted <name> (<id tail>)` and `decoder_note` when set) and `button[data-action="reject"]` (`Reject`, armed like Delete: `sure?` for `DELETE_CONFIRM_MS`, then `signature_reject`). The pane is eligible while any capture or any listed signature exists. `export function signatureText(rec)` for the evidence cell (the record's `evidence`, else `<modulation> <kHz> kHz`). `EXPLORE_SIGNATURE_STATES = ["proposed", "accepted"]`.

- [ ] **Step 1: Write the failing scenarios** (helpers: `propose(url, secret, capture, framing=None, name=None)` → `signature_propose` via `http_json`; `clear_signatures` rejects every proposed one; the fsk fixture's framing is `{"sync": {"hex": "2dd4", "bits": 16}, "length": {"fixed_bits": 1104}, "crc": "crc16-ccitt"}` with `FRAME_BITS`-shaped bodies from `crates/zeta/tests/ui.rs`'s fixture constants)

```python
def scenario_proposal_accepted_from_the_page(page, url, secret):
    clear_captures(url, secret); clear_signatures(url, secret)
    job = start_job(url, secret, "capture", "fsk", 433_920_000, 12_000)
    two_windows(url, secret, job)
    wait_until(lambda: len(capture_ops(url, secret, "capture_list", signal=None)) == 2, 30, "two captures")
    newest = capture_ops(url, secret, "capture_list", signal=None)[0]
    sig = propose(url, secret, newest["id"], framing=FSK_FRAMING)
    other = propose(url, secret, newest["id"])
    login(page, url, secret)
    page.locator('#panes .pane-tab[data-pane="explore"]').click()
    rows = page.locator("#explore tr.signature")
    expect(rows).to_have_count(2, timeout=20000)
    row = f'#explore tr.signature[data-id="{sig["id"]}"] >> '
    expect(page.locator(row + "td.confidence")).to_have_text("0.60")
    page.fill(row + "input.sig-name", "fsk test link")
    page.click(row + 'button[data-action="accept"]')
    expect(page.locator("#transcript .line.note", has_text="accepted fsk test link").first).to_be_visible(timeout=20000)
    expect(page.locator(row + "td.state")).to_contain_text("learned", timeout=20000)
    accepted = capture_ops(url, secret, "signature_show", id=sig["id"])
    assert accepted["state"] == "accepted" and accepted["name"] == "fsk test link", accepted
    assert all(c["pinned"] for c in capture_ops(url, secret, "capture_list", signal=None)), "captures pinned"
    other_row = f'#explore tr.signature[data-id="{other["id"]}"] >> '
    page.click(other_row + 'button[data-action="reject"]')
    expect(page.locator(other_row + 'button[data-action="reject"]')).to_have_text("sure?")
    page.click(other_row + 'button[data-action="reject"]')
    expect(rows).to_have_count(1, timeout=20000)
    # pinned captures are not deleted by clear_captures: unpin is not an op; leave them (the suite's state dir is fresh per run)

def scenario_allow_signatures_switch(page, url, secret):
    login(page, url, secret)
    run_cmd(page, "allow signatures on")
    expect(page.locator("#transcript .line.note:not(.error)", has_text="allow signatures: on")).to_have_count(1)
    run_cmd(page, "allow signatures off")
    expect(page.locator("#transcript .line.note:not(.error)", has_text="allow signatures: off")).to_have_count(1)
```

(The accepted signature writes `learned_<id>.json` into the suite's `--modes-dir`: the e2e node runs with one (`ui.rs` passes `--modes-dir`); the scenario leaves the row, which later scenarios ignore. Pinned captures stay in the store for the rest of the run: scenarios that count captures call `clear_captures` and so must tolerate pinned leftovers, or this scenario runs last; add the `pinned` filter to `clear_captures`'s callers' counts where they assert an exact count, or sort this scenario after them in `scenarios()` by name.)

- [ ] **Step 2: Run to verify they fail** — `ZETA_UI_ONLY=proposal_accepted_from_the_page,allow_signatures_switch cargo test -p zeta --test ui` — Expected: FAIL (no `tr.signature`).
- [ ] **Step 3: Implement** the proposals table, the styles, the eligibility.
- [ ] **Step 4: Run the UI suite** — `cargo test -p zeta --test ui -- --nocapture` — Expected: PASS, no `ui e2e skipped`.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-ui crates/zeta/tests/ui.rs
git commit -m "UI: the Explore pane lists proposed and accepted signatures under the captures with Accept (renaming inline), Reject and the learned row's id; the signature ribbon's style; the allow signatures scenario"
```

---

### Task 10: Field craft and the agent guide

**Files:**
- Modify: `docs/agents/field-craft.md` ("Exploring an unknown signal" step 5, "Working layer 4" items 6 and 11, a new "Frame rows and signatures" section under "Decoding modes"), `crates/zeta/src/agents_md.rs` (the modes note already names `frame`; the `allow` text; the "Working layer 4" link), `AGENTS.md`, `skills/zetasdr/SKILL.md` (regenerated)

- [ ] **Step 1: Write the field craft.** "Working layer 4" item 6 adds `frame_decode` (`graph_put` a `frame_decode {sync, length, crc, encoding}` after the slicer when a hypothesis checks; read `job_context.frame_decode`; `to_next_sync` for a `period_bits` 0 candidate; the frame event's `crc_ok`). Item 11 becomes: the node proposes a signature itself (a capture job's end with features only at 0.3; a bits or frame job on five CRC-clean frames at 0.9; the `signature` event and `job_context.proposal`); `signature_list {state: "proposed"}` to see them, `signature_show` for the evidence; report a proposal to the operator with its confidence and evidence; `signature_accept`/`signature_reject` are the operator's (`allow.accept_signatures`), never call them unasked; `signature_propose` by hand when the operator names a framing; an accepted framed signature becomes `learned_<id>`, a mode like any other, so the next encounter is proved by `identify` or decoded by `job_start --kind learned_<id>`; a features-only one labels later encounters `learned <name>` (an identify verdict with `learned`, the watch's summary), which is a layer-2 match, not a decode: say so. "Exploring an unknown signal" step 5 states the same ladder end. The new section: a frame row's fields, `modes check` over the `framing` family, the parsers (Task 11, or "none yet" when dropped), the boundary line (the spec's, once). Keep the legal line above "Working layer 4" as it is.
- [ ] **Step 2: Regenerate and check** — `cargo run -q -p zeta -- agents-md > AGENTS.md && cargo run -q -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md && cargo run -q -p zeta -- agents-md --check && cargo run -q -p zeta -- agents-md --skill --check`, `cargo test -p zeta --test agents_md`, `bash scripts/check-doc-links.sh` — Expected: clean.
- [ ] **Step 3: Commit**

```bash
git add docs/agents AGENTS.md skills crates/zeta/src/agents_md.rs
git commit -m "Guide: frames and signatures in the field craft (frame_decode in the layer-4 playbook, proposals and the operator's accept, learned rows and labels, frame rows)"
```

---

### Task group 11 (droppable): the parsers

> This group is the last of the plan and is droppable without touching the rest (spec 7.2, call 11). When it is dropped, Task 12 still runs and its TODO entry says so.

Boundary (verbatim from the spec, so a later session does not drift): this decodes clear, unencrypted, standards-based framing that the node is lawfully receiving. It never attempts to break encryption, never targets a specific operator's link as the deliverable, and emits no transmit path. An encrypted or proprietary payload is reported as `framed, opaque` with its length and CRC status, not decoded.

#### Task 11a: `frame_parse` and the four parsers

**Files:**
- Create: `crates/zeta-dsp/src/frame_parse.rs`, `crates/zeta-dsp/src/parsers/mod.rs`, `crates/zeta-dsp/src/parsers/{modbus_rtu,dnp3,iec101,iec104}.rs`
- Modify: `crates/zeta-dsp/src/lib.rs` (modules, `register_all`)
- Test: each parser's tests module, `frame_parse.rs` tests

**Interfaces:**
- Consumes: the `frame` event (Task 2), `crc::{CRC16_MODBUS, CRC16_DNP, crc}`.
- Produces: `pub const FRAME_PARSE_KIND: &str = "frame_parse"`; `pub const PARSERS: &[&str] = &["modbus_rtu", "dnp3", "iec101", "iec104"]`; `#[serde(deny_unknown_fields)] pub struct FrameParseParams { pub parser: String }` (`check_params`: `parser: one of modbus_rtu, dnp3, iec101, iec104`); `FrameParseStage` (inputs `[Event]`, outputs `[Event]`): for each `frame` event, `parsers::parse(name, &bytes) -> Result<serde_json::Value, String>`; `Ok(fields)` publishes `{"protocol": "<parser>", ..fields.., "t_ns", "summary"}` (the row envelope when `ctx.mode` is set), `Err` is counted in the note `{"frame_parse": {"parsed", "rejected", "last_error"}}` and nothing is published; every other event passes through. Each parser is a pure `pub fn parse(bytes: &[u8]) -> Result<Value, String>` gated by the protocol's own check:
  - `modbus_rtu`: `address`, `function`, `data` (hex), `crc_ok` (CRC-16/MODBUS over all but the last two bytes, little-endian field; `Err("crc")` when it fails), `exception` (bool, function bit 7); summary `modbus <address> fn <function>`.
  - `dnp3`: start `05 64`, `length`, `control` (`dir`, `prm`, `function`), `destination`, `source`, the header CRC and each 16-byte block's CRC (CRC-16/DNP, little-endian) all checked (`Err("crc")`), `user_data` (hex, CRCs removed); summary `dnp3 <source> -> <destination> fn <function>`.
  - `iec101` (FT1.2 variable frame): start `68`, `length` twice, start again, `control` (`prm`, `fcb_acd`, `fcv_dfc`, `function`), `address`, `asdu` (hex), the checksum (the arithmetic sum mod 256 of control through the ASDU; `Err("checksum")`) and the end byte `16` (`Err("end")`); summary `iec101 link <address> fn <function>`.
  - `iec104` (APCI/APDU): start `68`, `length`, the control field's format (`i`, `s` or `u` from the two low bits of the first control byte), `send_seq`, `recv_seq`, `asdu` (hex; `type_id`, `cause`, `common_address` when the format is `i`); summary `iec104 <format> type <type_id>`.

- [ ] **Step 1: Write the failing tests** (vectors from the standards' own examples: Modbus `01 03 00 00 00 0A C5 CD` (read holding registers; CRC C5CD), a DNP3 read request `05 64 0B C4 01 00 00 00 <crc> C0 C1 01 3C 02 06 <crc>` with the CRCs computed by `crc(&CRC16_DNP, ..)` in the test, an IEC 101 frame `68 09 09 68 73 01 64 01 06 01 00 00 14 <cs> 16` with the checksum computed, an IEC 104 I-frame `68 0E 02 00 02 00 64 01 06 00 01 00 00 00 00 14`; the dnp3 and iec vectors' bytes are built by helper fns so no magic CRC is typed)

```rust
#[test] fn modbus_parses_and_rejects_a_bad_crc() { /* fields address 1, function 3, data "0000000a", crc_ok true; the last byte flipped: Err("crc") */ }
#[test] fn dnp3_parses_header_and_blocks_and_rejects_a_bad_block_crc() {}
#[test] fn iec101_checks_the_sum_and_the_end_byte() {}
#[test] fn iec104_reads_the_apci_and_the_asdu_header() {}
#[tokio::test] async fn frame_parse_publishes_parsed_frames_in_the_parser_protocol_and_counts_rejects() { /* two frame events, one valid one not: one event with protocol "modbus_rtu"; note parsed 1, rejected 1 */ }
#[test] fn an_unknown_parser_is_refused() { /* check_params {"parser": "x"}: Err("parser: one of modbus_rtu, dnp3, iec101, iec104") */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-dsp --features test-fixtures parsers frame_parse` — Expected: FAIL to compile.
- [ ] **Step 3: Implement** and register.
- [ ] **Step 4: Run** — `cargo test -p zeta-dsp --features test-fixtures parsers frame_parse stage_params_schemas_are_strict` — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-dsp
git commit -m "DSP: frame_parse with the Modbus RTU, DNP3, IEC 60870-5-101 and -104 parsers, each a pure bytes-to-fields decoder behind the protocol's own check"
```

#### Task 11b: The parser rows and `modes check`

**Files:**
- Create: `modes/dnp3.json`, `modes/iec101.json`, `modes/iec104.json`
- Modify: `crates/zeta-core/src/modes/mod.rs` (`EMBEDDED`, 8 → 11), `crates/zeta-core/src/modes/row.rs` (`FRAME_PARSERS` = `opaque` plus the four), `crates/zeta-core/tests/modes.rs` (`EMBEDDED`), `crates/zeta/tests/modes_check.rs` (the in-crate rows count), `docs/agents/field-craft.md` ("Frame rows and signatures": the three rows and why Modbus RTU is a parser without a row), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-core/tests/modes.rs`, `crates/zeta/tests/modes_check.rs`

**Interfaces:**
- Produces: three rows, each `family iot`, `band {bandwidth_hz: 12500, centers_hz: [], min_hz: null, range_hz: null}` (no well-known centre: chosen by signature match or named by the operator), `modulation ["fsk", "fm"]`, `decoder {kind: frame, demod {modulation: fsk, symbol_rate_baud: 9600, order: 2, tone_spacing_hz: null}, framing: <below>, parser: <id>}`, `event` schema and `summary` as the parser's fields, `identify {proof: event, min_events: 1, dwell_ms: 10000, min_span_ms: 1000, quick: frame, priority: 0}`, `catalogue []`, `test {kind: synth, family: framing, modulation: fsk, symbol_rate_baud: 9600, deviation_hz: 4800.0, frames: 6, bodies: [<the Task 11a vector's bytes after the sync, six times>], framing: <the same>, repeat: 1, snr_db: 20, expect: {<two of the parser's fields>}, within_ms: 10000}`. Framings: `dnp3` sync `0564`/16, `length {to_next_sync: true}`, `crc null` (the parser checks the block CRCs); `iec101` sync `68`/8, `length {field: {offset_bits: 0, bits: 8, big_endian: true, unit: "bytes", add: 5}}`, `crc null` (the fixed-length `10` frame is not decoded; the row's `notes` say so); `iec104` sync `68`/8, `length {field: {offset_bits: 0, bits: 8, big_endian: true, unit: "bytes", add: 1}}`, `crc null`. The parser's own check (not a `frame_decode` CRC) is what proves these rows, so their `identify.proof` is `event` (an event is published only when the parser accepted the frame). Modbus RTU ships as a parser only (no built-in row): its frames carry no sync word or framing of their own on the air (they are UART characters delimited by silence), so a built-in `framing` would be an invention; an operator row names the modem framing in use and `parser: modbus_rtu`.

- [ ] **Step 1: Write the failing tests** — `tests/modes.rs`: `EMBEDDED` lists the eleven and each parses, validates and is `ready`; `a_parser_row_job_decodes_its_vector` for `dnp3` (a file source of its `test` spec, `job_start --kind dnp3`, the first event has `protocol "dnp3"` and the vector's `destination`); `modes_check.rs`: the in-crate rows test counts the three among the `ok` lines.
- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-core --test modes` — Expected: FAIL (8 embedded).
- [ ] **Step 3: Write the rows**, update the lists and the field craft, regenerate.
- [ ] **Step 4: Run** — `cargo test -p zeta-core --test modes --test frame_rows`, `cargo test -p zeta --test modes_check --test agents_md`, `cargo run -q -p zeta -- modes check --strict dnp3` (and `iec101`, `iec104`) — Expected: PASS, `<id>: ok`.
- [ ] **Step 5: Commit**

```bash
git add modes crates docs AGENTS.md skills
git commit -m "Modes: dnp3, iec101 and iec104 frame rows (iot family, proved by the parser's own check, checked over the framing family); Modbus RTU is a parser for operator rows"
```

---

### Task 12: TODO, the plan series, UAT, README, the gate

> The closing task, run after Task 11b, or after Task 10 when the parsers group is dropped.

**Files:**
- Modify: `TODO.md`, `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (row 19.3 and a "Plan 19.3 status" section after "Plan 19.2 status"; the outcome table's 19.1/19.2 rows say "19.3 built"), `UAT.md` (a "Plan 19.3" section), `README.md` (the plan line where 19.2's is, if the list is there; otherwise nothing)

- [ ] **Step 1: TODO.md** — Remove "Feature: generic layer-4 framing decoders (bits to structured frames)" (delivered: `frame_decode`, frame rows, the parsers; or, with the group dropped, rewrite it to the parsers alone, with the boundary paragraph kept verbatim). In "Feature: layer 4, working an unknown signal" drop the "Write-back of a successful hypothesis as a signature (plan 19.3)" line and keep the rest (live chains, LoRa, the `measure` upgrades). Leave "Bits: deferred minors from plan 19.2" as it is. Add "Signatures: deferred minors from plan 19.3": `matches` counts every classify, a `capture_classify` replay included; no decay of a learned signature's confidence (spec non-goal); a hand `signature_propose` with a framing carries no CRC count, so it is 0.6 at most; the Bits pane does not show `frame` events (a `crc ok|bad` tag per frame row would); a learned row's `band.centers_hz` is one centre, so a link that moves channel scores only the signature; the IEC 101 fixed-length frame is not decoded; `frame_decode`'s `manchester` phase is chosen per segment by the sync hunt. Add a gist "Program rows on events: a program row whose `decoder.input.stream` is `event` (JSON lines of a running job's events, `frame` events included, on stdin) so an operator pipes a frame row to an external parser or handler, Modbus RTU being the operator's example (2026-11-06); today only a plugin stage on a graph row takes event input". Add a `pulse_ook` line to "Feature: more digital decoding modes" if the 2026-11-04 remote gist has not already.
- [ ] **Step 2: Plan series** — row 19.3 says built on this branch with the commit range and "see Plan 19.3 status"; the status section lists what shipped (the stage, the frame rows and quick attempt, the store and ops, proposals and sessions, learned knowledge and scorer, the learned row and reload, superseding, the page, the parsers or their dropping), what the gate proved (the Task 2 round trips and vectors, the Task 3 chain, `modes check` on the frame rows, the proposal and supersede tests, the e2e accept), the calls made (the list under "Calls made in this plan" below, in one paragraph), and what waits for the field host (spec 9 items 1-5, the operator's acceptance of plan 19 as a whole). `README.md` gains the plan line where 19.2's is, when the README lists plans.
- [ ] **Step 3: UAT.md** — "Plan 19.3" with the field host's checks from spec 9: (4) a proposal appears after a watch capture of the 404.025 MHz link (`zeta signature list --state proposed`), accepting it from the page pins its captures, the next watch run reports `learned <name>` in the `unknown` summary (or, when a CRC was found, `learned_<id>` in `zeta modes list` and the watch decoding through it); (5) `zeta modes check --strict dnp3 iec101 iec104` passes (when shipped); and `zeta signature show` reads sensibly for the operator.
- [ ] **Step 4: Gate** — `cargo fmt -p <each crate> -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`, `cargo test -p zeta --test ui -- --nocapture` (no `ui e2e skipped`), `cargo run -q -p zeta -- agents-md --check`, `cargo run -q -p zeta -- agents-md --skill --check`, `bash scripts/public-check.sh`, `bash scripts/check-doc-links.sh`. Expected: all green.
- [ ] **Step 5: Commit**

```bash
git add TODO.md docs UAT.md README.md
git commit -m "Docs: plan 19.3 (frames and signatures) in TODO, the plan series status, UAT and README"
```

---

## Calls made in this plan (the builder's, on the spec's silence)

1. Proposals are made by the node, not by stages: `capture_finished` for capture jobs, a per-job event watcher (shaped as `tee_transcripts`) for bits, graph and frame-row jobs; the `signature` event is published on the stream the triggering event came from (the capture job's `burst_capture` stream). Stages stay ignorant of the signature store.
2. `LearnedKnowledge` and `LearnedClassifier` are registered by `Node::open` itself, so every node and every node test scores accepted signatures without transport wiring.
3. The mode registry becomes hot-swappable through `zeta_core::modes::ModesHandle` (replacing zeta-ext's `OnceLock` alias) and `Node::reload_modes`; program rows keep their probed state across a reload.
4. `frame_decode`'s `length` gains `{to_next_sync: true}`: `bit_view` already reports frames that run from each sync to the next (`period_bits` 0), and a proposal from such a candidate needs a framing that says so. `fixed_bits` counts the bits after the sync.
5. A hand `signature_propose` with a `framing` is 0.6 (no CRC count is known); the node's proposals are 0.3 or 0.9.
6. Manchester: `10` is 1 and `01` is 0, the pair phase chosen by the sync hunt; `pn9` is TI's (x^9 + x^5 + 1, seed 0x1FF, bit-serial in frame bit order), confirmed as the default by the operator on 2026-11-06 (Semtech's bit order and 802.15.4g's sequence are reached through `{poly, seed}` or a later name); `{poly, seed}` for any other.
7. CRC-8 is CRC-8/SMBUS (poly 0x07), listed last in the table; `crc16-modbus` joins it; `CrcDef` is the general form.
8. The `frame` quick attempt runs the real stages offline (`offline::run_chain`, moved from the test harness); the quick attempts become async closures that can carry the row.
9. The features-only proposal is skipped when an accepted signature already holds the band, and the watch neither captures nor proposes a signal identify labels `learned`; identify's unknown verdict carries `learned {id, name, confidence}` and names it in `label` from 0.5.
10. Superseding needs the proving band's centre inside the signature's band and its width within 0.5-2× the signature's; three hooks (identify's proof, a `proved` in classify and capture_classify, a mode-row job's first event); a `learned_*` row never supersedes.
11. The generated row: `band.centers_hz` the signature's one centre, `modulation` the signature's label (plus `fm` for `fsk`), `identify {proof: field, field: crc_ok, quick: frame}`, `parser: opaque`; written only for a framing with a CRC; the operator's modes directory, else `default_operator_dir()`, else accepted without a row and said so.
12. Session records are swept at open, after `capture_delete` and after each capture job's end (the store expires on writes); a session with no captures is never swept.
13. Modbus RTU ships as a parser without a built-in row (the operator accepted this shape on 2026-11-06); the IEC 101 row decodes the variable-length frame only. The operator notes a Modbus stream can also be piped to an external handler; a program row cannot take that route yet (`decoder.input.stream` is `iq` or `audio`, never `event`), so a program row consuming a frame row's `frame` events goes to TODO as a gist (Task 12), and today an external parser reaches those events only as a plugin stage on a graph row.
14. The page lists proposed and accepted signatures (rejected and superseded ones only through `signature_list`); renaming is the name input on the row, sent with Accept.

## Answered by the operator (2026-11-06)

1. Modbus RTU: the plan's shape stands (a parser, no built-in row); where an operator wants an external handler on the frames, see call 13 and the Task 12 gist.
2. `pn9`: TI's variant is the default (call 6).

## Self-review notes

- Spec coverage: 7.1 → Tasks 1, 2 (the CRC library completed; the stage, its event and context counters; `bit_view` on the same table). 7.2 → Tasks 3, 4 (the `frame` decoder kind, the expansion, `job_start --kind`, the quick attempt proving in `classify`, `identify` running it, `modes check` over the `framing` family) and 11a, 11b (the parsers and their rows, the boundary paragraph verbatim, droppable and last). 7.3 → Tasks 5 (the record, the ops, the gate, the pin), 6 (the three proposers and the `signature` event), 7 (`LearnedKnowledge`, the `Signature` fields, the `LearnedClassifier` terms, `matches`), 8 (superseding). 7.4 → Task 8. 7.5 → Task 6. 7.6 → Tasks 9, 10. Section 8's 19.3 lines: `frame_decode` on each encoding and length rule (Task 2), every CRC against a known vector (Task 1), proposals, accept with the pin and the generated row, reject, supersede by a proved row, the `ask` gate (Tasks 5-8), `modes check` on the frame rows over the `framing` family (Tasks 4, 11b), a proposal accepted from the page (Task 9), the gate (Task 12).
- Names used across tasks: `Framing`/`decode_frames`/`encode_frames`/`Length::ToNextSync` (2 → 3, 4, 6), `offline::run_chain` (3 → 4), `bits::decision_nodes` (4 → 8), `SignatureStore`/`SignatureRecord`/`NewSignature`/`confidence`/`PROPOSE_CRC_FRAMES`/`band_holds` (5 → 6, 7, 8, 9), `framing_from_hypothesis` (6), `ClassifierSet::learned` (7), `ModesHandle`/`reload_modes`/`learned_row`/`supersede_by` (8), `job_context.proposal` (6 → 9, 10), `decoder`/`decoder_file`/`decoder_note` (5 → 8, 9).
- Where the spec no longer matches the code (reported to the caller too): the CRC library has six specs, not the spec's list (CRC-8 and CRC-16-Modbus missing; `crc16-ccitt` is CCITT-FALSE and `crc16-x25` is separate); the row's decoder kind is `decoder.kind`, not a `decoder_kind` field (`modes_list` reports `decoder_kind`); the registry is an immutable `Arc` in four places and zeta-ext's handle a `OnceLock`, so "reloads the rows" needs Task 8's handle; `Signature` has no optional fields yet; `identify`'s quick `Attempt` is a plain fn pointer that cannot carry a row; `BurstStore::pin` takes no signature id (the pin is a flag); the page already draws any event with a `summary` as a ribbon, so the `signature` ribbon needs only a style; "Working layer 4" item 11 still says "not yet an op (plan 19.3)".
