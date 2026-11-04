# Plan 19.1: Explore (features, the register, burst captures) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An unknown signal is reported with honest burst-level features and the licence register's nearest assignments, and the watch's explore policy keeps a few of its bursts on disk, visible on the page.

**Architecture:** Layer 1 gains burst gating and modulation-order estimates inside `measure`; a node-level station (position plus antennas) feeds a new `acma` knowledge provider in `zeta-ext` that calls the operator's acma-rrl-server fork over MCP streamable HTTP; `identify`, `classify` and the watch consult it. A `burst_capture` stage writes SigMF windows into a `BurstStore` under the state directory, started by a new `capture` job kind or by the watch's `on_unknown: capture` policy; four `capture_*` ops and a `GET /captures/{file}` route expose them, and the page gets a pane tab strip (Subjects moved into it), an Explore pane, a cached-region bracket and the `too_old` retry.

**Tech Stack:** Rust (tokio, serde, schemars, reqwest in zeta-ext, axum in zeta-ws), the existing synthesiser families for fixtures, vanilla JS page with the Playwright e2e suite.

**Spec:** `docs/superpowers/specs/2026-11-02-layer4-explore-design.md`, sections 1-5, 8, 10, 11. Sections 6 and 7 are plans 19.2 and 19.3; nothing here starts them.

## Global Constraints

- Every op field is required and unknown fields are rejected (`deny_unknown_fields` on `Op`, `OpResult`, `OpError`); a nullable field uses `#[serde(deserialize_with = "crate::modes::nullable")]` plus `#[schemars(required, schema_with = "crate::types::nullable::<T>")]` as `MixStart.sources` does (`crates/zeta-core/src/ops.rs:192`).
- New params on existing stages and new fields on persisted records are `#[serde(default)]` so stored graphs and records keep loading; `FeatureRecord` new fields are `Option` with `#[serde(default)]` and `#[schemars(required)]`.
- `AGENTS.md` and `skills/zetasdr/SKILL.md` are generated: after any op, flag or note change run `cargo run -p zeta -- agents-md --check` (or the project's equivalent in `crates/zeta/src/agents_md.rs` `run(check, skill)`), regenerate, and commit the result in the same commit.
- Node tests that need synthesised IQ use `zeta_dsp::synth` and `write_cf32` with `FileSource::open` as `crates/zeta-core/tests/watch.rs:57-131` does; no live hardware in any test except the `#[ignore]` noise test of Task 12.
- The node runs, and every test passes, with no `--knowledge-acma`, no station and no network.
- `cargo fmt` per member (the nested worktree breaks `cargo fmt --all`), `cargo clippy --workspace --all-features -- -D warnings`, and the gate `cargo test --workspace --all-features --no-fail-fast` green with no `ui e2e skipped` line, before the plan is called done.
- Commits go on this branch (`worktree-plan19-spec`); one commit per task, message in the repo's style (`Area: what changed`), ending with `Claude-Session: https://claude.ai/code/session_015Zq4JYGWs3wEUS5jo6UWcn`.
- Numbers from the spec are named constants, never literals in logic.

## Models per task

Opus where the task touches the lease, supervisor or stage runtime, DSP estimators tuned against synthesised captures, or an interface several later tasks consume; Sonnet where the brief above is complete and the test pins the behaviour.

| Task | Model | Why |
|---|---|---|
| 1 synth gap_ms | Sonnet | a field threaded through three arms |
| 2 burst gating and the cyclostationary rate | Opus | DSP estimators with tolerance tests |
| 3 FSK/PSK order, labels | Opus | DSP rules ordered against each other |
| 4 station record and ops | Sonnet | an op end to end on the existing pattern |
| 5 sondehub defaults | Sonnet | small |
| 6 emission and reach | Sonnet | pure functions with tables |
| 7 MCP client | Sonnet | protocol pinned by the stub tests |
| 8 acma provider | Opus | the query, reach cut and error paths together |
| 9 flag and guide | Sonnet | |
| 10 identify/classify/watch consult the register | Opus | three call sites and the trial re-centring in the identify loop |
| 11 classify slice | Sonnet | |
| 12 detector | Opus | thresholds tuned against noise and the sonde |
| 13 BurstStore, burst_capture, capture kind | Opus | a store, a stage, a kind and the job finish path |
| 14 capture ops and route | Sonnet | |
| 15 watch explore policy | Opus | the watch loop and owned jobs |
| 16 bursts flags | Sonnet | |
| 17 pane tab strip | Opus | reshapes the page's layout and the plan-23 scenarios |
| 18 Explore pane | Sonnet | |
| 19 bracket and too_old | Sonnet | |
| 20 guide, TODO, series, gate | Sonnet | |

Ledger: `scratch/plan19.1-ledger.md`, one line per task with the implementer's and reviewer's verdicts.

## Review Focus

Inputs the spec implies but no obvious test exercises; each gets a test in the task that owns the code:

1. A continuous signal (duty 1.0) through the burst gate must measure exactly as before the change: an operator's existing NBFM measurement must not move. (Task 2: `a_continuous_signal_measures_as_before`.)
2. A capture window that straddles the end of an IQ block, or a burst still on at `window_ms`, must be cut at `window_ms` with the `post_ms` guard, not dropped or doubled. (Task 13: `a_burst_longer_than_the_window_is_cut_into_windows`.)
3. The register answering an `{"error"}` JSON-RPC body, a non-JSON body, or hanging past 5 s must each leave `knowledge_lookup` answering what the other providers give, with one log line. (Task 8: `a_service_error_a_bad_body_and_a_timeout_each_give_no_allocations`.)
4. A station whose declared antenna bands leave a gap (no antenna covers the band) must fall back to the kind's default, not refuse the lookup. (Task 6: `a_band_no_antenna_covers_takes_the_default_for_its_range`.)
5. `capture_delete` of a pinned capture, and expiry reaching a pinned one, must both leave it in place and say so. (Task 13: `expire_never_removes_a_pinned_capture`; Task 14: `capture_delete_refuses_a_pinned_capture`.)

---

### Task 1: `gap_ms` on the synthesiser's burst families

**Files:**
- Modify: `crates/zeta-dsp/src/synth.rs` (`SynthSpec::Fsk/Psk/Ook` at 990-1010, `generic_params` 933, `repeat_bursts` 881, the three `synthesise` arms 1285/1298/1310, `validate` 1124-1155)
- Test: `crates/zeta-dsp/src/synth.rs` tests module

**Interfaces:**
- Produces: `SynthSpec::Fsk { symbol_rate_baud, deviation_hz, order, bits, repeat, gap_ms: u64 }` (likewise `Psk`, `Ook`), `#[serde(default = "default_gap_ms")]` giving `BURST_GAP_MS` (50); `fn repeat_bursts(burst, repeat, gap_ms, rate_hz)`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn gap_ms_spaces_the_bursts() {
    let spec = SynthSpec::Fsk { symbol_rate_baud: 4800, deviation_hz: 4000.0, order: 2,
        bits: "ff00".into(), repeat: 3, gap_ms: 995 };
    let iq = synthesise(&spec, &Impairments::default(), 48_000, 0);
    // 3 bursts of 16 bits at 4800 Bd = 3.33 ms each, two gaps of 995 ms
    let burst = 16 * 48_000 / 4800;
    assert_eq!(iq.len(), 3 * burst + 2 * 995 * 48);
    assert!(iq[burst + 48 * 500].norm() == 0.0, "the gap is silent");
}

#[test]
fn gap_ms_defaults_to_50_and_is_bounded() {
    let (spec, _) = parse_params(json!({"family":"ook","symbol_rate_baud":1200,"bits":"a5","repeat":2}).as_object().unwrap().clone()).unwrap();
    assert!(matches!(spec, SynthSpec::Ook { gap_ms: 50, .. }));
    let err = parse_params(json!({"family":"ook","symbol_rate_baud":1200,"bits":"a5","repeat":2,"gap_ms":60001}).as_object().unwrap().clone()).unwrap_err();
    assert!(err.starts_with("gap_ms: "), "{err}");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p zeta-dsp --features test-fixtures gap_ms -- --nocapture`
Expected: FAIL to compile (no field `gap_ms`).

- [ ] **Step 3: Add the field and thread it**

Add `gap_ms: u64` with `#[serde(default = "default_gap_ms")]` to the three variants; `generic_params` checks `0..=60_000` (`"gap_ms: 0..=60000"`); `repeat_bursts` takes `gap_ms`; the doc comments say "`repeat` bursts `gap_ms` apart (50 by default)".

- [ ] **Step 4: Run the synth tests**

Run: `cargo test -p zeta-dsp --features test-fixtures synth`
Expected: PASS, including the existing `rates_follow_the_symbol_rate` and `bounds_are_errors_naming_the_field`.

- [ ] **Step 5: Commit**

```bash
git add crates/zeta-dsp/src/synth.rs
git commit -m "Synth: gap_ms on the fsk, psk and ook families (50 ms default) so a 1 % duty burst is synthesisable"
```

---

### Task 2: Burst-gated `measure`: duty, bursts, burst_ms, period_ms, the cyclostationary symbol rate

**Files:**
- Modify: `crates/zeta-core/src/classify.rs:13-23` (`FeatureRecord`)
- Modify: `crates/zeta-dsp/src/measure.rs` (`measure_band_masked` 382-456, `symbol_rate` 306, `layer1_label` evidence 516-528, constants 24-91)
- Test: `crates/zeta-dsp/src/measure.rs` tests module

**Interfaces:**
- Produces: `FeatureRecord { bandwidth_hz, center_hz, symbol_rate_baud, snr_db, burst, duty: Option<f32>, bursts: Option<u32>, burst_ms: Option<f32>, period_ms: Option<f32>, order: Option<u8>, tone_spacing_hz: Option<Hz> }` (the last two filled by Task 3; `None` here).
- Produces in `measure.rs`: `pub struct Bursts { pub runs: Vec<(usize, usize)>, pub duty: f32, pub burst_ms: Option<f32>, pub period_ms: Option<f32> }`; `pub fn burst_runs(iq: &[Complex<f32>], rate_hz: u64) -> Bursts`; `pub fn on_time(iq: &[Complex<f32>], b: &Bursts) -> Vec<Complex<f32>>`; `fn cyclo_symbol_rate(iq: &[Complex<f32>], rate_hz: u64, max_hz: f64) -> Option<Baud>`.
- Constants: `BURST_ENVELOPE_MS = 1`, `BURST_ON_DB = 6.0`, `BURST_MERGE_MS = 2`, `BURST_MIN_MS = 1`, `BURST_DUTY_MAX = 0.5`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_1_percent_duty_burst_reports_duty_period_and_band() {
    // 5 ms burst (24 bits at 4800 Bd) every 1000 ms, 3 bursts = 2 s of signal
    let spec = SynthSpec::Fsk { symbol_rate_baud: 4800, deviation_hz: 4000.0, order: 2,
        bits: "a5c3f0".into(), repeat: 3, gap_ms: 995 };
    let iq = synthesise(&spec, &Impairments { snr_db: Some(20.0), ..Default::default() }, 48_000, 0);
    let f = measure_band(&iq, 48_000, CENTER, 20_000);
    let duty = f.duty.unwrap();
    assert!((0.003..=0.007).contains(&duty), "duty {duty}");
    assert_eq!(f.bursts, Some(3));
    assert!((980.0..=1020.0).contains(&f.period_ms.unwrap()));
    assert!((4.0..=7.0).contains(&f.burst_ms.unwrap()));
    assert!(f.burst);
    // occupied band ≈ 2·deviation + rate = 12.8 kHz, within 10 %
    assert!((11_500..=14_100).contains(&f.bandwidth_hz.0), "bw {}", f.bandwidth_hz.0);
    let baud = f.symbol_rate_baud.unwrap().0;
    assert!((4700..=4900).contains(&baud), "baud {baud}");
}

#[test]
fn the_cyclostationary_line_finds_psk_and_ook_rates() {
    for spec in [
        SynthSpec::Psk { symbol_rate_baud: 2400, order: 2, bits: "3c5a96".repeat(20), repeat: 1, gap_ms: 50 },
        SynthSpec::Ook { symbol_rate_baud: 1200, bits: "a5".repeat(60), repeat: 1, gap_ms: 50 },
    ] {
        let iq = synthesise(&spec, &Impairments { snr_db: Some(10.0), offset_hz: 500.0, ppm: 20.0, ..Default::default() }, 48_000, 0);
        let f = measure_band(&iq, 48_000, CENTER, 20_000);
        let want = match spec { SynthSpec::Psk { symbol_rate_baud, .. } | SynthSpec::Ook { symbol_rate_baud, .. } => symbol_rate_baud, _ => unreachable!() };
        let got = f.symbol_rate_baud.expect("a rate").0 as f64;
        assert!((got - want as f64).abs() <= want as f64 * 0.02, "{spec:?}: {got}");
    }
}

#[test]
fn a_continuous_signal_measures_as_before() {
    let iq = nbfm_modulate(&tone(1000.0, RATE, 48_000), 5000.0, RATE);
    let f = measure_band(&iq, RATE, CENTER, 20_000);
    assert_eq!(f.duty, Some(1.0));
    assert_eq!(f.bursts, Some(1));
    assert_eq!(f.period_ms, None);
    assert!(!f.burst);
    // the same band and label the existing measure_nbfm_tone_reports_band_and_fm asserts
    assert!((8_000..=16_000).contains(&f.bandwidth_hz.0));
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p zeta-dsp --features test-fixtures measure::tests::a_1_percent -- --nocapture`
Expected: FAIL (no field `duty`).

- [ ] **Step 3: Extend `FeatureRecord`**

Add the six `Option` fields in `classify.rs` with `#[serde(default)]` and `#[schemars(required)]`; fix every struct literal (`measure_band_masked`, `zeta-dsp/tests`, `zeta-core/tests/ops.rs` if any) with `..` or explicit `None`.

- [ ] **Step 4: Implement `burst_runs`, `on_time`, `cyclo_symbol_rate` and gate the measurement**

`burst_runs`: power per `BURST_ENVELOPE_MS` chunk; floor = 15th percentile of chunk power; a chunk is on when `10·log10(p/floor) > BURST_ON_DB`; merge runs closer than `BURST_MERGE_MS`, drop shorter than `BURST_MIN_MS`; `duty` = on samples / total; `burst_ms` = median run length; `period_ms` = median on-edge to on-edge, `None` under two runs. In `measure_band_masked`: compute `b = burst_runs`; when `b.duty < BURST_DUTY_MAX` measure the spectrum, SNR, occupied band and symbol rate over `on_time(iq, &b)` (each run Hann-windowed, concatenated; if the concatenation is under `SIZE` samples, zero-pad to `SIZE`), else over the whole window; `burst = duty < BURST_DUTY_MAX || is_burst(..)`. `cyclo_symbol_rate`: `real_spectrum` of `|x|²` minus its mean, the strongest line at least `LINE_DB` over its local median in `MIN_LINE_HZ..=max_hz` (`max_hz` = occupied bandwidth), parabola-refined; `symbol_rate_baud = cyclo_symbol_rate(..).or_else(|| symbol_rate(..))`; the evidence string names `cyclo` or `keying`. Add `duty {duty:.3}` to the evidence.

- [ ] **Step 5: Run the whole measure module**

Run: `cargo test -p zeta-dsp --features test-fixtures measure`
Expected: PASS, all existing tests included (`measure_afsk_packet_reports_fsk_and_1200_baud` still passes with the fallback).

- [ ] **Step 6: Commit**

```bash
git add crates/zeta-core/src/classify.rs crates/zeta-dsp/src/measure.rs
git commit -m "Measure: burst-gated layer 1 (duty, bursts, burst_ms, period_ms) and the cyclostationary symbol rate"
```

---

### Task 3: FSK order and tone spacing, PSK order, the `psk` and `ook` labels

**Files:**
- Modify: `crates/zeta-dsp/src/measure.rs` (`classify_block` 632, `tone_pair` 462, record construction ~449)
- Test: `crates/zeta-dsp/src/measure.rs` tests module

**Interfaces:**
- Produces: `fn fsk_clusters(iq: &[Complex<f32>], rate_hz: u64, bandwidth_hz: u64) -> Option<(u8, Hz)>`; `fn psk_order(iq: &[Complex<f32>], rate_hz: u64, baud: Baud) -> Option<u8>`; labels `psk` (0.6, reason `"a line at {m}× the symbol rate in x^{m}"`) and `ook` (0.6, reason `"two envelope levels with a symbol-rate line"`); `fsk` keeps its name, record gets `order` and `tone_spacing_hz`.
- Constants: `FSK_CLUSTER_MIN_WEIGHT = 0.1` (a cluster holds at least this share of on-time samples), `PSK_ORDERS: [u8; 3] = [2, 4, 8]`, `PSK_LINE_DB = 10.0`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn fsk_order_and_tone_spacing_are_measured() {
    for (order, dev, want_spacing) in [(2u8, 4000.0, 8000u64), (4u8, 6000.0, 4000u64)] {
        let spec = SynthSpec::Fsk { symbol_rate_baud: 2400, deviation_hz: dev, order, bits: "3c5a96f0".repeat(16), repeat: 1, gap_ms: 50 };
        let iq = synthesise(&spec, &Impairments { snr_db: Some(20.0), ..Default::default() }, 48_000, 0);
        let f = measure_band(&iq, 48_000, CENTER, 24_000);
        let l = layer1_label(&f, &iq, 48_000);
        assert_eq!(l.name, "fsk", "{order}-fsk: {}", l.evidence);
        assert_eq!(f.order, Some(order));
        let s = f.tone_spacing_hz.unwrap().0;
        assert!((s as f64 - want_spacing as f64).abs() < 600.0, "spacing {s}");
    }
}

#[test]
fn psk_order_is_the_smallest_power_with_a_line() {
    for order in [2u8, 4u8] {
        let spec = SynthSpec::Psk { symbol_rate_baud: 2400, order, bits: "3c5a96f0e1".repeat(16), repeat: 1, gap_ms: 50 };
        let iq = synthesise(&spec, &Impairments { snr_db: Some(20.0), ..Default::default() }, 48_000, 0);
        let f = measure_band(&iq, 48_000, CENTER, 12_000);
        let l = layer1_label(&f, &iq, 48_000);
        assert_eq!(l.name, "psk", "{order}-psk: {}", l.evidence);
        assert_eq!(f.order, Some(order));
    }
}

#[test]
fn ook_is_labelled_ook() {
    let spec = SynthSpec::Ook { symbol_rate_baud: 1200, bits: "a5".repeat(60), repeat: 1, gap_ms: 50 };
    let iq = synthesise(&spec, &Impairments { snr_db: Some(20.0), ..Default::default() }, 48_000, 0);
    let f = measure_band(&iq, 48_000, CENTER, 8_000);
    assert_eq!(layer1_label(&f, &iq, 48_000).name, "ook");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p zeta-dsp --features test-fixtures measure::tests::fsk_order`
Expected: FAIL (`order` is `None`; label mismatch).

- [ ] **Step 3: Implement**

`fsk_clusters`: instantaneous frequency from the discriminator over on-time (Task 2's `on_time`), histogram at `bin = bandwidth_hz / 64` bins across ± bandwidth/2, smoothed by a 3-bin mean; clusters are local maxima holding at least `FSK_CLUSTER_MIN_WEIGHT` of the samples; 2 or 4 clusters give `(order, spacing)` where spacing is the median adjacent distance; else `None`. `psk_order`: for `m` in `PSK_ORDERS`, `real_spectrum` of `x^m` has a line within 2 % of `m × baud` standing `PSK_LINE_DB` over its local median; first `m` wins. Rule order in `classify_block`: noise → fsk (clusters first, then the existing two-peak rule; on a hit set `order`, `tone_spacing_hz` on the record) → ook (two envelope levels: `envelope_cv` above `AM_*` thresholds with a symbol-rate line and a histogram of the envelope with two modes) → wfm pilot → wfm → am → ssb → psk (steady envelope, `psk_order` is `Some`) → low-index am → carrier/fm → unknown. The record's `order` is set from whichever rule proved.

- [ ] **Step 4: Run the measure module and the layer-1 integration tests**

Run: `cargo test -p zeta-dsp --features test-fixtures measure && cargo test -p zeta-dsp --features test-fixtures --test layer1`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/zeta-dsp/src/measure.rs
git commit -m "Measure: FSK order and tone spacing, PSK order by the m-th power test, psk and ook labels"
```

---

### Task 4: The station record, its ops and CLI

**Files:**
- Create: `crates/zeta-core/src/station.rs`
- Modify: `crates/zeta-core/src/lib.rs` (export), `crates/zeta-core/src/node/mod.rs` (`Inner` field, load in `open_with_modes` before `restore_gains`, `station()`, `set_station`, `clear_station`), `crates/zeta-core/src/ops.rs` (`Op::StationSet/StationGet/StationClear`, `OpResult::Station`, `name()`, `read_only()` for get, `apply`), `crates/zeta-core/src/stage.rs:80` (`StageContext.station: Option<Station>`), `crates/zeta-core/src/node/launch.rs:207` (fill it)
- Modify: `crates/zeta/src/cli.rs` (`Command::Station(StationCmd)`, `StationCmd::{Set, Get, Clear}`, `mutates()` for set/clear, `into_op`), `crates/zeta/src/agents_md.rs` (`note("station_set")`, `cli_form` for `station set`)
- Test: `crates/zeta-core/tests/station.rs` (new), `crates/zeta/tests/cli.rs`, `crates/zeta/tests/agents_md.rs`

**Interfaces:**
- Produces: `pub struct Station { pub lat: f64, pub lon: f64, pub alt_m: f64, pub region: String, pub antennas: Vec<Antenna> }`; `pub struct Antenna { pub id: String, pub kind: AntennaKind, pub min_hz: Hz, pub max_hz: Hz, pub height_m: f64, pub azimuth_deg: Option<f64>, pub elevation_deg: Option<f64> }`; `pub enum AntennaKind { Discone, Dipole, Skyloop, Vertical, Yagi, Dish, Other }` (snake_case); `Station::default_antennas(alt_m) -> Vec<Antenna>` (`discone` 30 MHz-6 GHz and `dipole` 0-30 MHz, both `height_m` 5.0, ids `discone` and `dipole`); `Station::antenna_for(&self, hz: Hz) -> Antenna` (the first declared antenna whose band holds `hz`, else the default for that range); `Station::validate() -> Result<(), String>` (lat -90..=90, lon -180..=180, `min_hz < max_hz`, azimuth 0..360, elevation 0..=90, unique ids; errors `antennas[i].<field>: ...`).
- Ops: `Op::StationSet { lat: f64, lon: f64, alt_m: f64, region: String, antennas: Option<Vec<Antenna>> }` (nullable: `null` is the defaults), `Op::StationGet`, `Op::StationClear`; `OpResult::Station(Option<Station>)` (wire name `station`). CLI: `zeta station set --lat L --lon L --alt-m M [--region AU] [--antenna KIND=MIN_HZ-MAX_HZ[,height_m=N,azimuth_deg=N,elevation_deg=N]]...`, `zeta station get`, `zeta station clear`.
- Node: `pub fn station(&self) -> Option<Station>`, `pub async fn set_station(&self, s: Station) -> Result<Station, NodeError>`, `pub async fn clear_station(&self) -> Result<(), NodeError>`; file `state_dir/station.json`, written atomically like `persist_source`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/zeta-core/tests/station.rs
#[tokio::test]
async fn station_set_persists_and_get_reads_it_after_reopen() {
    let dir = tempdir();
    let node = open(&dir).await;
    let r = apply(&node, Op::StationSet { lat: -27.3842, lon: 153.1175, alt_m: 4.0, region: "AU".into(), antennas: None }).await.unwrap();
    let OpResult::Station(Some(s)) = r else { panic!("{r:?}") };
    assert_eq!(s.antennas.len(), 2);
    assert_eq!(s.antennas[0].kind, AntennaKind::Dipole);
    drop(node);
    let node = open(&dir).await;
    assert_eq!(node.station().unwrap().lat, -27.3842);
    assert!(matches!(apply(&node, Op::StationClear).await.unwrap(), OpResult::Station(None)));
    assert!(matches!(apply(&node, Op::StationGet).await.unwrap(), OpResult::Station(None)));
}

#[tokio::test]
async fn a_bad_antenna_is_refused_naming_the_field() {
    let node = open(&tempdir()).await;
    let a = Antenna { id: "y".into(), kind: AntennaKind::Yagi, min_hz: Hz(144_000_000), max_hz: Hz(100), height_m: 10.0, azimuth_deg: Some(400.0), elevation_deg: None };
    let e = apply(&node, Op::StationSet { lat: 0.0, lon: 0.0, alt_m: 0.0, region: "AU".into(), antennas: Some(vec![a]) }).await.unwrap_err();
    assert!(matches!(e, OpError::Other { ref message } if message.starts_with("antennas[0].")), "{e:?}");
    assert!(node.station().is_none());
}

#[test]
fn antenna_for_takes_the_declared_one_else_the_default() {
    let mut s = Station { lat: 0.0, lon: 0.0, alt_m: 10.0, region: "AU".into(), antennas: Station::default_antennas(10.0) };
    s.antennas.push(Antenna { id: "beam".into(), kind: AntennaKind::Yagi, min_hz: Hz(430_000_000), max_hz: Hz(440_000_000), height_m: 12.0, azimuth_deg: Some(90.0), elevation_deg: None });
    assert_eq!(s.antenna_for(Hz(435_000_000)).id, "beam");
    assert_eq!(s.antenna_for(Hz(404_025_000)).kind, AntennaKind::Discone);
    assert_eq!(s.antenna_for(Hz(7_100_000)).kind, AntennaKind::Dipole);
}
```

And in `crates/zeta/tests/cli.rs`: `station_set_get_clear_round_trip` runs `zeta station set --lat -27.38 --lon 153.12 --alt-m 4 --antenna yagi=430000000-440000000,azimuth_deg=90` then `zeta station get` and asserts the JSON has `"antennas"` with 3 entries, then `zeta station clear` gives `{"result":"station","value":null}`. In `agents_md.rs`: `station_set_is_in_the_guide` with needles ``"CLI: `zeta station set --lat"`` and `"\"op\":\"station_set\""`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p zeta-core --test station`
Expected: FAIL to compile.

- [ ] **Step 3: Implement `station.rs`, the node methods, the ops, the CLI, the guide note**

The guide note says: a node-level position and antennas, set once; used by the ACMA provider's distance and reach, `sink_sondehub`'s defaults and the page's home; `antennas: null` for the defaults.

- [ ] **Step 4: Run the tests and regenerate the guide**

Run: `cargo test -p zeta-core --test station && cargo test -p zeta --test cli station && cargo run -p zeta -- agents-md --check || cargo run -p zeta -- agents-md`
Expected: PASS; AGENTS.md diff shows the three ops.

- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core/src/station.rs crates/zeta-core/src/lib.rs crates/zeta-core/src/node/mod.rs crates/zeta-core/src/ops.rs crates/zeta-core/src/stage.rs crates/zeta-core/src/node/launch.rs crates/zeta-core/tests/station.rs crates/zeta/src/cli.rs crates/zeta/src/agents_md.rs crates/zeta/tests AGENTS.md skills/zetasdr/SKILL.md
git commit -m "Station: a node-level position, region and antennas (station set/get/clear), persisted and in the stage context"
```

---

### Task 5: `sink_sondehub` takes the station as its default position

**Files:**
- Modify: `crates/zeta-ext/src/sink_sondehub.rs:46-65` (`SondehubParams.lat/lon/alt_m` become `Option<f64>` with `#[serde(default)]`), `check_params` (:208), `run` (:225)
- Modify: `crates/zeta-core/src/graph.rs:92` (`check_sondehub` passes; the station check happens at run)
- Test: `crates/zeta-ext/tests/sink_sondehub.rs` (or the existing sondehub test file: find with `grep -rl sink_sondehub crates/*/tests`)

**Interfaces:**
- Consumes: `StageContext.station` (Task 4).
- Produces: a `sink_sondehub` with `lat`/`lon`/`alt_m` omitted uses the station's; with none set the stage fails `StageError::Failed("sink.params.lat: no station set (station set --lat --lon --alt-m)")`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn an_omitted_position_takes_the_station() { /* ctx.station = Some(..); params {callsign} only; one batched PUT body carries uploader_position == [lat, lon, alt_m] */ }

#[tokio::test]
async fn an_omitted_position_without_a_station_fails_naming_the_field() { /* run returns Err(Failed(m)) with m starting "sink.params.lat: no station set" */ }
```

Use the existing stub-server pattern of the sondehub tests for the PUT.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p zeta-ext sondehub`
Expected: FAIL (the params are required).

- [ ] **Step 3: Implement**

`check_params` keeps the range checks when present; `run` resolves `lat.or(station.lat)` etc. before building the client.

- [ ] **Step 4: Run the sondehub tests and the watch tests that use the sink**

Run: `cargo test -p zeta-ext sondehub && cargo test -p zeta-core --features test-fixtures --test watch sondehub`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/zeta-ext/src/sink_sondehub.rs crates/zeta-core/src/graph.rs crates/zeta-ext/tests
git commit -m "SondeHub sink: lat, lon and alt_m default to the station's"
```

---

### Task 6: Emission designator parser and the reach rule

**Files:**
- Create: `crates/zeta-core/src/emission.rs`, `crates/zeta-core/src/reach.rs`
- Modify: `crates/zeta-core/src/lib.rs` (exports)
- Test: unit tests in both files

**Interfaces:**
- Produces: `pub struct Emission { pub bandwidth_hz: Hz, pub modulation: char, pub modulation_name: &'static str, pub nature: char, pub nature_name: &'static str, pub info: char, pub info_name: &'static str }` (Serialize, JsonSchema); `pub fn parse_emission(code: &str) -> Option<Emission>` (trims, uppercases; the bandwidth is `NNNx` with `x` in `H K M G` as the decimal point; the three letters from the ITU tables; anything else `None`).
- Produces: `pub enum ReachClass { Ground, Skywave, Tropo, Uhf, Microwave, Space }` (snake_case); `pub struct Reach { pub km: Option<f64>, pub class: ReachClass, pub antenna: String, pub note: Option<String> }`; `pub fn reach_for(center_hz: Hz, antenna: &Antenna, space_band: bool, override_km: Option<f64>) -> Reach`; `impl Reach { pub fn km_toward(&self, bearing_deg: f64, antenna: &Antenna) -> Option<f64> }` (doubles inside ±`SECTOR_DEG` of `azimuth_deg`, halves outside, for a directional antenna; unchanged otherwise).
- Constants: `REACH_GROUND_KM = 300.0`, `REACH_TROPO_KM = 300.0`, `REACH_UHF_KM = 150.0`, `REACH_MICROWAVE_KM = 60.0`, `SECTOR_DEG = 30.0`, `SPACE_ELEVATION_DEG = 20.0`, band edges `GROUND_MAX_HZ = 2_000_000`, `SKYWAVE_MAX_HZ = 30_000_000`, `TROPO_MAX_HZ = 300_000_000`, `UHF_MAX_HZ = 1_000_000_000`.

- [ ] **Step 1: Write the failing tests**

```rust
// emission.rs
#[test]
fn decodes_16k0f1d_like_the_register_service() {
    let e = parse_emission("16K0F1D  ").unwrap();
    assert_eq!(e.bandwidth_hz, Hz(16_000));
    assert_eq!((e.modulation, e.nature, e.info), ('F', '1', 'D'));
    assert_eq!(e.modulation_name, "Frequency modulation");
    assert_eq!(e.info_name, "Data transmission, telemetry, telecommand");
}
#[test]
fn decodes_the_table_of_codes() {
    for (code, bw, m) in [("16K0F2D", 16_000, 'F'), ("25K0G7W", 25_000, 'G'), ("2K80J3E", 2_800, 'J'), ("6K00A3E", 6_000, 'A'), ("1M25F9W", 1_250_000, 'F'), ("500HA1A", 500, 'A')] {
        let e = parse_emission(code).unwrap();
        assert_eq!((e.bandwidth_hz.0, e.modulation), (bw, m), "{code}");
    }
    assert!(parse_emission("").is_none());
    assert!(parse_emission("16K0Z9Z").is_none());
}

// reach.rs
#[test]
fn reach_follows_the_band_class() {
    let d = Station::default_antennas(10.0);
    let discone = &d[1]; let dipole = &d[0];
    assert_eq!(reach_for(Hz(404_025_000), discone, false, None).km, Some(REACH_UHF_KM));
    assert_eq!(reach_for(Hz(146_000_000), discone, false, None).km, Some(REACH_TROPO_KM));
    assert_eq!(reach_for(Hz(1_090_000_000), discone, false, None).km, Some(REACH_MICROWAVE_KM));
    let hf = reach_for(Hz(7_100_000), dipole, false, None);
    assert_eq!((hf.km, hf.class), (None, ReachClass::Skywave));
    assert!(hf.note.unwrap().contains("abroad"));
    assert_eq!(reach_for(Hz(1_000_000), dipole, false, None).class, ReachClass::Ground);
}
#[test]
fn space_bands_and_pointed_up_antennas_are_space() {
    let discone = &Station::default_antennas(10.0)[1];
    let r = reach_for(Hz(137_500_000), discone, true, None);
    assert_eq!((r.class, r.km), (ReachClass::Space, Some(REACH_UHF_KM)));
    assert!(r.note.unwrap().contains("spacecraft"));
    let up = Antenna { elevation_deg: Some(45.0), ..discone.clone() };
    assert_eq!(reach_for(Hz(146_000_000), &up, false, None).class, ReachClass::Space);
}
#[test]
fn an_override_replaces_the_table_and_caps_skywave() {
    let d = Station::default_antennas(10.0);
    assert_eq!(reach_for(Hz(404_025_000), &d[1], false, Some(20.0)).km, Some(20.0));
    assert_eq!(reach_for(Hz(7_100_000), &d[0], false, Some(500.0)).km, Some(500.0));
}
#[test]
fn a_directional_antenna_doubles_in_its_sector_and_halves_outside() {
    let yagi = Antenna { id: "y".into(), kind: AntennaKind::Yagi, min_hz: Hz(400_000_000), max_hz: Hz(470_000_000), height_m: 10.0, azimuth_deg: Some(90.0), elevation_deg: None };
    let r = reach_for(Hz(404_025_000), &yagi, false, None);
    assert_eq!(r.km_toward(100.0, &yagi), Some(2.0 * REACH_UHF_KM));
    assert_eq!(r.km_toward(300.0, &yagi), Some(0.5 * REACH_UHF_KM));
}
#[test]
fn a_band_no_antenna_covers_takes_the_default_for_its_range() {
    let s = Station { lat: 0.0, lon: 0.0, alt_m: 0.0, region: "AU".into(),
        antennas: vec![Antenna { id: "y".into(), kind: AntennaKind::Yagi, min_hz: Hz(430_000_000), max_hz: Hz(440_000_000), height_m: 10.0, azimuth_deg: Some(0.0), elevation_deg: None }] };
    assert_eq!(s.antenna_for(Hz(146_000_000)).kind, AntennaKind::Discone);
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p zeta-core emission reach`
Expected: FAIL to compile.

- [ ] **Step 3: Implement both modules**

The ITU letter tables (modulation `N A H R J B C F G D P K L M Q V W X`, nature `0 1 2 3 7 8 9 X`, information `N A B C D E F W X`) with the register service's names; `reach_for` by the band edges above, `space` when `space_band || elevation_deg > SPACE_ELEVATION_DEG` (searched at `REACH_UHF_KM`, note "the transmitter may be a spacecraft the register does not list"); skywave note "HF skywave: the transmitter may be abroad and outside the register"; `override_km` replaces every class's km and caps `Skywave`/`Space` at itself.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p zeta-core emission reach`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core/src/emission.rs crates/zeta-core/src/reach.rs crates/zeta-core/src/lib.rs
git commit -m "Knowledge: the emission designator parser and the reach rule (band class and antenna)"
```

---

### Task 7: An MCP streamable-HTTP client in zeta-ext

**Files:**
- Create: `crates/zeta-ext/src/mcp_client.rs`
- Modify: `crates/zeta-ext/src/lib.rs` (export), `crates/zeta-ext/Cargo.toml` (dev-dep `axum` at the workspace version, `tokio` features for the stub)
- Test: `crates/zeta-ext/tests/mcp_client.rs` with a stub server (axum: `POST /mcp` answering `initialize` with a generated `mcp-session-id` header and an `event: message\ndata: {...}\n\n` body, `tools/call` by name; `GET /health`)

**Interfaces:**
- Produces: `pub struct McpClient`; `McpClient::new(base_url: &str, timeout: Duration) -> Result<Self, McpError>` (`base_url` is `http://HOST:PORT`, the endpoint `base_url/mcp`); `pub async fn call(&self, tool: &str, args: Value) -> Result<Value, McpError>` (opens the session on first use, sends `notifications/initialized`, parses the `data:` line(s), returns the first `content[].text` parsed as JSON, or the text as `Value::String` when it is not JSON; a JSON-RPC `error` becomes `McpError::Rpc { code, message }`; `Server not initialized` reopens the session once and retries); `pub async fn health(&self) -> Result<Value, McpError>`; `pub enum McpError { Transport(String), Rpc { code: i64, message: String }, Timeout, BadBody(String) }` (Display).
- Constants: `MCP_PROTOCOL_VERSION = "2025-03-26"`, `CLIENT_NAME = "zetasdr"`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn call_initialises_once_and_parses_the_sse_body() { /* stub counts initialize calls; two calls to "describe_schema" → 1 initialize; the result equals the stub's JSON */ }
#[tokio::test]
async fn a_lost_session_is_reopened_once() { /* stub forgets the session after the first call and answers "Server not initialized"; the second call succeeds; a stub that always answers it → Err(Rpc{..}) after one retry */ }
#[tokio::test]
async fn a_slow_server_is_a_timeout_and_a_bad_body_is_bad_body() { /* stub sleeps 6 s with a 1 s client timeout → Err(Timeout); stub answers "<html>" → Err(BadBody) */ }
#[tokio::test]
async fn health_reads_the_json() { /* {"status":"ok","version":"1.11.0","isSyncing":false} */ }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p zeta-ext --test mcp_client`
Expected: FAIL to compile.

- [ ] **Step 3: Implement with `reqwest` (already a dependency) and a `tokio::sync::Mutex<Option<String>>` session**

- [ ] **Step 4: Run the tests**

Run: `cargo test -p zeta-ext --test mcp_client`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/zeta-ext/src/mcp_client.rs crates/zeta-ext/src/lib.rs crates/zeta-ext/Cargo.toml crates/zeta-ext/tests/mcp_client.rs Cargo.lock
git commit -m "Ext: an MCP streamable-HTTP client (initialize, session header, tools/call, one reopen)"
```

---

### Task 8: The `acma` knowledge provider

**Files:**
- Create: `crates/zeta-ext/src/acma.rs`
- Modify: `crates/zeta-core/src/knowledge.rs` (`Allocation` new optional fields; `KnowledgeProvider::reach` default method), `crates/zeta-core/src/ops.rs` (`OpResult::Knowledge` gains `reach: Option<Reach>`, `knowledge_lookup` collects the first `Some`), `crates/zeta-ext/src/lib.rs`
- Test: `crates/zeta-ext/tests/acma.rs` (stub MCP server from Task 7's pattern answering `execute_sql` with the four 404.025 MHz rows recorded 2026-11-02 and `get_frequency_allocation` with the 403-406 MHz plan entry), `crates/zeta-core/tests/ops.rs` (the `TestProvider` keeps compiling; `knowledge_lookup` result carries `reach: null`)

**Interfaces:**
- Consumes: `McpClient` (Task 7), `Station`/`Antenna` (Task 4), `parse_emission`, `reach_for`, `Reach` (Task 6).
- Produces: `Allocation` gains `#[serde(default)]` fields `kind: Option<String>` (`plan`|`assignment`), `licensee`, `site`, `lat`, `lon`, `distance_km`, `bearing_deg`, `power_w`, `center_hz: Option<Hz>`, `bandwidth_hz: Option<Hz>`, `licence_no`, `role: Option<String>` (`tx`|`rx`), `emission: Option<String>`, `emission_decoded: Option<Emission>`, `as_of: Option<String>`.
- Produces: trait method `async fn reach(&self, center_hz: Hz, bandwidth_hz: Hz) -> Option<Reach> { None }`.
- Produces: `pub struct AcmaKnowledge`; `AcmaKnowledge::new(client: McpClient, override_km: Option<f64>, station: Arc<dyn Fn() -> Option<Station> + Send + Sync>) -> Self`; `name()` is `"acma"`; `allocations("AU", c, bw)`: `get_frequency_allocation {freq_hz: c}` → one `plan` row per service (`service` `"<NAME> (primary|secondary)"`, `min_hz`/`max_hz` the plan range), then `execute_sql {sql, limit: 200}` with the fixed query (`FREQUENCY - BANDWIDTH/2 <= hi AND FREQUENCY + BANDWIDTH/2 >= lo`, joined to `site`, `licence`, `client`; with a reach in km, a bounding box `LATITUDE BETWEEN .. AND LONGITUDE BETWEEN ..` of that many km around the station); each row an `assignment` with haversine `distance_km`, `bearing_deg`, kept when within `reach.km_toward(bearing, antenna)` (no cut when `None`), sorted nearest first (`tx` before `rx` at equal distance), truncated to `ACMA_MAX_ROWS` (50); `note` as the spec's one line; any other region → empty; `reach()` answers `Some(reach_for(..))` with `space_band` true when a plan service name contains `SATELLITE`, `SPACE` or `EARTH EXPLORATION`; a client error or timeout (`ACMA_TIMEOUT` 5 s) answers empty and logs `zeta: knowledge acma: <error>` at most once per `ACMA_LOG_EVERY` (60 s).

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn the_404_025_lookup_names_the_plan_and_the_four_assignments() {
    // station at the demo station (-27.3842, 153.1175), default antennas
    let a = provider.allocations("AU", Hz(404_025_000), Hz(16_000)).await;
    let plan: Vec<_> = a.iter().filter(|x| x.kind.as_deref() == Some("plan")).collect();
    assert!(plan.iter().any(|x| x.service == "FIXED (primary)"));
    let asg: Vec<_> = a.iter().filter(|x| x.kind.as_deref() == Some("assignment")).collect();
    assert_eq!(asg.len(), 4);
    assert_eq!(asg[0].licensee.as_deref(), Some("Example Water Authority"));
    assert_eq!(asg[0].role.as_deref(), Some("tx")); // 1 W, nearest tx
    assert_eq!(asg[0].emission_decoded.as_ref().unwrap().bandwidth_hz, Hz(16_000));
    assert!(asg[0].distance_km.unwrap() < 20.0);
    assert!(asg.windows(2).all(|w| w[0].distance_km <= w[1].distance_km));
    let r = provider.reach(Hz(404_025_000), Hz(16_000)).await.unwrap();
    assert_eq!((r.class, r.km), (ReachClass::Uhf, Some(150.0)));
}
#[tokio::test]
async fn without_a_station_the_list_is_by_frequency_distance_with_null_km() { /* station closure returns None; distance_km None; sorted by |FREQUENCY - c| */ }
#[tokio::test]
async fn another_region_is_empty() { /* "US" → [] and the stub saw no call */ }
#[tokio::test]
async fn a_service_error_a_bad_body_and_a_timeout_each_give_no_allocations() { /* three stubs; each → [] ; the log line appears once (capture stderr via the crate's log hook or assert the provider's `errors()` counter == 1) */ }
#[tokio::test]
async fn a_satellite_plan_sets_space_reach() { /* plan stub with "MOBILE-SATELLITE" → reach class Space, note mentions spacecraft */ }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p zeta-ext --test acma`
Expected: FAIL to compile.

- [ ] **Step 3: Implement `acma.rs`, extend `Allocation`, add the trait method, extend `OpResult::Knowledge`**

The haversine and bearing are 10 lines; the bounding box is `km / 111.0` in latitude and `km / (111.0 * cos(lat))` in longitude.

- [ ] **Step 4: Run the provider, the ops and the catalogue tests**

Run: `cargo test -p zeta-ext --test acma && cargo test -p zeta-core --test ops knowledge && cargo test -p zeta-dsp --test catalogue`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/zeta-ext/src/acma.rs crates/zeta-ext/src/lib.rs crates/zeta-ext/tests/acma.rs crates/zeta-core/src/knowledge.rs crates/zeta-core/src/ops.rs
git commit -m "Knowledge: the acma provider over the register fork (plan services and assignments with distance, role, decoded emission and reach)"
```

---

### Task 9: `--knowledge-acma URL[,reach_km=N]`, the health probe, the guide, `.mcp.json`

**Files:**
- Modify: `crates/zeta/src/cli.rs:19` (`Cli.knowledge_acma: Option<String>`, `#[arg(long = "knowledge-acma", global = true, env = "ZETA_KNOWLEDGE_ACMA", value_name = "URL[,reach_km=N]")]`; `configures_local_node()`), `crates/zeta/src/node_setup.rs:534-537` (parse, `McpClient::new`, probe `health()` and log `zeta: knowledge acma: <url> unreachable: <e>` on failure, `node.add_knowledge(Arc::new(AcmaKnowledge::new(..)))`), `crates/zeta/src/main.rs` (thread the flag), `crates/zeta/src/agents_md.rs` (`RUNNING` bullet; the `knowledge_lookup` note loses "Neither has allocations" and gains the `acma` provider, `reach` and the station), `crates/zeta/src/sources.rs` or a new `knowledge_flag.rs` (`parse_knowledge_acma(&str) -> Result<(String, Option<f64>), String>`)
- Create: `.mcp.json` at the repo root: `{"mcpServers": {"acma": {"type": "http", "url": "http://acma-host:3110/mcp"}}}`
- Test: `crates/zeta/tests/cli.rs` (`knowledge_acma_flag_is_refused_with_node` the usage error with `--node`; `knowledge_acma_with_a_bad_url_is_a_usage_error`), `crates/zeta/tests/agents_md.rs` (`knowledge_acma_is_in_the_running_section`)

- [ ] **Step 1: Write the failing tests** (as named above; the parse test asserts `parse_knowledge_acma("http://h:1,reach_km=20") == Ok(("http://h:1".into(), Some(20.0)))` and `"ftp://x"` is `Err` naming `--knowledge-acma`)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p zeta --test cli knowledge_acma`
Expected: FAIL.

- [ ] **Step 3: Implement; regenerate AGENTS.md**

- [ ] **Step 4: Run the tests and the guide check**

Run: `cargo test -p zeta --test cli knowledge_acma && cargo test -p zeta --test agents_md && cargo run -p zeta -- agents-md --check`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/zeta/src .mcp.json AGENTS.md skills/zetasdr/SKILL.md crates/zeta/tests
git commit -m "CLI: --knowledge-acma URL[,reach_km=N] attaches the register fork; .mcp.json registers it for Claude sessions"
```

---

### Task 10: identify, classify and the watch consult the register; identify re-centres its trials

**Files:**
- Modify: `crates/zeta-core/src/modes/score.rs:32` (`score_rows` gains `allocation: &dyn Fn(&LoadedMode) -> f32`; `ALLOCATION_SCORE = 0.2`), `crates/zeta-core/src/node/identify.rs` (layer-1 step calls every provider's `allocations` with the station's region or `"AU"`, keeps the nearest five as `allocations` beside `features` in the verdict, `candidate` events carry `allocation`, the score closure, `start_child` tuning centred on the measured centre when inside the band), `crates/zeta-dsp/src/identify.rs` (`DecodeToIdentify` gets the same term through a provider handle; `Classification` and `OpResult::Classification` gain `allocations: Vec<Allocation>`), `crates/zeta-core/src/classify.rs` (`Classification.allocations`), `crates/zeta-core/src/ops.rs:3438` (`classify` fills it), `crates/zeta-core/src/node/watch.rs:1266-1283` (the `unknown` event carries `allocations`; summary gains `; licensed: <licensee>, <site>, <d> km` for the nearest `tx`)
- Test: `crates/zeta-core/tests/identify.rs`, `crates/zeta-core/tests/ops.rs` (`TestProvider` answering one `tx` assignment with `emission_decoded` 16 kHz `F`), `crates/zeta-core/tests/watch.rs`

**Interfaces:**
- Consumes: `Allocation` fields (Task 8), `node.knowledge()`, `node.station()`.
- Produces: `pub fn allocation_term(row: &LoadedMode, allocs: &[Allocation]) -> f32` in `score.rs` (0.2 when a `tx` allocation's `emission_decoded.bandwidth_hz` or `bandwidth_hz` lies within 0.5-1.5× the row's `band.bandwidth_hz` and the modulation letter is compatible with one of the row's `modulation` labels: `F`,`G` ↔ `fm`,`fsk`; `G`,`D` ↔ `psk`; `A`,`H`,`J`,`R` ↔ `am`,`ssb`; `N` ↔ `carrier`; an allocation without a designator passes the modulation test); verdict `{"identify": {"verdict": {.., "features": .., "allocations": [..]}}}`; `candidate.allocation: Value|null`.

- [ ] **Step 1: Write the failing tests**

```rust
// identify.rs
#[tokio::test]
async fn the_verdict_carries_the_nearest_allocations_and_the_score_term() { /* TestProvider; identify over the random-GFSK fixture ends unknown; verdict.allocations[0].licensee == the stub's; the candidate for pocsag_decode (12.5 kHz, fm/fsk) has score ≥ 0.2 more than without the provider */ }
#[tokio::test]
async fn trial_jobs_start_on_the_measured_centre() { /* identify band centred 2 kHz off the synthesised signal; the child job's tuning.center_hz is within 500 Hz of the signal, not the band centre */ }
// ops.rs
#[tokio::test]
async fn classify_lists_allocations_beside_the_labels() { /* OpResult::Classification { allocations, .. } non-empty with TestProvider */ }
// watch.rs
#[tokio::test]
async fn an_unknown_summary_names_the_licensee() { /* summary contains "licensed: " and the stub's licensee and "km" */ }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p zeta-core --features test-fixtures --test identify allocations`
Expected: FAIL to compile.

- [ ] **Step 3: Implement; update the score_rows callers and the identify tests' expectations**

- [ ] **Step 4: Run identify, ops, classify, watch and the score unit tests**

Run: `cargo test -p zeta-core --features test-fixtures --test identify --test ops --test watch && cargo test -p zeta-core score && cargo test -p zeta-dsp --features test-fixtures --test classify`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core/src crates/zeta-dsp/src/identify.rs crates/zeta-core/tests
git commit -m "Identify and classify consult the register (allocations beside the features, the 0.2 score term); identify re-centres its trials; the watch's unknown names the licensee"
```

---

### Task 11: `classify` tries the row-width slice and counts only rows in range

**Files:**
- Modify: `crates/zeta-dsp/src/identify.rs` (`DecodeToIdentify::classify_measured` ~80-185; `CLASSIFY_TRIES` applied after dropping rows whose `range_hz` excludes the measured centre; after a `no_frame` on a box between `MIN_BW_HZ` and `RDS_MIN_BW_HZ` with a non-`noise` label, a second quick attempt on `extract_offset` at the measured centre with the row's `band.bandwidth_hz`), `crates/zeta-core/src/modes/score.rs` (the range term drops a row whose `range_hz` does not contain the centre: score 0 for `CENTER_SCORE`, and `fits_band` false)
- Test: `crates/zeta-dsp/tests/classify.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn a_packet_10_khz_off_a_40_khz_box_centre_is_proved_on_the_row_width_slice() { /* POCSAG synthesised at +10 kHz inside a 40 kHz box: outcome(c, "pocsag_decode") == Proved */ }
#[tokio::test]
async fn sonde_rows_do_not_fill_the_tried_list_outside_their_range() { /* a 12.5 kHz fm box at 146 MHz: tried contains no rs41_decode/dfm_decode/m20_decode; pocsag_decode and aprs_decode are present */ }
```

Remove the operator override in `tests/classify.rs` that today keeps the sonde rows out.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p zeta-dsp --features test-fixtures --test classify slice range`
Expected: FAIL.

- [ ] **Step 3: Implement**

- [ ] **Step 4: Run classify, identify and the watch tests**

Run: `cargo test -p zeta-dsp --features test-fixtures --test classify && cargo test -p zeta-core --features test-fixtures --test identify --test watch`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/zeta-dsp/src/identify.rs crates/zeta-core/src/modes/score.rs crates/zeta-dsp/tests/classify.rs
git commit -m "Classify: a row-width slice at the measured centre after a no_frame, and rows out of range never fill the tried list"
```

---

### Task 12: The detector on a real receiver, and the B210 noise recording

**Files:**
- Modify: `crates/zeta-dsp/src/detect.rs` (`DetectParams.energy_db: f32` default `ENERGY_DB_DEFAULT`; `width_hz` default `[4000, 30000]`; `runs` occupancy over the row median; run energy; `Track.sightings: u32`; `SIGHTINGS_MIN = 3`; `SIGNAL_EVENTS_PER_S = 10` rate limit in `run`), `crates/zeta-core/src/ops.rs` (`DETECT_STAGE` schema unchanged shape)
- Create: `scratch/live/record-noise.sh` (runs `z.sh iq read --source b210 --center-hz 403025000 --bandwidth-hz 2000000 --start-ns <now-10s> --end-ns <now>` on the field host and prints the capture path), `crates/zeta-dsp/tests/detect_noise.rs` (`#[ignore]`, reads `ZETA_NOISE_CAPTURE`, runs `fft` rows through `runs`+`Tracker` with the defaults for 60 s of rows and asserts no `new` signal)
- Test: `crates/zeta-dsp/src/detect.rs` tests, `crates/zeta-core/tests/watch.rs` (the whole file must still pass), `crates/zeta-dsp/tests/detect_noise.rs`

**Interfaces:**
- Produces: `DetectParams { threshold_db, energy_db, persist_ms, width_hz, dc_mask_hz, edge_mask, spurs_hz, gone_ms, decode_on, prefer, sondehub }`; `Signal` unchanged; `job_context.detect` unchanged.

- [ ] **Step 1: Record the noise capture (needs the field host's B210 and the running node)**

Run on the field host: `bash scratch/live/record-noise.sh` and copy the pair to `~/zeta-fixtures/b210-noise-403m.sigmf-{meta,data}` (outside the repo). If the B210 is held by a watch or a scan, do not stop it: record a `--source hackrf` capture instead and say so in the commit message. If neither radio is free, skip this step, leave the `#[ignore]` test in place, and note it in the task's commit.

- [ ] **Step 2: Write the failing unit tests**

```rust
#[test]
fn occupancy_is_over_the_median_and_a_run_needs_energy() {
    // a row at q=10 with a 2-bin bump of +3 dB (over threshold 10? no: under) and a 3-bin bump of +12 dB each bin (energy 36 dB ≥ ENERGY_DB_DEFAULT)
    let p = DetectParams { threshold_db: 10.0, energy_db: 30.0, ..Default::default() };
    let r = runs(&view(&row(&[(100, 2, 16), (300, 3, 34)])), &p);
    assert_eq!(r.len(), 1);
    assert!((r[0].center_hz as i64 - bin_center(301)).abs() <= BIN_HZ as i64);
}
#[test]
fn a_wide_weak_run_under_the_energy_floor_is_not_a_signal() { /* 6 bins at +11 dB = 66 dB energy passes; 2 bins at +11 = 22 dB fails with energy_db 30 */ }
#[test]
fn the_tracker_needs_three_sightings_before_new() { /* run seen at t=0, 1 s, 2 s → New at the third observe (persist 1500 ms), not at the second */ }
#[test]
fn a_5_ms_burst_every_second_is_new_after_three_sightings() { /* rows with the run present once per 20 rows, persist 3000, gone 30000 → New at ~3 s */ }
#[test]
fn signal_events_are_rate_limited_to_ten_per_second() { /* 100 distinct runs in one row → at most 10 New events in that second, the rest in the following seconds */ }
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test -p zeta-dsp detect`
Expected: FAIL (no `energy_db`).

- [ ] **Step 4: Implement; set `ENERGY_DB_DEFAULT`**

Start at 30.0; run the watch tests and, when the recording exists, `ZETA_NOISE_CAPTURE=~/zeta-fixtures/b210-noise-403m.sigmf-meta cargo test -p zeta-dsp --test detect_noise -- --ignored`; raise `ENERGY_DB_DEFAULT` until the noise test passes while the synthesised RS41 at 6 dB channel SNR (add `a_6_db_rs41_is_detected` to `detect.rs` using `SynthSpec::Rs41` through `fft` rows at 1.024 Msps) still is detected. Record the chosen value and both outcomes in the commit message.

- [ ] **Step 5: Run detect, watch and the field-craft note**

Run: `cargo test -p zeta-dsp detect && cargo test -p zeta-core --features test-fixtures --test watch`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/zeta-dsp/src/detect.rs crates/zeta-dsp/tests/detect_noise.rs scratch/live/record-noise.sh
git commit -m "Detect: occupancy over the row median with a run-energy floor, a 4 kHz width floor, three sightings before new, ten signal events a second"
```

---

### Task 13: The `BurstStore`, the `burst_capture` stage and the `capture` job kind

**Files:**
- Create: `crates/zeta-core/src/bursts.rs`, `crates/zeta-dsp/src/burst_capture.rs`
- Modify: `crates/zeta-core/src/lib.rs`, `crates/zeta-core/src/node/mod.rs` (`NodeConfig.burst_policy: BurstPolicy`, `Inner.bursts: Arc<BurstStore>`, opened in `open_with_modes` with `expire()` at open; `pub fn bursts(&self) -> Arc<BurstStore>`), `crates/zeta-core/src/stage.rs:80` (`StageContext.bursts: Option<Arc<BurstStore>>`), `crates/zeta-core/src/node/launch.rs:207` (fill it; a job whose stages all return `Ok(())` ends `stopped` with the stage's last note kept: verify the existing finish path and add it if a finished stage leaves the job `running`), `crates/zeta-core/src/ops.rs` (`JobKind::Capture` in the enum, `BUILTIN_KINDS` 18, `name` `"capture"`, `expand`: `channel{center,bw,out_rate} → burst_capture{}` over Iq; not attachable; `pub const BURST_CAPTURE_STAGE: &str = "burst_capture"`), `crates/zeta-dsp/src/lib.rs` (`register_all`)
- Test: `crates/zeta-core/src/bursts.rs` unit tests, `crates/zeta-dsp/src/burst_capture.rs` tests, `crates/zeta-core/tests/capture_job.rs` (new)

**Interfaces:**
- Produces: `pub struct BurstPolicy { pub keep_bytes: u64, pub keep_days: u32 }` (`Default`: 256 MiB, 7); `pub struct CaptureRecord { pub id: String, pub meta_path: PathBuf, pub data_path: PathBuf, pub source: SourceId, pub set: JobId, pub center_hz: Hz, pub bandwidth_hz: Hz, pub sample_rate_hz: Hz, pub start_ns: u64, pub end_ns: u64, pub bytes: u64, pub features: Value, pub signal: Option<Value>, pub job: JobId, pub watch: Option<JobId>, pub allocations: Value, pub pinned: bool }`; `pub struct BurstStore`; `BurstStore::open(state_dir: &Path, policy: BurstPolicy) -> io::Result<Arc<Self>>` (dir `state_dir/bursts`, `index.json`); `pub fn write(&self, rec: NewCapture, samples: &[Complex<f32>]) -> io::Result<CaptureRecord>` (ULID id, SigMF pair via `write_cf32` with the `zeta:` annotations, index updated atomically, then `expire()`); `pub fn list(&self) -> Vec<CaptureRecord>` (newest first); `pub fn get(&self, id: &str) -> Option<CaptureRecord>`; `pub fn delete(&self, id: &str) -> Result<CaptureRecord, StoreError>` (`StoreError::Pinned`, `StoreError::Unknown`); `pub fn pin(&self, id: &str) -> Result<(), StoreError>`; `pub fn expire(&self) -> Vec<String>` (ids removed: oldest unpinned first until `keep_bytes` holds, then any unpinned older than `keep_days`); `pub fn total_bytes(&self) -> u64`.
- Produces: `BurstCaptureParams { bursts: u32 (32), window_ms: u64 (2000), pre_ms: u64 (20), post_ms: u64 (20), timeout_ms: u64 (120_000), whole: bool (false) }` (`serde(default)`); the stage kind `burst_capture`, Iq → Event; `capture` events `{"protocol":"capture","id","start_ns","end_ns","bytes","center_hz","bandwidth_hz","features","summary","t_ns"}`; `job_context.capture = {"written": [ids], "bytes", "bursts_seen", "timed_out"}`; the stage returns `Ok(())` after `bursts` windows or `timeout_ms`.

- [ ] **Step 1: Write the failing tests**

```rust
// bursts.rs
#[test] fn write_lists_newest_first_and_get_finds_by_id() {}
#[test] fn expire_removes_oldest_unpinned_until_the_budget_holds() { /* keep_bytes = 2.5 windows: write 4 → 2 remain, the newest two */ }
#[test] fn expire_never_removes_a_pinned_capture() { /* pin the oldest; keep_bytes = 1 window → the pinned one and the newest remain */ }
#[test] fn expire_by_age_uses_keep_days() { /* a record with start_ns 8 days old is removed with keep_days 7 */ }
#[test] fn the_index_survives_reopen_and_a_missing_data_file_drops_its_record() {}

// burst_capture.rs (stage, over frames built like measure.rs's iq_frame)
#[tokio::test] async fn each_burst_becomes_one_window_with_the_guards() { /* 3 bursts of 5 ms every 1 s at 48 kHz → 3 capture events; each window's end_ns - start_ns == 5 ms + pre + post ± 1 ms; features.duty ≈ 0.1 over the window */ }
#[tokio::test] async fn a_burst_longer_than_the_window_is_cut_into_windows() { /* a 5 s continuous signal, window_ms 2000, bursts 2 → 2 windows of 2000 ms + post, then Ok(()) */ }
#[tokio::test] async fn whole_writes_windows_regardless_of_the_gate() { /* noise only, whole: true, bursts 2 → 2 windows */ }
#[tokio::test] async fn the_timeout_ends_the_stage_with_timed_out_true() { /* noise, bursts 5, timeout_ms 300 → context timed_out true, written [] */ }

// tests/capture_job.rs
#[tokio::test] async fn a_capture_job_ends_stopped_with_its_records_in_the_store() { /* job_start kind capture on a synthesised burst source; wait for stopped; node.bursts().list().len() == 32 or the fixture's burst count; job_context.capture.written matches */ }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p zeta-core bursts && cargo test -p zeta-dsp burst_capture`
Expected: FAIL to compile.

- [ ] **Step 3: Implement the store, the stage (the Task 2 `burst_runs` gate over a rolling 1 ms envelope; `pre_ms` from a ring of the last samples; features by `measure_band` over the window), the kind, the context field**

- [ ] **Step 4: Run the three test sets and the full zeta-core suite**

Run: `cargo test -p zeta-core --features test-fixtures && cargo test -p zeta-dsp --features test-fixtures burst_capture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core/src crates/zeta-dsp/src crates/zeta-core/tests/capture_job.rs
git commit -m "Captures: the BurstStore under state_dir/bursts with its policy, the burst_capture stage, and the capture job kind"
```

---

### Task 14: The `capture_*` ops, the agent gate, the CLI, `GET /captures/{file}`

**Files:**
- Modify: `crates/zeta-core/src/ops.rs` (`Op::CaptureList { signal: Option<SignalBand> }` with `SignalBand { center_hz: Hz, bandwidth_hz: Hz }`, `Op::CaptureShow { capture: String }`, `Op::CaptureClassify { capture: String }`, `Op::CaptureDelete { capture: String }`; `OpResult::Captures(Vec<CaptureRecord>)`, `OpResult::Capture { record: CaptureRecord, meta: Value }`; list and show are `read_only`; classify reads the pair through `zeta_source_file`-free code: add `pub fn read_cf32(meta_path) -> io::Result<(CaptureMeta, Vec<Complex<f32>>)>` to `capture.rs`), `crates/zeta-agent/src/agent.rs:14-60` (`Allow.delete_captures: bool`; `gate` refuses `capture_delete` without it, `field: "op"`), `crates/zeta-ws/src/ask.rs` (passes through), `crates/zeta/src/cli.rs` (`CaptureCmd::{List, Show, Classify, Delete}`, `--allow-delete` on `ask`), `crates/zeta/src/ask.rs`, `crates/zeta-ws/src/http.rs:325` (`GET /captures/{file}` behind `require_auth`: `file` is `<id>.sigmf-meta` or `<id>.sigmf-data`, served from the store's record paths with `application/json` or `application/octet-stream`; unknown → 404), `crates/zeta/src/agents_md.rs` (notes and `cli_form` for the four)
- Test: `crates/zeta-core/tests/capture_ops.rs` (new), `crates/zeta-agent/tests/agent.rs`, `crates/zeta-ws/tests/http.rs` (or the existing http test file), `crates/zeta/tests/cli.rs`, `crates/zeta/tests/agents_md.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// capture_ops.rs
#[tokio::test] async fn capture_list_filters_by_overlapping_band_and_show_carries_the_meta() {}
#[tokio::test] async fn capture_classify_runs_the_classifiers_over_the_window() { /* a POCSAG window → labels contain "POCSAG" (proved) */ }
#[tokio::test] async fn capture_delete_refuses_a_pinned_capture() { /* OpError::Other message "capture <id> is pinned by signature <sig>" or "is pinned" */ }
#[tokio::test] async fn an_unknown_capture_is_unknown_naming_the_field() { /* OpError::Unknown { field: "capture", .. } */ }
// agent.rs
#[test] fn capture_delete_needs_delete_captures_allowed() {}
#[test] fn gate_covers_every_delete_taking_op() {}
// http
#[tokio::test] async fn captures_are_served_with_the_bearer_and_404_without_a_record() {}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p zeta-core --features test-fixtures --test capture_ops`
Expected: FAIL to compile.

- [ ] **Step 3: Implement; regenerate the guide**

The guide notes: `capture_list` newest first, `signal` narrows; `capture_delete` is the operator's (through `ask` it needs `allow.delete_captures`); the files at `GET /captures/<id>.sigmf-meta|data`.

- [ ] **Step 4: Run the tests and the guide check**

Run: `cargo test -p zeta-core --features test-fixtures --test capture_ops && cargo test -p zeta-agent && cargo test -p zeta-ws && cargo test -p zeta --test cli capture && cargo run -p zeta -- agents-md --check`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates AGENTS.md skills/zetasdr/SKILL.md
git commit -m "Captures: capture_list, capture_show, capture_classify and capture_delete (gated for the agent), the CLI, and GET /captures/{file}"
```

---

### Task 15: The watch's explore policy

**Files:**
- Modify: `crates/zeta-dsp/src/detect.rs` (`DetectParams.on_unknown: OnUnknown` (`report`|`capture`, default `report`), `capture: Option<Value>`), `crates/zeta-core/src/graph.rs:92-149` (`check_capture(params, registry)` against `BURST_CAPTURE_STAGE`'s schema, errors `capture.<field>: ...`, called like `check_sondehub`), `crates/zeta-core/src/node/watch.rs` (`Policy { on_unknown, capture }`; `Tracked { captures: Vec<String>, captured: bool }`; `start_capture(&self, t: &Tracked)` launching an owned `capture` job on `(signal.center_hz, max(bw × 1.25, 12_500))` with the policy's params on the `burst_capture` node, once per signal; events `capturing {signal, job}` and `captured {signal, captures, summary}` on the child's end; `job_context.watch.signals[].captures`), `crates/zeta-core/src/node/capture.rs` (new: `capture_finished(inner, rec)` called from the launch finish path for a `capture` job: an unowned job runs `capture_classify` over its first window into `job_context.capture.classified`; an owned one is reported to its watch)
- Test: `crates/zeta-core/tests/watch.rs`, `crates/zeta-core/tests/capture_job.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test] async fn on_unknown_capture_captures_once_per_signal() { /* fsk fixture, on_unknown capture, capture {bursts: 2}; events: identifying → unknown → capturing → captured with 2 ids; a second unknown retry starts no second capture; ctx.watch.signals[0].captures.len() == 2 */ }
#[tokio::test] async fn a_bad_capture_block_is_refused_naming_the_field() { /* capture {bursts: "x"} → graph_put/job_start refused with "capture.bursts" */ }
#[tokio::test] async fn stopping_the_watch_stops_a_running_capture_child() {}
#[tokio::test] async fn a_hand_capture_classifies_its_first_window_and_a_watch_child_does_not() { /* capture_job.rs: hand capture on the pocsag fixture → context.capture.classified.labels proves POCSAG; the watch's child has no classified */ }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p zeta-core --features test-fixtures --test watch on_unknown`
Expected: FAIL.

- [ ] **Step 3: Implement**

- [ ] **Step 4: Run the watch, capture_job and recover suites**

Run: `cargo test -p zeta-core --features test-fixtures --test watch --test capture_job --test recover`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/zeta-dsp/src/detect.rs crates/zeta-core/src crates/zeta-core/tests
git commit -m "Watch: on_unknown: capture starts one owned capture job per unknown signal and reports capturing and captured"
```

---

### Task 16: `--bursts-keep-mb` and `--bursts-keep-days`

**Files:**
- Modify: `crates/zeta/src/cli.rs` (two global `Option<u64>` flags, env `ZETA_BURSTS_KEEP_MB`, `ZETA_BURSTS_KEEP_DAYS`), `crates/zeta/src/node_setup.rs:517` (`NodeConfig.burst_policy`), `crates/zeta/src/main.rs`, `crates/zeta/src/agents_md.rs` (`RUNNING` bullets)
- Test: `crates/zeta/tests/cli.rs` (`bursts_keep_flags_reach_the_store`: start a one-shot with `--bursts-keep-mb 1` over a state dir holding 3 one-MB fixture captures in `bursts/`; after the command `capture list` shows one), `crates/zeta/tests/agents_md.rs`

- [ ] **Step 1: Write the failing tests** — as named.
- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta --test cli bursts_keep`.
- [ ] **Step 3: Implement; regenerate the guide.**
- [ ] **Step 4: Run** — `cargo test -p zeta --test cli bursts_keep && cargo test -p zeta --test agents_md && cargo run -p zeta -- agents-md --check`. Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta/src crates/zeta/tests AGENTS.md skills/zetasdr/SKILL.md
git commit -m "CLI: --bursts-keep-mb and --bursts-keep-days set the capture store's policy"
```

---

### Task 17: The pane tab strip; Subjects moves into it

**Files:**
- Create: `crates/zeta-ui/web/panes.js` (`mountPanes(el, view, {footprint}) -> {register(id, label, {eligible}), select(id|null), current(), render()}`; buttons `button.pane-tab[role=tab][data-pane]`, `aria-selected`; a tab is shown when `eligible()` or selected)
- Modify: `crates/zeta-ui/web/index.html` (a `<nav id="panes" role="tablist" aria-label="Panes">` after `#tabs` in `#top`; `#subjects-toggle` removed; `<div id="pane" hidden>` under `#waterfall` in `#main` holding `<div id="subjects">`), `style.css` (`#pane {grid under the waterfall, max-height: 40%}`; drop `html[data-subjects="on"] #waterfall {right:50%}` and `#subjects {position:absolute…}`), `view.js` (`pane` policy: values `none|subjects|explore`, default `none`; `subjects` stays as an alias: `on` → `pane subjects`, `off` → `pane none`, `auto` keeps the eligibility rule), `app.js` (`applySubjectsPane`, `toggleSubjects`, `subjectsPressed` replaced by `panes`; narrow footprint keeps `data-pane`), `crates/zeta-ui/src/lib.rs:4-105` (`panes.js` in `ASSETS`)
- Test: `crates/zeta-ui/tests/e2e/ui.py` (`scenario_view_set_show_reset` count 11 → 12; the plan-23 subjects scenarios updated to click `.pane-tab[data-pane="subjects"]`; new `scenario_panes_show_one_at_a_time`)

- [ ] **Step 1: Write the failing scenario**

```python
def scenario_panes_show_one_at_a_time(page, url, secret):
    # with an adsb fixture job running the subjects tab is eligible
    expect(page.locator('#panes .pane-tab[data-pane="subjects"]')).to_be_visible()
    page.click('#panes .pane-tab[data-pane="subjects"]')
    expect(page.locator("#pane")).to_be_visible()
    expect(page.locator("#subjects")).to_be_visible()
    assert view_line(page, "pane") == "pane subjects"
    page.click('#panes .pane-tab[data-pane="subjects"]')   # a second click deselects
    expect(page.locator("#pane")).to_be_hidden()
    assert view_line(page, "pane") == "pane none"
```

- [ ] **Step 2: Run to verify it fails**

Run: `ZETA_UI_ONLY=panes_show_one_at_a_time,view_set_show_reset cargo test -p zeta --test ui`
Expected: FAIL (no `#panes`).

- [ ] **Step 3: Implement; update the subjects scenarios**

- [ ] **Step 4: Run the whole UI suite**

Run: `cargo test -p zeta --test ui`
Expected: PASS with no `ui e2e skipped`.

- [ ] **Step 5: Commit**

```bash
git add crates/zeta-ui
git commit -m "UI: a pane tab strip beside the radios (view set pane); the Subjects list moves into it"
```

---

### Task 18: The Explore pane, "Capture bursts", the watch's capture ribbons

**Files:**
- Create: `crates/zeta-ui/web/explore.js` (`mountExplorePane(el, api, view, transcript, {panes}) -> {render(), destroy()}`: polls `capture_list` every `EXPLORE_POLL_MS` 2000 while the pane is shown or until the first capture is seen; rows newest first with time, `bandText`, size, `features` summary (`<label> <bw> kHz, duty <d>, <baud> Bd`), nearest `tx` licensee; actions `What is this` (`capture_classify`, labels to the transcript), `Download` (two `fetch` calls with the bearer, saved as Blobs), `Delete` (`capture_delete`, confirm via a second click); registers the `explore` pane with `eligible: () => captures.length > 0`; auto-selects `explore` the first time a capture appears, once per page load)
- Modify: `menu.js:250-274` (`["capture", "Capture bursts"]` after classify; `choose("capture")` goes through `fitAndStart` with a `capture` job: `JOBS` gains `capture`, `FIT_KINDS` includes it), `overlays.js:598-640` (the watch's `capturing`/`captured` events are ribbons like its others: already generic through `summary`; add `protocol: "capture"` events as ribbons `captured <kHz> kHz`), `app.js` (mount), `index.html` (`<div id="explore" hidden>` in `#pane`), `crates/zeta-ui/src/lib.rs` (`explore.js`)
- Test: `ui.py` (`scenario_capture_bursts_from_the_box_menu_and_explore_lists_it`, `scenario_explore_what_is_this_and_delete`)

- [ ] **Step 1: Write the failing scenarios**

```python
def scenario_capture_bursts_from_the_box_menu_and_explore_lists_it(page, url, secret):
    open_menu_on_tone(page); page.click('#ctx .ctx-item[data-action="capture"]')
    expect(page.locator("#transcript .line.note")).to_contain_text("fitted capture to")
    expect(page.locator('#panes .pane-tab[data-pane="explore"]')).to_be_visible(timeout=20000)
    expect(page.locator("#explore tr.capture")).to_have_count(2, timeout=20000)
def scenario_explore_what_is_this_and_delete(page, url, secret):
    page.click('#explore tr.capture >> nth=0 >> button[data-action="classify"]')
    expect(page.locator("#transcript .line.note")).to_contain_text("carrier")
    page.click('#explore tr.capture >> nth=0 >> button[data-action="delete"]'); page.click('#explore tr.capture >> nth=0 >> button[data-action="delete"]')
    expect(page.locator("#explore tr.capture")).to_have_count(1)
```

The menu's capture job uses `{bursts: 2, whole: true}` on the `burst_capture` node through `graph_put` after the start, so a fixture with no bursts still yields windows for the test; the spec's defaults apply when started by op.

- [ ] **Step 2: Run to verify they fail** — `ZETA_UI_ONLY=capture_bursts_from_the_box_menu_and_explore_lists_it cargo test -p zeta --test ui`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the UI suite** — `cargo test -p zeta --test ui`. Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-ui
git commit -m "UI: the Explore pane (captures with What is this, Download, Delete), Capture bursts in the box menu, capture ribbons"
```

---

### Task 19: The cached-region bracket, `too_old` retry, the ring depth in the footer

**Files:**
- Modify: `crates/zeta-core/src/ops.rs:2092` (`OpError::Ring { detail: String, #[serde(default)] earliest_ns: Option<u64> }` filled from `RingError::TooOld`), `crates/zeta-core/src/ring.rs` (no change; `earliest()` exists), `crates/zeta-ui/web/waterfall.js` (`xForNs(ns) -> number|null`: walk `ringStart`/`ringEnd` from the newest slot; `null` when the ns is older than the ring), `overlays.js` (`ov.span(lo, hi, startNs, endNs, text, key)` and `ov.unspan(key)`: a bracket positioned by `yForHz` and `xForNs`, re-laid on `onColumn` and `onView`, removed when `xForNs(endNs)` is `null`), `explore.js` (a `span` per capture whose `end_ns` is still on screen), `app.js:376` (`applyOp` returns `e.json` for an applied error so callers can read `too_old`), `menu.js:721-746` (on `too_old`: re-issue with `end_ns = newestNs(wf)`, `start_ns = end_ns - (old length, floored at what the ring holds: earliest_ns)`, note `buffer moved on; classified the last <n> s`), `menu.js` footer (`showFooter` adds `ring <d> s`; the depth comes from `source_list`, whose record gains `ring: {"seconds": f64}` per source, filled from `ring_seconds_for` when the ring is sized at open)
- Test: `crates/zeta-core/tests/ops.rs` (`too_old_carries_the_rings_earliest_ns`; `source_list_reports_the_ring_depth`), `ui.py` (`scenario_too_old_classify_reissues_on_the_latest_span`: a short-ring node (`--ring-budget-mb 1` on the wide scanner fixture) and a box drawn then waited past the ring; `scenario_a_capture_draws_a_bracket_while_on_screen`)

- [ ] **Step 1: Write the failing tests** — as named; the ops test asserts `OpError::Ring { earliest_ns: Some(e), .. }` with `e >= ring start`.
- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-core --test ops too_old`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — `cargo test -p zeta-core --test ops && cargo test -p zeta --test ui`. Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core/src crates/zeta-core/tests crates/zeta-ui
git commit -m "UI: the cached-region bracket, a too_old classify re-issued on the latest span, the ring depth in the box footer (source_list.ring)"
```

---

### Task 20: Field craft, the guide, TODO and the plan series

**Files:**
- Modify: `crates/zeta/src/agents_md.rs` field-craft sections (a "Where to look" line on the register: call `knowledge_lookup` first on a licensed band and report licensee, site, distance, power and `as_of`; "Reading measurements": a low `duty` means the features were measured over the bursts, say so; a new "Exploring an unknown signal" section: the ladder of section 4 of the spec through 19.1, `on_unknown: capture`, `capture_list`, `capture_classify`, the `reach` line), `docs/agents/field-craft.md` if it mirrors the guide, `TODO.md` (remove "Feature: a burst-aware layer 1", "Feature: a `knowledge` provider over the ACMA licence register", "Feature: a station position" (keep the GPS-fix half as a gist), "Feature: the watch re-centres before it identifies", "Bug: the watch's detect defaults fire on receiver noise" (keep the spur-list half under the scan gist), "UI: \"What is this?\" says the data is no longer in the buffer", "Feature: classify on a 12.5-150 kHz box tries a row-width slice", "UI: the Subjects list must not take over the page", and items 1-3 of "Agent guide: SIGINT and UX additions"; each only with the task that shipped it), `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (rows 19.1, 19.2, 19.3 replacing row 19; a "Plan 19.1 status" section with the gate numbers, the `ENERGY_DB_DEFAULT` chosen and the noise recording's outcome), `docs/architecture/gen.py` and the page if they list stages and ops
- Test: `crates/zeta/tests/agents_md.rs` (`exploring_an_unknown_signal_is_in_the_guide`)

- [ ] **Step 1: Write the failing guide test** — needle `"## Exploring an unknown signal"` or the section's heading as rendered, and `"on_unknown"`.
- [ ] **Step 2: Run to verify it fails** — `cargo test -p zeta --test agents_md exploring`.
- [ ] **Step 3: Write the sections; regenerate; tidy TODO and the series.**
- [ ] **Step 4: The gate**

Run: `cargo fmt` per member, then `cargo clippy --workspace --all-features -- -D warnings`, then `cargo test --workspace --all-features --no-fail-fast 2>&1 | tee /tmp/gate.log; grep -c 'test result' /tmp/gate.log; grep 'ui e2e skipped\|FAILED\|failed' /tmp/gate.log`
Expected: clippy clean; every `test result` line `ok`; no `ui e2e skipped`; record the passed/failed/ignored totals in the plan-series status.

- [ ] **Step 5: Commit**

```bash
git add AGENTS.md skills/zetasdr/SKILL.md crates/zeta/src/agents_md.rs crates/zeta/tests/agents_md.rs TODO.md docs
git commit -m "Docs: plan 19.1 shipped (the explore guide, TODO tidied with evidence, the plan series rows 19.1-19.3 and status)"
```

---

## Self-review notes

- Spec coverage: 5.1 → Tasks 1-3; 5.2 → Task 4 (and 5 for the sink default); 5.3 → Tasks 6-9; 5.4 → Tasks 10-11; 5.5 → Tasks 13-16; 5.6 → Task 12; 5.7 → Tasks 17-19; 5.8 → Task 20; section 8's test list is spread over the tasks named; section 9 (acceptance) is the operator's after 19.3.
- Names used across tasks: `Station`, `Antenna`, `AntennaKind` (4, 6, 8); `Emission`, `parse_emission`, `Reach`, `ReachClass`, `reach_for`, `km_toward` (6, 8); `McpClient`, `McpError` (7, 8, 9); `AcmaKnowledge::new(client, override_km, station)` (8, 9); `Allocation` new fields (8, 10); `BurstStore`, `BurstPolicy`, `CaptureRecord`, `BURST_CAPTURE_STAGE`, `JobKind::Capture` (13, 14, 15, 16, 18); `DetectParams.on_unknown/capture` (15); `OpError::Ring.earliest_ns` (19).
- Left to the builder as the spec says: loop and smoothing constants, `ENERGY_DB_DEFAULT`, the SQL text, the `zeta:` annotation keys and `index.json` layout, the bracket's drawing, the pane markup.
