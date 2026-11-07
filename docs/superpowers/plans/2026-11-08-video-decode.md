# Plan 28.2: analog video, decoded and watched live — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `video_decode` mode row turns a wideband FM video carrier into black-and-white `gray8` fields on the stream bus, the websocket carries them to the page compressed and paced per subscriber, and the page shows each running feed as a live thumbnail on its band and, on a click or the Video tab, as the whole main area with the other feeds switchable.

**Architecture:** Two new `zeta-dsp` stages run their DSP on a thread of their own behind a drop-on-full queue: `video_fm_demod` (discriminator, integer decimation to a composite rate at or under 16 MHz, de-emphasis) publishes the composite as a one-channel `audio` stream marked `role: composite` so no mix or attach ever takes it; `composite_decode` (sync-tip clamp and slicer, line lock, vertical interval, field assembly, luma low-pass, line resampling) publishes one `gray8` field per 20 or 16.7 ms and a `video_lock` event. `zeta-vrt` gains a video body in the binary envelope and a gray8/rgb8 scaler plus PNG encoder (`png` crate); `zeta-ws` gains `video=smooth|sharp|thumb`, a latest-frame-only per-connection encoder and two stats; `zeta-client` and the CLI gain `stream save`. The page gains `video.js` (PNG bodies decoded with `createImageBitmap` onto canvases), a thumbnail canvas inside the job tag, a `full` pane option that gives a pane the whole of `#main`, the `video` view policy and a "Decode video" menu entry greyed with the lease's `insufficient bandwidth` text. A synthesised PAL/NTSC test card, FM-modulated, is the fixture for the unit tests, `zeta modes check` (at a test bandwidth of 8 MHz) and the e2e suite.

**Tech Stack:** Rust 1.97 workspace; one new crate, `png` 0.17 (MIT OR Apache-2.0, pure Rust, pulls `fdeflate`, `flate2`/`miniz_oxide`, `crc32fast`, all in `deny.toml`'s licence list) in `zeta-vrt` only; the page is plain ES modules under the existing CSP (`img-src 'self' data:`, no `blob:`, so pictures go through `createImageBitmap(Blob)` onto a `<canvas>`, never an `<img src=blob:>`).

**Spec:** `docs/superpowers/specs/2026-11-06-video-decode-design.md`, sections 1, 2, 4, 5.1–5.6, 6, 7 (section 5.7 was plan 28.1, merged 97d958a). The spec's open questions were answered 2026-11-06/07 (section 8); UAT is deferred to the operator's bench.

## Global Constraints

- `zeta-core` takes no new dependency (`crates/zeta-core/tests/no_privileged_deps.rs`); the only new crate is `png` in `crates/zeta-vrt/Cargo.toml`. `cargo deny check licenses bans sources` must stay green.
- The sample rate reaching the stages is whatever the capture gives: the row's default band is 24 000 000 Hz, the B210's lease rounds the capture to 30.72 Msps, and `$out_rate_hz` is then the capture rate (`ops::channel_rate_hz` returns the source rate for a band that wide). Every DSP rule is rate-agnostic: the demodulator picks the composite rate from the input rate, the decoder works in seconds, never in samples per line.
- No channel-grid assumptions: the job centres where it is started; no channel plan, no snap, no `plan_start` for video (`TODO.md` "Plans: VTX channel tables from the open VTX firmware").
- A video job must not degrade the node: the two stages' DSP runs on a dedicated `std::thread` each (Task 1's `run_map_threaded`), drops input frames when the thread is behind and counts them, and the ws encoder sends the newest frame only.
- No raw frames over the websocket: every video body on the ws is PNG; a video subscriber is one socket per shown picture.
- Params structs are `#[serde(deny_unknown_fields)] #[schemars(deny_unknown_fields)]`, every property required or carrying a schema default (`zeta-dsp` `stage_params_schemas_are_strict`). Wire structs are `deny_unknown_fields` except where this plan says `#[serde(default)]` for a new optional field.
- Repository rules: roles, never host names, in tracked files (`the field host`); `bash scripts/public-check.sh` and `bash scripts/check-doc-links.sh` clean before `main`; `AGENTS.md` and `skills/zetasdr/SKILL.md` regenerated, never edited — **every task that registers a stage, adds a row or touches `agents_md.rs` regenerates both** (`cargo run -q -p zeta -- agents-md > AGENTS.md && cargo run -q -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md`), since `## Stage kinds` and the `JobKind` list are generated.
- Gates: `cargo fmt --check` (per crate in a nested worktree), `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`, `cargo test -p zeta --test ui -- --nocapture` (Task 8; about 9 min, check for `ui e2e skipped`), `cargo run -q -p zeta -- agents-md --check`, `cargo run -q -p zeta -- agents-md --skill --check`. Use a private `CARGO_TARGET_DIR`.
- In the worktree the isolation guard refuses any command line containing the word `source` and compound git; put such commands in a script file under `.superpowers/sdd/<plan>/` and run `bash <file>`.
- Field-host work (Task 10) goes through `deploy/field-host/z.sh` and `deploy/field-host/deploy-sdr.sh <name>`; the radio is the operator's (never `jobs_stop` unasked); the bench transmitters are set up by the operator, so Task 10 ends at "deployed, UAT section written".

## Review Focus

1. **A capture whose rate changes under a running video job** (a retune to another rung, a source respawn at a `measured` cap): the demodulator must rebuild its decimator for the new rate and the decoder must relock rather than emit garbage fields. Pinned in Task 1 (`a_rate_change_rebuilds_the_decimator`) and Task 2 (`a_rate_change_relocks_within_three_fields`).
2. **Noise or a digital carrier with no line structure** (an OFDM HDMI sender, an empty channel): the decoder must publish no fields, say `locked: false` every 5 s, and spend bounded CPU per block. Pinned in Task 2 (`noise_never_locks_and_publishes_no_fields`).
3. **A page slower than the field rate on `smooth`** (a laptop on Wi-Fi): it must receive the newest field, never a backlog, with the skipped count in `lagged`. Pinned in Task 6 (`a_slow_video_subscriber_gets_the_newest_frame_not_a_backlog`).
4. **A frame the envelope cannot carry** (an rgb8 frame over `MAX_BODY`, a plugin's bad body): the ws must count it as missed and keep the connection, never panic or close. Pinned in Task 5 (`a_video_body_over_max_body_is_refused`) and Task 6 (`an_unencodable_frame_counts_as_missed`).
5. **Leaving the full view, or the shown feed's job stopping under it**: the sockets close, the waterfall returns intact, and the page logs no error. Pinned in Task 8 (`scenario_video_pane_returns_the_waterfall_when_left`, `scenario_video_feed_stopping_falls_back_to_the_next`).

---

### Task 0: The synthesised test card (PAL and NTSC, FM-modulated)

**Files:**
- Create: `crates/zeta-dsp/src/video_fm.rs` (only the `Shelf` network and `VideoStandard` table this task; the stage comes in Task 1)
- Modify: `crates/zeta-dsp/src/synth.rs` (`SynthSpec::Video`, `composite_test_card`, the `synthesise` arm, `rate_hz`, `validate`), `crates/zeta-dsp/src/lib.rs` (`pub mod video_fm;`)
- Test: `crates/zeta-dsp/src/synth.rs` (unit tests), `crates/zeta-dsp/tests/video_synth.rs`

**Interfaces:**
- Produces (in `video_fm.rs`):
  - `pub enum VideoStandard { Pal, Ntsc }` with `pub fn table(self) -> &'static StandardTable` and `serde(rename_all = "lowercase")`.
  - `pub struct StandardTable { pub line_s: f64, pub sync_s: f64, pub active_start_s: f64, pub active_s: f64, pub lines_per_field: [u32; 2], pub eq_lines: f64, pub broad_lines: f64, pub first_active_line: u32, pub active_lines: u32, pub luma_lowpass_hz: f64, pub preemphasis_crossover_hz: f64 }`. PAL: `64e-6, 4.7e-6, 10.5e-6, 52e-6, [312, 313], 2.5, 2.5, 23, 288, 3.0e6, 1.5e6`. NTSC: `63.556e-6, 4.7e-6, 9.4e-6, 52.6e-6, [262, 263], 3.0, 3.0, 22, 240, 2.8e6, 2.1e6`. Equalising pulses are `0.5 · sync_s` low twice a line; broad pulses are `0.5 · line_s − sync_s` low twice a line.
  - Composite levels (volts): `pub const SYNC_V: f32 = -0.3; pub const BLANK_V: f32 = 0.0; pub const WHITE_V: f32 = 0.7;`.
  - `pub struct Shelf { .. }` a first-order shelf, bilinear-transformed IIR: `pub fn ccir405(standard: VideoStandard, rate_hz: f64) -> Shelf` is the plan's `ccir405` network — 14 dB total (−7 dB low, +7 dB high), unity at `preemphasis_crossover_hz`, i.e. `τ1 = √5 / ω_c`, `τ2 = τ1 / 5`, pre-emphasis `H(s) = (1 + sτ1) / (√5 (1 + sτ2))`, de-emphasis its inverse; `pub fn one_pole(tau_us: f64, rate_hz: f64) -> Shelf` the plain de-emphasis of `wfm::deemphasis`; `pub fn pre(&mut self, x: &mut [f32])`, `pub fn de(&mut self, x: &mut [f32])`, `pub fn reset(&mut self)`.
- Produces (in `synth.rs`):
  - `SynthSpec::Video { standard: VideoStandard, deviation_hz: f64, subcarrier_hz: Option<f64>, rate_hz: u32, #[serde(default = "default_video_fields")] fields: u32, #[serde(default = "default_true")] preemphasis: bool }` (`family: "video"`; `fields` default 25). `deviation_hz` is hertz per volt of composite, so sync sits at `−0.3 · deviation_hz`.
  - `pub fn composite_test_card(standard: VideoStandard, rate_hz: u32, fields: u32) -> Vec<f32>`: `fields` consecutive fields of composite at `rate_hz`, volts, starting at field 0 line 1. The card: the top 3/4 of the active lines carry 8 vertical bars of luma `(7 − k) / 7 · WHITE_V` for bar `k = 0..8` (left to right, equal widths); the bottom quarter is black with one white block `1/8` of the active width wide and `1/8` of the active lines high whose left edge is at `((field_index · 16) mod active_width_samples)`; fields alternate `lines_per_field[0]`, `[1]`; the vertical interval is `eq_lines` of equalising pulses, `broad_lines` of broad pulses, `eq_lines` of equalising pulses, then blanked lines with normal sync up to `first_active_line − 1`.
  - `pub const TEST_CARD_BAR_LEVELS: [f32; 8]` (the eight luma fractions) for the decoder tests.
  - `SynthSpec::rate_hz()` returns `rate_hz` for `Video`; `validate()` requires `rate_hz ≥ 2_000_000`, `deviation_hz > 0`, `fields ≥ 1`, and a `subcarrier_hz` below `rate_hz / 2`.
  - The `synthesise` arm: the card (pre-emphasised with `Shelf::ccir405` when `preemphasis`), plus, when `subcarrier_hz` is set, a 0.1 V FM subcarrier (a 1 kHz tone at 50 kHz deviation) added to the composite, then `wfm_modulate_composite(&composite, rate_hz, deviation_hz)` (unit magnitude), then the existing offset/echo/ppm/awgn chain.

- [ ] **Step 1: Write the failing tests** in `crates/zeta-dsp/tests/video_synth.rs` (plain `cargo test`, no feature):
  - `pal_card_has_the_right_length_and_levels`: 2 fields at 8 MHz → `len == round((312 + 313) · 64e-6 · 8e6)`; `min()` within 0.02 of `SYNC_V`; `max()` within 0.02 of `WHITE_V`; the first `sync_s` of line 1 is below `−0.25` (an equalising pulse).
  - `ntsc_card_counts_525_lines_over_two_fields`: likewise `(262 + 263) · 63.556e-6`.
  - `bars_step_down_across_a_line`: on field 0, line `first_active_line + 50`, the mean of the middle 40% of each bar is within 0.03 V of `TEST_CARD_BAR_LEVELS[k] · WHITE_V`.
  - `the_block_moves_between_fields`: the block's left edge on fields 0 and 1 differs by 16 samples (mod the active width).
  - `fm_video_is_unit_magnitude_and_parses_from_a_test_block`: `parse_params` of `{"family":"video","standard":"pal","deviation_hz":1500000.0,"subcarrier_hz":null,"rate_hz":8000000,"fields":2,"snr_db":30}` then `synthesise` gives `len == composite len`, every `|z|` in `0.5..1.5` (AWGN at 30 dB), and `spec.rate_hz() == 8_000_000`.
  - `ccir405_pre_then_de_is_identity`: a 1000-sample ramp through `Shelf::ccir405(Pal, 8e6).pre` then a fresh `.de` is within 1e-3 of the input after the first 50 samples.
- [ ] **Step 2: Run them**: `cargo test -p zeta-dsp --test video_synth` — Expected: FAIL (no `Video` variant, no `video_fm` module).
- [ ] **Step 3: Implement** `video_fm.rs` (the table, the levels, `Shelf`) and the `synth.rs` additions. Add `pub mod video_fm;` to `lib.rs`'s module list.
- [ ] **Step 4: Run** `cargo test -p zeta-dsp --test video_synth` and `cargo test -p zeta-dsp synth` — Expected: PASS, and the existing synth tests still pass.
- [ ] **Step 5: Commit**: `git add crates/zeta-dsp && git commit -m "Plan 28.2: a synthesised PAL/NTSC test card, FM-modulated, as the video fixture; the ccir405 shelf"`.

---

### Task 1: `run_map_threaded` and the `video_fm_demod` stage

**Files:**
- Create: `crates/zeta-dsp/src/worker.rs`
- Modify: `crates/zeta-dsp/src/video_fm.rs` (the stage), `crates/zeta-dsp/src/lib.rs` (`pub mod worker;`, register `video_fm::VideoFmDemodStage::new()` in `register_all`), `AGENTS.md`, `skills/zetasdr/SKILL.md` (regenerated)
- Test: `crates/zeta-dsp/tests/video_fm.rs`, unit tests in `worker.rs`

**Interfaces:**
- Consumes: Task 0's `VideoStandard`, `Shelf`, `composite_test_card`, `SynthSpec::Video`; `nbfm::nbfm_demod` (the discriminator, `arg(x · conj(prev)) · rate / (2π · dev)`, clamped ±1 — **not** used as is: see Step 3), `util::lowpass_taps`, `util::fir_decimate_real`, `lib.rs`'s `Input`, `note_lag`.
- Produces:
  - `worker.rs`: `pub fn run_map_threaded<F>(kind: &'static str, inputs: Vec<broadcast::Receiver<Frame>>, bus: Arc<StreamBus>, cancel: CancellationToken, depth: usize, f: F) -> impl Future<Output = Result<(), StageError>> where F: FnMut(Input) -> Vec<Frame> + Send + 'static` — the async side receives frames, counts broadcast lag through `note_lag`, and `try_send`s `Input`s into a `std::sync::mpsc::sync_channel(depth)`; a full queue drops the input, increments `pub static DROPPED_INPUTS: AtomicU64` and passes the next input as `Input::Lagged { missed }`; a named `std::thread` runs `f` and `bus.publish`es every returned frame; cancel or a closed input drops the sender, joins the thread and returns as `run_map` does. `pub const VIDEO_QUEUE_DEPTH: usize = 8`.
  - `video_fm.rs`: `pub struct VideoFmParams { #[serde(default = "default_deviation")] pub deviation_hz: u64 /* 8_000_000 */, #[serde(default = "default_deemphasis")] pub deemphasis: String /* "ccir405" | "none" | "<tau_us>" */, #[serde(default = "default_composite_max")] pub composite_max_hz: u64 /* 16_000_000 */, #[serde(default)] pub standard: Option<VideoStandard> /* for the ccir405 crossover; None = PAL's */ }`.
  - `pub fn composite_rate_hz(in_rate_hz: u64, max_hz: u64) -> Option<u64>`: the largest exact divisor rate of `in_rate_hz` at or under `max_hz` (30 720 000 → 15 360 000; 20 480 000 → 10 240 000; 8 000 000 → 8 000 000; an input rate at or under `max_hz` is returned as is; `None` only for `in_rate_hz == 0`).
  - `pub struct VideoFmDemod { .. }` with `pub fn new(p: &VideoFmParams) -> Result<Self, String>` (refuses an unparsable `deemphasis`), `pub fn process(&mut self, iq: &[Complex<f32>], rate_hz: u64) -> Result<(Vec<f32>, u64), String>` returning composite volts (`Δf / deviation_hz`, **unclamped**, so a sync tip is −0.3 at the nominal deviation) and the composite rate; a rate change rebuilds the FIR (cutoff `0.45 · composite_rate`, taps `clamp(8 · decim + 1, 63, 1025)`) and the shelf and resets state; `pub fn reset(&mut self)`.
  - `pub struct VideoFmDemodStage` with `kind: "video_fm_demod"`, inputs `[Iq]`, outputs `[Audio]`, `Backend::Cpu`; the stage's `run` uses `run_map_threaded` with `VIDEO_QUEUE_DEPTH`, the output frame `Payload::Audio { sample_rate_hz: Hz(composite_rate), channels: 1, samples }` with the input block's span, and `ctx.note(json!({"video_fm": {"composite_rate_hz": r}}))` once per rate.
  - The stage implements Task 3's `output_roles` as `vec![Some("composite")]` (add it in Task 3; this task leaves the default).

- [ ] **Step 1: Write the failing tests** in `crates/zeta-dsp/tests/video_fm.rs`:
  - `composite_rate_is_the_largest_divisor_under_the_cap`: the three pairs above plus `(61_440_000, 16_000_000) == Some(15_360_000)` and `(0, 16_000_000) == None`.
  - `a_modulated_card_demodulates_to_the_card`: `composite_test_card(Pal, 8_000_000, 2)` → `wfm_modulate_composite(.., 8_000_000, 1_500_000.0)` → `VideoFmDemod::new(&{deviation 1_500_000, deemphasis "none", ..}).process(.., 8_000_000)` → rate `8_000_000`; after the first 100 samples, the output is within 0.02 V of the card (RMS over the field).
  - `a_rate_change_rebuilds_the_decimator`: process a block at 30_720_000 (rate 15_360_000 returned, `len == in / 2`) then one at 20_480_000 → rate `10_240_000`, `len == in / 2`, no panic, output finite.
  - `deemphasis_tau_is_accepted_and_garbage_is_not`: `deemphasis: "0.5"` → `Ok`; `"ccir405"` → `Ok`; `"loud"` → `Err` containing `deemphasis`.
  - `the_stage_spec_is_registered`: `zeta_dsp::register_all` into a `StageRegistry` then `registry.spec("video_fm_demod")` has inputs `[Iq]`, outputs `[Audio]`.
  - In `worker.rs` unit tests: `a_full_queue_drops_and_marks_the_next_input_lagged` (a closure that sleeps 50 ms per input, 40 inputs published, depth 2 → at least one `Input::Lagged` seen, `DROPPED_INPUTS > 0`, the task ends on cancel within 1 s) and `the_thread_publishes_what_the_closure_returns`.
- [ ] **Step 2: Run** `cargo test -p zeta-dsp --test video_fm` — Expected: FAIL to compile.
- [ ] **Step 3: Implement** `worker.rs` and the stage. The discriminator: write `fn discriminate(iq, prev, rate, deviation) -> Vec<f32>` in `video_fm.rs` returning `arg(x · conj(prev)) · rate / (2π · deviation)` **without** the ±1 clamp (`nbfm_demod` clamps, which would flatten the sync tip at a low deviation setting); a non-finite sample gives 0. Order: discriminate → FIR decimate (`fir_decimate_real`) → `Shelf::de` (or nothing for `none`, `one_pole(tau)` for a number).
- [ ] **Step 4: Regenerate** `AGENTS.md` and `skills/zetasdr/SKILL.md`; run `cargo test -p zeta-dsp --test video_fm`, `cargo test -p zeta-dsp --lib worker`, `cargo test -p zeta-dsp --lib stage_params_schemas_are_strict`, `cargo run -q -p zeta -- agents-md --check` — Expected: all PASS.
- [ ] **Step 5: Commit**: `git add crates/zeta-dsp AGENTS.md skills && git commit -m "Plan 28.2: video_fm_demod on its own thread behind a drop-on-full queue; the composite rate rule and de-emphasis"`.

---

### Task 2: The `composite_decode` stage

**Files:**
- Create: `crates/zeta-dsp/src/composite.rs`
- Modify: `crates/zeta-dsp/src/lib.rs` (`pub mod composite;`, register `composite::CompositeDecodeStage::new()`), `AGENTS.md`, `skills/zetasdr/SKILL.md` (regenerated)
- Test: `crates/zeta-dsp/tests/composite.rs`

**Interfaces:**
- Consumes: Task 0's `VideoStandard`, `StandardTable`, `SYNC_V/BLANK_V/WHITE_V`, `composite_test_card`, `TEST_CARD_BAR_LEVELS`; Task 1's `run_map_threaded`, `VIDEO_QUEUE_DEPTH`, `VideoFmDemod` (for the chained test); `util::event(ctx, start_ns, "video", fields)`; `Payload::Video { width, height, format: VideoFormat::Gray8, frame }`.
- Produces:
  - `pub struct CompositeParams { #[serde(default)] pub standard: StandardChoice /* auto | pal | ntsc, default auto */, #[serde(default)] pub scale: Scale /* full | half | quarter → widths 720 | 360 | 180 */, #[serde(default = "default_report_s")] pub report_s: u64 /* 5 */ }`.
  - `pub struct Field { pub width: u32, pub height: u32, pub luma: Vec<u8>, pub start_ns: u64, pub index: u64 }`.
  - `pub struct Lock { pub standard: Option<VideoStandard>, pub locked: bool, pub line_hz: f64, pub fields: u64, pub snr_db: i32 }`.
  - `pub struct CompositeDecoder { .. }`: `pub fn new(p: &CompositeParams) -> Self`, `pub fn push(&mut self, x: &[f32], rate_hz: u64, t0_ns: u64) -> Vec<Field>`, `pub fn lock(&self) -> Lock`, `pub fn reset(&mut self)`.
  - The algorithm (the plan's decisions; the implementer writes the body):
    - **Levels:** the sync tip is the running minimum over a line period with a 0.5 s decay; the blanking level is an IIR (τ = 2 ms) of the samples `sync_s + 0.5 µs .. active_start_s − 0.5 µs` after each accepted sync edge; the slice is their midpoint. Luma `= clamp(round((v − blank) / WHITE_V · 255), 0, 255)`.
    - **Horizontal:** a sync edge is a downward slice crossing; with a standard chosen the expected period is `line_s`; an edge within ±2 µs of the prediction is accepted and the period is corrected by 1/16 of the error (the PLL); a miss keeps the prediction. In `auto`, the standard is the one whose line count between vertical intervals matches (312/313 → PAL, 262/263 → NTSC); until then both are tried on the same edges and the one with more accepted edges over 50 lines is used.
    - **Vertical:** a broad pulse is the signal held below the slice for more than `0.4 · line_s`; the first line after the last broad pulse is line `broad_end`, and the field's line 1 is `broad_end − eq_lines − broad_lines`; the field index parity comes from whether the last broad pulse ended on a half line.
    - **Lock:** `locked` becomes true after two consecutive fields with ≥ 90 % of expected edges accepted and a vertical interval found, false after three consecutive fields under 50 %. `snr_db = round(20 · log10((blank − tip) / σ))` where σ is the standard deviation of the back-porch samples of the last field (clamped to 0..=60).
    - **Fields:** only while locked; each active line (`first_active_line .. first_active_line + active_lines`) is sampled at `edge + active_start_s + (k + 0.5) · active_s / width` with linear interpolation after a luma FIR low-pass at `luma_lowpass_hz` (63 taps; this plan's substitute for the spec's subcarrier notch: the low-pass at 3.0/2.8 MHz already removes 4.43/3.58 MHz; chroma is a later plan). `height = active_lines`, `start_ns` = the field's line 1 edge time.
  - `pub struct CompositeDecodeStage` with `kind: "composite_decode"`, inputs `[Audio]`, outputs `[Video, Event]`, `Backend::Cpu`; `run` via `run_map_threaded`; for each `Field` a `Payload::Video { width, height, Gray8, luma.into() }` with provenance span `start_ns .. start_ns + field_s`; the lock event `event(&ctx, t_ns, "video", json!({"system": "PAL 625/50" | "NTSC 525/60" (absent when no standard), "lock": "locked" | "no lock", "standard": "pal"|"ntsc"|null, "locked": bool, "line_hz": f64, "fields": u64, "snr_db": i32, "dropped": worker::DROPPED_INPUTS}))` on every change of `locked` or `standard` and every `report_s` seconds. The first event is sent when `locked` first becomes true or 2 s after the first block, whichever is first, so a row test comparing the first event sees the lock (`modes check` compares only the first event). A `Lagged` input calls `reset()`. A rate change calls `reset()`.

- [ ] **Step 1: Write the failing tests** in `crates/zeta-dsp/tests/composite.rs` (each feeds `composite_test_card(standard, 8_000_000, N)` to `CompositeDecoder` in 1 ms blocks):
  - `pal_card_locks_within_three_fields`: 6 fields, `standard: pal` → `lock().locked`, `standard == Some(Pal)`, `line_hz` within 1 % of 15 625, at least 3 fields returned with `width 720, height 288`, `luma.len() == 720 · 288`.
  - `ntsc_card_locks_and_has_240_lines`: likewise `Ntsc`, `height 240`, `line_hz` within 1 % of 15 734.
  - `auto_tells_pal_from_ntsc`: `standard: auto` on each card → the right `standard` within 8 fields.
  - `bars_are_recovered_within_tolerance`: on a locked PAL field, row 100: the mean of the middle 40 % of bar `k` is within ±24 of `round(TEST_CARD_BAR_LEVELS[k] · 255)`; the bottom-quarter block is ≥ 200 inside and ≤ 40 beside it.
  - `awgn_at_10_20_30_db_still_locks`: `fixtures::awgn_real` (feature `test-fixtures`; gate this test with `#![cfg(feature = "test-fixtures")]` in a sibling file `composite_noise.rs`) at each SNR → locked within 8 fields; `snr_db` at 30 dB input is ≥ 20.
  - `noise_never_locks_and_publishes_no_fields`: 1 s of Gaussian noise at 0.1 V → `locked == false`, zero fields, and the time spent is under 2 s (a `std::time::Instant` bound, generous for a 2-CPU runner).
  - `a_rate_change_relocks_within_three_fields`: PAL at 8 MHz locked, then the same card at 10 MHz pushed with `rate_hz: 10_000_000` → locked again within 3 fields.
  - `half_scale_halves_the_width`: `scale: half` → `width 360`.
  - `a_6_5_mhz_subcarrier_does_not_disturb_the_picture`: Task 0's synth at 16 MHz with `subcarrier_hz: Some(6.5e6)`, `deviation_hz 2.0e6`, `snr_db 30` → `VideoFmDemod` (composite rate 16 MHz) → decoder locked, bars within ±32.
  - `the_fm_chain_decodes_end_to_end`: Task 0's synth at 8 MHz, `deviation_hz 1.5e6`, `preemphasis true`, `snr_db 25` → `VideoFmDemod` (`deemphasis "ccir405"`) → `CompositeDecoder` → locked, bars within ±32.
  - Stage test (the `bits_chain.rs` harness pattern: a bus of capacity 16, `StageContext::new(source)` with `.with_mode(Some(row))` **not** needed, since without a row `event()` adds only `protocol`): `the_stage_emits_a_lock_event_and_fields`: feed 6 PAL fields as `Payload::Audio` frames → at least one `Event` with `protocol == "video"` and `locked == true`, at least 3 `Video` frames with `format gray8`.
- [ ] **Step 2: Run** `cargo test -p zeta-dsp --test composite` — Expected: FAIL to compile.
- [ ] **Step 3: Implement** `composite.rs` per the interfaces, register the stage.
- [ ] **Step 4: Regenerate** AGENTS.md/SKILL.md; run `cargo test -p zeta-dsp --test composite`, `cargo test -p zeta-dsp --features test-fixtures --test composite_noise`, `cargo run -q -p zeta -- agents-md --check` — Expected: PASS.
- [ ] **Step 5: Commit**: `git add crates/zeta-dsp AGENTS.md skills && git commit -m "Plan 28.2: composite_decode: sync, line lock, vertical interval, gray8 fields and the video lock event"`.

---

### Task 3: Output roles, so the composite is never "the job's audio"

**Files:**
- Modify: `crates/zeta-core/src/stage.rs` (`Stage::output_roles`), `crates/zeta-core/src/stream.rs` (`StreamInfo.role`, `register_with_role`), `crates/zeta-core/src/node/mod.rs` (`JobEntry.outputs`, `last_audio`, `job_outputs`), `crates/zeta-core/src/node/launch.rs` (record roles at launch, register with role), `crates/zeta-core/src/node/identify.rs:133` (the tuple → `JobOutput`), `crates/zeta-dsp/src/video_fm.rs` (`output_roles`), `crates/zeta-ui/web/audio.js` (skip `role === "composite"`), `crates/zeta/src/agents_md.rs` (one sentence in the `job_attach` note)
- Test: `crates/zeta-core/tests/ops.rs` (or the suite that holds `job_attach`/mix tests), `crates/zeta-core/src/stream.rs` unit test

**Interfaces:**
- Produces:
  - `Stage::output_roles(&self) -> Vec<Option<&'static str>>` default `vec![None; self.spec().outputs.len()]`; `VideoFmDemodStage` returns `vec![Some("composite")]`.
  - `StreamInfo { id, kind, provenance, #[serde(default, skip_serializing_if = "Option::is_none")] role: Option<String> }` (keeps `deny_unknown_fields`); `StreamBus::register_with_role(&self, kind, provenance, role: Option<&str>) -> StreamId`; `register` calls it with `None`.
  - `pub(crate) struct JobOutput { pub kind: StreamKind, pub id: StreamId, pub role: Option<&'static str> }`; `JobEntry.outputs: Vec<JobOutput>`; `fn last_audio(outputs: &[JobOutput]) -> Option<StreamId>` takes the last `Audio` **with `role.is_none()`**; `Node::job_outputs` keeps its public return type (`Vec<(StreamKind, StreamId)>`).
  - `GET /streams` therefore shows `"role":"composite"` on the composite stream; `audio.js`'s stream pick (`audio.js:80-83`) skips entries with a `role`.
- The spec placed the mark on `StageSpec`'s output declaration; this plan puts it on the trait (no change to the 30 `StageSpec` literals) and also on the stream's registration, so `/streams`, `stream list` and the page can see it. Record this in the ledger as a ruling.

- [ ] **Step 1: Write the failing tests**:
  - `crates/zeta-core/src/stream.rs`: `a_role_is_listed_and_defaults_to_none`.
  - `crates/zeta-core/tests/ops.rs` (next to the attach tests): `a_composite_output_is_never_the_jobs_audio`: a test stage registered in the test's registry with outputs `[Audio, Audio]` and `output_roles() == [Some("composite"), None]`, started as a graph job; `job_audio_stream(job)` is the second stream; `job_attach --kind transcribe` reaches it; with a stage whose only output is a `composite` audio, `job_attach` fails with the existing "no audio output" error and the mix lists no input from it.
- [ ] **Step 2: Run** them — Expected: FAIL (`output_roles` undefined).
- [ ] **Step 3: Implement** across the files above.
- [ ] **Step 4: Run** `cargo test -p zeta-core` and `cargo test -p zeta-dsp`, regenerate AGENTS.md/SKILL.md, `cargo run -q -p zeta -- agents-md --check` — Expected: PASS.
- [ ] **Step 5: Commit**: `git add crates AGENTS.md skills && git commit -m "Plan 28.2: a stage output can carry a role; a composite is never a job's audio for attach, the mix or the page"`.

---

### Task 4: The `video_decode` row, the test block's bandwidth, `modes check`

**Files:**
- Create: `modes/video_decode.json`
- Modify: `crates/zeta-core/src/modes/mod.rs:89` (`EMBEDDED` → 9 entries), `crates/zeta-core/src/modes/row.rs` (`RowTest.bandwidth_hz: Option<u64>`), `crates/zeta/src/modes_check.rs` (use it), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-core/tests/modes.rs` (or the registry suite), `crates/zeta/tests/modes_check.rs`, `crates/zeta-core/tests/identify.rs` (feature `test-fixtures`)

**Interfaces:**
- Produces: the row —
  ```json
  { "id": "video_decode", "name": "Analog FM video", "family": "tv",
    "band": { "bandwidth_hz": 24000000, "centers_hz": [], "min_hz": 6000000 },
    "modulation": ["fm"], "label": "Video", "protocol": "video",
    "decoder": { "kind": "graph",
      "nodes": [
        { "id": "channel", "kind": "channel", "params": { "center_hz": "$center_hz", "bandwidth_hz": "$bandwidth_hz", "out_rate_hz": "$out_rate_hz" } },
        { "id": "demod", "kind": "video_fm_demod", "params": {} },
        { "id": "decode", "kind": "composite_decode", "params": {} } ],
      "edges": [ { "from": "channel", "to": "demod", "kind": "iq" }, { "from": "demod", "to": "decode", "kind": "audio" } ] },
    "event": { "schema": { "type": "object", "properties": {
        "protocol": {"type":"string"}, "system": {"type":"string"}, "lock": {"type":"string"},
        "standard": {"type":["string","null"]}, "locked": {"type":"boolean"}, "line_hz": {"type":"number"},
        "fields": {"type":"integer"}, "snr_db": {"type":"integer"}, "dropped": {"type":"integer"} } },
      "summary": "{system?} {lock} · {snr_db?} dB" },
    "identify": { "proof": "field", "field": "locked", "min_events": 1, "dwell_ms": 4000, "min_span_ms": 0, "quick": null },
    "catalogue": [],
    "notes": "Analog FM video (FPV, reverse cameras, analog HDMI senders): luma only; sync negative; a digital link (OFDM, no line structure) never locks and is reported as not yet decodable.",
    "test": { "kind": "synth", "family": "video", "standard": "pal", "deviation_hz": 1500000.0, "subcarrier_hz": null, "rate_hz": 8000000, "fields": 25, "snr_db": 25,
              "bandwidth_hz": 8000000, "expect": { "locked": true, "standard": "pal" }, "within_ms": 6000 } }
  ```
  `min_hz: 6000000` so `Band::fits` lets an 8 MHz check run and `identify` on a narrow box skips the row with `skipped_band`. If the `summary` template engine rejects `{system?}` next to a literal, use `"{lock} · {snr_db?} dB"` and record it.
- `RowTest { #[serde(flatten)] kind, expect, within_ms, #[serde(default)] bandwidth_hz: Option<u64> }`; `modes_check.rs:184` uses `m.row.test.bandwidth_hz.unwrap_or(m.row.band.bandwidth_hz)`. `modes_check` must also pull `bandwidth_hz` out of the synth `Map` before `parse_params` (the `SynthSpec` is `deny_unknown_fields`): take it out of the map the way `snr_db` is.
- Validation: `modes check` compares the first event as a subset; `expect` names `locked` and `standard` only, which Task 2's first-event rule (the lock, or 2 s) makes the locked event.

- [ ] **Step 1: Write the failing tests**:
  - `crates/zeta-core/tests/modes.rs`: `video_decode_is_embedded_and_validates` (`ModeRegistry` has `video_decode`, family `Tv`, `test.bandwidth_hz == Some(8_000_000)`); `a_row_test_without_bandwidth_hz_still_parses` (the POCSAG row's `test.bandwidth_hz == None`).
  - `crates/zeta/tests/modes_check.rs`: `video_decode_check_passes_on_the_synth` — runs `zeta modes check video_decode` against the embedded registry (the suite's existing runner) and expects a line starting `video_decode: ok`. Mark it `#[ignore]` only if the suite's wall-clock budget forbids a 6 s job; otherwise run it.
  - `crates/zeta-core/tests/identify.rs`: `identify_skips_video_decode_on_a_narrow_band`: an identify over a 200 kHz box lists a `candidate` event with `mode == "video_decode"`, `outcome == "skipped_band"`.
- [ ] **Step 2: Run** them — Expected: FAIL (unknown row).
- [ ] **Step 3: Implement**: the row, `EMBEDDED`, `RowTest.bandwidth_hz`, `modes_check`; adjust Task 2's first-event timing if needed.
- [ ] **Step 4: Run** `cargo test -p zeta-core --features test-fixtures`, `cargo test -p zeta --test modes_check`, `cargo run -q -p zeta -- modes check video_decode` (Expected: `video_decode: ok`), regenerate AGENTS.md/SKILL.md, `agents-md --check` — Expected: PASS.
- [ ] **Step 5: Commit**: `git add modes crates AGENTS.md skills && git commit -m "Plan 28.2: the video_decode row; a test block may name its own bandwidth so modes check runs an 8 MHz synthesised carrier"`.

---

### Task 5: The envelope's video body, the scaler and PNG encoder, `zeta stream save`

**Files:**
- Create: `crates/zeta-vrt/src/video.rs`
- Modify: `crates/zeta-vrt/Cargo.toml` (`png = "0.17"`), `crates/zeta-vrt/src/envelope.rs:20` (video body), `crates/zeta-vrt/src/lib.rs` (`pub mod video;`), `crates/zeta-client/src/ws.rs` (`subscribe_video`), `crates/zeta/Cargo.toml` (`zeta-vrt` dependency if not already direct), `crates/zeta/src/cli.rs` (`StreamCmd::Save`), `crates/zeta/src/transport.rs` (the remote handler), `crates/zeta/src/main.rs` (the local refusal)
- Test: `crates/zeta-vrt/tests/envelope.rs`, `crates/zeta-vrt/tests/video.rs`, `crates/zeta-client/tests/ws.rs`, `crates/zeta/tests/transports.rs`

**Interfaces:**
- Produces:
  - `envelope()` writes `frame.to_vec()` as the body for `Payload::Video` (after `check_body`; a body over `MAX_BODY` is `Err`), header payload unchanged (`{width,height,format,bytes}`); `parse_envelope` unchanged.
  - `zeta-vrt/src/video.rs`: `pub fn scale_to_width(src: &[u8], width: u32, height: u32, channels: u8 /* 1 or 3 */, out_width: u32) -> (Vec<u8>, u32, u32)` — box-filter downscale keeping the aspect (`out_height = max(1, height · out_width / width)`), identity when `out_width ≥ width`; `pub fn encode_png(bytes: &[u8], width: u32, height: u32, channels: u8) -> Result<Vec<u8>, String>` (`png::Compression::Fast`, `ColorType::Grayscale | Rgb`, `BitDepth::Eight`); `pub fn gray8_of_rgb8(rgb: &[u8]) -> Vec<u8>` (Rec. 601 weights); `pub struct VideoProfile { pub name: &'static str, pub width: Option<u32>, pub min_interval_ms: u64 }` with `pub const SMOOTH: VideoProfile = {"smooth", Some(360), 0}`, `SHARP = {"sharp", None, 200}`, `THUMB = {"thumb", Some(120), 500}`, `pub fn profile(name: &str) -> Option<&'static VideoProfile>`; `pub fn encode_frame(width, height, format: VideoFormat, body: &[u8], profile: &VideoProfile) -> Result<(Vec<u8>, u32, u32), String>` — png in → passthrough unscaled (`Ok((body.to_vec(), w, h))`); gray8/rgb8 → scale then PNG.
  - `zeta-client`: `pub async fn subscribe_video(&self, stream: &StreamId, profile: &str) -> Result<Subscription, ClientError>` (URL `?format=binary&video=<profile>`; refuses a non-video stream with `ClientError::Stream { detail: "stream <id> is <kind>, not video" }`); `ws_url` grows a `query: &str` parameter.
  - CLI: `StreamCmd::Save { #[arg(long, value_name = "ID")] stream: String, #[arg(long, value_name = "FILE")] path: PathBuf, #[arg(long, value_name = "MS", default_value_t = 10_000)] timeout_ms: u64 }` (the house convention is `--long` flags; the spec's `<stream> <path>` positionals are not used). `--node` only: `transport::remote` handles it before `into_op` — `subscribe_video(id, "sharp")`, the first `Received::Frame` whose header kind is `video` is written to `path` (the PNG body as it arrives), prints `{"saved": path, "width": w, "height": h, "bytes": n}` (`--json`) or `saved <path> (<w>x<h>, <n> bytes)`; a non-video stream prints the client's error plus `; IQ is saved with `zeta iq read`, audio has no still` as a usage error; the timeout prints `no video frame in <ms> ms` (exit 1). Locally (no `--node`): `usage("stream save reads the node's websocket: use --node")` in `main.rs` before `into_op`. `into_op` must still cover `Save`: return the same usage text as an `OpError` in the variant `into_op` already uses for a non-op command (never `unreachable!`).
- [ ] **Step 1: Write the failing tests**:
  - `crates/zeta-vrt/tests/envelope.rs`: `a_video_frame_roundtrips_with_its_body` (a 4×2 gray8 frame → `envelope` → `parse_envelope` → header payload `{"width":4,"height":2,"format":"gray8","bytes":8}` and body equal); `a_video_body_over_max_body_is_refused` (`check_body`-valid rgb8 of 4096×1366 = 16.8 MB → `Err`, built with `Arc<[u8]>` of zeros).
  - `crates/zeta-vrt/tests/video.rs`: `scaling_720_to_360_gives_360_by_144_and_keeps_the_mean` (a gradient; the mean within 1); `png_of_a_gray8_field_decodes_back` (encode then `png::Decoder` → same bytes); `a_png_body_passes_through_unscaled`; `rgb8_scales_per_channel`; `smooth_encode_of_a_360_by_144_field_is_fast_and_small` (100 encodes of a test-card-like gradient: mean under 10 ms, size under 60 000 bytes); `profiles_by_name` (`profile("thumb").width == Some(120)`, `profile("x") == None`).
  - `crates/zeta-client/tests/ws.rs`: `subscribe_video_refuses_an_audio_stream`.
  - `crates/zeta/tests/transports.rs`: `stream_save_without_node_is_a_usage_error` (exit 1, stderr contains `use --node`); `stream_save_writes_the_first_video_frame` — against the suite's served node with a stage that publishes one gray8 frame (the ws server test helper, or `--source-file` plus a `graph_put` of `passthrough`... if no cheap producer exists, publish through the test's own `StreamBus` as the `zeta-ws` tests do and mark the CLI test to use that server). Expected file is a valid PNG (`\x89PNG` magic).
- [ ] **Step 2: Run** them — Expected: FAIL.
- [ ] **Step 3: Implement** the four crates' pieces. Add `png` to `zeta-vrt`; run `cargo deny check licenses bans sources` (Expected: ok; if `cargo deny` is not installed, say so in the report and leave it to CI).
- [ ] **Step 4: Run** `cargo test -p zeta-vrt -p zeta-client -p zeta --test transports`, regenerate AGENTS.md/SKILL.md if `RUNNING` changed (the prose lands in Task 9; nothing here) — Expected: PASS.
- [ ] **Step 5: Commit**: `git add Cargo.lock crates && git commit -m "Plan 28.2: the envelope carries a video body; a gray8/rgb8 scaler and PNG encoder in zeta-vrt; zeta stream save over the websocket"`.

---

### Task 6: The websocket's `video=` profiles

**Files:**
- Modify: `crates/zeta-ws/src/stream.rs` (`FormatQuery.video`, `connection_video`), `crates/zeta-ws/src/http.rs` (`ServerStats.video_frames_sent`, `video_frames_skipped`, the counters), `crates/zeta-ws/src/wire.rs` (no change expected)
- Test: `crates/zeta-ws/tests/binary.rs` (or a new `video.rs` beside it using its helpers)

**Interfaces:**
- Consumes: Task 5's `VideoProfile`, `profile()`, `encode_frame`; the existing `connection_binary` (credits, `lagged`, `Live`), `Limits.ws_send_timeout`.
- Produces:
  - `FormatQuery { format: Option<String>, video: Option<String> }`. `video=` on a non-video stream → 400 `"video= applies to a video stream"`; an unknown profile → 400 `"video must be smooth, sharp or thumb"`; a video stream in binary mode without `video=` → `smooth`; text mode ignores `video=`.
  - `async fn connection_video(socket, rx, st, profile: &'static VideoProfile, permit, tracked)`: the credit handling of `connection_binary`; a `latest: Option<Frame>` slot replaced on every `rx.recv()` (a replaced, unsent frame adds 1 to `missed` and `video_frames_skipped`); a send is due when `latest.is_some() && credit > 0 && now − last_sent ≥ min_interval_ms`; the loop `select!`s over shutdown, inbound, `rx.recv()` and `sleep_until(last_sent + min_interval)` when a frame waits; a due frame is `encode_frame`d (an `Err` adds 1 to `missed` and drops it), sent as one binary message in the envelope layout (the header's `format` is `"png"`, `width`/`height` the scaled size, `bytes` the PNG length) after the pending `lagged` text, spending one credit, `video_frames_sent += 1`. `Lagged(n)` from the bus adds to `missed`. `Closed` → close `"stream ended"`.
  - `ServerStats { ws_connections, ws_dropped_slow, ws_credit_skipped, video_frames_sent: u64, video_frames_skipped: u64 }`, `Health` unchanged.
- [ ] **Step 1: Write the failing tests** (helpers from `binary.rs`: `connect`, `credit`, `next`, `lagged`; a video stream registered on the test bus and fed from the test):
  - `a_video_frame_arrives_as_a_png_envelope_under_smooth`: publish one 720×288 gray8 → the message parses with `parse_envelope`, header `format == "png"`, `width == 360`, `height == 144`, body starts `\x89PNG`.
  - `a_slow_video_subscriber_gets_the_newest_frame_not_a_backlog`: grant 1 credit, publish 20 numbered frames (the first byte is the index) with no further credit, then grant 1 → the next frame decoded (`png::Decoder`) has first byte 19, and a `lagged` text with `missed ≥ 18` precedes it; `video_frames_skipped ≥ 18`.
  - `sharp_sends_full_width_at_most_five_a_second`: 50 frames over 1 s at 20 ms spacing → between 4 and 7 messages, all `width == 720`.
  - `thumb_is_120_wide`.
  - `video_on_an_audio_stream_is_400`, `an_unknown_profile_is_400`, `text_mode_ignores_video`.
  - `an_unencodable_frame_counts_as_missed`: a `png`-format frame whose body is one byte passes `check_body` and passes through (`encode_frame` passthrough) — so the unencodable case is an rgb8 body over `MAX_BODY`: 4096×1366 rgb8 → no message, `missed` 1 in the next `lagged`, connection still open (a following small frame arrives).
- [ ] **Step 2: Run** `cargo test -p zeta-ws` — Expected: FAIL.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run** `cargo test -p zeta-ws` — Expected: PASS, the existing `binary.rs` tests included.
- [ ] **Step 5: Commit**: `git add crates/zeta-ws && git commit -m "Plan 28.2: the websocket sends video as paced PNG, newest frame only, under smooth, sharp and thumb profiles"`.

---

### Task 7: The page: thumbnail, full view, setting, menu

**Files:**
- Create: `crates/zeta-ui/web/video.js`
- Modify: `crates/zeta-ui/web/index.html` (`<div id="video" hidden>` inside `#pane`), `crates/zeta-ui/web/api.js` (`streamBinary` takes `query` extra: `{video}` → `?format=binary&video=<p>`), `crates/zeta-ui/web/audio.js` (`parseEnvelope` returns the raw `body` too, or a sibling `parseEnvelopeRaw`), `crates/zeta-ui/web/panes.js` (`full` option), `crates/zeta-ui/web/overlays.js` (video tag media hook, `video_lock` → snippet), `crates/zeta-ui/web/view.js` (`video` policy, `AGENT_VIEW_KEYS`), `crates/zeta-ui/web/menu.js` ("Decode video", greying), `crates/zeta-ui/web/app.js` (mount, wiring), `crates/zeta-ui/web/style.css`, `crates/zeta-agent/src/page.rs` (`VIEW_KEYS` + `video`), `crates/zeta-ui/tests/e2e/ui.py:1661` (13 → 14 policies)
- Test: Task 8 (e2e); `crates/zeta-agent/tests/agent.rs` (`VIEW_KEYS` equality already exists)

**Interfaces:**
- Consumes: `panes.register(id, label, {eligible, el, onShow, onHide})`, `view.get/set/onChange`, `api.streamBinary(id, onFrame, opts)`, `api.streams()` (`[{id, kind, provenance:{source, job, channel, span}, role?}]`), `overlays.js` `tag(o, key)` / `refreshTag(o)` / `render(job, p)`, `menu.js` `ENTRIES`/`JOBS`/`startJob`, `wf.sourceId`, `capHz(cap)` (waterfall.js:338), `state.jobs` and the jobs poll.
- Produces:
  - `panes.js`: `register(id, label, {…, full = false})`; `render()` sets `root.dataset.full = sel` when the selected pane is `full` (both footprints) and deletes it otherwise.
  - `style.css`: `html[data-full] #pane { top: 0; height: auto; border-top: 0; z-index: 5 }`, `html[data-full] #waterfall, html[data-full] #map { visibility: hidden }` (hidden, not removed: the sockets and the ring stay). `#video { height: 100%; display: grid; grid-template-rows: auto 1fr auto; background: var(--bg) }`, `.video-status` (mono font, `--dim`, one line, a `button.video-profile`), `canvas.video-main { width: 100%; height: 100%; object-fit: contain; image-rendering: auto }`, `.video-strip` (a row of `canvas.video-thumb` 120×90 with `.selected` in `--accent`), `.job-tag canvas.video-thumb { width: 64px; height: 48px; display: block }`, `.video-thumb.nolock { filter: grayscale(1) brightness(0.6) }` with a `::after` "no lock" label on the strip thumbs only, `.ctx-item[aria-disabled="true"] { opacity: .45; pointer-events: none }`.
  - `video.js`: `export function mountVideo(api, {panes, view, el, jobs, onSelectPane})` returns `{ eligible(), openThumb(jobId, canvas), closeThumb(jobId), onLock(jobId, event), show(), hide(), select(jobId), destroy() }`. A `Feed` is `{jobId, streamId, socket, canvas, frames, lastSecond, lock}`; `streamId` comes from `api.streams()` filtered `kind === "video" && provenance.job === jobId`. A frame handler: `parseEnvelopeRaw(buf)` → `createImageBitmap(new Blob([body], {type: "image/png"}))` → `canvas.getContext("2d").drawImage(bmp, 0, 0, canvas.width, canvas.height)` (canvas 4:3 of the display size; one in-flight decode at a time, a newer frame replaces a waiting one). `show()` opens the selected feed's socket at `view.get("video")` and a `thumb` socket per other running video job (every source); `hide()` closes all; `view.onChange("video")` reopens the main socket. The status line: `<source> · <centre MHz> · <system> <lock> · <frames/s> f/s · <snr> dB · <profile>` from the last `video_lock` event and the received-frame counter. The strip: one thumb per running video job (the jobs poll), click → `select(jobId)`; the selected feed's job stopping selects the next running one or, when none, `panes.select(null)`. `eligible()` is "any running `video_decode` job on any source".
  - `overlays.js`: `tag(o, key)` appends `canvas.video-thumb` (64×48, `data-job`) when `job.kind === "video_decode"` and calls `this.onTagMedia?.(job.id, canvas, true)`; outline removal calls `onTagMedia(job.id, null, false)`; a click on the thumb `stopPropagation`s and calls `this.onThumbClick?.(job.id)` (app.js wires it to `panes.click("video")` then `video.select(id)`). In `render()`, an event with `protocol === "video"` and a boolean `locked` sets `this.snippets.set(job.id, p.summary)`, `refreshTag`, calls `this.onVideoLock?.(job.id, p)` and does **not** become a ribbon.
  - `view.js`: the policy `{key: "video", values: ["smooth", "sharp"], default_: "smooth", describe: "video picture: smooth (every field, 360 wide) or sharp (full width, 5 a second)"}` appended last; `AGENT_VIEW_KEYS` gains `video`; `crates/zeta-agent/src/page.rs` `VIEW_KEYS` gains `"video"`.
  - `menu.js`: `ENTRIES` gains `["video", "Decode video"]`, `JOBS.video = {kind: "video_decode"}`; `open()` sets `aria-disabled="true"` and `title = "insufficient bandwidth: <source> delivers at most <cap> Hz"` on it when `capHz(source.capabilities.max_bandwidth_hz) < sel.bandwidthHz` (the source from the app's source list), else clears both; `startJob` for `video` uses the box's centre and width, `elastic: false`.
  - `app.js`: `const video = mountVideo(api, {...})`, `panes.register("video", "video", {eligible: video.eligible, el: $("#video"), onShow: video.show, onHide: video.hide, full: true})`, `ov.onTagMedia = (id, canvas, on) => on ? video.openThumb(id, canvas) : video.closeThumb(id)`, `ov.onVideoLock = video.onLock`, `ov.onThumbClick = …`; `applyPanes()` after each jobs poll already re-renders eligibility.
- [ ] **Step 1: Make the changes** (no unit harness for the page; Task 8 holds the tests). Keep each file's change small and in its idiom; `video.js` under 400 lines.
- [ ] **Step 2: Smoke in a browser**: add the example `crates/zeta-dsp/examples/gen_video.rs` (writes `scratch/video-pal-8m.sigmf-*` with Task 0's synth: PAL, 8 MHz, deviation 1.5e6, 25 fields, snr 25); run `cargo run -q -p zeta -- serve --source-file v=scratch/video-pal-8m.sigmf-meta --listen 127.0.0.1:7981`, start `video_decode` over the file (`job start --kind video_decode --source v --center-hz 100000000 --bandwidth-hz 8000000`), open the page: a thumbnail appears on the tag within 2 s, the Video tab appears, clicking either fills `#main` with the picture and the status line, `view set video sharp` switches, leaving restores the waterfall, the console shows no error. Record what you saw in the report.
- [ ] **Step 3: Run** `cargo test -p zeta-agent` (the `VIEW_KEYS` equality) and `python3 -c "import ast,sys; ast.parse(open('crates/zeta-ui/tests/e2e/ui.py').read())"` — Expected: PASS.
- [ ] **Step 4: Commit**: `git add crates/zeta-ui crates/zeta-agent crates/zeta-dsp/examples && git commit -m "Plan 28.2: the page shows a video job as a live thumbnail on its tag and as the whole main area under the Video tab, smooth or sharp; Decode video on the menu"`.

---

### Task 8: The e2e scenarios

**Files:**
- Modify: `crates/zeta/tests/ui.rs` (`video_fixture`, a `video` file source), `crates/zeta-ui/tests/e2e/ui.py` (scenarios appended)
- Test: `cargo test -p zeta --test ui -- --nocapture`

**Interfaces:**
- Consumes: `zeta_dsp::synth::{SynthSpec, Impairments, synthesise}`, `zeta_dsp::fixtures::write_fixture(dir, name, &iq, rate, center_hz)`; `ServeProc::start_full` with `--source-file video=<meta>`; ui.py helpers `select_source`, `start_job`, `stop_job`, `wait_until`, `view_value`, `run_cmd`, `pane_tab`, `page_errors`.
- Produces:
  - `ui.rs`: `fn video_fixture(dir) -> PathBuf`: `SynthSpec::Video { Pal, 1.5e6, None, 8_000_000, fields: 25, preemphasis: true }`, `snr_db 25`, written as `video` at centre 100 000 000 (about 32 MB in the temp dir).
  - Scenarios (appended, in this order):
    - `scenario_video_thumbnail_rides_the_tag`: select source `video`, `start_job(video_decode, "video", 100_000_000, 8_000_000)`, poll; `#waterfall .job-tag[data-job=ID] canvas.video-thumb` visible within 10 s and loses `.nolock` within 10 s; the tag's `.snippet` contains `locked`; a `wait_for_function("() => …")` reads a non-zero pixel from the thumb canvas (`getImageData` of the centre: a bar, > 20).
    - `scenario_video_pane_fills_main_and_switches_profile`: click the thumb → `html[data-strip="video"]` and `html[data-full="video"]`; `#pane`'s bounding box equals `#main`'s within 1 px; `#waterfall` has `visibility: hidden`; `.video-status` contains `locked` and `smooth` within 10 s; `run_cmd("view set video sharp")` → status contains `sharp`, `view_value("video") == "video sharp"`.
    - `scenario_video_pane_returns_the_waterfall_when_left`: with the pane open, click the `video` tab → no `data-full`, `#waterfall` visible, no `page_errors`.
    - `scenario_video_feed_stopping_falls_back_to_the_next`: the node gets a second file source `video2=<the same meta>` (a second `--source-file` arg in `ui.rs`), so two sources carry the fixture; a `video_decode` on each; open the pane on the first: `.video-strip canvas` count 2; `stop_job` the selected → the status line names the other source within 5 s; stop that too → `data-strip` absent and the Video tab hidden.
    - `scenario_decode_video_greys_when_the_box_is_too_wide`: on the `video` source (an 8 MHz file, cap `asserted 8000000`), box 10 MHz wide → the menu's `[data-action=video]` has `aria-disabled="true"` and a `title` starting `insufficient bandwidth`; a 6 MHz box → enabled.
    - `ui.py:1661`: `shown.count("\n") == 14`.
- [ ] **Step 1: Write the scenarios and the fixture.**
- [ ] **Step 2: Run** `ZETA_UI_ONLY=video cargo test -p zeta --test ui -- --nocapture` if the harness supports a filter (`ZETA_UI_ONLY` is passed through to ui.py; check its semantics in `ui.rs`), else the whole suite — Expected: PASS, no `ui e2e skipped` line.
- [ ] **Step 3: Run the whole suite once** (`cargo test -p zeta --test ui -- --nocapture`) — Expected: PASS within the 690 s deadline; report the wall time.
- [ ] **Step 4: Commit**: `git add crates/zeta/tests crates/zeta-ui/tests && git commit -m "Plan 28.2: e2e: the thumbnail, the full view, the profile switch, feed fallback and the greyed menu on a synthesised video source"`.

---

### Task 9: Docs, prose, gists and the UAT section

**Files:**
- Modify: `crates/zeta/src/agents_md.rs` (`note("job_start")`: the row; `RUNNING`: `stream save` and the `video=` profiles; `note("job_attach")` if Task 3 left it), `docs/agents/field-craft.md` (a `### Watching analog video` section and a "not yet decodable" bullet under `### Where to look`), `docs/guide.md` and `README.md` (one line each on video), `TODO.md` (gists), `UAT.md` (`## Plan 28.2` deferred checks), `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (the 28.2 row: plan path, status), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `cargo run -q -p zeta -- agents-md --check`, `--skill --check`, `bash scripts/check-doc-links.sh`, `bash scripts/public-check.sh`

**Interfaces:**
- Produces, in `field-craft.md` `### Watching analog video`: centre on the measured carrier (`classify` or the waterfall's box), the default band 24 MHz and the B210's 30.72 rung (a 20 MHz band keeps the 20.48 rung), what "no lock" looks like (a noise thumbnail, `lock: no lock`, `snr_db` under 10), the `video` view setting, `zeta stream save --stream <id> --path still.png --node URL`, and the verdict rule — one bullet under `### Where to look`: "Digital video links (WHDI, WiFi HDMI kits, DJI): a 20–40 MHz OFDM carrier at 5 GHz with no line structure; `video_decode` never locks on it. Report it as not yet decodable; do not try another demod. That is a different answer from `insufficient bandwidth`, which names the source's cap."
- `TODO.md` gists (heading convention `## <Area>: <statement>`): `## Video: the audio subcarrier stage` (`subcarrier_demod` per spec 5.4, taken from the demodulator before decimation when the composite rate is under 2 × 6.5 MHz), `## Video: a true chroma notch and chroma decode` (the 3 MHz low-pass stands in), `## Video: fixtures of the four bench transmitters` (0.5 s IQ cuts at 15.36 Msps cf32, about 31 MB each, kept out of the repository under a private path; the composite tests skip when they are absent, as `bits_chain.rs` does for private captures).
- `UAT.md` `## Plan 28.2: analog video (deferred: the operator sets up each transmitter)`: style B checklist — 1. the 5.8 GHz reverse camera shows live (thumbnail, full view, `locked`, SNR); 2. the FPV VTX likewise; 3. the HDMI sender shows live **or** never locks and is reported not yet decodable (either is a pass; say which); 4. the 1.3 GHz VTX; 5. smooth and sharp both watchable, the f/s line matching; 6. two feeds switch in the full view (the two-radio case if a second radio carries a channel, else the e2e proof stands); 7. while a video job runs on the B210, the CB plan on the other radio and the page's spectrum are unharmed (node CPU noted); 8. a 0.5 s capture of each transmitter saved (the fixtures gist).
- [ ] **Step 1: Write the prose and the sections.**
- [ ] **Step 2: Regenerate** AGENTS.md/SKILL.md; run the four checks above — Expected: all clean.
- [ ] **Step 3: Commit**: `git add docs README.md TODO.md UAT.md crates/zeta/src/agents_md.rs AGENTS.md skills && git commit -m "Plan 28.2: field craft for analog video and the not-yet-decodable verdict; stream save and the video profiles in the guide; gists and the deferred UAT"`.

---

### Task 10: Land, deploy, status (the field host)

**Files:**
- Modify: `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (`## Plan 28.2 status`), the memory file for plan 28 (outside the repo)

- [ ] **Step 1: Gates** on the branch with a private `CARGO_TARGET_DIR`: fmt per crate, clippy, the workspace tests, `agents-md --check` both, `public-check.sh`, `check-doc-links.sh`, `cargo deny check licenses bans sources` (CI runs it if not installed locally) — Expected: all green; paste the summary lines in the ledger.
- [ ] **Step 2: Rebase onto `main`** if it moved; rerun clippy and the tests of the touched crates; `ExitWorktree` (keep) and `git merge --ff-only <branch>` from `/Projects/ZetaSDR`. **Stop here for the operator's word** before anything leaves the machine: the fast-forward of `main` is a shared-branch side effect per this project's rules.
- [ ] **Step 3: Deploy** (the operator's word): `RUSTFLAGS="--cfg soapy" cargo build -p zeta --release`, `bash deploy/field-host/deploy-sdr.sh plan28.2-zeta`; check `deploy/field-host/z.sh source list` shows both radios healthy, the running jobs resumed, and `deploy/field-host/z.sh modes list` lists `video_decode`.
- [ ] **Step 4: Write `## Plan 28.2 status`**: merged commit, deploy name, `modes check video_decode` output, the e2e wall time, the ws smooth budget measured (bytes per frame, frames a second on the LAN page), node CPU with a `video_decode` on the file source; UAT deferred to the operator's bench (`UAT.md` "Plan 28.2").
- [ ] **Step 5: Commit** the status (`git commit -m "Plan 28.2 status: merged and deployed; UAT deferred to the bench"`) and update the plan 28 memory.

---

## Self-review notes

- **Spec coverage.** 5.1 row and stages: Tasks 0–2, 4. The composite as `audio` with a role kept out of the mix and attach: Task 3. `video_lock` event, proof `field: locked`, `quick: null`: Tasks 2, 4. 5.2 envelope body, profiles, newest-frame rule, stats: Tasks 5, 6. 5.3 thumbnail, full view (`full: true`, `data-strip="video"`, waterfall hidden not destroyed), setting, menu with the greyed text, no channel plans, themes untouched: Task 7. 5.4 designed only: the TODO gist in Task 9. 5.5 crate placement: `png` in `zeta-vrt`, nothing in core, `stream save` through `zeta-client`: Task 5. 5.6 the two verdicts: the field craft in Task 9, `identify`'s `skipped_band` pinned in Task 4, the lease's `why` from 28.1 greying the menu in Task 7. Section 6 proofs: unit tests on the synth (PAL/NTSC, AWGN 10/20/30, the subcarrier present is **not** tested since no subcarrier stage ships — the synth can add one and Task 2's noise test covers lock robustness; the 1 MHz offset case is covered by the `channel` stage, not the decoder, so it is not re-tested here), `modes check` at 8 MHz with `test.bandwidth_hz`, the ws budget test, the e2e fixture; the bench fixtures and the live checks are UAT.
- **Deviations from the spec, recorded:**
  - The output role lives on `Stage::output_roles` and the stream registration (`StreamInfo.role`), not on `StageSpec`'s declaration, so the page can skip the composite when picking a job's audio (`audio.js` takes the first audio stream).
  - The demodulator does not track black level; the decoder's sync-tip clamp does, which is where FM video's reference actually is.
  - The subcarrier notch is a 3.0/2.8 MHz luma low-pass; a true notch arrives with chroma.
  - `ccir405` is defined here as a single-pole shelf (14 dB, crossover 1.5/2.1 MHz); the bench fixtures decide whether the real network's second pole matters.
  - At 20.48 Msps the composite rate is 10.24 MHz, below 2 × 6.5 MHz, so the spec's "10.24 does, barely" is wrong for the subcarrier; the subcarrier gist takes its input before decimation.
  - `video_lock` summaries feed the tag snippet and never become ribbons (a ribbon every 5 s would be noise); the spec's "as other rows' summaries do" described ribbons.
  - `stream save` uses `--stream`/`--path` flags (the CLI convention) and requires `--node` (a one-shot process has no bus of its own under `serve`).
  - The row carries `min_hz: 6000000` so an 8 MHz check and a narrow-box identify both behave; the spec named only the 24 MHz band.
- **Type consistency checked:** `VideoStandard`, `StandardTable`, `Shelf` (Task 0) are used by Tasks 1, 2; `run_map_threaded`/`VIDEO_QUEUE_DEPTH`/`DROPPED_INPUTS` (Task 1) by Task 2; `output_roles` (Task 3) by `VideoFmDemodStage`; `VideoProfile`/`profile`/`encode_frame` (Task 5) by Task 6; `streamBinary` query (Task 7) matches Task 6's `video=`; `RowTest.bandwidth_hz` (Task 4) matches `modes_check`.
- **Proportion:** the plan carries signatures, constants and test assertions; the only algorithmic prose is Task 2's decoder rules, which the signature and tests do not determine.
