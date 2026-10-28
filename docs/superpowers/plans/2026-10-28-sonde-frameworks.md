# Sonde Frameworks Implementation Plan (plan 17)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Radiosondes decode end to end through generic frameworks: audio-input program rows (RS41 through `rs41mod`, DFM and M20 as rows), generic synthesiser families plus an RS41 frame generator that the real decoder proves, a `watch` job that spots a new signal in a pinned band, identifies it and keeps the proved decoder running on a free radio or in the watch capture, a `source_free` op, a normalised `position` on events with a map layer on the page, and a SondeHub sink stage last.

**Architecture:** `zeta-core::modes` grows the row schema (`decoder.demod`, `input.container`, `band.range_hz`, `position`, `identify.priority`), expands audio rows to `channel → nbfm_demod → external_decode`, formats numbers in summaries and adds `position` to the envelope; `zeta-ext`'s adaptor feeds audio frames (optionally under a WAV header) and gains the sink stage; `zeta-dsp` gains the `fsk`/`psk`/`ook`/`rs41` synthesiser families, the echo impairment and the `detect` stage over spectrum rows; `zeta-core::node` gains `watch.rs` (a controller job modelled on `identify.rs`), `free_sources` on the lease manager, and ephemeral job records (`owner`); the page gains `map.js` with vendored Leaflet, watch rendering and one menu entry.

**Tech Stack:** Rust 2024 (`zeta-core`, `zeta-dsp`, `zeta-ext`, `zeta`), serde/schemars, tokio; `reqwest` (already in the workspace, `zeta-ws`/`zeta-client`) for the sink; vanilla JS plus vendored Leaflet 1.9.4 in `crates/zeta-ui/web`; Playwright scenarios in `crates/zeta-ui/tests/e2e/ui.py`; the rs1729 decoders (`rs41mod`, `dfm09mod`, `m20mod`, package `sonde-decoders`) on the host for the live check only.

**Spec:** `docs/superpowers/specs/2026-10-28-sonde-frameworks-design.md` (sections 4 audio rows, 5 synthesiser, 6 watch, 7 positions and map, 8 free tuner, 9 sink, 11 builder's calls, 12 live acceptance).

## Global Constraints

- Plan 16's constraints carry over verbatim (`docs/superpowers/plans/2026-10-27-mode-registry.md` Global Constraints): the unit rule; every row field required unless marked optional, unknown fields rejected; a broken row or a missing program never stops the node; programs under the cleared environment in a process group, found on `PATH` or by `ZETA_HANDLER_<PROGRAM>`; unknown ids answer `unknown { field, value, known }`; op results additive; gates per task and the final gate; `CARGO_TARGET_DIR=/Projects/ZetaSDR/target`, `CARGO_INCREMENTAL=0`; temp files under the job's tmp directory, never `/tmp`; `python3 -I`.
- No new external crate. `zeta-ext` may add `reqwest = { version = "0.13.5", default-features = false, features = ["json", "rustls"] }` (already in the lock) for Task 10 only. The one new vendored asset is Leaflet 1.9.4 (BSD-2-Clause) under `crates/zeta-ui/web/vendor/leaflet/` (`leaflet.js`, `leaflet.css`, `LICENSE`), embedded like the other web files; no Leaflet image assets (markers are `L.circleMarker`). Tiles only from `https://tile.openstreetmap.org/{z}/{x}/{y}.png` with the OSM attribution; the e2e suite aborts that host.
- No channel-grid assumptions: the watch starts decoders on the measured centre; the sonde rows' band is `range_hz`, with `centers_hz: []`.
- The RS41 frame generator is written from public protocol descriptions (bazjo/RS41_Decoding wiki, Vaisala frame notes), never ported from `rs41mod.c`; `rs41mod` is the oracle only. Its budget is Task 4; if the oracle does not decode the frame at the end of it, the row ships with `test: null` (Task 4 Step 8) and the generator moves to the later wave.
- Host-dependent tests pin the programs: `ZETA_HANDLER_RS41MOD`, `ZETA_HANDLER_DFM09MOD`, `ZETA_HANDLER_M20MOD` point at `crates/zeta-ext/tests/fake_lines` (with `FAKE_WAV=1`) or a nonexistent path; a test that needs the real `rs41mod` skips with `rs41mod not on this host` when it is absent.
- Constants (one definition each, named here): `WAV_HEADER_LEN = 44`, `WAV_STREAM_SIZE = 0xFFFF_FFFF`; detect params `threshold_db 10`, `persist_ms 3000`, `width_hz [5000, 30000]`, `dc_mask_hz 25000`, `edge_mask 0.05`, `spurs_hz []`, `gone_ms 30000`; watch `RETRY_MS = 60_000`, `RETRY_MAX = 3`, `SILENCE_MS = 120_000`; map `TRACK_POINTS = 3600`; sink `batch_ms 5000`, `SONDEHUB_URL = "https://api.v2.sondehub.org/sondes/telemetry"`; synth rates `fsk`/`psk`/`ook` 48 000 Hz up to 4800 baud, 240 000 Hz up to 24 000 baud, else 2 400 000 Hz; `rs41` 48 000 Hz, 4800 baud, deviation 2 400 Hz.
- Every task that changes an op, result, note, field craft or stage regenerates `AGENTS.md` and `skills/zetasdr/SKILL.md` (`cargo run -p zeta -- agents-md > AGENTS.md`, `-- agents-md --skill > skills/zetasdr/SKILL.md`) and runs `cargo test -p zeta --test agents_md --test transports`; `rendering_lists_every_op_variant` (`crates/zeta/tests/agents_md.rs:14`) is bumped 21 → 22 in Task 5.
- UI gate: `cargo test -p zeta --test ui` with no `ui e2e skipped` line within its 300 s deadline (214 s today; this plan adds at most four scenarios, under 30 s together).
- Branch from `main` (94664e6 or later) in a worktree; one commit per task; messages in the repo's style (`Modes: ...`, `Adaptor: ...`, `Synth: ...`, `Watch: ...`, `UI: ...`, `Sink: ...`).

## Review Focus

1. **A program that wants a WAV header must get a header it can parse once, never twice.** A rate change mid-run (the demod's audio rate is fixed, but the adaptor's converter restarts on a rate change) must not write a second header into the stream. Pinned in Task 2 (`a_wav_header_is_written_once_per_run`).
2. **A watch must never retune the radio or take a band from another job.** A signal that overlaps a running job's band is ignored, and the watch's identify children run inside its capture; the page's elastic spectrum keeps following. Pinned in Task 7 (`a_signal_inside_another_jobs_band_is_ignored`, `watch_children_run_inside_the_watch_capture`).
3. **A day of spurs must not eat the operator's history.** Watch-owned identify runs and their children drop from the history the moment they stop, and the twenty kept records are the operator's own. Pinned in Task 7 (`owned_jobs_never_take_a_history_slot`).
4. **A sonde row's event without a fix must still render.** `rs41mod` prints frames before the GPS locks (`lat`/`lon` 0 or absent): the summary renders without `undefined`/`null` leaking (the spec's `?` rule) and the envelope adds no `position` for an event whose lat/lon are absent. Pinned in Task 1 (`an_event_without_a_fix_gets_no_position`).
5. **The map must not break the page when tiles cannot load.** With the tile host aborted, markers, tracks and the attribution still draw and no console error is thrown. Pinned in Task 9 (`scenario_map_marker_draws_without_tiles`).

---

## File map

- Modify `modes/` (create `rs41_decode.json`, `dfm_decode.json`, `m20_decode.json`); `crates/zeta-core/data/signatures.json` (RS41 `decoder`).
- Modify `crates/zeta-core/src/modes/row.rs` (`Band.range_hz`, `ModeRow.position`, `Identify.priority`, `ProgramBlock.demod`, `ProgramInput.container`, validation), `template.rs` (audio expansion), `summary.rs` (`{f:fmt}`, `position`), `score.rs` (range term, priority tie-break), `mod.rs` (`ModeInfo.band` prints `range_hz`); `src/ops.rs` (`Op::SourceFree`, `OpResult::SourceFree`, `JobKind::Watch`, `BUILTIN_KINDS` 15 → 16); `src/job.rs` (`JobRecord.owner`); `src/lease.rs` (`free_sources`); `src/node/mod.rs` (prune rule, `source_free`, `launch` arm for `watch`); create `src/node/watch.rs`.
- Modify `crates/zeta-ext/src/external.rs` (audio feed, WAV header, `inputs: [Iq, Audio]`), `src/proc.rs` (nothing new expected), `tests/fake_lines.py` (`FAKE_WAV`), `tests/external.rs`; create `src/sink_sondehub.rs`, `tests/sink.rs`.
- Modify `crates/zeta-dsp/src/synth.rs` (`Fsk`, `Psk`, `Ook`, `Rs41` families, `Impairments.echo`); create `src/rs41_frame.rs` (frame encoder, CRC-16, RS(255,231) GF(256) encoder), `src/detect.rs` (the `detect` stage); `src/lib.rs` registers `detect`.
- Modify `crates/zeta/src/cli.rs` (`SourceCmd::Free`), `src/main.rs`, `src/transport.rs`, `src/agents_md.rs` (notes for `source_free`, `watch`, the audio row shape, the map), `src/node_setup.rs` (register `detect`, `sink_sondehub`), `tests/agents_md.rs`, `tests/cli.rs`, `tests/modes_check.rs`.
- Modify `crates/zeta-ui/web/{app.js,view.js,menu.js,overlays.js,index.html,style.css}`; create `web/map.js`, `web/vendor/leaflet/{leaflet.js,leaflet.css,LICENSE}`; modify `crates/zeta-ui/src/lib.rs` (assets), `crates/zeta-ui/tests/e2e/ui.py` (scenarios).
- Modify `README.md`, `docs/agents/field-craft.md`, `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (row 17, "Plan 17 status"), `TODO.md`, `AGENTS.md`, `skills/zetasdr/SKILL.md`.

---

### Task 1: Row schema for audio rows, ranges, positions and summary formats (Opus)

**Files:**
- Modify: `crates/zeta-core/src/modes/row.rs` (`Band:78`, `ModeRow:48`, `Identify:144`, `ProgramBlock:173`, `ProgramInput:188`, `validate:371`, `validate_program:552`), `template.rs:24-100` (`expand`), `summary.rs:32-80` (`fill`, `envelope`), `score.rs:31-85` (`score_rows`), `mod.rs` (`ModeInfo`), `crates/zeta/src/agents_md.rs` (the `modes_list` note names `range_hz`, `position`)
- Test: `row.rs` unit tests, `summary.rs` unit tests, `score.rs` unit tests, `crates/zeta-core/tests/modes.rs`

**Interfaces:**
- Produces: `Band { bandwidth_hz, centers_hz, min_hz, range_hz: Option<[u64; 2]> }` (`#[serde(default)]`); `ModeRow.position: Option<PositionMap>` with `pub struct PositionMap { pub lat: String, pub lon: String, pub alt_m: Option<String> }`; `ModeRow.time: Option<String>` (`#[serde(default)]`), the name of an event field holding an ISO 8601 UTC time (`YYYY-MM-DDTHH:MM:SS[.fff]Z`) that `summary::envelope` parses by hand (no new crate) into `t_ns` when present and well-formed, else `t_ns` is `start_ns` as today; `Identify.priority: i32` (`#[serde(default)]`); `ProgramBlock.demod: Option<Demod>` with `pub enum Demod { Nbfm { deviation_hz: u64, audio_rate_hz: u64 } }` (tagged `kind`, snake_case); `ProgramInput.container: Container` with `pub enum Container { Raw, Wav }` (`#[serde(default)]`, `Raw`). `summary::fill` understands `{field:.N}` and `{field:+.N}` for numbers (`N` 0-6; a non-number renders as today's text) combined with `?` as `{field?:.1}`; `summary::envelope` adds `position: { lat, lon, alt_m }` when the row has a `position` map and the named lat and lon fields are present numbers (`alt_m` null when its field is absent). `score_rows` gives `CENTER_SCORE` when the centre lies in `range_hz` too, and breaks equal scores by `identify.priority` descending before family and id. `template::expand` of a program row with `input.stream == Audio` yields `channel → nbfm_demod { deviation_hz, audio_rate_hz, squelch: false } → external_decode { mode }` with edge kinds `iq`, `audio`.
- Consumes: plan 16's row types and `validate`.

- [ ] **Step 1: Write the failing row tests** in `row.rs`: `audio_row_needs_a_demod` (an audio program row without `demod` → `RowError { field: "decoder.demod", error: "audio input needs a demod" }`; with `demod` → ok, replacing the plan 16 "arrive in plan 17" assertion at `:735`), `demod_on_an_iq_row_is_refused` (`decoder.demod`, `an iq row takes no demod`), `range_must_be_ordered` (`band.range_hz`, `[lo, hi] with lo < hi`), `position_names_schema_fields` (a `position.lat` not in `event.schema.properties` → `position.lat`), `container_wav_needs_a_pcm_format` (`wav` with `cf32` → `decoder.input.container`).
- [ ] **Step 2: Run** `cargo test -p zeta-core --lib modes::row` → FAIL (unknown fields).
- [ ] **Step 3: Add the fields and the checks** per Interfaces; `validate_program` checks `demod` presence by stream, `container`/`format` compatibility (`wav` needs `s16`/`f32` or `cs16`/`cf32`), `range_hz` order; `validate` checks `position` names against the schema.
- [ ] **Step 4: Run** the row tests → PASS; `embedded_rows_parse` still passes (the five rows have no new fields).
- [ ] **Step 5: Write the failing summary tests** in `summary.rs`: `formats_numbers` (`fill("{alt:.0} m {vel_v:+.1}", {"alt": 12000.4, "vel_v": 5.04})` = `"12000 m +5.0"`; `fill("{id} {lat?:.4}", {"id": "S1", "lat": -27.41234})` = `"S1 -27.4123"`; a string under `:.1` renders as the string), `position_added_from_the_map` (row with `position {lat: "lat", lon: "lon", alt_m: "alt"}` → envelope has `position: {lat, lon, alt_m}`), `t_ns_comes_from_the_time_field` (row `time: "datetime"`, event `datetime "2026-10-28T02:15:30.500Z"` → `t_ns == 1_793_153_730_500_000_000`; a malformed value → `t_ns == start_ns`), `an_event_without_a_fix_gets_no_position` (`lat` absent, or `lat` null, → no `position` key; `alt_m` null when only `alt` is absent).
- [ ] **Step 6: Implement** the format suffix in `fill` and `position` in `envelope`; run → PASS.
- [ ] **Step 7: Write the failing score tests** in `score.rs`: `a_centre_inside_the_range_scores_the_centre_term` (row with `range_hz: [400050000, 406000000]`, centre 402 500 000 → score ≥ 0.4; centre 410 000 000 → 0 for that term), `priority_breaks_ties_first` (two rows of equal score and family, priority 1 vs 0 → the priority-1 row first regardless of id order).
- [ ] **Step 8: Implement**; run → PASS.
- [ ] **Step 9: Write the failing expansion test** in `crates/zeta-core/tests/modes.rs`: `an_audio_program_row_expands_to_channel_nbfm_external` (an operator row file copied from `adsb_decode.json` with `input.stream: audio`, `format: s16`, `rate_hz: 48000`, `container: wav`, `demod: {kind: nbfm, deviation_hz: 5000, audio_rate_hz: 48000}` → `expand` gives three nodes `channel`, `nbfm_demod` (params exactly `{"deviation_hz": 5000, "audio_rate_hz": 48000, "squelch": false}`), `external_decode` (`{"mode": "<id>"}`), edges `channel→nbfm_demod: iq`, `nbfm_demod→external_decode: audio`).
- [ ] **Step 10: Implement** in `template::expand`; `check_band` keeps returning `Ok` for audio rows. Run → PASS.
- [ ] **Step 11: `modes list` prints `range_hz`:** `ModeInfo.band` is the row's `Band` (it already is: confirm `range_hz` appears, `null` for the five rows), update the `modes_list` note in `agents_md.rs`; regenerate the guides; `cargo test -p zeta --test agents_md`.
- [ ] **Step 12: Gates and commit:** `cargo fmt -p zeta-core -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test -p zeta-core --features test-fixtures`. `git commit -m "Modes: audio program rows (demod, container), band ranges, position maps, summary number formats, priority tie-break"`.

### Task 2: The adaptor feeds audio under a WAV header; the three sonde rows (Opus)

**Files:**
- Modify: `crates/zeta-ext/src/external.rs` (`ExternalDecodeStage::new` spec `inputs: vec![Iq, Audio]`, `run_row:206`, `Feed::run:689`, `Converter:768`), `tests/fake_lines.py`, `tests/external.rs`, `crates/zeta-ext/tests/adsb.rs` (unchanged behaviour), `crates/zeta/src/node_setup.rs` (handler report lists the new programs), `crates/zeta-core/data/signatures.json:6` (`"decoder": "rs41_decode"`), `crates/zeta/tests/cli.rs`, `crates/zeta/tests/modes_check.rs`
- Create: `modes/rs41_decode.json`, `modes/dfm_decode.json`, `modes/m20_decode.json`
- Test: `crates/zeta-ext/tests/external.rs`, `crates/zeta/tests/cli.rs`

**Interfaces:**
- Produces: `Converter::new(in_rate_hz, out_rate_hz, format)` also accepts audio (`format` `S16`/`F32`) and gains `pub fn convert_audio(&mut self, samples: &[f32], channels: u16) -> Vec<u8>` (mono mixdown when `channels > 1`); `pub fn wav_header(rate_hz: u32, format: SampleFormat) -> [u8; WAV_HEADER_LEN]` (RIFF size and data size `WAV_STREAM_SIZE`, `fmt` tag 1 for `s16`, 3 for `f32`, 2 channels for an IQ format); `Feed` writes the header once before the first sample bytes when `container == Wav`. The three rows per spec 4.2 (`programs`, `args ["--json", "--ecc", "--crc"]`, `demod nbfm 5000/48000`, `input audio s16 48000 stdin wav`, `output stdout json_lines`, `install debian "sonde-decoders", url "https://github.com/rs1729/RS"`, `position {lat, lon, alt_m: "alt"}`, `time "datetime"`, `summary "{id} {lat?:.4},{lon?:.4} {alt?:.0} m {vel_v?:+.1} m/s"`, `label`/`protocol` `RS41`/`rs41`, `DFM`/`dfm`, `M20`/`m20`, `identify {proof: event, min_events: 1, dwell_ms: 5000, min_span_ms: 1000, quick: null, priority: 1 for rs41 else 0}`, `test: null` for all three in this task). `fake_lines.py` with `FAKE_WAV=1` reads 44 bytes first, exits 3 unless they start `RIFF`/`WAVE`, and prints `{"wav": {"rate_hz": R, "bits": B, "channels": C}}` as its first line.
- Consumes: Task 1's row fields and expansion.

- [ ] **Step 1: Write the failing adaptor tests** in `tests/external.rs`: `an_audio_row_feeds_wav_to_the_program` (a `json_lines` row with `input.stream audio ... container wav`, program `fake_lines` with `FAKE_WAV=1` and `FAKE_LINES` one JSON line; feed 48 kHz audio frames → the first event is the `wav` line with `rate_hz 48000, bits 16, channels 1`, the second the fake line with the envelope), `a_wav_header_is_written_once_per_run` (two audio frames at different rates → the program still sees exactly one header: the fake exits 3 on a second `RIFF` and the stage would fail), `raw_audio_has_no_header` (`container raw` → the fake with `FAKE_WAV=1` exits 3, the stage fails naming the program).
- [ ] **Step 2: Run** `cargo test -p zeta-ext --test external` → FAIL.
- [ ] **Step 3: Implement** `wav_header`, `convert_audio`, the `Feed` audio arm (`Payload::Audio` frames: `sample_rate_hz`, `channels`, `samples`), the header-once flag, and `inputs: vec![Iq, Audio]`; `run_row` fails `expects audio frames`/`expects iq frames` by the row's `input.stream`.
- [ ] **Step 4: Run** → PASS; `cargo test -p zeta-ext` (adsb unchanged).
- [ ] **Step 5: Write the three rows** and set the RS41 signature's `decoder`. Each row's `catalogue` lists the iNTERCEPT radiosonde entry ids found by grepping `crates/zeta-dsp/src/catalogue.rs`'s data for "radiosonde"/"RS41" (empty if none). Add to `crates/zeta-core/tests/modes.rs` `embedded_rows_parse_and_validate` coverage (it iterates the embedded set; confirm eight rows now) and a `the_rs41_row_is_an_audio_program_row` assertion (`decoder.input.stream == audio`, `position.lat == "lat"`, `band.range_hz == [400050000, 406000000]`).
- [ ] **Step 6: Write the failing CLI test** in `crates/zeta/tests/cli.rs`: `rs41_decode_runs_the_handler_on_demodulated_audio` (`ZETA_HANDLER_RS41MOD=crates/zeta-ext/tests/fake_lines`, `FAKE_WAV=1` through `ZETA_PLUGIN_ENV_FAKE_WAV`, `FAKE_LINES` one rs1729-shaped line with `id "S1234567", datetime "2026-10-28T02:15:30.000Z", lat -27.4, lon 153.1, alt 12000.0, vel_v 5.0`; `job start --kind rs41_decode --source tone --center-hz 100000000 --bandwidth-hz 12500 --then-tail 2,5000` → the second frame's payload has `protocol "rs41"`, `summary "S1234567 -27.4000,153.1000 12000 m +5.0 m/s"`, `position {lat: -27.4, lon: 153.1, alt_m: 12000.0}`, `t_ns 1793153730000000000`), and `the_handler_report_names_the_sonde_programs` (`ZETA_HANDLER_RS41MOD` nonexistent → stderr has `zeta: handlers: rs41mod: missing (apt install sonde-decoders; https://github.com/rs1729/RS) — modes: rs41_decode` and `modes list --state unavailable` lists `rs41_decode`).
- [ ] **Step 7: Run** → FAIL, then make them pass (the report already derives from rows; confirm `dfm09mod` and `m20mod` get their own lines).
- [ ] **Step 8: `modes check`:** `cargo run -p zeta -- modes check` reports the three rows `skipped: no test`; `--strict` fails them (expected, documented in Task 11).
- [ ] **Step 9: Regenerate the guides** (the `job_start` note gains one sentence on `rs41_decode`: band range, measured centre, one sonde per job); gates; `git commit -m "Adaptor: audio-input rows fed under a WAV header; rs41/dfm/m20 rows through the rs1729 decoders"`.

### Task 3: Generic synthesiser families and the echo impairment (Sonnet)

**Files:**
- Modify: `crates/zeta-dsp/src/synth.rs` (`SynthSpec:727`, `Impairments:763`, `validate:783`, `rate_hz:834`, `synthesise:866`), `modes/aprs_decode.json` and `modes/pocsag_decode.json` (optional echo variants, see Step 6)
- Test: `synth.rs` unit tests, `crates/zeta/tests/modes_check.rs`

**Interfaces:**
- Produces: `SynthSpec::Fsk { symbol_rate_baud: u32, deviation_hz: f64, order: u8, bits: String, repeat: u32 }`, `SynthSpec::Psk { symbol_rate_baud: u32, order: u8, bits: String, repeat: u32 }`, `SynthSpec::Ook { symbol_rate_baud: u32, bits: String, repeat: u32 }`; `bits` is hex (`"a5a5..."`) or `"pattern:<hex>"` repeated to 1 s; `repeat` bursts with 50 ms gaps; `order` 2 or 4 (`fsk`), 2 or 4 (`psk`), else `validate` fails `order: 2 or 4`; GFSK BT 0.5; raised-cosine 0.35; `Impairments.echo: Option<Echo>` with `pub struct Echo { pub delay_us: u32, pub gain_db: f32 }` applied after the family's IQ and before `awgn`; `rate_hz()` per the Global Constraints rates; `pub fn gfsk_modulate(symbols: &[u8], order: u8, symbol_rate_baud: u32, deviation_hz: f64, rate_hz: u32) -> Vec<Complex<f32>>` reused by Task 4.
- Consumes: plan 16's `SynthSpec`, `Impairments`, `awgn`.

- [ ] **Step 1: Write the failing unit tests:** `fsk_tones_sit_at_plus_minus_deviation` (2-FSK, 4800 baud, 2400 Hz, alternating bits → the power spectrum peaks within one bin of ±2400 Hz), `fsk4_has_four_tones`, `psk_has_a_carrier_null_and_the_symbol_rate_line` (BPSK: spectrum symmetric, no DC line after squaring shows a line at the symbol rate), `ook_is_silent_between_ones`, `echo_adds_a_delayed_copy` (an impulse-like `ook` single bit with `echo {delay_us: 100, gain_db: -6}` → a second pulse 100 us later 6 dB down), `rates_follow_the_symbol_rate` (4800 → 48 000; 9600 → 240 000; 100 000 → 2 400 000), `validate_rejects_bad_order_and_bits`.
- [ ] **Step 2: Run** `cargo test -p zeta-dsp --lib synth` → FAIL.
- [ ] **Step 3: Implement** the families and `echo`.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: `modes check` accepts them:** add to `crates/zeta/tests/modes_check.rs` `a_row_test_may_name_the_fsk_family` (an operator row copied from `pocsag_decode.json` with `test: {kind: synth, family: fsk, symbol_rate_baud: 1200, deviation_hz: 4500, order: 2, bits: "pattern:aa", repeat: 1, expect: {}, within_ms: 2000}` runs and reports `fail` or `ok` without a parse error: the point is the family is accepted and the signal is produced; assert the output line starts `<id>: ` and is not `fail: row:`).
- [ ] **Step 6: Echo variants:** run `modes check aprs_decode` and `pocsag_decode` with an added `echo {delay_us: 10, gain_db: -6}` in a scratch copy of each row; if a row still passes, keep the echo in the embedded row's `test`; if not, leave the row untouched and record which failed in the task report (the plan-series status carries it).
- [ ] **Step 7: Regenerate the guides** (the `modes_list` note's synthesiser family list and impairments); gates; `git commit -m "Synth: fsk, psk and ook families and the echo impairment"`.

### Task 4: The RS41 frame generator and the row's proof against `rs41mod` (Opus)

**Files:**
- Create: `crates/zeta-dsp/src/rs41_frame.rs`
- Modify: `crates/zeta-dsp/src/synth.rs` (`SynthSpec::Rs41`), `src/lib.rs`, `modes/rs41_decode.json` (`test`), `crates/zeta/tests/modes_check.rs`, `crates/zeta/src/modes_check.rs` (nothing expected; the row's audio chain already runs through the node)
- Test: `rs41_frame.rs` unit tests, `crates/zeta/tests/modes_check.rs`

**Interfaces:**
- Produces: `pub fn crc16_ccitt(data: &[u8]) -> u16` (poly 0x1021, init 0xFFFF); `pub struct Rs255_231` with `pub fn encode(&self, data: &[u8; 231]) -> [u8; 24]` (GF(256) poly 0x11D, the RS41's generator per the public description); `pub struct Rs41Params { pub serial: String, pub lat: f64, pub lon: f64, pub alt_m: f64, pub vel_v: f64, pub frame_no: u32 }`; `pub fn rs41_frame(p: &Rs41Params) -> Vec<u8>` (one 320-byte scrambled frame with header, both RS parity halves, the STATUS, MEAS (zero), GPSINFO, GPSRAW (minimal), GPSPOS (ECEF from lat/lon/alt, velocity from `vel_v`) and EMPTY blocks, each with its CRC); `SynthSpec::Rs41 { serial, lat, lon, alt_m, vel_v, frames }` producing `frames` frames one second apart through Task 3's `gfsk_modulate` (order 2, 4800 baud, 2400 Hz, 48 000 Hz).
- Consumes: Task 3's `gfsk_modulate`; Task 2's row.

- [ ] **Step 1: Write the failing unit tests** in `rs41_frame.rs`: `crc16_matches_a_known_vector` (`crc16_ccitt(b"123456789") == 0x29B1`), `rs_parity_corrects_itself` (encode a block, flip 12 bytes, a straightforward syndrome check in the test (or decode with the in-tree encoder's generator) shows the parity is consistent: assert that re-encoding the unflipped data reproduces the parity and that the parity differs after a flip), `frame_has_the_public_header_and_length` (first 8 bytes equal the public RS41 header after descrambling; length 320), `gpspos_block_round_trips` (decode the GPSPOS block's ECEF back to lat/lon/alt within 1 m).
- [ ] **Step 2: Run** → FAIL; implement; run → PASS.
- [ ] **Step 3: Write the failing check test** in `crates/zeta/tests/modes_check.rs`: `rs41_decode_passes_its_synthesised_check` (skips with `rs41mod not on this host` when `which rs41mod` fails; otherwise `modes check rs41_decode` prints `rs41_decode: ok`). The row's `test`: `{"kind": "synth", "family": "rs41", "serial": "S1234567", "lat": -27.4, "lon": 153.1, "alt_m": 12000, "vel_v": 5.0, "frames": 6, "snr_db": 20, "offset_hz": 0, "ppm": 0, "expect": {"id": "S1234567"}, "within_ms": 8000}`.
- [ ] **Step 4: Run** on this host (rs41mod present) → FAIL until the frame decodes.
- [ ] **Step 5: Iterate** on the frame until `rs41mod --json --ecc --crc` prints the id: inspect its stderr with `-vv` on the synthesised WAV written under the test's scratch dir; note each finding (which subframes must be non-zero) in the module doc.
- [ ] **Step 6: Run** → PASS; also `cargo test -p zeta-core --features test-fixtures` (nothing changes) and the full `modes check` (five `ok` plus `rs41_decode: ok`, two skipped).
- [ ] **Step 7: Regenerate the guides** (the `modes_list` note's family list gains `rs41`); gates; `git commit -m "Synth: an RS41 frame generator; rs41_decode proven by modes check against rs41mod"`.
- [ ] **Step 8: Fallback, only if Step 5 fails within this task's budget:** keep `rs41_frame.rs` and its unit tests, set the row's `test` back to `null`, make `rs41_decode_passes_its_synthesised_check` assert `rs41_decode: skipped: no test`, write what was learned in the task report and in `TODO.md` ("RS41 frame generator does not yet satisfy rs41mod: ..."), and commit as `Synth: an RS41 frame generator (not yet accepted by rs41mod; row test deferred)`.

### Task 5: `free_sources` and the `source_free` op (Opus)

**Files:**
- Modify: `crates/zeta-core/src/lease.rs` (`LeaseManager`, beside `holders:566`), `src/ops.rs` (`Op::SourceFree` beside `ChannelPlans:193`, `OpResult::SourceFree`, names, read-only set, dispatch beside `:2127`), `src/node/mod.rs` (`Node::source_free`), `crates/zeta/src/cli.rs` (`SourceCmd::Free { band: BandArgs }` at `:383`), `src/main.rs`, `src/transport.rs`, `src/agents_md.rs` (note), `tests/agents_md.rs:14` (21 → 22), `tests/cli.rs`
- Test: `crates/zeta-core/tests/ops.rs`, `crates/zeta/tests/cli.rs`

**Interfaces:**
- Produces: `LeaseManager::free_sources(&self, ports: &[Arc<dyn Port>], center_hz: u64, bandwidth_hz: u64) -> Vec<SourceId>` (no rigid holder, no pin, no fault, `tune_min_hz <= center - bw/2`, `center + bw/2 <= tune_max_hz`, `max_bandwidth_hz >= bw`, in the given order; elastic jobs ignored); `Op::SourceFree { center_hz: Hz, bandwidth_hz: Hz }` → `OpResult::SourceFree { free: Vec<SourceId>, busy: Vec<BusySource> }` with `pub struct BusySource { pub id: SourceId, pub holders: Vec<JobId>, pub elastic: Vec<JobId>, pub pin: Option<Tuning>, pub fault: Option<String> }` (every source not in `free`, with why); CLI `zeta source free --center-hz N --bandwidth-hz N`.
- Consumes: `holders`, `pinned`, `elastic_holders`, the port's `capabilities()` and fault.

- [ ] **Step 1: Write the failing op tests** in `crates/zeta-core/tests/ops.rs`: `source_free_lists_idle_sources_that_cover_the_band` (two file sources with tune ranges; one holds a rigid job → `free` has the other only, `busy` names the holder), `an_elastic_job_does_not_make_a_source_busy`, `a_pinned_or_faulted_source_is_busy` (a pin → `busy[].pin` set; a faulted file source → `busy[].fault` set), `a_source_that_cannot_cover_the_band_is_in_neither_list` (a band outside a source's tune range, or wider than its `max_bandwidth_hz` → the source appears in neither `free` nor `busy`), `source_free_is_read_only` (it is not logged with a result and is allowed while `serve` owns the state dir).
- [ ] **Step 2: Run** → FAIL; implement; run → PASS.
- [ ] **Step 3: CLI:** `zeta source free` prints the result; `tests/cli.rs` `source_free_prints_free_and_busy`; the `--node` path works (`transport.rs`).
- [ ] **Step 4: Regenerate the guides** (note: a free source is a suggestion, not a reservation; the next `job_start` may still conflict); bump the op-variant count; gates; `git commit -m "Sources: free_sources on the lease manager and the source_free op"`.

### Task 6: The `detect` stage over spectrum rows (Opus)

**Files:**
- Create: `crates/zeta-dsp/src/detect.rs`
- Modify: `crates/zeta-dsp/src/lib.rs` (`register_all`), `crates/zeta/src/node_setup.rs` (nothing if `register_all` is used), `crates/zeta/src/agents_md.rs` (stage list is generated; a sentence in the `job_start` note waits for Task 7)
- Test: `detect.rs` unit tests, `crates/zeta-dsp/tests/detect.rs`

**Interfaces:**
- Produces: stage kind `detect` (`inputs: [Event]`, `outputs: [Event]`), `pub struct DetectParams { pub threshold_db: f32 (10.0), pub persist_ms: u64 (3000), pub width_hz: [u64; 2] ([5000, 30000]), pub dc_mask_hz: u64 (25000), pub edge_mask: f32 (0.05), pub spurs_hz: Vec<u64> ([]), pub gone_ms: u64 (30000) }` (all `#[serde(default)]`); the pure function `pub fn runs(row: &SpectrumRowView, p: &DetectParams) -> Vec<Run>` with `pub struct Run { pub center_hz: u64, pub bandwidth_hz: u64, pub snr_db: f32 }` (bins over `floor_db + threshold_db`, grouped into contiguous runs, the centre power-weighted, masks applied); the tracker `pub struct Tracker` with `pub fn observe(&mut self, t_ns: u64, runs: &[Run]) -> Vec<Signal>` matching runs to tracked signals by centre within `max(bandwidth_hz, 2 × bin_hz)`, emitting `Signal { state: New, .. }` once a run has persisted `persist_ms` and `Signal { state: Gone, .. }` after `gone_ms` without a match; event payload `{ "protocol": "signal", "state": "new"|"gone", "center_hz", "bandwidth_hz", "snr_db", "first_ns", "last_ns", "summary": "<state> <center MHz> <bw kHz> <snr dB>", "t_ns" }`.
- Consumes: the spectrum row payload of plan 8 (`size`, `bin_hz`, `center_hz`, `floor_db`, `step_db`, `q`).

- [ ] **Step 1: Write the failing unit tests:** `runs_find_a_10_khz_bump_and_skip_the_dc_spike` (a synthetic row: floor at q=10, a 2-bin spike at the centre, a 3-bin run 12 dB up at +1 MHz → one run near +1 MHz, width 3 × bin_hz), `runs_respect_the_width_limits_and_spurs` (a 60-bin run is dropped; a run on a listed spur is dropped), `tracker_reports_new_after_persist_and_gone_after_silence` (the same run at t = 0, 1, 2, 3 s → `New` at 3 s once; none at 4 s; absent from 4 s → `Gone` at 34 s), `tracker_follows_a_drifting_centre` (centre moving 1 bin per row stays one signal).
- [ ] **Step 2: Run** → FAIL; implement; run → PASS.
- [ ] **Step 3: Write the failing stage test** in `crates/zeta-dsp/tests/detect.rs`: `detect_publishes_new_and_gone_for_a_synthesised_burst` (a `fft → detect` graph on a file source whose IQ is Task 3's `fsk` family 4800 baud at +1 MHz offset in a 6 MHz capture for 5 s then silence → a `signal new` event with `center_hz` within 6 kHz of the burst's centre and `bandwidth_hz` within `[5000, 30000]`, then `gone`).
- [ ] **Step 4: Run** → FAIL; register the stage; run → PASS.
- [ ] **Step 5: Regenerate the guides** (the stage list gains `detect` with its params); gates; `git commit -m "DSP: the detect stage, new and gone signals over spectrum rows"`.

### Task 7: The `watch` job: identify what appears, place the decoder, own what it starts (Opus)

**Files:**
- Create: `crates/zeta-core/src/node/watch.rs`
- Modify: `crates/zeta-core/src/ops.rs` (`JobKind::Watch`, `BUILTIN_KINDS` 15 → 16, `expand` gives `fft → detect` with `fft` sized like `spectrum`'s and `detect` params from the record's stored params, `watch` refused for `job_attach`), `src/job.rs` (`JobRecord.owner: Option<JobId>`, `#[serde(default)]`), `src/node/mod.rs` (`launch` arm, `prune_history:328` drops owned terminal records at once, `job_list` unchanged), `src/node/identify.rs` (`launch_identify` takes an `owner` for records it creates; `start_child` sets `owner` on the child), `src/node/launch.rs` (shared `spawn_controller` helper if the implementer extracts it), `crates/zeta/src/node_setup.rs::tests::every_job_kind_expands_to_a_valid_graph` (`watch` expands), `crates/zeta/src/agents_md.rs` (the `job_start` note's `watch` paragraph, per spec 6), `crates/zeta/src/cli.rs` (`--then-tail` on a watch tails its event stream, nothing special)
- Test: `crates/zeta-core/tests/watch.rs` (new, `required-features = ["test-fixtures"]`), `crates/zeta/tests/cli.rs`

**Interfaces:**
- Produces: `JobKind::Watch` (name `watch`); the watch's params on the `detect` node plus `decode_on: "capture" | "free"` (default `capture`), `prefer: Vec<SourceId>` (`[]`), `sondehub: Option<serde_json::Value>` (`null`; stored opaque, Task 10 parses it); events on the watch's event stream with `protocol: "watch"` and `state` `identifying | decoding | unknown | lost` carrying `signal` (the `detect` signal), `job`, `mode`, `source`, `placed` (`capture | free`), `features` (for `unknown`), `summary`, `t_ns`; `job_context.watch = { signals: [signal...], decoding: [{mode, job, source, signal, since_ns}], current: <identify id or null> }`; `JobRecord.owner`; `node/watch.rs::watch_loop(inner, run)`; the placement helper `Inner::place_decoder(&self, row, signal, policy) -> Result<(JobId, Placed), String>`.
- Consumes: Task 5's `free_sources`, Task 6's `detect` events, identify's `start_child`/`await_proof` (made `pub(super)` where needed), the lease manager's gate rules of plan 16's identify (gated stops).

- [ ] **Step 1: Write the failing core tests** in `crates/zeta-core/tests/watch.rs` (file sources from Task 3's `fsk` family and plan 16's POCSAG fixture; captures at 1 024 000 Hz, under the 2 Msps bank threshold, because a plain `pocsag_decode` never decodes on the debug-build bank at 2.048 MHz (TODO.md); the loop's identify proves `pocsag_decode` on a burst inside that capture): `a_new_signal_is_identified_and_the_proved_decoder_kept` (a POCSAG burst at +300 kHz in a 1.024 MHz file source repeating every 2 s → `watch` events `identifying` then `decoding` with `mode pocsag_decode`, `placed capture`; `job_context.watch.decoding[0].job` is a running `pocsag_decode` job on the measured centre within 6 kHz), `a_signal_inside_another_jobs_band_is_ignored` (a rigid `measure` job already on that band → no `identifying` event for it), `watch_children_run_inside_the_watch_capture` (`source_list.current` never changes while the watch runs; the identify child and the decoder hold the source inside it), `an_unknown_signal_is_retried_at_most_three_times` (an `fsk` burst no row proves → `unknown` events with `features`, three retries at `RETRY_MS` which the test lowers through a `#[cfg(feature = "test-fixtures")]` override `watch::test_retry_ms(ms)`), `a_lost_signal_stops_its_decoder` (the burst stops → `gone` → after `SILENCE_MS` (overridden) a `lost` event and the decoder stopped), `owned_jobs_never_take_a_history_slot` (after the run, `job_list` holds no stopped identify or child record owned by the watch, and the operator's own stopped jobs are all still there), `stopping_the_watch_stops_its_decoders`, `graph_put_on_the_watch_readopts_its_decoders` (a `graph_put` changing `threshold_db` → the decoder job keeps running under the same id and `watch.decoding` lists it after the restart), `a_restart_readopts_or_drops` (drop the node mid-decode, reopen with resume → the decoder resumed and re-adopted; a second case where the file source no longer carries the burst → dropped after `gone_ms`).
- [ ] **Step 2: Run** → FAIL (no `watch` kind).
- [ ] **Step 3: Implement** `JobKind::Watch`, the expansion, `owner`, the prune rule, `watch.rs` (loop per spec 6.2 with a `ChildGuard`-style drop guard for the identify child; decoders are stopped by the loop's exit path on `job_stop` but re-adopted on `graph_put`/resume: the record's context tells which, `watch.decoding` persisted on every change), `place_decoder` (`capture`: keep `verdict.job`; `free`: `free_sources` minus the watch's source, in `prefer` order first, `job_start` the row on the signal's measured centre and row width, on `conflict` try the next, else `capture`), the events.
- [ ] **Step 4: Run** → PASS; `cargo test -p zeta-core --features test-fixtures` (identify tests unchanged).
- [ ] **Step 5: Write the failing CLI test** in `tests/cli.rs`: `watch_then_tail_prints_signal_events` (one-shot `job start --kind watch ... --then-tail 1,8000` on the burst file → the frames hold a `watch` event; `{"stopped": true}`; no job left running).
- [ ] **Step 6: Run** → PASS.
- [ ] **Step 7: Regenerate the guides** (the `job_start` note's watch paragraph; `job_attach` refuses `watch`; `schema` lists 16 built-in kinds); `every_job_kind_expands_to_a_valid_graph` treats `watch` like a graph kind; gates; `git commit -m "Watch: a controller job that identifies new signals in its band, keeps the proved decoder on a free radio or in its capture, and owns what it starts"`.

### Task 8: The page: watch rendering and "Watch this band" (Sonnet)

**Files:**
- Modify: `crates/zeta-ui/web/menu.js` (entry `["watch", "Watch this band"]` beside `identify:111`; `jobTuning` passes the selection as is), `overlays.js` (a `watch` job's band outlined like a scan's; `signal new` events as small markers at their centre inside the band, removed on `gone`; owned jobs (`owner != null`) hidden from the jobs pane; `watch` events as ribbons with their `summary`), `app.js` (jobs pane filter), `crates/zeta-ui/tests/e2e/ui.py`
- Test: `ui.py` scenarios `scenario_watch_entry_starts_a_watch_and_outlines_its_band` (menu on the tone source → a `watch` job on the selection; the band outline appears; stop it after), `scenario_owned_jobs_are_hidden_from_the_jobs_pane` (start a watch through the API on the burst file, wait for an `identifying` event over the websocket, assert the jobs pane lists the watch and not its identify child; `job list` over HTTP does list the child)

- [ ] **Step 1: Write the two scenarios** (they fail: no entry, no filter).
- [ ] **Step 2: Implement**; run `cargo test -p zeta --test ui` → PASS, time noted.
- [ ] **Step 3: Commit** `UI: watch jobs outlined with their signals; owned jobs hidden; "Watch this band"`.

### Task 9: Positions on the map (Opus)

**Files:**
- Create: `crates/zeta-ui/web/map.js`, `web/vendor/leaflet/{leaflet.js,leaflet.css,LICENSE}` (1.9.4, fetched from the Leaflet release and committed verbatim; the task report records the sha256 of each)
- Modify: `crates/zeta-ui/src/lib.rs` (assets: `map.js`, `vendor/leaflet/leaflet.js`, `vendor/leaflet/leaflet.css` with their MIME types), `web/index.html` (a `<div id="map" hidden>` in `main` beside the waterfall; the narrow renderer's tab), `web/style.css`, `web/view.js` (`POLICIES` gain `map` (`on|off`, default `off`) and `home` (`LAT,LON` parsed to two finite numbers, or `none`, default `none`)), `web/app.js` (mount `map.js`; the jobs pane row of a job with a position shows `d km, b°` from home), `crates/zeta-ui/tests/e2e/ui.py`
- Test: `ui.py` scenarios `scenario_map_marker_draws_without_tiles`, `scenario_home_gives_distance_and_bearing`

**Interfaces:**
- Produces: `export function mountMap(el, api, view, { jobs })` in `map.js`: shows/hides with `view.map`; one `L.circleMarker` plus `L.polyline` per running job whose events carry `position`; the marker's popup is the last `summary` and its age; `TRACK_POINTS = 3600`; `export function distanceBearing(home, pos) -> { km, deg }` (haversine, initial bearing); the OSM tile layer with attribution `© OpenStreetMap contributors`; a home marker from `view.home`.
- Consumes: Task 1's `position` on events; the websocket frames the ribbons read (`overlays.js` `mountOverlays` wiring; the implementer exposes a `onEvent(fn)` hook there if none exists).
- e2e fixture: the harness starts `serve` with `--modes-dir <scratch>` holding `fake_pos.json`: a `json_lines` program row (programs `["fake_lines"]`, `ZETA_HANDLER_FAKE_LINES` set to `crates/zeta-ext/tests/fake_lines`, `input iq cs16 48000 stdin raw`, `position {lat, lon, alt_m: "alt"}`, `summary "{id} {lat:.2},{lon:.2}"`, `FAKE_LINES` through `ZETA_PLUGIN_ENV_FAKE_LINES` with two lines moving from `-27.40,153.10` to `-27.41,153.11`); `page.route("**/tile.openstreetmap.org/**", abort)` in every map scenario.

- [ ] **Step 1: Write the failing scenarios:** `scenario_map_marker_draws_without_tiles` (`view set map on`; start `fake_pos` on the tone source; the map pane shows one `.leaflet-interactive` marker and a polyline with two points; no console error), `scenario_home_gives_distance_and_bearing` (`view set home -27.50,153.00`; the jobs pane row shows a text matching `/\d+(\.\d+)? km, \d+°/`; `distanceBearing({lat:-27.5,lon:153.0},{lat:-27.4,lon:153.1})` evaluated in the page ≈ `{km: 15.0 ± 0.3, deg: 42 ± 2}`).
- [ ] **Step 2: Vendor Leaflet, add the assets, write `map.js`, the policies, the pane and the row text.**
- [ ] **Step 3: Run** `cargo test -p zeta --test ui` → PASS; the suite's time noted (budget: under 300 s).
- [ ] **Step 4: Regenerate the guides** (field craft gains one paragraph in Task 11; here only the `view` note if `agents_md.rs` carries it); `git commit -m "UI: a map layer for events with a position (vendored Leaflet, OSM tiles when online), home distance and bearing"`.

### Task 10: The SondeHub sink (Sonnet; last, deferrable)

**Files:**
- Create: `crates/zeta-ext/src/sink_sondehub.rs`, `crates/zeta-ext/tests/sink.rs`
- Modify: `crates/zeta-ext/Cargo.toml` (`reqwest`), `src/lib.rs` (register `sink_sondehub`), `crates/zeta-core/src/node/watch.rs` (`sondehub` param appends the sink node to each decoder graph it starts), `crates/zeta/src/agents_md.rs` (note)
- Test: `tests/sink.rs` against a local `tokio` TCP listener standing in for the API (`url` param)

**Interfaces:**
- Produces: stage kind `sink_sondehub` (`inputs: [Event]`, `outputs: []`), `pub struct SondehubParams { pub callsign: String, pub lat: f64, pub lon: f64, pub alt_m: f64, pub batch_ms: u64 (5000), pub url: String (SONDEHUB_URL) }`; one `PUT`/`POST` per batch with SondeHub's v2 telemetry JSON (`software_name "zetasdr"`, `software_version` the crate version, `uploader_callsign`, `uploader_position`, `time_received`, `datetime`, `manufacturer`/`type` from `protocol` (`rs41` → `Vaisala`/`RS41`), `serial` from `id`, `frame`, `lat`, `lon`, `alt`, `vel_h`, `vel_v`, `heading`, `sats`, `batt`); `job_context.sondehub = { sent, failed, last_error }`.
- Consumes: Task 2's events; Task 7's `sondehub` param.

- [ ] **Step 1: Write the failing tests:** `a_batch_is_posted_with_the_station_fields` (two rs41 events → one request after `batch_ms` whose body is a JSON array of two objects with the fields above), `events_without_a_position_are_skipped`, `a_failed_post_counts_and_does_not_retry_in_a_loop` (the listener answers 500 → `failed 1`, `last_error` set, the next batch still goes).
- [ ] **Step 2: Run** → FAIL; implement; run → PASS.
- [ ] **Step 3: The watch appends it** when `sondehub` is set (`crates/zeta-core/tests/watch.rs` `a_watch_with_sondehub_appends_the_sink`: the decoder's `graph_get` has a `sink_sondehub` node).
- [ ] **Step 4: Regenerate the guides**; gates; `git commit -m "Sink: sink_sondehub posts sonde telemetry in batches; the watch appends it when asked"`.

### Task 11: Field craft, README, plan series, TODO, final gate (Sonnet)

**Files:**
- Modify: `docs/agents/field-craft.md` ("Decoding modes" gains the audio-row shape; new section "Watching a band for sondes": the one-radio and the two-radio form, `source free`, reading `watch` events and `job_context.watch`, the map and `view set home`, SondeHub), `README.md` ("Decoder modes" paragraph, the job-kind count 15 → 16, the map), `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (row 17 Delivered; a "Plan 17 status" section with what was built, the echo-variant outcome, the RS41 generator outcome, deferred items, live acceptance pending), `TODO.md` (the later-wave gist: voice, AIS, FLEX, spans, stereo, DFM/M20 frames, APRS positions; anything deferred), `AGENTS.md`, `skills/zetasdr/SKILL.md`

- [ ] **Step 1: Write the docs**; regenerate the guides; `agents-md --check`, `--skill --check`.
- [ ] **Step 2: Final gate:** `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features --no-fail-fast` (no `FAILED`, no `ui e2e skipped`), `cargo run -p zeta -- modes check` (six `ok` or five plus the Task 4 fallback, two or three `skipped`).
- [ ] **Step 3: Commit** `Plan 17: field craft (watch, sondes, map), README, plan series, TODO`.

---

## Live acceptance (controller, after the final review)

Per spec section 12, on the home station with the discones back during a Brisbane launch window: a `watch` on 400-406 MHz on the HackRF reports the sonde as a `signal` within `persist_ms` of its first frame, identifies it as `rs41_decode` within one dwell, and `job_context.watch.decoding` names a running `rs41_decode` job whose events carry `id`, `position` and a `summary`; the page's map shows the marker moving and the ribbon scrolling; `view set home` gives distance and bearing; with the field host's B210 attached as a `--source-node` and `decode_on: free`, the decoder lands on the B210 and the HackRF keeps watching; stopping the watch stops the decoder; `zeta modes check --strict rs41_decode` exits 0 on this host; `zeta source free` lists the idle radio; nothing uploads until a `sink_sondehub` node with a callsign is on the job.
