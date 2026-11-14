# Plan 38: Digital voice (P25 phase 1 and DMR) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two decoder rows, `p25_decode` and `dmr_decode`, that report calls as events and publish clear speech as ordinary job audio through a vocoder program the operator builds.

**Architecture:** The M17 stage's vocoder becomes one shared seat in `zeta-dsp` (`vocoder.rs`): frames to a child process, speech back. A handler may be `optional`, so a row stays ready and reports calls with no vocoder on the host. A shared 4-level FSK front end (`fsk4.rs`) feeds two framing modules (`p25.rs`, `dmr.rs`), each with a stage and a synthesiser family. The codec itself is `zeta-mbe`, one C file linked against `mbelib-neo` by `tools/install-vocoders.sh` on the operator's machine, never by cargo or CI.

**Tech Stack:** Rust (tokio `process`, serde, schemars; `zeta_dsp::synth`, `nbfm`, `resample::Resampler`), the row machinery in `zeta-core/src/modes`, bash and C99 for the vocoder tooling, `mbelib-neo` and `dsd-fme` as host programs on a developer's machine only.

**Spec:** `docs/superpowers/specs/2026-11-14-digital-voice-design.md` (all of it; section 9 lists the calls the operator has read). Executors read both.

## Global Constraints

- No codec in the tree: no vocoder source, table or binary is committed, vendored, fetched by cargo or built by CI. `deny.toml` is not edited. `mbelib-neo` (GPL-2.0-or-later) is linked only into `zeta-mbe`.
- No decryption and no key input of any kind, DMR basic privacy included. An encrypted call gets its events and no vocoder.
- No region or radio assumptions: the rows have empty `centers_hz` and `range_hz: null`.
- One audio output per job; speech is published as the voice demodulators publish it (48 kHz mono `Payload::Audio`).
- Row fields: unknown fields are rejected, so every new field is `#[serde(default)]` and every existing embedded and operator row still loads. Every field a row's `summary` or `subject` names is in `event.schema.properties`.
- A stage an embedded graph row names lives in `zeta-dsp` and is registered in `zeta_dsp::register_all` (`crates/zeta-dsp/src/lib.rs`).
- `m17_decode` keeps its behaviour: `c2dec` required, event names unchanged, every existing M17 test passing unedited.
- Section 4 of the spec is working notes. Every protocol constant is checked against TIA-102.BAAA (P25) or ETSI TS 102 361-1/-2 (DMR) before it is used, and each module's doc comment cites the clause. No code is written from a GPL decoder's source; `dsd-fme` is an oracle run by hand, not a reference to copy.
- Numbers from the spec are named constants: `P25_BAUD = 4800`, `P25_DEVIATION_HZ = 1800.0`, `P25_SYNC = 0x5575_F5FF_77FF`, `P25_ALGID_CLEAR = 0x80`, `DMR_BAUD = 4800`, `DMR_DEVIATION_HZ = 1944.0`, `DMR_SYNC_BS_VOICE = 0x755F_D7DF_75F7`, `DMR_SYNC_BS_DATA = 0xDFF5_7D75_DF5D`, `DMR_SYNC_MS_VOICE = 0x7F7D_5DD5_7DFD`, `DMR_SYNC_MS_DATA = 0xD5D7_F77F_D757`, `VOICE_BANDWIDTH_HZ = 12_500`, `MBE_FRAME_SAMPLES = 160`, `MBE_RATE_HZ = 8000`, `IMBE_FEED_BYTES = 184`, `AMBE_FEED_BYTES = 96`, `VOCODER_WATCHDOG_MS = 2000`, `IDENTIFY_DWELL_MS = 2000`.
- Test identities are invented and outside registered ranges: P25 `nac` 291 (`0x123`), `src` 9990001, `dst` 65000; DMR `color_code` 5, `src` 9990001, `dst` 9990. No real station identifier, callsign or capture enters the repository.
- The repository is public: roles, never private host names or LAN addresses, in code, tests, docs and commit messages.
- `AGENTS.md` and `skills/zetasdr/SKILL.md` are generated. After any op field, op note, row note or field-craft change, regenerate both and commit them in the same commit; never edit them by hand.
- Tests that read asynchronous state poll for the end state (the suites' `until` helpers); none reads once after an operation.
- Gates per task: `cargo fmt -p <crate> -- --check` for each touched crate (the nested worktree breaks `cargo fmt --all`), `cargo clippy --workspace --all-targets --all-features -- -D warnings`, the touched crates' tests. Use a private `CARGO_TARGET_DIR` and delete it when the plan's gates are done. The full gate list in `CLAUDE.md` runs in task 9.
- One commit per task on this branch, message in the repository's style (`Area: what changed`), no trailer.

## Models per task

Opus where a task defines an interface later tasks consume, tunes a demodulator or implements a code against a standard; Sonnet where the brief and tests pin the behaviour.

| Task | Model | Why |
|---|---|---|
| 1 the vocoder seat | Opus | a lift under unchanged M17 tests, and the interface tasks 6 and 8 use |
| 2 optional handlers and `missing` | Opus | a public op field across four crates and CI |
| 3 `zeta-mbe` and the install script | Opus | C against a library's API, read from its header at the pinned commit |
| 4 the 4-level FSK front end | Opus | timing and level recovery under noise and offset |
| 5 P25 codes, frames and the `p25` family | Opus | four block codes against the standard |
| 6 the `p25_decode` stage and row | Opus | call tracking and the seat in use |
| 7 DMR codes, bursts and the `dmr` family | Opus | TDMA framing and BPTC against the standard |
| 8 the `dmr_decode` stage and row | Opus | two slots, one audio output |
| 9 guides, TODO, UAT, the oracle run, the gate | Sonnet | prose and commands, all pinned here |

Tasks 1, 2 and 3 are independent. Task 4 needs none of them. Tasks 5-6 and 7-8 are independent of each other once 1, 2 and 4 are in.

## Review Focus

Conditions the spec implies and a user will meet, each pinned by a test in the task named:

1. **An inverted or off-centre signal.** A discriminator with inverted polarity, or a carrier 1 kHz off the job's centre, still syncs and decodes (task 4 `inverted_and_offset_signals_still_sync`; tasks 6 and 8 `a_call_1_khz_off_centre_decodes`).
2. **A call that never ends.** The transmitter drops with no terminator: the closing event arrives after one superframe of lost sync and the vocoder's input is closed (tasks 6 and 8 `a_call_cut_short_closes_after_its_timeout`).
3. **Many short calls.** Twenty calls back to back leave no vocoder child running and no growing task list (task 1 `twenty_calls_leave_no_child`).
4. **Somebody else's 12.5 kHz signal.** Noise, NBFM speech, an M17 stream and a POCSAG burst give no event and no panic from either stage; P25 fed to DMR and DMR fed to P25 likewise (tasks 6 and 8 `other_signals_give_no_event`).
5. **A stream that stutters or changes rate.** A lagged input or a new sample rate mid-call closes the open call with its closing event and starts clean (tasks 6 and 8 `a_lagged_input_closes_the_call`), and a rate under four samples per symbol is refused with the rate named (task 4 `a_low_rate_is_refused`).

---

### Task 1: The vocoder seat, with M17 on it

**Files:**
- Create: `crates/zeta-dsp/src/vocoder.rs`
- Modify: `crates/zeta-dsp/src/m17_decode.rs` (remove `VocoderEnv`, `Vocoder`, `vocoder_program`; use the seat), `crates/zeta-dsp/src/lib.rs` (`pub mod vocoder;`)
- Test: inline `#[cfg(test)]` in `vocoder.rs`; the existing tests in `m17_decode.rs`

**Interfaces:**
- Produces:

```rust
pub const VOCODER_WATCHDOG_MS: u64 = 2000;
pub const AUDIO_RATE_HZ: u64 = 48_000; // moved here from m17_decode, re-exported there

/// What a stage states about the vocoder for one call.
#[derive(Clone, Debug)]
pub struct VocoderSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub feed_bytes: usize,    // bytes handed over per feed
    pub feed_samples: usize,  // samples expected back per feed, at rate_hz
    pub rate_hz: u64,
    pub context: &'static str, // "m17" | "p25" | "dmr": the note is {context: {"vocoder": text}}
}

#[derive(Clone)]
pub struct VocoderEnv {
    pub bus: Arc<StreamBus>,
    pub out: StreamId,
    pub ctx: StageContext,
    pub drains: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

pub struct Vocoder { /* private */ }
impl Vocoder {
    pub fn spawn(env: &VocoderEnv, spec: &VocoderSpec, prov: Provenance) -> Result<Self, String>;
    /// Hand over one feed; `bytes.len()` must equal `spec.feed_bytes`.
    pub fn feed(&self, span: Span, bytes: &[u8]);
    /// False once the child has exited or the watchdog killed it.
    pub fn alive(&self) -> bool;
    /// Close the input; the reader, still publishing, is returned to be awaited.
    pub fn finish(&mut self) -> Option<JoinHandle<()>>;
}

/// The row's handler `index`, looked up now (so a program installed while the job runs is
/// found at the next call). `Ok(None)` when it is absent and the row marks it optional or
/// the stage runs without a row and `fallback` is not on the host and `required` is false.
pub fn program(ctx: &StageContext, index: usize, fallback: &str, required: bool)
    -> Result<Option<PathBuf>, String>;
```

  Until task 2 adds `Handler::optional`, `program` treats every row handler as required; task 2 changes that one branch.

- [ ] **Step 1: Write the failing tests** in `vocoder.rs`, using a fake program written to a temp dir as `m17_decode.rs`'s `fake_c2dec` does (a shell script that copies fixed-size input to fixed-size output):
  - `feeds_come_back_as_48k_audio_with_their_spans`: 5 feeds of 96 bytes into a fake returning 320 bytes each; 5 audio frames arrive, each 960 samples at 48 000 Hz, spans equal to those fed.
  - `an_exit_mid_call_notes_once_and_is_not_alive`: a fake that exits after 2 feeds; `alive()` becomes false; `job_context` holds `{"p25": {"vocoder": <text naming the program>}}` exactly once.
  - `a_hang_is_killed_by_the_watchdog`: a fake that reads and never writes; within `VOCODER_WATCHDOG_MS` + 1 s `alive()` is false and the note says it stopped answering.
  - `finish_publishes_the_speech_in_flight`: 3 feeds then `finish()`; awaiting the handle yields all 3 frames.
  - `twenty_calls_leave_no_child`: 20 spawn-feed-finish cycles; after the drains are awaited no process with the fake's name remains.
  - `a_wrong_sized_feed_is_dropped_not_sent`: a feed of the wrong length sends nothing and does not panic.
- [ ] **Step 2: Run** `cargo test -p zeta-dsp vocoder::` — expected: FAIL, module missing.
- [ ] **Step 3: Implement `vocoder.rs`** by moving `Vocoder` out of `m17_decode.rs` and replacing its literals with `VocoderSpec` fields. Add the watchdog as a deadline in the reader task that resets on each read and is armed only while feeds are outstanding.
- [ ] **Step 4: Move M17 onto it.** `m17_decode.rs` builds a `VocoderSpec` from `codec(lsf)`: args `[mode, "-", "-"]`, `feed_bytes` 16 (3200) or 8 (1600), `feed_samples` 320, `rate_hz` 8000, `context` `"m17"`; `program(ctx, 0, C2DEC, true)` at stage start, failing the stage as today.
- [ ] **Step 5: Run** `cargo test -p zeta-dsp` — expected: PASS, with every test in `m17_decode.rs` unedited.
- [ ] **Step 6: Commit** `Vocoder: one seat for every voice stage, M17 on it, a watchdog for a program that stops answering`.

### Task 2: Optional handlers, `Install.script`, and `missing` on a mode

**Files:**
- Modify: `crates/zeta-core/src/modes/row.rs` (`Handler`, `Install`, `Decoder::handler_blocks`), `crates/zeta-core/src/modes/mod.rs` (`LoadedMode`, `ModeRegistry`), `crates/zeta-core/src/ops.rs` (`ModeInfo`), `crates/zeta/src/node_setup.rs` (`probe_handlers`), `crates/zeta/src/modes_check.rs`, `crates/zeta-dsp/src/vocoder.rs` (`program`), `crates/zeta/src/agents_md.rs` (the `modes_list` note), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: inline tests in `row.rs`, `mod.rs`, `node_setup.rs`; `crates/zeta/tests/modes_check.rs`

**Interfaces:**
- Produces:
  - `Handler { programs, install, #[serde(default)] optional: bool }`.
  - `Install { debian, fedora, brew, #[serde(default)] script: Option<String>, url }`.
  - `pub struct HandlerBlock<'a> { pub programs: &'a [String], pub install: &'a Install, pub optional: bool }`; `Decoder::handler_blocks(&self) -> Vec<HandlerBlock<'_>>` (a program row's block is never optional). Every caller is updated.
  - `LoadedMode.missing: Vec<String>` and `ModeInfo.missing: Vec<String>` (`#[serde(default)]`, always present in output): one line per optional handler not found, `"<program>: missing (<hint>; <url>) — voice silent"`.
  - `ModeRegistry::set_missing(&mut self, id: &str, line: String)`; a re-probe clears the list first.
  - Install hint order: `bash <script>` when `script` is set, else the first of apt, dnf, brew as today.

- [ ] **Step 1: Write the failing tests:**
  - `row.rs` `an_optional_handler_parses_and_defaults_false`: the M17 row has `optional == false`; the row edited to `"optional": true` parses with `true`; `"optional": 1` is broken naming `decoder.handlers[0].optional`.
  - `row.rs` `an_install_script_is_optional_in_a_row`: a row with `"script": "tools/install-vocoders.sh"` parses; one without has `None`.
  - `mod.rs` `a_missing_optional_handler_leaves_the_row_ready`: after `set_missing`, `state == Ready`, `reason == None`, `missing.len() == 1`; a required handler missing on the same row still gives `Unavailable`.
  - `node_setup.rs` `the_probe_reports_an_optional_handler_as_voice_silent`: the line is `zeta: handlers: zeta-no-such-handler: missing (bash tools/x.sh; https://example.invalid) — voice silent: <id>` and the row is `Ready`.
  - `tests/modes_check.rs` `an_optional_handler_missing_passes_and_strict_fails`: with a temp `--modes-dir` row (the M17 row copied under a new id with `optional: true` and a handler name that does not exist), `zeta modes check <id>` prints `<id>: ok` and writes no `checked`; with `--strict` it prints `<id>: fail: zeta-no-such-handler missing` and exits non-zero.
  - `vocoder.rs` `an_optional_handler_absent_is_none`: `program(ctx, 0, "x", false)` on such a row is `Ok(None)`.
- [ ] **Step 2: Run** the four crates' tests — expected: FAIL on the new names.
- [ ] **Step 3: Implement** the fields, `HandlerBlock`, `set_missing`, the probe branch, the hint order, the `modes check` rule (`--strict` fails a row with a non-empty `missing`; `checked` is recorded only when `missing` is empty), and the optional branch in `vocoder::program`.
- [ ] **Step 4: Update the `modes_list` note** in `agents_md.rs` with one sentence: a ready row may list `missing`, optional programs it runs without, each with what is lost. Regenerate `AGENTS.md` and the skill.
- [ ] **Step 5: Run** `cargo test -p zeta-core -p zeta -p zeta-dsp -- --skip ui_scenarios_pass_in_headless_chromium`, then `cargo run -q -p zeta -- agents-md --check` and `--skill --check` — expected: PASS, both checks clean. Fix any schema snapshot that pins `ModeInfo` in `zeta-ws`, `zeta-mcp` or `zeta-client`.
- [ ] **Step 6: Commit** `Modes: a handler may be optional (the row stays ready and lists what is missing), and an install hint may name a script`.

### Task 3: `zeta-mbe`, the install script and the legal note

**Files:**
- Create: `tools/vocoder/zeta-mbe.c`, `tools/install-vocoders.sh`, `docs/vocoders.md`
- Modify: `README.md` (one line linking `docs/vocoders.md`)
- Test: `crates/zeta/tests/install_vocoders.rs`

**Interfaces:**
- Produces: the program `zeta-mbe <codec>` exactly as spec section 5.2 states it: `<codec>` is `imbe7200x4400` (184 bytes in) or `ambe3600x2450` (96 bytes in); one byte per bit, 0 a certain zero, 255 a certain one; rows laid out as `mbelib` takes them (IMBE 8 rows of 23, AMBE 4 rows of 24, unused cells zero); 160 samples of s16le at 8 kHz out per frame, flushed per frame; `--version`; any other argument prints usage and exits 2; end of input exits 0.
- The script's options: `--yes`, `--src DIR`, `--prefix DIR` (default `$HOME/.local`), `--help`.

- [ ] **Step 1: Write the failing test** `install_vocoders.rs` (it never builds anything):
  - `help_exits_zero_and_names_the_options`: `bash tools/install-vocoders.sh --help` exits 0 and its output contains `--yes`, `--src`, `--prefix`.
  - `without_yes_it_stops_before_downloading`: run with stdin closed and `PATH` holding a fake `git` that touches a marker file; exit status non-zero, the notice was printed (contains "patent" and "GPL-2.0-or-later"), the marker does not exist.
  - `the_script_passes_bash_n`: `bash -n tools/install-vocoders.sh` exits 0.
- [ ] **Step 2: Run** `cargo test -p zeta --test install_vocoders` — expected: FAIL, script missing.
- [ ] **Step 3: Pin the library.** Choose the newest `mbelib-neo` release tag, resolve its commit with `git ls-remote https://github.com/arancormonk/mbelib-neo`, and record both in the script as `PIN_TAG` and `PIN_COMMIT`. Clone it into `scratch/upstream/` (untracked) and read its public header for the frame functions and their arguments at that commit.
- [ ] **Step 4: Write `zeta-mbe.c`.** Use the float frame functions and scale to s16 with a named gain constant and saturation. Use the library's soft-decision entry points if the pinned header has them; otherwise threshold each input byte at 128. Record which in a comment and in `--version`'s output. Set stdout unbuffered.
- [ ] **Step 5: Write `install-vocoders.sh`** as spec section 5.2's six steps. The self-test pipes 50 frames of zero bytes per codec into the built program and requires exactly 16 000 bytes back. Missing `git`, `cmake` or a C compiler is reported by Debian package name (`git`, `cmake`, `build-essential`) and exits before the notice.
- [ ] **Step 6: Write `docs/vocoders.md`** with the five points of spec section 5.9, the command, the options, and how the node finds the program (`PATH` or `ZETA_HANDLER_ZETA_MBE`). The script's notice says the same five points.
- [ ] **Step 7: Run the script end to end on the developer machine** (`bash tools/install-vocoders.sh --yes --prefix "$CLAUDE_JOB_DIR/tmp/mbe"` or any scratch prefix) — expected: the self-test line prints ok for both codecs. Record the pinned tag and the soft-decision finding in the commit body. Do not commit anything it built.
- [ ] **Step 8: Run** `cargo test -p zeta --test install_vocoders`, `bash scripts/check-doc-links.sh`, `bash scripts/public-check.sh` — expected: PASS, both clean.
- [ ] **Step 9: Commit** `Vocoders: zeta-mbe and the script that builds it against mbelib-neo on the operator's machine; the legal note`.

### Task 4: The 4-level FSK front end

**Files:**
- Create: `crates/zeta-dsp/src/fsk4.rs`
- Modify: `crates/zeta-dsp/src/lib.rs`, `crates/zeta-dsp/src/synth.rs` (a C4FM modulator beside `rrc_modulate_4fsk`)
- Test: inline in `fsk4.rs`

**Interfaces:**
- Produces:

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape { Rrc { alpha: f64 }, C4fm }

#[derive(Clone, Copy, Debug)]
pub struct Fsk4Params { pub symbol_rate_baud: u32, pub deviation_hz: f64, pub shape: Shape }

/// One received symbol. Dibit to level, for P25 and DMR alike: 01 → +3, 00 → +1, 10 → −1, 11 → −3.
#[derive(Clone, Copy, Debug)]
pub struct SoftDibit {
    pub dibit: u8,       // 0..=3, hard decision
    pub soft: [u8; 2],   // confidence that each bit is 1: 0 certain zero, 255 certain one
    pub at: f64,         // sample position in the input stream since the demodulator was made
}

pub struct Fsk4Demod { pub rate_hz: u64, /* private */ }
impl Fsk4Demod {
    pub fn new(rate_hz: u64, params: Fsk4Params) -> Result<Self, String>; // Err names the rate under 4 samples/symbol
    pub fn feed(&mut self, iq: &[Complex<f32>]) -> Vec<SoftDibit>;
    pub fn invert(&mut self, inverted: bool); // set by the framer once sync tells polarity
}

/// The first place `pattern` (dibits) matches with at most `max_errors` dibit errors,
/// in either polarity: (index, inverted).
pub fn find_sync(dibits: &[SoftDibit], pattern: &[u8], max_errors: usize) -> Option<(usize, bool)>;

pub fn sync_dibits(word: u64, bits: u32) -> Vec<u8>;
```

  In `synth.rs`: `pub fn c4fm_modulate(symbols: &[u8], baud: u32, deviation_hz: f64, rate_hz: u32) -> Vec<Complex<f32>>`, symbols as dibits in the mapping above. `rrc_modulate_4fsk` already exists for DMR (roll-off 0.2).

- [ ] **Step 1: Write the failing tests:**
  - `clean_symbols_round_trip_at_three_rates`: 2000 pseudo-random dibits through `rrc_modulate_4fsk` at 48 000, 50 000 and 96 000 Hz and back: at most 0 errors after the filter settles; the same through `c4fm_modulate` with `Shape::C4fm`.
  - `soft_bits_follow_confidence`: on a clean signal every soft bit is under 32 or over 223; at 6 dB SNR the wrong decisions are, on average, nearer 128 than the right ones.
  - `inverted_and_offset_signals_still_sync`: a 48-bit sync pattern in noise at 12 dB with the IQ conjugated and with a 1 kHz carrier offset: `find_sync` returns the right index, `inverted == true` for the conjugated case, and after `invert(true)` the dibits match.
  - `noise_at_10_db_keeps_errors_low`: dibit error rate under 2 % at 10 dB SNR in 12.5 kHz for both shapes.
  - `half_rate_works`: 2400 baud, `Rrc { alpha: 0.2 }`, clean round trip.
  - `a_low_rate_is_refused`: 16 000 Hz at 4800 baud errors with the rate and the baud in the message.
  - `sample_positions_advance_by_a_symbol`: consecutive `at` values differ by `rate / baud` within 5 %.
- [ ] **Step 2: Run** `cargo test -p zeta-dsp fsk4::` — expected: FAIL.
- [ ] **Step 3: Implement.** Discriminator from `nbfm`, a matched filter per shape, symbol timing and level tracking of the builder's choice (spec section 8), DC removal slow enough to survive a run of one symbol. M17 is not moved onto it in this task.
- [ ] **Step 4: Run** `cargo test -p zeta-dsp` — expected: PASS.
- [ ] **Step 5: Commit** `DSP: a 4-level FSK front end giving soft dibits, with a sync search in both polarities`.

### Task 5: P25 codes, frames and the `p25` synthesiser family

**Files:**
- Create: `crates/zeta-dsp/src/p25.rs`
- Modify: `crates/zeta-dsp/src/synth.rs` (`SynthSpec::P25`, its `validate`, `rate_hz`, `synthesise` arms), `crates/zeta-dsp/src/lib.rs`
- Test: inline in `p25.rs` and `synth.rs`

**Interfaces:**
- Consumes: `fsk4::{SoftDibit, find_sync, sync_dibits}`, `synth::c4fm_modulate`.
- Produces:

```rust
pub const P25_BAUD: u32 = 4800;
pub const P25_DEVIATION_HZ: f64 = 1800.0;
pub const P25_SYNC: u64 = 0x5575_F5FF_77FF;
pub const P25_ALGID_CLEAR: u8 = 0x80;
pub const IMBE_FEED_BYTES: usize = 184;
pub const P25_SUPERFRAME_MS: u64 = 360;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call { Group, Private }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkControl { pub call: Call, pub dst: u32, pub src: u32, pub mfid: u8, pub emergency: bool }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncSync { pub algid: u8, pub kid: u16 }

pub type ImbeSoft = [u8; IMBE_FEED_BYTES]; // mbelib's 8×23 rows, one soft byte per bit

#[derive(Clone, Debug, PartialEq)]
pub enum P25Unit {
    Header { enc: Option<EncSync>, dst: Option<u32> },
    Ldu1 { lc: Option<LinkControl>, voice: Box<[ImbeSoft; 9]>, errors: u32 },
    Ldu2 { enc: Option<EncSync>, voice: Box<[ImbeSoft; 9]>, errors: u32 },
    Terminator { lc: Option<LinkControl> },
    Trunking,
    Packet,
    Other(u8),
}

pub struct P25Framer { /* private */ }
impl P25Framer {
    pub fn new() -> Self;
    /// Units found, each with the sample position of its sync and its NAC.
    pub fn push(&mut self, dibits: &[SoftDibit]) -> Vec<(f64, u16, P25Unit)>;
    pub fn inverted(&self) -> Option<bool>; // known after the first sync
}

/// A clear or encrypted call as dibits: header, `superframes` × (LDU1, LDU2), terminator.
pub fn encode_call(nac: u16, lc: LinkControl, enc: EncSync, superframes: u32) -> Vec<u8>;
/// A control channel: `units` trunking signalling units with idle content.
pub fn encode_control(nac: u16, units: u32) -> Vec<u8>;
```

  `SynthSpec::P25 { nac: u16, src: u32, dst: u32, superframes: u32, #[serde(default)] encrypted: bool, #[serde(default)] control: bool }`, JSON family `"p25"`, rate 48 000 Hz, `superframes` 1..=20.

- [ ] **Step 1: Write the failing tests:**
  - `the_nid_round_trips_and_corrects_eleven_errors`: every DUID with NAC 291; 11 flipped bits still decode; 12 do not return a wrong NAC silently for more than 1 in 1000 random patterns.
  - `link_control_round_trips`: group (`dst` 65000, `src` 9990001) and private; the Reed-Solomon and Hamming layers correct their designed number of symbol errors.
  - `encryption_sync_round_trips`: `algid` `0x80` and `0xAA`, `kid` 7.
  - `an_imbe_frame_survives_its_own_coding`: an 88-bit vector encoded to 144 bits and laid into `ImbeSoft` reads back in the 8×23 layout with the Golay and Hamming rows in place and the pseudo-random mask applied as the standard states; 3 errors in a Golay row are corrected.
  - `status_symbols_are_removed`: a unit encoded with status symbols decodes; the same dibits with status symbols left in do not.
  - `a_call_decodes_to_its_units`: `encode_call(291, …, 3)` through `c4fm_modulate`, `Fsk4Demod` and `P25Framer` gives Header, 3 × (Ldu1 with the link control, Ldu2 with `algid` `0x80`), Terminator, all with NAC 291.
  - `synth.rs` `p25_family_validates`: the JSON `{"family":"p25","nac":291,"src":9990001,"dst":65000,"superframes":3}` validates; `superframes` 0 and 21, `nac` 4096 and `src` over 24 bits are refused naming the field.
- [ ] **Step 2: Run** `cargo test -p zeta-dsp p25` — expected: FAIL.
- [ ] **Step 3: Implement `p25.rs`.** The module's doc comment cites the TIA-102.BAAA clauses for sync, NID, status symbols, each data unit and the IMBE interleave. The voice payload in `encode_call` is one fixed 88-bit vector, encoded by this module's own codes: use the published IMBE silence vector if it can be confirmed from a public source, otherwise zeros, and say which in a comment.
- [ ] **Step 4: Wire `SynthSpec::P25`.**
- [ ] **Step 5: Run** `cargo test -p zeta-dsp` — expected: PASS.
- [ ] **Step 6: Commit** `P25: the phase 1 codes and data units, and the p25 synthesiser family`.

### Task 6: The `p25_decode` stage and row

**Files:**
- Create: `crates/zeta-dsp/src/p25_decode.rs`, `crates/zeta-dsp/src/voice_call.rs`, `modes/p25_decode.json`
- Modify: `crates/zeta-dsp/src/lib.rs` (module, `register_all`), `crates/zeta-core/src/modes/mod.rs` (embed the row), `crates/zeta-core/tests/modes.rs` and `crates/zeta/tests/modes_check.rs` (the pinned row lists), `.github/workflows/ci.yml` (a `modes check p25_decode` step without `--strict`)
- Test: inline in `p25_decode.rs` and `voice_call.rs`

**Interfaces:**
- Consumes: `vocoder::{Vocoder, VocoderEnv, VocoderSpec, program}`, `fsk4::*`, `p25::*`.
- Produces, in `voice_call.rs`, for task 8 as well:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Silent { NoVocoder, Encrypted, OtherCall, VocoderFailed }

/// The shared fields of a call event (spec 5.5); a stage adds its own to the map.
pub struct CallFields {
    pub protocol: &'static str,
    pub src: u32, pub dst: u32,
    pub call: &'static str,          // "group" | "private"
    pub encrypted: bool, pub emergency: bool,
    pub silent: Option<Silent>,      // None means audio is true
    pub frames: u32, pub errors: u32, pub eot: bool,
}
impl CallFields { pub fn to_json(&self) -> serde_json::Map<String, serde_json::Value>; } // includes "type": "call" and "audio"

pub fn channel_event(protocol: &'static str, control: bool) -> serde_json::Map<String, serde_json::Value>; // "type": "channel"
```

  Stage kind `p25_decode`, params `P25Params {}`, inputs `[Iq]`, outputs `[Event, Audio]`, shaped as `M17DecodeStage` is.

- [ ] **Step 1: Write the failing tests** (the stage tests use a fake `zeta-mbe` through the row's handler, as the M17 stage tests use `fake_c2dec`):
  - `a_call_gives_two_events_and_audio`: the task 5 call gives an opening event `{type: "call", protocol: "p25", nac: 291, src: 9990001, dst: 65000, call: "group", encrypted: false, audio: true, silent: null, eot: false}` and a closing one with `eot: true`, `frames: 54`; 54 audio frames of 960 samples arrive.
  - `joining_mid_call_opens_within_a_superframe`: the capture with its first 400 ms removed gives the opening event with a time inside `P25_SUPERFRAME_MS` of the first sync.
  - `an_encrypted_call_is_reported_and_silent`: `algid` `0xAA` gives both events with `encrypted: true`, `audio: false`, `silent: "encrypted"`, `algid: 170`, `kid` set; the fake vocoder was never started (it writes a marker when run).
  - `no_vocoder_gives_events_and_says_so`: with the handler absent, both events have `audio: false`, `silent: "no_vocoder"`; no audio frame; `job_context.p25.vocoder` names `zeta-mbe` and `tools/install-vocoders.sh`, written once across two calls.
  - `a_vocoder_installed_mid_job_is_used_from_the_next_call`: two calls, the fake created between them: the first is `no_vocoder`, the second has audio.
  - `a_dead_vocoder_marks_the_call`: a fake that exits after 10 frames: the closing event has `silent: "vocoder_failed"`; the next call has audio.
  - `a_control_channel_gives_one_channel_event`: `encode_control(291, 40)` gives exactly one `{type: "channel", protocol: "p25", nac: 291, control: true}` and `job_context.p25.trunking_units` counts them.
  - `a_call_cut_short_closes_after_its_timeout`, `a_call_1_khz_off_centre_decodes`, `other_signals_give_no_event` (noise, an NBFM tone, the `m17` family, a POCSAG burst), `a_lagged_input_closes_the_call`: Review Focus 2, 1, 4 and 5.
  - `params_are_strict`.
  - `voice_call.rs` `call_fields_serialise_as_the_spec_names_them`: the exact key set of spec 5.5's shared table.
- [ ] **Step 2: Run** `cargo test -p zeta-dsp p25_decode` — expected: FAIL.
- [ ] **Step 3: Implement the stage.** One open call at a time. The vocoder is looked up with `program(&ctx, 0, "zeta-mbe", false)` when a call opens and is spawned with args `["imbe7200x4400"]`, `feed_bytes` 184, `feed_samples` 160, `rate_hz` 8000, `context` `"p25"`. Each LDU feeds its nine frames with spans of 20 ms from the unit's time. A call is encrypted when the last encryption sync seen has `algid != P25_ALGID_CLEAR`; until one is seen a call that opened from a header uses the header's, and one joined mid-call is held silent for the first LDU1 and opens on LDU2. A channel event is emitted once per NAC when a unit decodes with no call open.
- [ ] **Step 4: Write `modes/p25_decode.json`** on `m17_decode.json`'s shape with spec 5.6's values: `family` `two_way`, `bandwidth_hz` 12500, `label` `P25`, `protocol` `p25`, the graph `channel → p25_decode`, the handler `{"programs": ["zeta-mbe"], "optional": true, "install": {"script": "tools/install-vocoders.sh", "url": "https://github.com/arancormonk/mbelib-neo"}}`, the event schema with every field of spec 5.5 plus `nac`, `mfid`, `algid`, `kid`, `control`, `type`, summary `"{src} > {dst}: {call} {silent?}"`, `position` all null, `identify` `{proof: "event", min_events: 1, dwell_ms: 2000, min_span_ms: 0, quick: null, priority: 0}`, `catalogue` `["project-25-p25"]`, `subject` keyed on `src` named `radios` with columns `dst`, `call`, `nac`, `encrypted`, `silent`, `access` `"restricted"`, `notes` one paragraph, `test` `{kind: "synth", family: "p25", nac: 291, src: 9990001, dst: 65000, superframes: 3, expect: {src: 9990001, dst: 65000, nac: 291}, within_ms: 5000}`.
- [ ] **Step 5: Add the CI step** `cargo run -q -p zeta -- modes check p25_decode` beside the existing strict steps, without `--strict`, and the pinned line `p25_decode: ok` in `crates/zeta/tests/modes_check.rs`.
- [ ] **Step 6: Run** `cargo test -p zeta-dsp -p zeta-core -p zeta -- --skip ui_scenarios_pass_in_headless_chromium`, then regenerate and check `AGENTS.md` and the skill (the row's `notes` appear in them) — expected: PASS and clean.
- [ ] **Step 7: Commit** `P25: the p25_decode stage and row; calls without a vocoder say why they are silent`.

### Task 7: DMR codes, bursts and the `dmr` synthesiser family

**Files:**
- Create: `crates/zeta-dsp/src/dmr.rs`
- Modify: `crates/zeta-dsp/src/synth.rs` (`SynthSpec::Dmr`), `crates/zeta-dsp/src/lib.rs`
- Test: inline in `dmr.rs` and `synth.rs`

**Interfaces:**
- Consumes: `fsk4::*`, `synth::rrc_modulate_4fsk`, `p25::Call` (move `Call` to `voice_call.rs` if task 6 has landed; otherwise define it there in this task and re-export).
- Produces:

```rust
pub const DMR_BAUD: u32 = 4800;
pub const DMR_DEVIATION_HZ: f64 = 1944.0;
pub const DMR_RRC_ALPHA: f64 = 0.2;
pub const AMBE_FEED_BYTES: usize = 96;
pub const DMR_SUPERFRAME_MS: u64 = 360;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin { Repeater, Handheld }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FullLc { pub call: Call, pub dst: u32, pub src: u32, pub fid: u8, pub encrypted: bool, pub emergency: bool }

pub type AmbeSoft = [u8; AMBE_FEED_BYTES]; // mbelib's 4×24 rows, one soft byte per bit

#[derive(Clone, Debug, PartialEq)]
pub enum DmrContent {
    VoiceHeader(Option<FullLc>),
    Voice { first: bool, lc: Option<FullLc>, privacy: bool, voice: Box<[AmbeSoft; 3]>, errors: u32 },
    Terminator(Option<FullLc>),
    PrivacyHeader,
    Control,   // CSBK
    Idle,
    Other(u8),
}

#[derive(Clone, Debug, PartialEq)]
pub struct DmrBurst { pub at: f64, pub slot: Option<u8>, pub origin: Origin, pub color_code: Option<u8>, pub content: DmrContent }

pub struct DmrFramer { /* private */ }
impl DmrFramer { pub fn new() -> Self; pub fn push(&mut self, dibits: &[SoftDibit]) -> Vec<DmrBurst>; }

pub struct SlotPlan { pub lc: Option<FullLc>, pub superframes: u32 } // None: the slot idles
/// A repeater's continuous downlink with CACH: both slots, each a call or idle.
pub fn encode_repeater(color_code: u8, slots: [SlotPlan; 2]) -> Vec<u8>;
/// A handheld's bursts: one slot in two, silence (no symbols) between.
pub fn encode_handheld(color_code: u8, lc: FullLc, superframes: u32) -> Vec<Option<u8>>;
```

  `SynthSpec::Dmr { color_code: u8, src: u32, dst: u32, superframes: u32, #[serde(default)] origin: "repeater"|"handheld" (default repeater), #[serde(default)] second: Option<{src, dst}>, #[serde(default)] encrypted: bool, #[serde(default)] idle: bool }`, JSON family `"dmr"`, rate 48 000 Hz, `superframes` 1..=20.

- [ ] **Step 1: Write the failing tests:**
  - `bptc_round_trips_and_corrects`: 96 bits through BPTC(196,96); one error per row and column is corrected.
  - `full_lc_round_trips_with_its_checksum_masks`: group and private; a header-masked LC does not validate as a terminator.
  - `the_emb_and_slot_type_round_trip`: colour codes 0..=15; their codes correct the designed errors.
  - `embedded_lc_reassembles_from_bursts_b_to_e`.
  - `the_cach_names_the_slot`: TACT round trip with its Hamming(7,4).
  - `an_ambe_frame_lands_in_mbelib_rows`: 72 transmitted bits deinterleave to the 4×24 layout the standard's tables give; row 0 is a valid Golay(24,12) word.
  - `a_repeater_call_decodes_to_its_bursts`: slot 1 a call of 3 superframes (colour code 5, `src` 9990001, `dst` 9990), slot 2 idle: VoiceHeader, 18 Voice bursts with `first` on every sixth, Terminator, all `slot == Some(1)`, `origin == Repeater`; slot 2 gives Idle bursts with `slot == Some(2)`.
  - `two_calls_are_told_apart_by_slot`.
  - `a_handheld_call_decodes_without_a_cach`: `origin == Handheld`, `slot == None`.
  - `a_mid_call_join_has_the_lc_within_a_superframe`.
  - `synth.rs` `dmr_family_validates`: colour code 16, `src` over 24 bits, `superframes` 0 and 21 refused naming the field.
- [ ] **Step 2: Run** `cargo test -p zeta-dsp dmr` — expected: FAIL.
- [ ] **Step 3: Implement `dmr.rs`.** The doc comment cites the ETSI TS 102 361-1 clauses for burst layout, sync patterns, CACH, EMB, slot type, BPTC, full LC and the vocoder socket's interleave. The voice payload is one fixed 72-bit transmitted frame: the published AMBE+2 silence frame if confirmed from a public source, otherwise a frame this module encodes from zeros, with a comment saying which. `encode_handheld`'s gaps are modulated as zero amplitude.
- [ ] **Step 4: Wire `SynthSpec::Dmr`.**
- [ ] **Step 5: Run** `cargo test -p zeta-dsp` — expected: PASS.
- [ ] **Step 6: Commit** `DMR: bursts, link control and the two-slot framing, and the dmr synthesiser family`.

### Task 8: The `dmr_decode` stage and row

**Files:**
- Create: `crates/zeta-dsp/src/dmr_decode.rs`, `modes/dmr_decode.json`
- Modify: as task 6's list, for `dmr_decode`
- Test: inline in `dmr_decode.rs`

**Interfaces:**
- Consumes: `vocoder::*`, `fsk4::*`, `dmr::*`, `voice_call::{CallFields, Silent, channel_event}`.
- Produces: stage kind `dmr_decode`, params `DmrParams {}`, inputs `[Iq]`, outputs `[Event, Audio]`.

- [ ] **Step 1: Write the failing tests:**
  - `a_repeater_call_gives_two_events_and_audio`: opening `{type: "call", protocol: "dmr", color_code: 5, slot: 1, origin: "repeater", src: 9990001, dst: 9990, call: "group", audio: true, silent: null}`, closing with `eot: true`, `frames: 54`; 54 audio frames.
  - `two_calls_at_once_report_both_and_play_the_first`: slot 1 starts one superframe before slot 2: four events; slot 2's have `audio: false`, `silent: "other_call"`; every audio frame's span lies inside slot 1's call. When slot 1's call ends first and slot 2's continues, slot 2's stays silent to its end.
  - `a_handheld_call_has_a_null_slot`: `origin: "handheld"`, `slot: null`, audio present.
  - `an_encrypted_call_is_reported_and_silent`: the service-options bit, and separately a privacy header, each give `encrypted: true`, `silent: "encrypted"`, no vocoder started.
  - `an_idle_repeater_gives_one_channel_event`: `{type: "channel", protocol: "dmr", color_code: 5, control: false}`, once.
  - `no_vocoder_gives_events_and_says_so`, `a_dead_vocoder_marks_the_call`, `joining_mid_call_opens_within_a_superframe`, `a_call_cut_short_closes_after_its_timeout`, `a_call_1_khz_off_centre_decodes`, `other_signals_give_no_event` (including the `p25` family), `a_lagged_input_closes_the_call`, `params_are_strict`: as task 6, with `job_context.dmr.vocoder`.
- [ ] **Step 2: Run** `cargo test -p zeta-dsp dmr_decode` — expected: FAIL.
- [ ] **Step 3: Implement the stage.** A call per slot key (`Some(1)`, `Some(2)`, `None`). The audio belongs to the call that opened first among those open; it is not handed over when that call ends. Vocoder args `["ambe3600x2450"]`, `feed_bytes` 96, `feed_samples` 160, `rate_hz` 8000, `context` `"dmr"`; each voice burst feeds three frames with 20 ms spans from the burst's time.
- [ ] **Step 4: Write `modes/dmr_decode.json`** as task 6 step 4 with `label` `DMR`, `protocol` `dmr`, the fields `color_code`, `slot`, `origin`, `fid` in place of P25's, `catalogue` `["digital-mobile-radio-dmr"]`, subject columns `dst`, `call`, `color_code`, `slot`, `encrypted`, `silent`, and `test` `{kind: "synth", family: "dmr", color_code: 5, src: 9990001, dst: 9990, superframes: 3, expect: {src: 9990001, dst: 9990, color_code: 5}, within_ms: 5000}`.
- [ ] **Step 5: Add the CI step and pinned line** for `dmr_decode`, as task 6 step 5.
- [ ] **Step 6: Run** the three crates' tests and the two guide checks, regenerating — expected: PASS and clean.
- [ ] **Step 7: Commit** `DMR: the dmr_decode stage and row; two slots reported, one heard`.

### Task 9: Field craft, TODO, UAT, the oracle run and the gate

**Files:**
- Modify: `docs/agents/field-craft.md` ("Decoding digital voice", the third-party paragraph, the `modes check` family list), `TODO.md`, `UAT.md`, `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md`, `README.md` (the modes list, if it names decoders), `AGENTS.md`, `skills/zetasdr/SKILL.md`

- [ ] **Step 1: Rewrite "Decoding digital voice"** to cover the three rows: the start command for each on the measured centre with `--bandwidth-hz 12500`; the two call events and the channel event with their fields; `audio` and the four `silent` values; that P25 and DMR run with no vocoder and M17 does not; `zeta-mbe`, `tools/install-vocoders.sh` and `docs/vocoders.md`; one audio call at a time on DMR; encrypted calls reported, never decoded, no keys accepted. Replace "DMR and D-STAR have no decoder row" with "D-STAR, NXDN and System Fusion have no decoder row".
- [ ] **Step 2: Add one sentence to the third-party paragraph**: the P25 and DMR rows decode clear speech on public protocols; whose traffic it is, and whether listening to it is lawful where the node sits, is the operator's responsibility, said once when a request goes that way.
- [ ] **Step 3: `TODO.md`.** Replace the gist "Build: an install script for the AMBE/IMBE vocoder libraries" with two lines: done in plan 38, and the kept `blip25-vocoder` paragraph. Strike "DMR voice" and "A `vocoder` handler". In "Architecture: a mode registry…", "Time", drop DMR from the modes that need true block timestamps and say why (slot timing is in the signal). Add to "Identify dwell cost": `p25_decode` and `dmr_decode` each dwell 2 s on any 12.5 kHz FM or FSK signal, on every host. Add to "Access: ACLs on restricted rows": the marker reaches events only; the audio and transcripts of a restricted row carry nothing. Add a new gist "Digital voice: the next rows" listing D-STAR (`ambe3600x2400` in `zeta-mbe`), NXDN, System Fusion, dPMR and P25 phase 2 as a framing module and a row each; a slot choice for DMR audio; `c2dec` optional for M17; trunking.
- [ ] **Step 4: `UAT.md`.** Add "Plan 38" with the six checks of spec section 7, worded for the field host, check 2 as: the operator's own handheld in the 70 cm amateur band into a dummy load or at low power.
- [ ] **Step 5: The oracle run, on the developer machine.** Write the synthesised captures (`zeta` has a synth-to-file path used by `modes check`; if none is exposed, a `test-fixtures` example beside `gen_aprs`) for the `p25` family and for `dmr` in repeater, two-call and handheld forms as WAV discriminator audio or IQ as `dsd-fme` accepts, run `dsd-fme` on each, and confirm it reports NAC `123` hex, talkgroup 65000 and source 9990001 for P25, and colour code 5, target 9990 and source 9990001 with the right slot for DMR. If it disagrees, the decoder and synthesiser share an error: stop, fix it in the task that owns it, and rerun. `dsd-fme` is installed outside the repository; say in the report how.
- [ ] **Step 6: Record the outcome** in the plan series: a "Plan 38 status" entry with the date, the pinned `mbelib-neo` tag, whether soft decisions are used, the oracle run's result line by line, and what stays open (the UAT checks).
- [ ] **Step 7: Regenerate** `AGENTS.md` and the skill.
- [ ] **Step 8: Run the full gate** from `CLAUDE.md`: fmt per crate, clippy, `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`, `cargo test -p zeta --test ui -- --nocapture` (no `ui e2e skipped` line), both `agents-md` checks, `bash scripts/public-check.sh`, `bash scripts/check-doc-links.sh`. Then `cargo run -q -p zeta -- modes check` and confirm `p25_decode: ok`, `dmr_decode: ok`, `m17_decode: ok`. Expected: all clean.
- [ ] **Step 9: Commit** `Plan 38: field craft, TODO, UAT and the plan series for digital voice`.

## Calls made in this plan (the builder's, on the spec's silence)

1. **A shared `voice_call.rs`** holds the call event fields and `Silent`, so the two stages cannot drift and later rows reuse it.
2. **Audio is not handed to the second DMR call when the first ends.** A call that began silent stays silent, so a listener never joins mid-sentence and a transcript never starts mid-call.
3. **A P25 call joined mid-call opens on LDU2**, not LDU1, so its encryption state is known before any frame reaches the vocoder.
4. **CI checks the two rows without `--strict`**, since it has no vocoder; `--strict` on the field host is their full gate.
5. **No new end-to-end page scenario.** The suite runs near its deadline, and no page code changes; the page is checked in UAT.
6. **The synthesised voice payload may be zeros** if a published silence frame cannot be confirmed. No codec runs in CI, so nothing there depends on it.
7. **Test identities** use 999-prefixed radio IDs, which no registry assigns.

## Self-review notes

- Spec coverage: 5.1 → task 1; 5.2 and 5.9 → task 3; 5.3 → task 2 (`silent` in tasks 6 and 8); 5.4 → task 4; 5.5 → tasks 5 to 8; 5.6 → tasks 6 and 8; 5.7 → each task's tests, the oracle in task 9; 5.8 → tasks 6, 8 and 9; section 7's live checks → `UAT.md` in task 9.
- Names used across tasks: `VocoderSpec.context`, `vocoder::program`, `HandlerBlock`, `ModeInfo.missing`, `SoftDibit`, `find_sync`, `ImbeSoft`, `AmbeSoft`, `Call`, `Silent`, `CallFields`.
- The live checks are not part of this plan's gate: they need the field host and on-air traffic.
