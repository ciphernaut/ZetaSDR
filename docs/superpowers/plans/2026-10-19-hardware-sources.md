# ZetaSDR Hardware and Foreign Sources Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Real and foreign signal sources behind the unchanged port boundary: a KiwiSDR websocket client (remote, push), any SoapySDR device with HackRF as the reference (local, pull), and external-process stages speaking the typed envelope from a Python SDK; with jobs that expand against the source's actual sample rate, so a 12 kHz Kiwi, a 2.048 MS/s RTL and an 8 MS/s HackRF all run the same job kinds.

**Architecture:** Six carry-overs from plan 2's final review land first because every later source depends on them: `JobKind::expand` learns the source rate from the port's tuned band and validates the job band against the channel rate it picks; `fm_demod` takes a deviation; `measure` and `classify` honour the requested band; DSP stages reset their state when their input lags; a table test runs every job kind against the CLI's real registry; and an AX.25 frame made by Dire Wolf proves the deframer against an implementation we did not write. Then one additive core change (`Port::fault`, supervised by the job runtime) lets a source fail the jobs that read it. Three new plugin crates follow, none of which `zeta-core` depends on: `zeta-source-kiwi` (tokio-tungstenite client, fake Kiwi server for tests), `zeta-source-soapy` (all device logic behind a `SoapyDevice` trait with a mock; the `soapysdr` binding is an optional `driver` feature, so the default build never links libSoapySDR) and `zeta-plugin` (a stdio wire protocol whose frame headers are plan 3's `WsMessage::Frame` JSON, an `ExternalStage` host implementing `Stage`, and a fake plugin binary for failure tests). `sdk/python/zeta_plugin` is the stdlib-only Python side, `examples/python/gain.py` its example. The `zeta` CLI funnels every `--source-*` flag into one `SourceSpec` and registers plugins from `--plugin`.

**Tech Stack:** Rust 1.97 and the existing tokio/serde/schemars/clap stack; `tokio-tungstenite` 0.30.0 (`default-features = false`, features `connect`; ws:// only, no TLS) and `futures-util` 0.3 (`default-features = false`, features `sink`, `std`) in `zeta-source-kiwi`; `soapysdr` 0.5.1 (optional, feature `driver`; links the system libSoapySDR 0.8.1 and needs `libsoapysdr-dev`, which the dev box does not have yet); tokio `process` and `io-util` in `zeta-plugin`; Python 3 standard library only (`json`, `struct`, `sys`, `os`). Dire Wolf 1.7 (`/usr/bin/gen_packets`, `/usr/bin/atest`) produced the external AX.25 vector once; it is not a build or test dependency.

**Spec:** `docs/superpowers/specs/2026-10-18-zetasdr-design.md` (sections 2 anti-goals, 3 footprints, 4.1, 4.3, 4.5, 4.8, 5.1, 8 items 1 and 7)

## Base and ordering

Branch `hardware-sources` from `main` **after** plan 3 (`transports-agent`) has merged; do not start from `9b1cc01` and do not touch `.claude/worktrees/*`. This plan edits files plan 3 also edits: `crates/zeta/src/{cli.rs,main.rs,node_setup.rs,transport.rs,ask.rs}`, `AGENTS.md`, `skills/zetasdr/SKILL.md`, `README.md`. Read them as merged before Task 1. `AGENTS.md` and `skills/zetasdr/SKILL.md` are never hand-edited: regenerate with `cargo run -q -p zeta -- agents-md > AGENTS.md && cargo run -q -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md` whenever a task changes the schema or the guide text.

Interfaces this plan consumes, as found on `main`:

- `zeta_core::port::{Port, Tuning, IqBlock, IqReceiver, PortError}`; `Port` has `id`, `capabilities`, `tune`, `current`, `subscribe`. The file source's `tune` accepts any sub-band of the recording and answers with the recording's fixed band (`bandwidth_hz` = sample rate); its producer exits when it has no subscribers or is dropped; `t0` is `start + samples_sent / rate` and keeps rising across `repeat`.
- `zeta_core::ops::{Op, OpResult, OpError, JobKind, FrameSummary, SourceInfo, suggest}`; `JobKind::expand(&self, &SourceId, Tuning) -> Graph` hard-codes `out_rate_hz: 48_000`.
- `zeta_dsp::channel::{extract_offset, extract_offset_streaming, ChannelState, ChannelStage}`, `zeta_dsp::{run_map, frame_span, lagged_frames}` (`run_map` drops lag silently apart from the `LAGGED_FRAMES` counter), `zeta_dsp::identify::DecodeToIdentify` (finds the strongest band with the block centre taken as `rate/2`).
- Plan 3's wire, `zeta_ws::wire::{WsMessage, WsFrame, ws_frame}`: `{"type":"frame", stream, kind, source, job, channel, start_ns, end_ns, payload}`, `{"type":"lagged","missed":N}`, `{"type":"closed","reason":"..."}`; `WsFrame` has exactly the fields of `zeta_core::ops::FrameSummary`. If the plan-3 merge already replaced `WsFrame` by `FrameSummary` (its recommended convergence), use that; the JSON is identical and Task 12 pins it.
- `node_setup::open(state_dir, source_files: &[(String, PathBuf)], resume) -> Result<Node, String>`, called from `main.rs` and `ask.rs`; `transport::dispatch` rejects `--source-file`/`--state-dir` with `--node`.

## KiwiSDR protocol, as observed

Taken from the kiwiclient reference (`kiwi/client.py`, `kiwirecorder.py`, client `VERSION = 'v1.11'`) and confirmed against the acceptance receiver `kiwisdr.local:8073` (KiwiSDR 2 v1.837, `rx8_wf2`, 8 users max) on 2026-10-19. The implementer copies these strings; nothing here is to be guessed.

- **URL**: `ws://HOST:PORT/<TS>/SND`, where `<TS>` is decimal `unix_seconds & 0xffffffff` (the reference adds its pid and connection index to keep it unique; use `unix_seconds + pid`). Both `/<TS>/SND` (what the reference sends) and the older `/kiwi/<TS>/SND` answered `101 Switching Protocols` on the live Kiwi; use `/<TS>/SND`. Wideband is `/wb/<TS>/SND` and is not built (see self-review).
- **Client to Kiwi**, text frames, exact:
  1. on open: `SET auth t=kiwi p=<PASSWORD>` (empty password: `SET auth t=kiwi p=`).
  2. on `MSG sample_rate=…`: `SET squelch=0 max=0`, `SET genattn=0`, `SET gen=0 mix=-1`, `SET ident_user=zetasdr`, then the tuning commands below if a tuning is pending, then `SET keepalive`.
  3. tuning: `SET mod=iq low_cut=-<H> high_cut=<H> freq=<KHZ>` with `H = ceil(bandwidth_hz / 2)` clamped to `1..=6000` and `<KHZ>` the centre in kHz with exactly three decimals, formatted from integers (`7_100_000` → `7100.000`, `7_100_250` → `7100.250`); then `SET agc=1 hang=0 thresh=-100 slope=6 decay=1000 manGain=50`, then `SET compression=0`.
  4. on `MSG … audio_rate=<N>`: `SET AR OK in=<N> out=44100`.
  5. `SET keepalive` once per second for the life of the connection.
- **Kiwi to client**: every message is a websocket frame (binary on the live Kiwi; accept text too) whose first three bytes are a tag.
  - `MSG`: bytes 3.. are ASCII, starting with one space, then space-separated `name` or `name=value` pairs; several pairs can share one message (live: `MSG version_maj=1 version_min=837 debian_ver=11 model=2 platform=0 hw=1 ext_clk=0 freq_offset=0.000 abyy=A26 dx_db_name=dx has_attn=1`). Observed order after auth: `sample_rate=11998.902088` (first, before anything is sent but auth), `client_public_ip=…`, `rx_chans=8`, `chan_no_pwd=0`, `chan_no_pwd_true=0`, `is_local=5,1,0`, `max_camp=4`, `badp=0`, the version line, `load_cfg=…` (URL-encoded JSON, ignore), `load_dxcfg=…`, `load_dxcomm_cfg=…`, `cfg_loaded`, `center_freq=15000000 bandwidth=30000000 adc_clk_nom=66666600`, `audio_init=1 audio_rate=12000`, then others. Meaning: `sample_rate` is the true IQ rate as a decimal float; `audio_rate` is the nominal integer rate; `badp=0` is **success**; `badp=1..7` are refusals (1 bad password or all channels busy that don't require a password; 2 still determining local interface address, try again in a few moments; 3 admin connection not allowed from this ip address; 4 no admin password set, can only connect from the Kiwi's local network; 5 no multiple connections from the same ip address; 6 database update in progress, try again after one minute; 7 another admin connection already open); `too_busy=<N>` all N client slots taken; `down` server is down; `redirect=<url-encoded url>`; `freq_offset=<kHz float>` (non-zero on transverter Kiwis); everything else is ignored.
  - `SND`, byte offsets from the start of the websocket payload: `[0..3]` = `SND`; `[3]` flags (`SND_FLAG_ADC_OVFL = 0x02`, `SND_FLAG_STEREO = 0x08`, `SND_FLAG_COMPRESSED = 0x10`, `SND_FLAG_LITTLE_ENDIAN = 0x80`; live frames carry 0x08 or 0x0a, the first 0x0f); `[4..8]` sequence number, `u32` little-endian, +1 per frame; `[8..10]` S-meter, `u16` **big-endian**, `rssi_dbm = 0.1 · smeter − 127`; `[10..]` data. In IQ mode (`STEREO` set, never compressed) data begins with 10 GPS bytes, `u8 last_gps_solution, u8 dummy, u32 LE gpssec, u32 LE gpsnsec`, followed by interleaved `I, Q` as **big-endian** `i16` (the reference reads them with `'>h'` unless `SND_FLAG_LITTLE_ENDIAN` is set; the brief's "little-endian" was wrong). Live frames are 2068 bytes: 10 header + 10 GPS + 2048 sample bytes = 512 complex samples, about 23.4 frames per second.
  - Real bytes of live frame 2 (first 30 bytes): `534e44 0a 02000000 0234 ff00c37102008b13561f 017604b9060afb72fed2`: flags `0x0a` (stereo, ADC overflow), seq 2, smeter 564 (−70.6 dBm), GPS `ff 00`, gpssec 160195, gpsnsec 525734795, samples (374, 1209), (1546, −1166), then (−302, …).

---

## Global Constraints

- `zeta-core` changes only where this plan lists them: `JobKind::expand`'s signature and graphs, `ops::channel_rate_hz`, `Op::JobStart.deviation_hz`, `classify::Band` and the `band` argument of `Classifier::classify`/`ClassifierSet::run_all`, the `Port::fault` default method and the `Port` doc contract, `SourceInfo.fault`, the supervised IQ forwarder in `node/launch.rs`, and the banned-deps list. No new `zeta-core` dependency.
- No driver or transport crate in `zeta-core` (design anti-goal 1): the `no_privileged_deps` test gains `"tungstenite"` (`"soapysdr"` is already banned). `soapysdr` appears only in `zeta-source-soapy` behind its `driver` feature; `tokio-tungstenite` only in `zeta-source-kiwi`.
- Every source is a plugin crate registered in `crates/zeta/src/node_setup.rs`; every external stage is registered there from `--plugin`. Nothing in `zeta-core` or `zeta-dsp` knows a source or plugin by name.
- Port contract (doc comment on `Port`, Task 7): `tune` and `current` return the capture band, whose `bandwidth_hz` equals the complex sample rate of the blocks the port emits; a port's producer exits when it has no subscribers or is dropped; consecutive blocks have `t0` rising by exactly `samples · 1e9 / sample_rate_hz` (integer nanoseconds) unless samples were really lost, in which case `t0` jumps forward and never backward; unrecoverable failures are reported through `Port::fault`, never by panicking or by closing the stream silently.
- Stage contract (plan 1): `Err(Cancelled)` on cancellation, `Ok(())` when inputs close, `Err(Failed(msg))` otherwise, with the message naming the field or the plugin and the value. External stages obey it too: a plugin process is the stage.
- Every frequency, bandwidth and rate is an integer `*_hz`; times `*_ns`; durations `*_ms`; schemas strict (`additionalProperties: false`, every field required, nullable where optional); plugin `params_schema` must be strict too or the plugin is refused.
- Untrusted input never panics the node: Kiwi frames and plugin output are parsed with explicit length checks and size limits (plugin header ≤ 65 536 bytes, body ≤ 16 777 216 bytes, control line ≤ 65 536 bytes); a malformed message is an error naming what was wrong.
- Secrets: a Kiwi password never appears in `Debug`, error messages, `source list` or logs (`p=<redacted>`).
- CI needs no hardware, network or Python: the live Kiwi test and the HackRF test are `#[ignore]` and print `skipped: …` when their target is absent; Python tests return early with `skipped: python3 not found`; fake servers bind `127.0.0.1:0`. `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass on the dev box as it is (no `libsoapysdr-dev`).
- Build the minimum (ponytail): reuse before stdlib before dependency before new code.

## Review Focus

1. **Kiwi `too_busy`.** At `tune` (job start) it must be a port error naming `too_busy` and the slot count, with no job recorded; on a reconnect mid-job it must fail the running job with that reason. Tests `too_busy_at_tune_is_port_error_naming_slots` and `too_busy_on_reconnect_fails_job` in Task 9.
2. **Kiwi server drops the socket mid-stream.** The port reconnects with bounded backoff, re-sends auth and tuning, and `t0` never goes backward; when the attempts are exhausted the job fails naming the attempts. Tests `drop_mid_stream_reconnects_and_t0_stays_monotonic` and `reconnect_exhausted_fails_job_with_reason` in Task 9.
3. **Soapy device unplugged mid-stream.** A fatal read error or one second without samples faults the port and fails the job with the driver's message; the reader thread exits. Test `unplug_mid_stream_fails_job` in Task 11.
4. **Plugin process crashes mid-frame.** The stage fails with a message naming the plugin, how many body bytes arrived of how many promised and the exit status; nothing partial is published. Test `plugin_crash_mid_frame_fails_stage_with_byte_counts` in Task 13.
5. **Plugin sends a frame of the wrong kind.** The frame is not published; the stage fails naming the kind sent and the kind declared. Test `plugin_wrong_kind_frame_fails_stage` in Task 13.
6. **A `job_start` whose band exceeds what the Kiwi can deliver.** It is `OutOfRange { field: "bandwidth_hz", max: 12000 }` before any websocket is opened, and `expand` refuses a band wider than the channel rate as a backstop. Tests `job_start_wider_than_kiwi_is_out_of_range` in Task 9 and `expand_rejects_band_wider_than_channel_rate` in Task 1.

---

### Task 1: Rate-aware job expansion and band validation (carry-overs a, e)

**Files:**
- Modify: `crates/zeta-core/src/ops.rs` (`channel_rate_hz`, `JobKind::expand`, `Op::JobStart` arm)
- Test: `crates/zeta-core/tests/ops.rs` (update the three `expand` tests, add new), `crates/zeta-dsp/tests/jobs.rs`

**Interfaces:**
- Produces: `pub const CHANNEL_RATE_MIN_HZ: u64 = 48_000;` and `pub fn channel_rate_hz(source_rate_hz: u64, bandwidth_hz: u64) -> u64`: with `target = max(bandwidth_hz, CHANNEL_RATE_MIN_HZ)`, returns `source_rate_hz` when `target >= source_rate_hz`, else `source_rate_hz / d` for the largest integer `d` in `1..=source_rate_hz / target` that divides `source_rate_hz` exactly. Pure integer arithmetic (core stays DSP-free); the narrowest exact rate that holds the band, which is what lets a Pi ship narrowband (design 3).
- Produces: `pub fn expand(&self, source: &SourceId, tuning: Tuning, source_rate_hz: Hz) -> Result<Graph, OpError>`. With `out = channel_rate_hz(source_rate_hz, tuning.bandwidth_hz)`: `fm_demod` = `channel {center_hz, bandwidth_hz, out_rate_hz: out}` → `nbfm_demod {deviation_hz: 2500, audio_rate_hz: out}` (Task 2 makes the deviation a parameter); `aprs_decode` = `channel {…, out}` → `nbfm_demod {3000, out}` → `aprs_decode {}`; `passthrough`, `spectrum`, `measure` unchanged here (Task 3 changes `measure`). Before building any graph with a `channel` node: `source_rate_hz == 0` → `OpError::Other { message: "source rate unknown: tune returned bandwidth_hz 0" }`; `tuning.bandwidth_hz > out` → `OpError::OutOfRange { field: "bandwidth_hz", value, min: 1, max: out, suggested: None }` (carry-over e).
- `Op::JobStart` now runs: `checked_port` → `node.tune_if_unheld(port, tuning)` (the granted capture band; `bandwidth_hz` is the source rate by the port contract) → `kind.expand(&source, tuning, granted.bandwidth_hz)?` → `node.job_start(…)`. A conflict from `tune_if_unheld` is the same `Conflict` `job_start` would have returned. Ports must make a repeated `tune` with an unchanged band a no-op (Tasks 9 and 11 test it), since `job_start` tunes again when the source is unheld.
- `JobKind` doc: `fm_demod`, `aprs_decode` (and, after Task 3, `measure`) channelise the job band at the narrowest exact divisor of the source rate that is at least 48 kHz and at least the band; `spectrum` covers the whole capture band.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/zeta-core/tests/ops.rs
#[test] fn channel_rate_table() {
    for (rate, bw, want) in [(48_000, 12_500, 48_000), (12_000, 10_000, 12_000), (1_000_000, 12_500, 50_000),
        (2_048_000, 12_500, 51_200), (2_400_000, 12_500, 48_000), (8_000_000, 12_500, 50_000),
        (10_000_000, 12_500, 50_000), (20_000_000, 200_000, 200_000), (12_007, 5_000, 12_007)] {
        assert_eq!(channel_rate_hz(rate, bw), want, "{rate} {bw}");
    }
}
#[test] fn expand_uses_source_rate_for_channel_and_audio() { /* FmDemod, tuning (144_800_000, 12_500), rate 2_048_000 -> channel.out_rate_hz 51_200 and nbfm.audio_rate_hz 51_200; rate 12_000 bw 10_000 -> both 12_000 */ }
#[test] fn expand_rejects_band_wider_than_channel_rate() { /* FmDemod, bw 20_000, rate 12_000 -> Err(OutOfRange{field:"bandwidth_hz", value:20000, min:1, max:12000, suggested:None}) */ }   // Review Focus 6
// crates/zeta-dsp/tests/jobs.rs (fixtures via nbfm_modulate/aprs_packet_iq + write_fixture)
#[tokio::test] async fn fm_demod_job_on_12_khz_source() { /* 1 kHz tone, nbfm_modulate(audio 48k -> iq 12_000, dev 2500, offset 0), centre 7.1 MHz; JobStart fm_demod bw 10_000 -> Running after 500 ms; audio frames sample_rate_hz 12_000; DFT peak at 1 kHz */ }
#[tokio::test] async fn fm_demod_job_at_2_048_msps() { /* 0.5 s at 2_048_000, tone FM at +100 kHz; JobStart centre+100 kHz bw 12_500 -> audio sample_rate_hz 51_200, DFT peak at 1 kHz */ }
#[tokio::test] async fn aprs_decode_job_decodes_at_12_khz() { /* aprs_packet_iq("VK3ABC","APRS",">hi", 12_000, 0.0) repeated; first Event within 3 s has src "VK3ABC" */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-core --test ops expand channel_rate` (compile errors), `cargo test -p zeta-dsp --features test-fixtures --test jobs 12_khz 2_048`.
- [ ] **Step 3: Implement**; update every `expand` call site (ops.rs and the three tests at `tests/ops.rs` ~793/814/843, which pass `Hz(48_000)` and `.unwrap()`). If `aprs_decode_job_decodes_at_12_khz` fails because `Afsk1200Demod` cannot hold lock at 10 samples per bit, fix the demodulator (it is the only rate-dependent part); do not insert an upsampler into the graph, and report the change.
- [ ] **Step 4: Run tests** — `cargo test --workspace --features zeta-dsp/test-fixtures` PASS; regenerate AGENTS.md if the `JobKind` doc changed; drift check PASS.
- [ ] **Step 5: Commit** — `git commit -am "feat(core): jobs expand against the source rate and refuse bands wider than the channel"`

---

### Task 2: FM deviation as a job parameter (carry-over d)

**Files:**
- Modify: `crates/zeta-core/src/ops.rs` (`Op::JobStart`, `expand`), `crates/zeta/src/cli.rs` (`--deviation-hz`), every `Op::JobStart { … }` literal (`crates/zeta-core/tests/ops.rs`, `crates/zeta-dsp/tests/{jobs.rs,tail.rs}`, `crates/zeta/src/main.rs`, plus any plan-3 file that builds one)
- Test: `crates/zeta-core/tests/ops.rs`, `crates/zeta/tests/cli.rs`

**Interfaces:**
- Produces: `Op::JobStart { kind, source, center_hz, bandwidth_hz, #[schemars(required)] deviation_hz: Option<Hz> }`; `null` means the kind's default. `pub const FM_DEVIATION_DEFAULT_HZ: u64 = 5_000;` Accepted range `100..=75_000` → otherwise `OutOfRange { field: "deviation_hz", min: 100, max: 75_000, suggested: suggest(..) }`. A non-null value with any kind but `fm_demod` → `OpError::Other { message: "deviation_hz applies only to kind fm_demod; pass null" }` (APRS deviation is fixed by the protocol at 3000 Hz).
- `expand(&self, source, tuning, source_rate_hz: Hz, deviation_hz: Option<Hz>) -> Result<Graph, OpError>`; `fm_demod`'s `nbfm_demod.deviation_hz` = the value or 5000.
- CLI: `job start --deviation-hz HZ` (optional; absent → `null`). The JSON example in AGENTS.md must render `"deviation_hz":null`; extend `agents_md.rs`'s example table if it does not already render nullable fields as `null`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn fm_demod_default_deviation_is_5000() { /* expand(FmDemod, .., Hz(48_000), None) -> nbfm node deviation_hz 5000 */ }
#[test] fn deviation_param_reaches_nbfm_node() { /* Some(Hz(2_500)) -> 2500 */ }
#[tokio::test] async fn deviation_out_of_range_names_field() { /* JobStart fm_demod deviation Some(50) -> OutOfRange field "deviation_hz", suggested Some(5000) */ }
#[tokio::test] async fn deviation_on_other_kind_rejected() { /* measure with Some(5000) -> Other message starts "deviation_hz applies only" */ }
#[test] fn job_start_schema_requires_nullable_deviation() { /* schema op JobStart: "deviation_hz" in required, type allows null */ }
// cli.rs
#[test] fn job_start_accepts_deviation_flag() { /* --deviation-hz 3000 on fm_demod with the tone fixture -> exit 0; job's graph (graph get) nbfm deviation_hz 3000 */ }
```

- [ ] **Step 2: Run to verify they fail**.
- [ ] **Step 3: Implement**; add `deviation_hz: None` to every existing literal.
- [ ] **Step 4: Run tests** — PASS; regenerate AGENTS.md and the skill; drift check PASS.
- [ ] **Step 5: Commit** — `git commit -am "feat(core): fm_demod deviation is a job parameter, default 5000 Hz"`

---

### Task 3: `measure` and `classify` honour the requested band (carry-over b)

**Files:**
- Modify: `crates/zeta-core/src/classify.rs` (`Band`, trait and `run_all` signatures, `StubClassifier`), `crates/zeta-core/src/ops.rs` (`classify` passes the band; `JobKind::Measure` through `channel`; doc comments on `Op::Classify` and `JobKind`), `crates/zeta-dsp/src/{measure.rs,identify.rs}`
- Test: `crates/zeta-dsp/tests/classify.rs`, `crates/zeta-dsp/tests/jobs.rs`, unit tests in `identify.rs`, `crates/zeta-core/tests/ops.rs`

**Interfaces:**
- Produces (`zeta_core::classify`): `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct Band { pub offset_hz: i64, pub bandwidth_hz: Hz }` (offset of the band centre from the block centre; negative below) with `pub fn whole(sample_rate_hz: Hz) -> Band { Band { offset_hz: 0, bandwidth_hz: sample_rate_hz } }` and `pub fn is_whole(&self, sample_rate_hz: Hz) -> bool` (offset 0 and bandwidth ≥ rate). Trait: `async fn classify(&self, iq: &[Complex<f32>], sample_rate_hz: Hz, band: Band, features: Option<&FeatureRecord>) -> Vec<Label>`; `run_all(iq, sample_rate_hz, band, features)`.
- `ops::classify`: `band = Band { offset_hz: sel.center_hz as i64 - ring_center as i64, bandwidth_hz: sel.bandwidth_hz }` (`check_band` already proved it lies inside the ring). `Op::Classify` doc: "Run every registered classifier over the selection's band and span of the source ring."
- `Layer1Classifier`: whole band → as today; otherwise `extract_offset(iq, rate, band.offset_hz, bw, channel_rate_hz(rate, bw))` then `measure`/`layer1_label` on the channel.
- `DecodeToIdentify`: whole band → today's strongest-band search; otherwise decode exactly the requested band, `bw = band.bandwidth_hz.max(12_500).min(out)` with `out = channel_rate_hz(rate, band.bandwidth_hz)`, audio at `out`. Delete `audio_rate()` and the "`center_hz` measured with the block centre at `rate/2`" doc; replace `MIN_AUDIO_HZ = 24_000` by `9_600` so a 12 kHz Kiwi channel is decoded (Task 1 proved AFSK at 12 kHz).
- `JobKind::Measure` → `channel {center_hz, bandwidth_hz, out_rate_hz: out}` → edge Iq → `measure {}`; the `measure` events then report the band's absolute `center_hz` (the channel block's centre is the job centre).

- [ ] **Step 1: Write the failing tests**

```rust
// zeta-dsp/tests/classify.rs; fixture: 48 kHz at 144.8 MHz = aprs_packet_iq(.., 48_000, +12_000.0) + 3.16 × nbfm_modulate(tone 1 kHz, .., 48_000, 2_500, -12_000.0) (tone 10 dB stronger)
#[tokio::test] async fn classify_honours_selection_band() { /* Op::Classify over 0..2 s: centre 144_812_000 bw 12_500 -> a layer-2 "APRS" label; centre 144_788_000 bw 12_500 -> no layer-2 label */ }
// zeta-dsp/tests/jobs.rs
#[tokio::test] async fn measure_job_measures_only_its_band() { /* same fixture; JobStart measure centre 144_812_000 bw 12_500 -> first event center_hz within 144_812_000 ± 200 and bandwidth_hz <= 12_500 */ }
// identify.rs: existing whole-band tests pass Band::whole(Hz(48_000)) and stay green
#[tokio::test] async fn identifies_aprs_in_requested_band_only() { /* same two-signal block: band +12_000 -> APRS; band -12_000 -> empty */ }
// zeta-core tests/ops.rs
#[test] fn measure_kind_expands_to_channel_then_measure() {}
```

- [ ] **Step 2: Run to verify they fail**.
- [ ] **Step 3: Implement**; update `StubClassifier`, the core test classifier at `classify.rs:111`, and every `classify(`/`run_all(` call.
- [ ] **Step 4: Run tests** — PASS; regenerate AGENTS.md (doc comments are in the schema); drift check PASS.
- [ ] **Step 5: Commit** — `git commit -am "feat: classify and measure work on the requested band"`

---

### Task 4: Stage state resets on lag (carry-over c)

**Files:**
- Modify: `crates/zeta-dsp/src/lib.rs` (`run_map`, `Input`), `crates/zeta-dsp/src/{channel.rs,nbfm.rs,aprs.rs,measure.rs,fft.rs}`
- Test: unit tests in `lib.rs`, `channel.rs`, `nbfm.rs`, `aprs.rs`

**Interfaces:**
- Produces: `pub(crate) enum Input { Frame(Frame), Lagged(u64) }`; `run_map` takes `F: FnMut(Input) -> Result<Vec<Frame>, StageError> + Send`, still counts `LAGGED_FRAMES` and logs, then calls `f(Input::Lagged(n))`. Resets on `Lagged`: `channel` `state = ChannelState::new()`; `nbfm_demod` `prev = 0` and `filter = AudioFilter::default()`; `aprs_decode` `dec = None` (drops the partial AFSK and HDLC state, so a packet cut by the lag yields nothing and is not counted as an FCS failure); `measure` `window = None; buf.clear()`; `fft` ignores it (stateless). Each returns `Ok(vec![])` for `Lagged`.
- Deterministic lag in tests: `#[tokio::test]` is single-threaded, so sending `capacity + k` frames into a `broadcast::channel(capacity)` without an `.await` in between makes the stage's next `recv` return `Lagged(k)`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test] async fn run_map_passes_lag_marker() { /* capacity 2; send 4 frames; f records -> [Lagged(2), Frame, Frame] */ }
#[tokio::test] async fn channel_resets_on_lag() { /* block A, await its output, then flood so block B arrives after Lagged -> B's output equals extract_offset(B, ..) from fresh state exactly */ }
#[tokio::test] async fn nbfm_resets_on_lag() { /* output after lag equals nbfm_demod with prev = 0 plus a fresh AudioFilter */ }
#[tokio::test] async fn aprs_lag_mid_packet_discards_partial() { /* first half of packet "VK3AAA" audio, await, flood with silence to force Lagged, then a whole packet "VK3BBB" -> exactly one event, src "VK3BBB" */ }
```

- [ ] **Step 2: Run to verify they fail**.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run tests** — `cargo test -p zeta-dsp --features test-fixtures` PASS.
- [ ] **Step 5: Commit** — `git commit -am "feat(dsp): stages reset their state when their input lags"`

---

### Task 5: Every job kind validates against the CLI registry (carry-over f)

**Files:**
- Modify: `crates/zeta/src/node_setup.rs` (test module only)
- Test: `#[cfg(test)] mod tests` in `node_setup.rs`

**Interfaces:**
- Consumes: `node_setup::registry()`, `zeta_core::graph::validate`, `JobKind::expand`, `zeta_core::ops::schema()`.
- The kind list is read from the schema, `schema()["op"]["$defs"]["JobKind"]["enum"]`, and deserialised with `serde_json::from_value::<JobKind>`, so a kind added later is covered without editing the test.

- [ ] **Step 1: Write the failing test**

```rust
#[test] fn every_job_kind_expands_to_a_valid_graph() {
    /* for kind in schema kinds, rate in [12_000, 48_000, 1_000_000, 2_048_000, 2_400_000, 8_000_000, 10_000_000, 20_000_000],
       bw in [10_000, 12_500]: match kind.expand(&src, Tuning{centre 7_100_000, bw}, Hz(rate), None) {
         Err(OutOfRange{field:"bandwidth_hz",..}) => assert!(rate == 12_000 && bw == 12_500),
         Ok(g) => { validate(&g, &registry()).unwrap();
                    for each "channel" node: rate % out_rate_hz == 0 && bandwidth_hz <= out_rate_hz;
                    for each "nbfm_demod" fed by a channel: channel out_rate_hz % audio_rate_hz == 0 }
         Err(e) => panic!("{kind:?} {rate} {bw}: {e}") } */
}
```

- [ ] **Step 2: Run** — `cargo test -p zeta every_job_kind` — it should pass if Tasks 1–3 are right; temporarily break `channel_rate_hz` (return `rate / 7`) to watch it fail, then restore.
- [ ] **Step 3: Commit** — `git commit -am "test: every job kind expands to a graph the cli registry accepts"`

---

### Task 6: External AX.25 vectors from Dire Wolf (carry-over g)

**Files:**
- Create: `crates/zeta-dsp/tests/data/direwolf-1.7-vk3abc9-48k.wav`, `crates/zeta-dsp/tests/external_vectors.rs`
- Modify: `crates/zeta-dsp/src/ax25.rs` only if a test below fails (that is the point of an external vector)

**Interfaces:**
- The vector was produced on the dev box with Dire Wolf 1.7: `printf '%s\n' 'VK3ABC-9>APRS,WIDE1-1,WIDE2-1:!3748.12S/14458.33E>ZetaSDR test' > pkt.txt && gen_packets -r 48000 -o direwolf-1.7-vk3abc9-48k.wav pkt.txt`; 67 446 bytes (44-byte RIFF header, mono s16le at 48 000 Hz), SHA-256 `bc1209f1ee87e730bd0a7e9180015b83ff791d3203a07a5a5a5bcdb06f9f146b`. Regenerate with that command; if the hash differs, keep the file only if `atest -h` decodes it to the frame below, and record the new hash in the test's doc comment. `atest -h` printed the 63-byte frame without FCS:

```
82 a0 a4 a6 40 40 e0 ac 96 66 82 84 86 f2 ae 92 88 8a 62 40 62 ae 92 88 8a 64 40 63 03 f0 21 33
37 34 38 2e 31 32 53 2f 31 34 34 35 38 2e 33 33 45 3e 5a 65 74 61 53 44 52 20 74 65 73 74 0a
```
- Note the C/R bits (`e0`, `f2`: bit 7 set) and the SSID 9 in `f2`; the info field ends in the `0a` from the input file. CRC-16/X.25 over the 63 bytes is `0x02F6`, sent little-endian `f6 02`.

- [ ] **Step 1: Write the tests**

```rust
const DW_FRAME_HEX: &str = "82a0a4a64040e0ac9666828486f2ae92888a624062ae92888a64406303f021333734382e3132532f31343435382e3333453e5a65746153445220746573740a";
#[test] fn direwolf_frame_parses() { /* parse_ax25_ui -> src "VK3ABC-9", dst "APRS", path ["WIDE1-1","WIDE2-1"], info b"!3748.12S/14458.33E>ZetaSDR test\n"; crc16_x25(frame) == 0x02F6 */ }
#[test] fn direwolf_frame_through_deframer() { /* hdlc_frame_bits(frame ++ [0xf6,0x02], 8, 2) -> HdlcDeframer::feed gives exactly [frame]; with [0x02,0xf6] -> no frames, fcs_failures()==1 */ }
#[test] fn direwolf_audio_decodes() { /* check RIFF/WAVE, fmt: 1 channel, 48000 Hz, 16 bits; samples from byte 44 as i16 LE / 32768 -> Afsk1200Demod::new(48_000).feed -> HdlcDeframer::feed -> exactly one frame, equal to DW_FRAME_HEX bytes (FCS made by Dire Wolf, checked by us) */ }
```

- [ ] **Step 2: Run** — `cargo test -p zeta-dsp --features test-fixtures --test external_vectors`. Any failure is a real interoperability bug in `ax25.rs`/`afsk.rs`: fix it there with a unit test naming the cause.
- [ ] **Step 3: Commit** — `git add -A && git commit -m "test(dsp): ax25 deframer and afsk demod against a Dire Wolf frame"`

---

### Task 7: Port faults fail the jobs reading the port (core, additive)

**Files:**
- Modify: `crates/zeta-core/src/port.rs`, `crates/zeta-core/src/node/launch.rs`, `crates/zeta-core/src/ops.rs` (`SourceInfo.fault`), `crates/zeta-core/tests/no_privileged_deps.rs` (add `"tungstenite"`)
- Test: `crates/zeta-core/tests/jobs.rs`, `crates/zeta-core/tests/ops.rs`

**Interfaces:**
- Produces: `Port::fault(&self) -> Option<String>` with default body `None`, documented: "The port's unrecoverable failure, if any (a remote receiver that is full or gone, a device that was unplugged). Jobs reading the port fail with it as their reason. A successful `tune` clears it." The port-contract paragraph of the Global Constraints becomes the `Port` trait's doc comment.
- `forward_iq(bus, prov, stream, port: Arc<dyn Port>, rx, cancel) -> Result<(), StageError>` is spawned into the job's `JoinSet` (not `tokio::spawn`) so `supervise` sees it. Its `select!` gains a 200 ms `tokio::time::interval` (`MissedTickBehavior::Delay`): if `port.fault()` is `Some(f)` → `Err(Failed(format!("source {}: {f}", id)))`. On `RecvError::Closed`: fault → `Failed` as above; no fault → unregister the IQ stream (stages drain and end `Ok`, as today) then `cancel.cancelled().await` and return `Err(Cancelled)`, so the forwarder never ends a job by itself. Cancellation → `Err(Cancelled)`.
- `SourceInfo { id, capabilities, current, holders, #[schemars(required)] fault: Option<String> }`, filled from `port.fault()`.

- [ ] **Step 1: Write the failing tests**

```rust
// tests/jobs.rs, with a FaultyPort double (broadcast sender, fault: Mutex<Option<String>>, tune clears it)
#[tokio::test] async fn port_fault_fails_running_job_with_reason() { /* passthrough job running; set fault "device gone" -> within 1 s state Failed{reason:"source flaky: device gone"}; lease_holders empty; job streams unregistered */ }
#[tokio::test] async fn closed_port_without_fault_still_drains_and_stops() { /* sender dropped after 3 blocks -> job output saw 3 frames, state Stopped */ }
// tests/ops.rs
#[tokio::test] async fn source_list_reports_fault() { /* fault set -> SourceInfo.fault == Some("device gone"); file source -> None */ }
```

- [ ] **Step 2: Run to verify they fail**.
- [ ] **Step 3: Implement**; add `"tungstenite"` to the banned list.
- [ ] **Step 4: Run tests** — `cargo test --workspace` PASS (every existing job test still green); regenerate AGENTS.md (`SourceInfo` is in the result schema).
- [ ] **Step 5: Commit** — `git commit -am "feat(core): a port fault fails the jobs that read it; source list shows it"`

---

### Task 8: KiwiSDR protocol codec

**Files:**
- Create: `crates/zeta-source-kiwi/{Cargo.toml,src/lib.rs,src/proto.rs}`
- Test: unit tests in `proto.rs`

**Interfaces:**
- Crate deps: `zeta-core`, `num-complex`, `thiserror`, `async-trait`, `tokio` (`net`, `time`, `sync`, `macros`, `rt`), `tokio-util`, `tokio-tungstenite` 0.30.0 (`default-features = false`, `features = ["connect"]`), `futures-util` 0.3 (`default-features = false`, `features = ["sink", "std"]`); feature `test-fake = []`; self dev-dependency with `test-fake` (as `zeta-source-file` does for `test-fixtures`).
- Produces (`proto`), all pure:
  - `pub fn snd_path(ts: u32) -> String` → `/<ts>/SND`; `pub fn snd_url(host: &str, port: u16, ts: u32) -> String` → `ws://host:port/<ts>/SND`.
  - `pub fn cmd_auth(password: Option<&str>) -> String`; `pub fn cmd_mod_iq(center_hz: u64, bandwidth_hz: u64) -> String`; `pub fn cmd_ar_ok(audio_rate: u32) -> String`; consts `CMD_SQUELCH = "SET squelch=0 max=0"`, `CMD_GENATTN = "SET genattn=0"`, `CMD_GEN = "SET gen=0 mix=-1"`, `CMD_IDENT = "SET ident_user=zetasdr"`, `CMD_AGC = "SET agc=1 hang=0 thresh=-100 slope=6 decay=1000 manGain=50"`, `CMD_COMPRESSION_OFF = "SET compression=0"`, `CMD_KEEPALIVE = "SET keepalive"`. A password containing a space, `=` or a control character is refused by `KiwiConfig::new` (Task 9), never sent.
  - `pub enum KiwiMsg { SampleRate(f64), AudioRate(u32), BadPassword(u8), TooBusy(u32), Down, Redirect(String), FreqOffsetKhz(f64), Other(String) }`; `pub fn parse_msg(payload: &[u8]) -> Result<Vec<KiwiMsg>, ProtoError>` (payload starts `MSG`; skip the tag and the leading space; split on `' '`; `badp=0` is `BadPassword(0)`, which the session treats as success).
  - `pub const SND_FLAG_ADC_OVFL: u8 = 0x02; SND_FLAG_STEREO: u8 = 0x08; SND_FLAG_COMPRESSED: u8 = 0x10; SND_FLAG_LITTLE_ENDIAN: u8 = 0x80;` `pub struct Snd { pub flags: u8, pub seq: u32, pub rssi_dbm: f32, pub gps_sec: u32, pub gps_nsec: u32, pub samples: Vec<Complex<f32>> }`; `pub fn parse_snd(payload: &[u8]) -> Result<Snd, ProtoError>` per the protocol section: length ≥ 20, `STEREO` required (`ProtoError::NotIq { flags }`), `COMPRESSED` refused, sample bytes a multiple of 4, `i16` big-endian unless `LITTLE_ENDIAN`, each component `/ 32768.0`.
  - `pub enum Tag { Msg, Snd, Other([u8; 3]) }`, `pub fn tag(payload: &[u8]) -> Option<Tag>`.
  - Behind `test-fake`: `pub fn encode_snd(flags: u8, seq: u32, smeter: u16, samples: &[(i16, i16)]) -> Vec<u8>` (big-endian samples, zero GPS bytes) and `pub fn encode_msg(pairs: &str) -> Vec<u8>` (`b"MSG "` + pairs).
- `ProtoError` (thiserror) variants name the offset or field: `Short { len }`, `NotIq { flags }`, `Compressed`, `OddSamples { bytes }`, `BadValue { name, value }`, `NotUtf8`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn commands_are_exact() { /* cmd_auth(None)=="SET auth t=kiwi p=", cmd_auth(Some("pw"))=="SET auth t=kiwi p=pw";
    cmd_mod_iq(7_100_000, 10_000)=="SET mod=iq low_cut=-5000 high_cut=5000 freq=7100.000";
    cmd_mod_iq(7_100_250, 20_000)=="SET mod=iq low_cut=-6000 high_cut=6000 freq=7100.250";
    cmd_mod_iq(10_000_000, 1)=="SET mod=iq low_cut=-1 high_cut=1 freq=10000.000"; cmd_ar_ok(12000)=="SET AR OK in=12000 out=44100";
    snd_url("kiwisdr.local", 8073, 1760000000)=="ws://kiwisdr.local:8073/1760000000/SND" */ }
#[test] fn parses_live_msgs() { /* b"MSG sample_rate=11998.902088" -> [SampleRate(11998.902088)]; b"MSG audio_init=1 audio_rate=12000" -> [Other("audio_init=1"), AudioRate(12000)];
    b"MSG badp=0" -> [BadPassword(0)]; b"MSG too_busy=8" -> [TooBusy(8)]; b"MSG down" -> [Down]; the version line -> contains FreqOffsetKhz(0.0) */ }
#[test] fn parses_live_snd_frame_bytes() { /* hex "534e44" "0a" "02000000" "0234" "ff00c37102008b13561f" "017604b9060afb72" -> flags 0x0a, seq 2, rssi -70.6±0.01,
    gps_sec 160195, gps_nsec 525734795, samples [(374, 1209), (1546, -1166)] / 32768 */ }
#[test] fn full_live_size_frame_is_512_samples() { /* encode_snd(0x08, 7, 564, &[(0,0); 512]).len()==2068 and parse -> 512 samples */ }
#[test] fn little_endian_flag_is_honoured() { /* flags 0x88 with LE bytes -> same values */ }
#[test] fn malformed_frames_are_errors_not_panics() { /* len 0..20 -> Short; flags 0x00 -> NotIq; 0x18 -> Compressed; 3 sample bytes -> OddSamples; every prefix of a valid frame -> Err or Ok, never panic */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-source-kiwi` (crate missing).
- [ ] **Step 3: Implement** the crate skeleton and `proto.rs`; `lib.rs` re-exports `proto` and (Task 9) `KiwiSource`, `KiwiConfig`.
- [ ] **Step 4: Run tests** — PASS; `cargo test -p zeta-core --test no_privileged_deps` PASS.
- [ ] **Step 5: Commit** — `git add -A && git commit -m "feat(kiwi): kiwisdr snd protocol codec with live-captured vectors"`

---

### Task 9: KiwiSDR source port

**Files:**
- Create: `crates/zeta-source-kiwi/src/{source.rs,session.rs,testing.rs}` (`testing.rs` behind `test-fake`)
- Test: `crates/zeta-source-kiwi/tests/kiwi.rs`, `crates/zeta-source-kiwi/tests/live.rs`

**Interfaces:**
- Produces: `pub struct KiwiConfig { pub host: String, pub port: u16, pub password: Option<String>, pub reconnect_delays_ms: Vec<u64>, pub read_timeout_ms: u64, pub handshake_timeout_ms: u64 }` with `pub fn new(host, port, password) -> Result<Self, String>` (defaults `[1000, 2000, 4000, 8000, 16000]`, `10_000`, `10_000`; refuses a bad password character); `Debug` prints `password: <redacted>`.
- `pub struct KiwiSource` with `pub fn new(id: SourceId, cfg: KiwiConfig) -> Self` (no network until the first `tune`), `pub fn audio_rate_hz(&self) -> Option<u32>`, `pub fn reported_sample_rate(&self) -> Option<f64>` (the `sample_rate=` float), `pub fn rssi_dbm(&self) -> Option<f32>`, `pub fn adc_overflows(&self) -> u64`, `pub fn seq_gaps(&self) -> u64`; `Drop` cancels the session (close frame sent, slot freed).
- `impl Port`: capabilities `tune_min_hz Asserted(0)`, `tune_max_hz Asserted(30_000_000)`, `max_bandwidth_hz Asserted(12_000)`, `formats [Ci16]`, `channels 1`, `rx true`, `tx false`, `remote true`, `push true`. `tune(t)`: bandwidth outside `1..=12_000` or centre above 30 MHz → `OutOfRange` naming the field; if no live session or the port is faulted, clear the fault, record the band as the pending tuning, spawn the session (which sends it on `sample_rate`, as the reference does) and wait for both `sample_rate` and `audio_rate` (≤ `handshake_timeout_ms`), mapping a refusal to `PortError::Io(format!("kiwi {host}:{port}: {reason}"))`; non-zero `freq_offset` → `PortError::Unsupported("kiwi freq_offset <x> kHz not supported")`; on a live session send `cmd_mod_iq` + `CMD_AGC` + `CMD_COMPRESSION_OFF` unless the band is unchanged; return `Tuning { center_hz: t.center_hz, bandwidth_hz: Hz(audio_rate) }`. `current()` is `None` until the first successful tune. `fault()` returns the session's terminal reason.
- Session (one tokio task per connection lifetime; commands to it over an `mpsc`): connect to `snd_url`, send auth, answer `MSG`s exactly as the protocol section says, send `CMD_KEEPALIVE` on a 1 s interval whose first tick is 1 s after connect (`interval_at`, not `interval`, whose first tick is immediate), convert each `SND` to one `IqBlock { t0, sample_rate_hz: Hz(audio_rate), center_hz: current centre, samples }` and broadcast it (capacity 64). Blocks are stamped with the nominal `audio_rate` so the ring and `measure` see contiguous blocks; `t0 = anchor + count · 1e9 / audio_rate` with `anchor` the wall clock at the first `SND`. Re-anchor (`anchor = max(now, last_end)`, `count = 0`) on: a reconnect, a sequence gap (count it), and wall-clock drift beyond 500 ms (the live Kiwi runs at 11998.902 Hz, 91 ppm slow, so this happens about every 1.5 h and clears the ring once; documented). Terminal reasons (no retry; set `fault`, close): `too_busy: all <N> client slots taken`; `badp=<N>: <meaning>` for N ≠ 0; `down: server is down`; `redirect to <url> not supported`; 10 consecutive unparseable `SND` frames → `protocol: <error>`. A dropped or silent socket (no message for `read_timeout_ms`) → reconnect after each delay in `reconnect_delays_ms` in turn, re-sending auth and the current tuning; when all fail → `connection lost (<error>); <n> reconnect attempts failed`. The producer exits when the port is dropped or the broadcast has no receivers.
- `testing::FakeKiwi` (feature `test-fake`): `pub async fn start(script: FakeScript) -> FakeKiwi` on `127.0.0.1:0` with `pub fn addr(&self) -> SocketAddr`, `pub fn commands(&self) -> Vec<String>` (every text received, in order), `pub fn connections(&self) -> usize`, `pub fn paths(&self) -> Vec<String>` (request paths via `accept_hdr_async`). `pub struct FakeScript { pub on_auth: Vec<FakeReply>, pub frames_per_connection: Option<usize>, pub frame_interval_ms: u64, pub listen_after_drop: bool }`, `pub enum FakeReply { Ok, TooBusy(u32), BadPassword(u8), Down }` (one per connection, last repeated). `Ok` replays the live order: `MSG sample_rate=11998.902088`, `MSG badp=0`, the version line, `MSG audio_init=1 audio_rate=12000`, then `encode_snd(0x08, seq, 564, ramp)` frames of 512 samples.

- [ ] **Step 1: Write the failing tests**

```rust
// tests/kiwi.rs (features test-fake)
#[tokio::test] async fn tune_handshakes_and_sends_exact_commands() { /* tune(7_100_000, 10_000) -> Ok(Tuning{7_100_000, 12_000}); commands() starts
    ["SET auth t=kiwi p=", "SET squelch=0 max=0", "SET genattn=0", "SET gen=0 mix=-1", "SET ident_user=zetasdr",
     "SET mod=iq low_cut=-5000 high_cut=5000 freq=7100.000", "SET agc=1 hang=0 thresh=-100 slope=6 decay=1000 manGain=50",
     "SET compression=0", "SET keepalive", "SET AR OK in=12000 out=44100"]; paths()[0] matches ^/\d+/SND$; second identical tune sends nothing */ }
#[tokio::test] async fn blocks_carry_audio_rate_and_contiguous_t0() { /* 5 blocks: sample_rate_hz 12_000, center 7_100_000, 512 samples, t0[k+1]-t0[k] == 42_666_666 or 42_666_667 ns (integer division of the running count) */ }
#[tokio::test] async fn keepalive_once_per_second() { /* after 2.5 s, 2 or 3 "SET keepalive" beyond the first */ }
#[tokio::test] async fn too_busy_at_tune_is_port_error_naming_slots() { /* on_auth [TooBusy(8)] -> Err(Io(m)), m contains "too_busy: all 8 client slots taken"; fault() Some; Node::apply(JobStart measure) -> Err(Other) and job_list() empty */ }   // Review Focus 1
#[tokio::test] async fn too_busy_on_reconnect_fails_job() { /* on_auth [Ok, TooBusy(8)], frames_per_connection Some(20), delays [10]; Node + measure job -> within 3 s Failed{reason contains "source kiwi0: too_busy"} */ }   // Review Focus 1
#[tokio::test] async fn drop_mid_stream_reconnects_and_t0_stays_monotonic() { /* [Ok, Ok], 20 frames per connection, delays [10,10]; collect 40 blocks: connections()==2; "SET mod=…7100.000" sent twice; t0 strictly increasing; first block after reconnect t0 >= previous end */ }   // Review Focus 2
#[tokio::test] async fn reconnect_exhausted_fails_job_with_reason() { /* listen_after_drop false, delays [10,10,10]; job Failed reason contains "connection lost" and "3 reconnect attempts failed" */ }   // Review Focus 2
#[tokio::test] async fn bad_password_and_down_are_named() { /* BadPassword(1) -> "badp=1: bad password"; Down -> "down: server is down"; password never in any error or Debug output */ }
#[tokio::test] async fn job_start_wider_than_kiwi_is_out_of_range() { /* Node with KiwiSource on a FakeKiwi; JobStart measure 7_100_000 bw 20_000 -> OutOfRange{field:"bandwidth_hz", max:12000}; centre 31_000_000 -> OutOfRange{field:"center_hz"}; fake.connections()==0 */ }   // Review Focus 6
#[tokio::test] async fn measure_job_runs_on_fake_kiwi() { /* JobStart measure 7_100_000 bw 10_000 -> first event within 3 s, finite snr_db */ }
// tests/live.rs
#[tokio::test] #[ignore = "needs ws://kiwisdr.local:8073"] async fn live_kiwi_7100_khz_iq() {
    /* if TcpStream::connect("kiwisdr.local:8073") fails within 3 s: eprintln!("skipped: kiwisdr.local:8073 unreachable"); return;
       subscribe; tune(7_100_000, 10_000); collect >= 12_000 samples within 15 s; every block.sample_rate_hz == audio_rate_hz().unwrap() as u64 == 12_000;
       reported_sample_rate() within 12_000 ± 50; mean |s|² finite and > 0; t0 strictly increasing; drop the source (frees the slot) */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-source-kiwi --features test-fake`.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run tests** — PASS; then once by hand `cargo test -p zeta-source-kiwi --test live -- --ignored --nocapture` against the live Kiwi (read-only: it only opens a receiver channel; check `/status` shows `users` below `users_max` first) and paste the output into the report.
- [ ] **Step 5: Commit** — `git add -A && git commit -m "feat(kiwi): kiwisdr websocket source with reconnect and faults"`

---

### Task 10: Soapy device seam, capabilities and rate choice

**Files:**
- Create: `crates/zeta-source-soapy/{Cargo.toml,src/lib.rs,src/device.rs,src/caps.rs,src/convert.rs,src/mock.rs}` (`mock.rs` behind `test-mock`)
- Test: unit tests in `caps.rs`, `convert.rs`

**Interfaces:**
- Crate deps: `zeta-core`, `num-complex`, `async-trait`, `tokio` (`sync`, `time`, `rt`), `thiserror`; optional `soapysdr = { version = "0.5.1", optional = true }`; features `driver = ["dep:soapysdr"]`, `test-mock = []`; self dev-dependency with `test-mock`. Before writing `driver.rs` (Task 11), check the API against `~/.cargo/registry/src/*/soapysdr-0.5.1/src/device.rs` (verified while writing this plan: `enumerate(args) -> Result<Vec<Args>, Error>`, `Device::new`, `frequency_range(Direction, usize) -> Result<Vec<Range>, Error>`, `get_sample_rate_range(..) -> Result<Vec<Range>, Error>` (there is no `list_sample_rates`), `stream_formats(..) -> Result<Vec<Format>, Error>`, `native_stream_format(..) -> Result<(Format, f64), Error>`, `num_channels(Direction)`, `set_frequency(Direction, usize, f64, args)`, `set_sample_rate(Direction, usize, f64)`, `sample_rate(..)`, `driver_key()`, `rx_stream::<E>(&[usize]) -> Result<RxStream<E>, Error>`, `RxStream::{activate(Option<i64>), deactivate(Option<i64>), read(&mut [&mut [E]], timeout_us: i64) -> Result<usize, Error>}`, `Error { code: ErrorCode, message }`, `ErrorCode::{Timeout, Overflow, ..}` `#[non_exhaustive]`; `StreamSample` for `Complex<i8>`, `Complex<i16>`, `Complex<f32>`).
- Produces (`device`): `pub struct RangeHz { pub min_hz: f64, pub max_hz: f64, pub step_hz: f64 }`; `pub enum WireFormat { Cs8, Cs16, Cf32 }`; `pub enum ReadError { Timeout, Overflow, Fatal(String) }`;
  `pub trait SoapyDevice: Send + Sync { fn driver_key(&self) -> String; fn frequency_ranges(&self) -> Result<Vec<RangeHz>, String>; fn sample_rate_ranges(&self) -> Result<Vec<RangeHz>, String>; fn formats(&self) -> Result<(Option<WireFormat>, Vec<WireFormat>), String> /* native, supported ∩ {CS8, CS16, CF32} */; fn rx_channels(&self) -> Result<usize, String>; fn set_frequency(&self, hz: f64) -> Result<(), String>; fn set_sample_rate(&self, hz: f64) -> Result<(), String>; fn sample_rate(&self) -> Result<f64, String>; fn open_rx(&self, format: WireFormat) -> Result<Box<dyn SoapyRx>, String>; }`
  `pub trait SoapyRx: Send { fn activate(&mut self) -> Result<(), String>; fn deactivate(&mut self) -> Result<(), String>; fn read(&mut self, out: &mut [Complex<f32>], timeout_us: i64) -> Result<usize, ReadError>; }` (implementations convert to cf32 inside `read`).
- Produces (`caps`): `pub fn capabilities(dev: &dyn SoapyDevice) -> Result<Capabilities, String>`: `tune_min_hz`/`tune_max_hz` = `Asserted` min of minimums / max of maximums (rounded to integer Hz), `max_bandwidth_hz` = `Asserted` largest rate, `formats` from `formats()` mapped to `Ci8/Ci16/Cf32`, `channels` from `rx_channels()`, `rx true`, `tx false` (design 2: TX off until enabled), `remote` = `driver_key() == "remote"` (SoapyRemote), `push false` (the runtime pulls). `pub fn pick_rate(ranges: &[RangeHz], bandwidth_hz: u64) -> Option<u64>`: the smallest supported rate ≥ `bandwidth_hz`; a range with `step_hz > 0` offers `min + k·step`; `min == max` offers that value; a continuous range offers `max(bandwidth_hz, min)` rounded up to a multiple of 100 000 Hz (so `channel_rate_hz` finds clean divisors) when that stays ≤ `max`, else `max`. `pub fn pick_format(native: Option<WireFormat>, supported: &[WireFormat]) -> WireFormat`: native when it is one of the three, else `Cf32`.
- Produces (`convert`): `pub fn cs8_to_cf32(x: &[Complex<i8>], out: &mut [Complex<f32>])` (÷128), `pub fn cs16_to_cf32(x: &[Complex<i16>], out: &mut [Complex<f32>])` (÷32768), matching the file source's scaling.
- `mock` (feature `test-mock`): `pub struct MockDevice` built with `MockDevice::hackrf_like()` (freq `1e6..6e9`, rate one continuous range `1e6..20e6`, formats native `Cs8`, supported all three, 1 channel, driver `"hackrf"`), `.with_driver(&str)`, `.with_rates(Vec<RangeHz>)`, `.with_reads(Vec<MockRead>)` where `pub enum MockRead { Samples(usize), Timeout, Overflow, Fatal(String) }` (after the script, `Timeout` forever; `Samples(n)` yields a deterministic ramp), and `pub fn calls(&self) -> Vec<String>` recording `set_frequency(..)`, `set_sample_rate(..)`, `open_rx(..)`, `activate`, `deactivate`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn pick_rate_table() { /* discrete [250k,1.024M,2.048M,2.4M] (min==max each): bw 1_500_000 -> 2_048_000; bw 3_000_000 -> None;
    continuous 1e6..20e6: bw 12_500 -> 1_000_000; bw 2_345_678 -> 2_400_000; bw 19_950_000 -> 20_000_000; bw 21_000_000 -> None;
    stepped min 1e6 step 5e5 max 1e7: bw 1_200_000 -> 1_500_000 */ }
#[test] fn hackrf_like_capabilities() { /* tune 1_000_000..=6_000_000_000 asserted, max_bandwidth 20_000_000 asserted, formats [Ci8,Ci16,Cf32], channels 1, rx, !tx, !remote, !push */ }
#[test] fn remote_driver_is_remote() { /* with_driver("remote") -> remote true */ }
#[test] fn conversions_scale_to_unit() { /* i8 -128 -> -1.0, 127 -> 0.9921875; i16 -32768 -> -1.0, 16384 -> 0.5 */ }
#[test] fn pick_format_prefers_native() { /* Some(Cs8) -> Cs8; Some(other unsupported) modelled as None -> Cf32 */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-source-soapy --features test-mock`.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run tests** — PASS, and `cargo test --workspace` PASS without the `driver` feature.
- [ ] **Step 5: Commit** — `git add -A && git commit -m "feat(soapy): device seam, asserted capabilities and sample-rate choice"`

---

### Task 11: Soapy source port, reader thread and driver

**Files:**
- Create: `crates/zeta-source-soapy/src/source.rs`, `crates/zeta-source-soapy/src/driver.rs` (feature `driver`)
- Test: `crates/zeta-source-soapy/tests/soapy.rs` (feature `test-mock`), `crates/zeta-source-soapy/tests/hardware.rs` (feature `driver`, `#[ignore]`)

**Interfaces:**
- Produces: `pub struct SoapySource` with `pub fn with_device(id: SourceId, dev: Arc<dyn SoapyDevice>) -> Result<Self, PortError>` (capabilities computed once, no streaming yet), `pub fn overflows(&self) -> u64`, `pub fn measured_rate_hz(&self) -> Option<f64>` (samples / elapsed over the last 10 s, for the report; capabilities stay `Asserted`); behind `driver`: `pub fn open(id: SourceId, args: &str) -> Result<Self, PortError>` (`soapysdr::Device::new(args)` wrapped in `driver::Driver`) and `pub fn find(filter: &str) -> Result<Vec<std::collections::BTreeMap<String, String>>, String>` (`soapysdr::enumerate`, the `SoapySDRUtil --find` equivalent).
- `impl Port`: `tune(t)` → centre outside the asserted range → `OutOfRange { field: "center_hz", .. }`; `pick_rate` `None` → `OutOfRange { field: "bandwidth_hz", max: largest rate }`; unchanged band → no-op; first tune: `set_sample_rate`, `set_frequency`, `open_rx(pick_format(..))`, `activate`, start the reader thread; later tunes hand a retune to the reader thread and await its acknowledgement (≤ 2 s; timeout → `PortError::Io("soapy retune timed out")`); the thread applies it between reads (rate change: `deactivate` → `set_sample_rate` → `set_frequency` → `activate`; centre only: `set_frequency`) and re-anchors `t0`. Answer `Tuning { center_hz, bandwidth_hz: Hz(sample_rate().round()) }`. Device gains are left at the driver's defaults; a `source.set_gain` operation is future work (documented in the crate doc and README).
- Reader thread (`std::thread`, named `soapy-<id>`, blocking reads): block size `max(16_384, rate / 100)` samples; `read(.., 100_000 µs)`; `t0 = anchor + count · 1e9 / rate` with `anchor` the wall clock at activation; `Overflow` → count it, re-anchor `max(now, last_end)`; `Timeout` ten times in a row (1 s without samples) → fault `no samples for 1000 ms (device unplugged?)`, exit; `Fatal(m)` → fault `read failed: <m>`, exit; exits too when the source is dropped or the broadcast (capacity 64) has no receivers; `deactivate` on exit.
- `driver::Driver` implements `SoapyDevice` over `soapysdr::Device` (channel 0, `Direction::Rx`), and its `SoapyRx` holds the typed `RxStream<Complex<i8|i16|f32>>` with a scratch buffer, mapping `ErrorCode::Timeout` → `Timeout`, `ErrorCode::Overflow` → `Overflow`, anything else → `Fatal(e.to_string())`.

- [ ] **Step 1: Write the failing tests**

```rust
// tests/soapy.rs
#[tokio::test] async fn tune_sets_frequency_and_smallest_rate() { /* hackrf_like; tune(100_000_000, 12_500) -> Tuning{100_000_000, 1_000_000}; calls() == ["set_sample_rate(1000000)", "set_frequency(100000000)", "open_rx(Cs8)", "activate"]; repeating the tune adds no call */ }
#[tokio::test] async fn tune_out_of_range_names_field() { /* centre 500_000 -> OutOfRange center_hz; bw 25_000_000 -> OutOfRange bandwidth_hz max 20_000_000 */ }
#[tokio::test] async fn blocks_have_contiguous_t0_from_sample_count() { /* reads [Samples(16_384) x3] at 1 MS/s -> t0 deltas exactly 16_384_000 ns, sample_rate_hz 1_000_000, center 100 MHz */ }
#[tokio::test] async fn overflow_reanchors_without_going_backwards() { /* [Samples, Overflow, Samples] -> second block t0 >= first end; overflows()==1 */ }
#[tokio::test] async fn retune_rate_change_reactivates_stream() { /* tune 1 MS/s then bw 2_000_000 -> calls end with ["deactivate", "set_sample_rate(2000000)", "set_frequency(..)", "activate"]; next block sample_rate_hz 2_000_000 */ }
#[tokio::test] async fn unplug_mid_stream_fails_job() { /* Node with SoapySource; reads [Samples x5, Fatal("LIBUSB_ERROR_NO_DEVICE")]; passthrough job -> within 1 s Failed{reason contains "read failed" and "LIBUSB_ERROR_NO_DEVICE"};
    second case reads [Samples x2] then Timeout forever -> Failed within 2 s, reason contains "no samples for 1000 ms" */ }   // Review Focus 3
#[tokio::test] async fn reader_exits_when_source_dropped() { /* drop the source -> the thread joins within 500 ms (exposed via a test-mock hook or calls() ending "deactivate") */ }
// tests/hardware.rs (cargo test -p zeta-source-soapy --features driver -- --ignored)
#[tokio::test] #[ignore = "needs a HackRF and libsoapysdr-dev"] async fn hackrf_streams_one_second_at_100_mhz() {
    /* if find("driver=hackrf") is empty: eprintln!("skipped: no hackrf attached"); return;
       open("driver=hackrf"); tune(100_000_000, 2_000_000); 1 s of blocks: sample_rate_hz 2_000_000, t0 strictly increasing, power finite */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-source-soapy --features test-mock`.
- [ ] **Step 3: Implement** `source.rs`, then `driver.rs`. The `driver` feature needs `libsoapysdr-dev` (headers and `SoapySDR.pc`; `soapysdr-sys` may also need `libclang-dev`). It is not installed on the dev box: ask the user to `sudo apt install libsoapysdr-dev libclang-dev`, then run `cargo clippy -p zeta-source-soapy --features driver --all-targets -- -D warnings`. If it stays uninstalled, write `driver.rs` against the verified API above, leave it uncompiled, and say so in the report; never make the default build depend on it.
- [ ] **Step 4: Run tests** — `cargo test --workspace` PASS; `cargo test -p zeta-source-soapy --features test-mock` PASS; with the dev package present, the ignored hardware test prints `skipped: no hackrf attached` (no HackRF is attached now).
- [ ] **Step 5: Commit** — `git add -A && git commit -m "feat(soapy): soapysdr source with sample-count timestamps and unplug faults"`

---

### Task 12: Plugin wire protocol

**Files:**
- Create: `crates/zeta-plugin/{Cargo.toml,src/lib.rs,src/wire.rs}`
- Test: unit tests in `wire.rs`; `crates/zeta-plugin/tests/envelope.rs`

**Interfaces:**
- Crate deps: `zeta-core`, `tokio` (`process`, `io-util`, `sync`, `time`, `macros`, `rt`), `async-trait`, `num-complex`, `serde`, `serde_json`, `schemars`, `thiserror`, `tokio-util`; dev: `zeta-ws` (the envelope cross-check only).
- **Protocol 1** (also the Python SDK's docstring, verbatim). A child process speaks it on stdin (host to plugin) and stdout (plugin to host); stderr is free text that the host copies to its own stderr prefixed `[plugin <kind>] `. Each message begins with one byte:
  - `0x7B` (`{`): a **control message**, one compact UTF-8 JSON object ending in `0x0A`, at most 65 536 bytes including the newline.
  - `0x00`: a **data frame**: `u32` little-endian header length `H` (1..=65 536), `H` bytes of UTF-8 JSON header, `u32` little-endian body length `B` (0..=16 777 216), `B` body bytes.
  - any other byte: protocol error `unexpected byte 0x<hh> at message start`.
- Control messages, tagged by `"type"` (snake_case), the same tag plan 3's `WsMessage` uses: plugin to host `{"type":"hello","protocol":1,"kind":"<k>","inputs":["iq"],"outputs":["iq"],"params_schema":{…}}` (first message, within 5000 ms of spawn), `{"type":"error","message":"<m>"}`; host to plugin `{"type":"run","protocol":1,"params":{…},"inputs":[{"kind":"iq"}],"outputs":[{"stream":"<id>","kind":"iq"}]}`, `{"type":"lagged","missed":N}` and `{"type":"closed","reason":"<r>"}` (byte-identical to `WsMessage::Lagged`/`Closed`), `{"type":"stop"}`.
- Data frame header = `WsMessage::Frame` JSON exactly: `{"type":"frame","stream","kind","source","job","channel","start_ns","end_ns","payload"}`, with `payload` the `FrameSummary` summary for the kind. Body by kind: `iq` — payload `{"sample_rate_hz","center_hz","samples":N}`, body `N × 8` bytes, interleaved `f32` little-endian I then Q (SigMF `cf32_le`), `t0 = start_ns`; `audio` — payload `{"sample_rate_hz","samples":N,"peak"}`, body `N × 4` bytes `f32` LE; `event` — payload is the event, `B = 0`; `text` — payload is the string, `B = 0`; `video` and `sink` are refused in protocol 1. `B` must match `N`.
- Produces (`wire`): `pub const PROTOCOL: u32 = 1;` `pub enum Control { Hello(Hello), Error { message: String }, Run(Run), Lagged { missed: u64 }, Closed { reason: String }, Stop }` (serde `tag = "type"`, `rename_all = "snake_case"`, `deny_unknown_fields`); `pub struct Hello { protocol: u32, kind: String, inputs: Vec<StreamKind>, outputs: Vec<StreamKind>, params_schema: serde_json::Value }`; `pub struct Run { protocol: u32, params: serde_json::Value, inputs: Vec<PortKind>, outputs: Vec<OutPort> }`; `pub enum Message { Control(Control), Frame(Frame) }`; `pub fn encode_frame(f: &Frame) -> Result<Vec<u8>, WireError>`; `pub fn encode_control(c: &Control) -> Vec<u8>`; `pub async fn read_message<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<Message>, WireError>` (`Ok(None)` only on EOF at a message boundary). `WireError`: `UnexpectedByte(u8)`, `TooLong { what, len, max }`, `Truncated { what: &'static str, got: usize, want: usize }` (EOF inside a message: `"header"` or `"body"`), `BodyMismatch { kind, samples, bytes }`, `KindNotCarried(StreamKind)`, `Json(String)`, `Io(String)`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test] async fn iq_frame_roundtrips_bytes_exactly() { /* IqBlock t0 5e9, 48 kHz, 100 MHz, [1+2j, -0.5+0.25j]: bytes[0]==0x00, u32 LE header len, header JSON == {"type":"frame",…,"payload":{"sample_rate_hz":48000,"center_hz":100000000,"samples":2}}, body == f32 LE 1,2,-0.5,0.25; read_message gives the same block */ }
#[tokio::test] async fn audio_event_text_roundtrip() {}
#[tokio::test] async fn control_line_roundtrips() { /* encode_control(Lagged{missed:3}) == b"{\"type\":\"lagged\",\"missed\":3}\n" */ }
#[tokio::test] async fn truncated_body_reports_bytes_read() { /* 0x00 + header + B=8000 + 12 bytes + EOF -> Truncated{what:"body", got:12, want:8000} */ }
#[tokio::test] async fn limits_and_bad_starts_are_errors() { /* H=70_000 -> TooLong; first byte 0x41 -> UnexpectedByte(0x41); iq with 3 samples and 16 body bytes -> BodyMismatch; video -> KindNotCarried */ }
// tests/envelope.rs
#[test] fn header_is_plan3_ws_frame_json() { /* for an iq, audio, event and text Frame: our header JSON == serde_json::to_value(WsMessage::Frame(ws_frame(&f))); Lagged/Closed controls == WsMessage::Lagged/Closed JSON */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-plugin`.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run tests** — PASS.
- [ ] **Step 5: Commit** — `git add -A && git commit -m "feat(plugin): stdio wire protocol reusing the websocket frame envelope"`

---

### Task 13: `ExternalStage`, the host side

**Files:**
- Create: `crates/zeta-plugin/src/host.rs`, `crates/zeta-plugin/src/bin/fake.rs` (`[[bin]] name = "zeta-plugin-fake"`, a test double; the crate is never installed)
- Test: `crates/zeta-plugin/tests/host.rs`

**Interfaces:**
- Produces: `pub struct PluginCommand { pub program: PathBuf, pub args: Vec<String> }` (spawned directly, never through a shell); `pub struct ExternalStage`; `pub async fn probe(kind: &str, cmd: PluginCommand) -> Result<ExternalStage, PluginError>`: spawn, read `hello` within 5000 ms, require `protocol == 1`, `hello.kind == kind`, non-empty `inputs`, kinds in `{iq, audio, text, event}`, and a strict `params_schema` (`"type": "object"` and `"additionalProperties": false`), send `stop`, wait 2000 ms then kill; build `StageSpec { kind: Box::leak(kind.into()), inputs, outputs, params_schema: Schema::try_from(params_schema) }` (one leaked string per registered plugin, once at startup).
- `impl Stage for ExternalStage` (`backend() == Cpu`): spawn with `kill_on_drop(true)`; read `hello` again (must equal the probed spec, else `Failed("plugin <kind> changed its hello")`); send `run` with the params and the output stream ids; then:
  - a reader task decodes stdout into an `mpsc` (capacity 64); a writer task drains an `mpsc` (capacity 16) into stdin; the main loop never blocks on stdin, so a plugin that stops reading cannot deadlock the host: when the writer queue is full the input frame is dropped, counted, and `{"type":"lagged","missed":N}` is queued once there is room.
  - input `Ok(frame)` → queue its encoding; `Lagged(n)` → queue `lagged`; `Closed` → queue `closed`, close stdin, keep publishing outputs until stdout EOF, then exit status 0 → `Ok(())`, otherwise `Failed("plugin <kind> exited with status <s>")`.
  - output frame → validate: `stream` is one of the run's output ids and `kind` equals that output's kind, else `Failed("plugin <kind> sent a <sent> frame on stream <id> declared <declared>")` and nothing published; set provenance `source`/`job` to the run's (from the first input frame; the plugin cannot claim another job) and keep its `channel`/span; publish via `Frame::new`.
  - `error` control → `Failed("plugin <kind>: <message>")`; a wire error → `Failed("plugin <kind>: <error> (exit status <s>)")`; EOF before the input closed → `Failed("plugin <kind> exited unexpectedly (status <s>)")`.
  - cancel → queue `stop`, wait 2000 ms, kill → `Err(Cancelled)`.
- `zeta-plugin-fake --mode <m>` built on `wire`: `echo` (iq→iq, echoes frames), `crash-mid-frame` (after the first input frame writes `0x00`, a valid header promising 8000 body bytes and 12 of them, then exits 3), `wrong-kind` (declares outputs `[iq]`, answers with an audio frame), `no-hello` (sleeps 10 s), `loose-schema` (schema without `additionalProperties`), `error` (sends `{"type":"error","message":"boom"}`), `lag-report` (iq→event, emits `{"lagged":N}` on `lagged` and `{"samples":n}` per frame), `ignore-stop` (never exits on `stop`).

- [ ] **Step 1: Write the failing tests** (`env!("CARGO_BIN_EXE_zeta-plugin-fake")`)

```rust
#[tokio::test] async fn echo_plugin_passes_iq_through_unchanged() { /* probe("fake", echo) -> spec inputs [Iq] outputs [Iq]; run on a local broadcast; 3 blocks out == in exactly, provenance job = input's */ }
#[tokio::test] async fn plugin_crash_mid_frame_fails_stage_with_byte_counts() { /* Failed(m): m contains "plugin fake", "body", "12", "8000", "status 3"; bus received nothing from it */ }   // Review Focus 4
#[tokio::test] async fn plugin_wrong_kind_frame_fails_stage() { /* Failed("plugin fake sent a audio frame on stream <id> declared iq"); nothing published; bus.dropped()==0 */ }   // Review Focus 5
#[tokio::test] async fn probe_times_out_without_hello() { /* no-hello -> Err naming "hello" and "5000 ms" within 6 s; child killed */ }
#[tokio::test] async fn probe_rejects_loose_params_schema() { /* error names "additionalProperties" */ }
#[tokio::test] async fn probe_rejects_kind_mismatch() { /* probe("gain", echo whose hello says "fake") -> error names both */ }
#[tokio::test] async fn plugin_error_message_fails_stage() { /* "plugin fake: boom" */ }
#[tokio::test] async fn lagged_input_is_reported_to_plugin() { /* capacity-2 input flooded before the stage reads -> an event {"lagged":N} with N >= 1 */ }
#[tokio::test] async fn cancel_kills_a_plugin_that_ignores_stop() { /* ignore-stop: cancel -> Err(Cancelled) within 3 s; process gone */ }
#[tokio::test] async fn graph_validation_uses_plugin_schema() { /* registry with the echo plugin; graph params {"bogus":1} -> GraphError::BadParams before anything runs */ }
```

- [ ] **Step 2: Run to verify they fail**.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run tests** — `cargo test -p zeta-plugin` PASS, and no `zeta-plugin-fake` process remains (`pgrep -f zeta-plugin-fake` empty).
- [ ] **Step 5: Commit** — `git add -A && git commit -m "feat(plugin): external-process stages with crash and kind checks"`

---

### Task 14: Python SDK and the gain example

**Files:**
- Create: `sdk/python/zeta_plugin/__init__.py`, `examples/python/gain.py` (executable: `git update-index --chmod=+x`)
- Test: `crates/zeta-plugin/tests/python.rs`

**Interfaces:**
- Produces (`zeta_plugin`, stdlib only: `json`, `struct`, `sys`; module docstring = protocol 1 from Task 12, verbatim): `PROTOCOL = 1`; `class Frame` with attributes `kind`, `header` (dict), `samples` (`list[complex]` for iq, `list[float]` for audio, `None` otherwise), `payload` (event/text value), and `def with_samples(self, samples) -> "Frame"` (copy with new samples and `payload["samples"]` updated); `def run(kind, inputs, outputs, params_schema, on_frame, on_lagged=None)`: writes `hello` to `sys.stdout.buffer` and flushes, reads `run`, then reads messages from `sys.stdin.buffer` until EOF or `stop`; for each data frame calls `on_frame(params, frame)` which returns `None`, a `Frame` or a list of `Frame`s; each returned frame is sent on the first output whose kind matches (`header["stream"]` set from `run.outputs`); `lagged` calls `on_lagged(params, missed)`; `closed` or EOF returns after flushing; an exception in a callback sends `{"type":"error","message":"<ExceptionName>: <str>"}` and exits 1. IQ bodies are packed and unpacked with `struct` (`'<%df' % (2 * n)`).
- `examples/python/gain.py`: `#!/usr/bin/env python3`; prepends `os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "sdk", "python")` to `sys.path` (so it also runs under `python3 -I`); `kind="gain"`, inputs `["iq"]`, outputs `["iq"]`, `params_schema = {"type": "object", "properties": {"factor": {"type": "number"}}, "required": ["factor"], "additionalProperties": False}`; `on_frame` returns `frame.with_samples([s * params["factor"] for s in frame.samples])`.

- [ ] **Step 1: Write the failing tests** (each begins `if Command::new("python3").arg("--version").output().is_err() { eprintln!("skipped: python3 not found"); return; }`; the command is `python3 -I <repo>/examples/python/gain.py`)

```rust
#[tokio::test] async fn python_gain_plugin_scales_iq() { /* probe("gain", ..) -> spec kind "gain"; run with {"factor":0.5} on [1+2j, -0.5+0.25j] -> [0.5+1j, -0.25+0.125j] within 1e-6; t0, rate, centre unchanged */ }
#[tokio::test] async fn python_plugin_runs_in_a_job_via_graph_put() { /* registry() + the probed gain stage; Node with the tone file source; job_start passthrough; graph_put [gain {"factor":0.5}]; the job's iq output stream yields frames within 2 s whose samples equal 0.5 × the source's (core untouched: design 2's "integrate something weird") */ }
#[tokio::test] async fn python_exception_becomes_stage_failure() { /* a five-line script written to a tempdir that puts <repo>/sdk/python on sys.path and whose on_frame raises ValueError("bad block") -> Failed(m), m contains "plugin raiser" and "ValueError: bad block" */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-plugin --test python`.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run tests** — PASS here (Python 3 present); `PATH=/nonexistent cargo test -p zeta-plugin --test python` prints the skip lines and passes.
- [ ] **Step 5: Commit** — `git add -A && git commit -m "feat(plugin): stdlib python sdk and a gain example stage"`

---

### Task 15: CLI: one `SourceSpec`, Kiwi and Soapy flags, `--plugin`, agent guide

**Files:**
- Create: `crates/zeta/src/sources.rs`
- Modify: `crates/zeta/src/{cli.rs,node_setup.rs,main.rs,transport.rs,ask.rs,agents_md.rs}`, `crates/zeta/Cargo.toml` (deps `zeta-source-kiwi`, `zeta-source-soapy`, `zeta-plugin`; feature `soapy = ["zeta-source-soapy/driver"]`), `docs/agents/field-craft.md`
- Test: `crates/zeta/tests/cli.rs`, unit tests in `sources.rs`

**Interfaces:**
- Produces (`sources.rs`): `pub enum SourceSpec { File { id: String, path: PathBuf }, Soapy { id: String, args: String }, Kiwi { id: String, host: String, port: u16, password: Option<String> } }` with `pub fn id(&self) -> &str`; `pub fn parse_file(s) / parse_soapy(s) / parse_kiwi(s) -> Result<SourceSpec, String>` (`ID=PATH.sigmf-meta`; `ID=ARGS` split at the first `=`, `ARGS` a Soapy kwargs string such as `driver=hackrf,serial=…`; `ID=HOST:PORT[/PASSWORD]`, port required); `pub struct PluginSpec { pub kind: String, pub program: PathBuf }` with `parse_plugin` (`KIND=PATH`, kind matching `^[a-z][a-z0-9_]*$`).
- CLI globals: `--source-file`, `--source-soapy ID=ARGS`, `--source-kiwi ID=HOST:PORT[/PASSWORD]`, `--plugin KIND=PATH` (each repeatable); `Cli::sources(&self) -> Vec<SourceSpec>` in flag order by kind; `try_parse_checked` rejects an id used twice across all three kinds (`source id "x" given more than once`) and a plugin kind given twice. New subcommand `zeta soapy find [--filter ARGS]` (not an operation: device discovery is local to the machine) printing `soapy::find` as JSON; without the `soapy` feature it and `--source-soapy` exit 1 with `zeta was built without SoapySDR; rebuild with --features soapy (needs libsoapysdr-dev)`.
- `node_setup::open(state_dir: PathBuf, sources: &[SourceSpec], plugins: &[PluginSpec], resume: bool) -> Result<Node, String>`: probes each plugin (`PluginCommand { program, args: vec![] }`), refuses a kind that collides with a built-in stage (`plugin kind "channel" collides with a built-in stage`), registers them after `registry()`; builds `FileSource` (and `start()`s it after `Node::open` as today), `KiwiSource::new`, `SoapySource::open`; errors are prefixed with the flag, e.g. `--source-kiwi kiwi0=host:8073: …`, and never contain a password. `main.rs` and `ask.rs` pass `cli.sources()` and the plugins; `transport::dispatch` rejects `--source-soapy`, `--source-kiwi` and `--plugin` with `--node` exactly as it rejects `--source-file` (message: `--source-*, --plugin and --state-dir belong to the serving node; drop them with --node`).
- `agents_md.rs`, "Running the node": the flags list gains the three new flags with one line each; a new sentence: "`source list` shows each source's capability record: `remote` is true for a KiwiSDR, `push` false for SoapySDR devices, and `fault` names a source that has failed (a full Kiwi, an unplugged device); a job on a faulted source fails with that reason." `docs/agents/field-craft.md` gains "### Remote HF receivers (KiwiSDR)": 0–30 MHz, 12 kHz of IQ per connection, `bandwidth_hz` at most 12 000, a limited number of user slots (`too_busy`), the connection opens on the first tune and holds a slot until the node exits; and "### External stages": a plugin is a stage kind like any other, listed in `schema` under `stages`, used through `graph put`.

- [ ] **Step 1: Write the failing tests**

```rust
// sources.rs
#[test] fn parse_table() { /* "kiwi0=kiwisdr.local:8073" -> Kiwi{port 8073, password None}; "k=h:1/pw" -> Some("pw"); "k=h" -> Err "expected ID=HOST:PORT"; "hackrf0=driver=hackrf,serial=ab" -> Soapy{args "driver=hackrf,serial=ab"}; "gain=examples/python/gain.py" plugin ok; "Gain=x" -> Err */ }
// tests/cli.rs
#[test] fn source_list_shows_kiwi_capabilities_without_connecting() { /* --source-kiwi kiwi0=127.0.0.1:9 source list -> exit 0; value[..] id kiwi0 capabilities.remote true, push true, max_bandwidth_hz {"asserted":12000}, current null, fault null */ }
#[test] fn duplicate_source_id_across_kinds_rejected() { /* --source-file a=x --source-kiwi a=h:1 -> exit 2, stderr names "a" */ }
#[test] fn plugin_flag_registers_stage_in_schema() { /* skip without python3; --plugin gain=<repo>/examples/python/gain.py schema -> stages contains kind "gain" with params_schema.required ["factor"] */ }
#[test] fn plugin_kind_colliding_with_builtin_rejected() { /* --plugin channel=<gain.py> source list -> exit 1, stderr "collides" */ }
#[cfg(not(feature = "soapy"))] #[test] fn soapy_without_feature_is_explained() { /* --source-soapy h=driver=hackrf source list -> exit 1, stderr contains "--features soapy" */ }
#[test] fn node_mode_rejects_source_and_plugin_flags() { /* --node u --secret s --source-kiwi k=h:1 source list -> exit 1 */ }
#[test] fn job_start_on_fake_kiwi_measures() { /* FakeKiwi from zeta-source-kiwi test-fake (dev-dep); zeta --source-kiwi k=127.0.0.1:<port> job start --kind measure --source k --center-hz 7100000 --bandwidth-hz 10000 --then-tail 1,5000 -> exit 0, frames[0].payload has center_hz */ }
```

- [ ] **Step 2: Run to verify they fail**.
- [ ] **Step 3: Implement**; regenerate AGENTS.md and the skill.
- [ ] **Step 4: Run tests** — `cargo test --workspace` PASS; drift check PASS; `cargo build -p zeta --features soapy` succeeds when `libsoapysdr-dev` is installed (else record it as not run).
- [ ] **Step 5: Commit** — `git add -A && git commit -m "feat(cli): kiwi and soapy source flags, --plugin, one source spec; guide updated"`

---

### Task 16: Whole-workspace verification, README, plan series

**Files:**
- Modify: `README.md` (sections "Sources": file, KiwiSDR, SoapySDR with the `soapy` feature and its apt prerequisite, gains left at driver defaults; "External stages": protocol summary, the Python SDK, `gain.py`), `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (plan 4 **Delivered**, noting what is pending: HackRF hardware test, the `driver` build if `libsoapysdr-dev` was not installed)

- [ ] **Step 1: Run** `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` — PASS, no warnings; `cargo test -p zeta-source-soapy --features test-mock` and `cargo test -p zeta-source-kiwi --features test-fake` PASS; with `libsoapysdr-dev`: `cargo clippy -p zeta --features soapy --all-targets -- -D warnings` PASS.
- [ ] **Step 2: By hand, Kiwi**: `zeta --source-kiwi kiwi=kiwisdr.local:8073 source list` (remote true, current null); `zeta --source-kiwi kiwi=kiwisdr.local:8073 job start --kind measure --source kiwi --center-hz 7100000 --bandwidth-hz 10000 --then-tail 2,5000` prints two measure events with finite `snr_db` and `center_hz` near 7 100 000; the same with `--bandwidth-hz 20000` exits 2 with `out_of_range` `bandwidth_hz` max 12000; `cargo test -p zeta-source-kiwi --test live -- --ignored --nocapture` PASS. Check `http://kiwisdr.local:8073/status` before (users below users_max) and after (our slot released).
- [ ] **Step 3: By hand, Python plugin in a graph** (plan 3's server): generate the tone fixture (`cargo run -q -p zeta-source-file --features test-fixtures --example gen_tone -- scratch/fx`); `zeta --state-dir scratch/st --source-file tone=scratch/fx/tone.sigmf-meta --plugin gain=examples/python/gain.py serve --listen 127.0.0.1:7373 --secret s &`; `zeta --node http://127.0.0.1:7373 --secret s job start --kind passthrough --source tone --center-hz <c> --bandwidth-hz 12500`; write `scratch/gain-graph.json` (one node `{"id":"g","kind":"gain","params":{"factor":0.5}}`, no edges); `graph put --job <id> --file scratch/gain-graph.json`; `stream list` shows the job's iq stream; `stream tail --stream <id> --max-frames 3 --timeout-ms 3000` prints three iq summaries; stop the job and the server; `pgrep -f gain.py` is empty. Paste every output into the report.
- [ ] **Step 4: Commit** — `git commit -am "docs: README sources and external stages; plan series updated"`

---

## Self-review notes

- Spec coverage: design 2 anti-goals: no privileged backend (`soapysdr` optional in its own crate, `tungstenite` banned from core: Tasks 7, 10, 11), foreign frameworks as external processes over a socket-like stream with a Python SDK and "integrate something weird without changing core" as an executed test (Tasks 12–14, `python_plugin_runs_in_a_job_via_graph_put`); 3 footprints: the Pi channelises and ships narrowband, so jobs pick the narrowest exact channel rate rather than shipping the source rate (Task 1); 4.1 capability records with asserted ranges and `remote`/`push` per source, push (Kiwi) and pull (Soapy) behind one `Port`, remote as the normal case, v1 sources file/HackRF via SoapySDR/KiwiSDR (Tasks 8–11, 15); "another node" is plan 5; 4.3 provenance kept across the process boundary (Task 13); 4.5 classifiers given an explicit band (Task 3); 4.8 in-process plugins and external processes (Tasks 12–14); 5.1 strict schemas, units, corrective errors (Tasks 1, 2, 15); 8 item 1 "a local HackRF and a remote KiwiSDR at once" is wired (both flags on one node) and acceptance-tested for Kiwi live, HackRF pending hardware; item 7 (Pi) gets rate-aware narrowband jobs here, the Pi build in plan 5.
- Plan-2 carry-overs: (a) Task 1, (b) Task 3, (c) Task 4, (d) Task 2, (e) Task 1, (f) Task 5, (g) Task 6.
- Review Focus items each have a named test: 1 → Task 9 (two tests), 2 → Task 9 (two), 3 → Task 11, 4 → Task 13, 5 → Task 13, 6 → Task 9 and Task 1.
- Names used across tasks checked: `channel_rate_hz`, `CHANNEL_RATE_MIN_HZ`, `FM_DEVIATION_DEFAULT_HZ`, `JobKind::expand(source, tuning, source_rate_hz, deviation_hz) -> Result<Graph, OpError>`, `Op::JobStart.deviation_hz`, `Band`, `Band::whole`, `Port::fault`, `SourceInfo.fault`, `Input::{Frame, Lagged}`, `KiwiConfig`, `KiwiSource`, `FakeKiwi`, `FakeScript`, `FakeReply`, `parse_snd`, `parse_msg`, `cmd_mod_iq`, `SoapyDevice`, `SoapyRx`, `RangeHz`, `WireFormat`, `ReadError`, `MockDevice`, `MockRead`, `pick_rate`, `pick_format`, `SoapySource::{with_device, open, find}`, `PROTOCOL`, `Control`, `Hello`, `Run`, `read_message`, `encode_frame`, `PluginCommand`, `ExternalStage`, `probe`, `SourceSpec`, `PluginSpec`.
- Corrections to the brief, from the sources: Kiwi IQ samples are big-endian `i16` (little-endian only with `SND_FLAG_LITTLE_ENDIAN`); the S-meter is big-endian; IQ frames carry 10 GPS bytes before the samples; `badp=0` means success; the reference URL has no `/kiwi` prefix (both work on v1.837); `sample_rate` (11998.902088 live) and `audio_rate` (12000) differ, and blocks use `audio_rate`.
- Not built, on purpose: Kiwi wideband (`/wb/<TS>/SND`; the reference only says "if supported", the live Kiwi is `rx8_wf2`, and no rate for it appears in the clone, so 20.25 kHz is unverified); Kiwi transverter offsets (refused); a Unix-socket plugin transport (stdio only; a socket would reuse the same framing and add plan 3's bearer secret); gain control and offset tuning to dodge the HackRF DC spike (a job centred exactly on the tuned frequency sees the DC spike); measured capability claims for Soapy (only `measured_rate_hz()` is exposed).
- Risks the implementer should watch: AFSK at 12 kHz (10 samples per bit) may need demodulator work (Task 1 test); the Kiwi passband limit for `iq` is assumed to be ±6000 Hz; `soapysdr-sys` may also need `libclang-dev`; one-shot CLI commands that tune a Kiwi hold a slot only for the life of that process.
