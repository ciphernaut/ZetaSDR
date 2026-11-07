//! Layer-4 chains end to end: synthesised IQ through `resample`, a demodulator,
//! `clock_recover` and `slicer`, the recovered bits checked against the planted ones.
#![cfg(feature = "test-fixtures")]

use num_complex::Complex;
use serde_json::{Value, json};
use zeta_core::types::{JobId, SourceId};
use zeta_dsp::offline::{run_chain, run_chain_at};
use zeta_dsp::synth::{Impairments, SynthSpec, synthesise};

const NS_PER_S: u64 = 1_000_000_000;

/// The bits of every `bits` event in `events`, in order.
pub fn recovered_bits(events: &[Value]) -> Vec<u8> {
    events
        .iter()
        .filter(|e| e["protocol"] == "bits")
        .flat_map(|e| {
            zeta_dsp::slicer::bits_from_hex(
                e["bits"].as_str().expect("bits hex"),
                e["count"].as_u64().expect("bit count") as usize,
            )
        })
        .collect()
}

/// Symbols a chain through `resample` loses at the input's end: the resampler does not
/// flush its prototype's history (about 0.8 symbol at 4800 Bd), so the last symbol never
/// reaches the demodulator. The demodulators flush their own low-pass, and
/// `clock_recover` and `slicer` flush at the end, so a chain without `resample` loses
/// none (measured: 2-FSK 1 bit, 4-FSK 2 bits, OOK none).
pub const RESAMPLE_TAIL_SYMBOLS: usize = 1;

/// [`align_errors_tail`] with no tail tolerance: every planted bit must be recovered.
pub fn align_errors(got: &[u8], want: &[u8]) -> usize {
    align_errors_tail(got, want, 0)
}

/// The fewest bit errors over every alignment of `want` inside `got`, each also tried
/// inverted (a binary decision may come out flipped), `want` allowed to run up to
/// `tail_bits` past the end of `got` (bits a chain loses at the input's end; see
/// [`RESAMPLE_TAIL_SYMBOLS`]), those missing bits not counted; `want.len()` when no
/// alignment fits. A 4-level stream compares the same way: the slicer's natural mapping
/// gives a symbol's index (lowest tone 0) MSB first, which is how the synthesiser groups
/// planted bits into symbols, and an inverted 4-level stream (index `3 - s`) is every bit
/// flipped.
pub fn align_errors_tail(got: &[u8], want: &[u8], tail_bits: usize) -> usize {
    fewest_errors(got, want, tail_bits, 1, true)
}

/// The fewest bit errors over the alignments of `want` inside `got` at offsets that are
/// multiples of `step`, each also tried inverted when `invert`, `want` allowed to run up
/// to `tail_bits` past the end of `got`; `want.len()` when no alignment fits.
fn fewest_errors(got: &[u8], want: &[u8], tail_bits: usize, step: usize, invert: bool) -> usize {
    let need = want.len().saturating_sub(tail_bits);
    if got.len() < need {
        return want.len();
    }
    (0..=got.len() - need)
        .step_by(step)
        .map(|off| {
            let n = (got.len() - off).min(want.len());
            let e = got[off..off + n]
                .iter()
                .zip(want)
                .filter(|(g, w)| g != w)
                .count();
            if invert { e.min(n - e) } else { e }
        })
        .min()
        .unwrap_or(want.len())
}

/// `n` deterministic random bits.
pub fn random_bits(n: usize, seed: u64) -> Vec<u8> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (s >> 63) as u8
        })
        .collect()
}

/// `bits` packed MSB first as hex, the last byte zero-padded.
pub fn hex_of(bits: &[u8]) -> String {
    bits.chunks(8)
        .map(|b| {
            let v = b
                .iter()
                .enumerate()
                .fold(0u8, |acc, (i, &x)| acc | (x << (7 - i)));
            format!("{v:02x}")
        })
        .collect()
}

const IN_RATE: u32 = 48_000;
const FSK_RATE: u64 = 38_400;
const FSK_BAUD: u32 = 4_800;
const OOK_BAUD: u32 = 2_400;
const PLANTED_BITS: usize = 2048;
const SEED: u64 = 0x05ee_d192;
const OFFSET_HZ: f64 = 500.0;
const PPM: f64 = 20.0;
/// Bits of a known pattern sent before the planted bits, as a real burst's preamble: the
/// clock and the slicer's levels settle on it, so the planted bits start on a settled
/// chain.
const PREAMBLE_BITS: usize = 64;

/// `want` after a [`PREAMBLE_BITS`] preamble of `pattern` repeated.
fn with_preamble(want: &[u8], pattern: &[u8]) -> Vec<u8> {
    let preamble: Vec<u8> = pattern
        .iter()
        .copied()
        .cycle()
        .take(PREAMBLE_BITS)
        .collect();
    [preamble.as_slice(), want].concat()
}

/// The preamble pattern of `order`: alternating bits for 2 levels, and for 4 the outer
/// symbols alternating (`00`, `11`), each a transition per symbol for the clock.
fn preamble_pattern(order: u8) -> &'static [u8] {
    if order == 4 { &[0, 0, 1, 1] } else { &[0, 1] }
}

fn impairments(snr_db: f32, offset_hz: f64) -> Impairments {
    Impairments {
        snr_db: Some(snr_db),
        offset_hz,
        ppm: PPM,
        echo: None,
    }
}

/// The FSK chain's bit errors: `order`-FSK of [`PLANTED_BITS`] random bits at
/// `deviation_hz` under `imp`, resampled to [`FSK_RATE`], demodulated, clocked with
/// `clock` (`clock_recover`'s params) and sliced into `order` levels, the resampler's lost
/// tail ([`RESAMPLE_TAIL_SYMBOLS`]) allowed.
async fn fsk_errors(order: u8, deviation_hz: f64, imp: Impairments, clock: Value) -> usize {
    let want = random_bits(PLANTED_BITS, SEED);
    let spec = SynthSpec::Fsk {
        symbol_rate_baud: FSK_BAUD,
        deviation_hz,
        order,
        bits: hex_of(&with_preamble(&want, preamble_pattern(order))),
        repeat: 1,
        gap_ms: 0,
    };
    let iq = synthesise(&spec, &imp, IN_RATE, 0);
    let events = run_chain(
        &[
            ("resample", json!({"out_rate_hz": FSK_RATE})),
            (
                "fsk_demod",
                json!({"symbol_rate_baud": FSK_BAUD, "order": order}),
            ),
            ("clock_recover", clock),
            ("slicer", json!({"levels": order})),
        ],
        &iq,
        IN_RATE as u64,
    )
    .await
    .expect("the chain runs");
    let bits_per_symbol = if order == 4 { 2 } else { 1 };
    align_errors_tail(
        &recovered_bits(&events),
        &want,
        RESAMPLE_TAIL_SYMBOLS * bits_per_symbol,
    )
}

/// The OOK chain's bit errors at `snr_db` over [`PLANTED_BITS`] random bits at 48 kHz.
async fn ook_errors(snr_db: f32) -> usize {
    let want = random_bits(PLANTED_BITS, SEED);
    let spec = SynthSpec::Ook {
        symbol_rate_baud: OOK_BAUD,
        bits: hex_of(&with_preamble(&want, preamble_pattern(2))),
        repeat: 1,
        gap_ms: 0,
    };
    let iq = synthesise(&spec, &impairments(snr_db, OFFSET_HZ), IN_RATE, 0);
    let events = run_chain(
        &[
            ("ook_demod", json!({"symbol_rate_baud": OOK_BAUD})),
            (
                "clock_recover",
                json!({"symbol_rate_baud": OOK_BAUD, "algorithm": "early_late"}),
            ),
            ("slicer", json!({})),
        ],
        &iq,
        IN_RATE as u64,
    )
    .await
    .expect("the chain runs");
    align_errors(&recovered_bits(&events), &want)
}

#[tokio::test]
async fn fsk_chain_recovers_the_bits_clean_at_20_db() {
    let clock = json!({"symbol_rate_baud": FSK_BAUD});
    let e = fsk_errors(2, 2400.0, impairments(20.0, OFFSET_HZ), clock).await;
    assert_eq!(e, 0, "{e} bit errors");
}

#[tokio::test]
async fn fsk_chain_stays_under_1_percent_at_10_db() {
    let clock = json!({"symbol_rate_baud": FSK_BAUD});
    let e = fsk_errors(2, 2400.0, impairments(10.0, OFFSET_HZ), clock).await;
    assert!(e < 21, "{e} bit errors");
}

/// The tones sit at ±1200 and ±3600 Hz about a 500 Hz carrier offset, which `fsk_demod`
/// removes (the slicer's 4-level thresholds are fixed at 0 and ±2/3 of the outer level).
/// `mm` decides by sign alone, which mistimes the inner symbols, so the clock runs
/// early-late.
#[tokio::test]
async fn four_fsk_chain_recovers_the_symbols() {
    let clock = json!({"symbol_rate_baud": FSK_BAUD, "algorithm": "early_late"});
    let e = fsk_errors(4, 3600.0, impairments(20.0, OFFSET_HZ), clock).await;
    assert_eq!(e, 0, "{e} bit errors");
}

#[tokio::test]
async fn ook_chain_recovers_the_bits() {
    let clean = ook_errors(20.0).await;
    assert_eq!(clean, 0, "{clean} bit errors at 20 dB");
    let noisy = ook_errors(10.0).await;
    assert!(noisy < 21, "{noisy} bit errors at 10 dB");
}

/// The fewest bit errors of `got` against `want` over exactly the four rotations of a
/// QPSK stream: each symbol pair of `got` (the slicer's natural mapping, MSB first)
/// read as index `s` and remapped to `(s + r) mod 4`, then aligned on symbol boundaries
/// without inversion (an inversion, `3 - s`, is a reflection, not a rotation), `want`
/// allowed to run up to `tail_bits` past the end. `psk_demod` does not resolve the
/// phase ambiguity of its order.
pub fn best_rotation(got: &[u8], want: &[u8], tail_bits: usize) -> usize {
    (0..4u8)
        .map(|r| {
            let rotated: Vec<u8> = got
                .chunks_exact(2)
                .flat_map(|p| {
                    let s = (p[0] * 2 + p[1] + r) % 4;
                    [s >> 1, s & 1]
                })
                .collect();
            fewest_errors(&rotated, want, tail_bits, 2, false)
        })
        .min()
        .unwrap_or(want.len())
}

/// The PSK chain's recovered bits and the planted ones: `order`-PSK of
/// [`PLANTED_BITS`] random bits (after the preamble of `order`) under `imp`, resampled
/// to [`FSK_RATE`], demodulated, clocked (Gardner, the 2-channel default) and sliced on
/// `constellation`.
async fn psk_bits(order: u8, constellation: &str, imp: Impairments) -> (Vec<u8>, Vec<u8>) {
    let want = random_bits(PLANTED_BITS, SEED);
    let spec = SynthSpec::Psk {
        symbol_rate_baud: FSK_BAUD,
        order,
        bits: hex_of(&with_preamble(&want, preamble_pattern(order))),
        repeat: 1,
        gap_ms: 0,
    };
    let iq = synthesise(&spec, &imp, IN_RATE, 0);
    let events = run_chain(
        &[
            ("resample", json!({"out_rate_hz": FSK_RATE})),
            (
                "psk_demod",
                json!({"symbol_rate_baud": FSK_BAUD, "order": order}),
            ),
            ("clock_recover", json!({"symbol_rate_baud": FSK_BAUD})),
            ("slicer", json!({"constellation": constellation})),
        ],
        &iq,
        IN_RATE as u64,
    )
    .await
    .expect("the chain runs");
    (recovered_bits(&events), want)
}

#[tokio::test]
async fn bpsk_chain_recovers_the_bits_up_to_inversion() {
    let (got, want) = psk_bits(2, "bpsk", impairments(20.0, OFFSET_HZ)).await;
    let clean = align_errors_tail(&got, &want, RESAMPLE_TAIL_SYMBOLS);
    assert_eq!(clean, 0, "{clean} bit errors at 20 dB");
    let (got, want) = psk_bits(2, "bpsk", impairments(10.0, OFFSET_HZ)).await;
    let noisy = align_errors_tail(&got, &want, RESAMPLE_TAIL_SYMBOLS);
    assert!(noisy < 21, "{noisy} bit errors at 10 dB");
}

#[tokio::test]
async fn qpsk_chain_recovers_the_bits_up_to_rotation() {
    let (got, want) = psk_bits(4, "qpsk", impairments(20.0, OFFSET_HZ)).await;
    let e = best_rotation(&got, &want, RESAMPLE_TAIL_SYMBOLS * 2);
    assert_eq!(e, 0, "{e} bit errors");
}

/// Data bits of the short-burst chain, after its [`PREAMBLE_BITS`]: 128 symbols, 1024
/// samples at [`FSK_RATE`].
const BURST_BITS: usize = 64;
const BURST_OFFSET_HZ: f64 = 1200.0;

/// One short BPSK burst at [`FSK_RATE`] through `psk_demod → clock_recover → slicer`
/// (no `resample`): its bit errors up to inversion, no tail tolerance.
#[tokio::test]
async fn a_short_bpsk_burst_recovers_its_bits_without_resample() {
    let want = random_bits(BURST_BITS, SEED);
    let spec = SynthSpec::Psk {
        symbol_rate_baud: FSK_BAUD,
        order: 2,
        bits: hex_of(&with_preamble(&want, preamble_pattern(2))),
        repeat: 1,
        gap_ms: 0,
    };
    let iq = synthesise(
        &spec,
        &impairments(20.0, BURST_OFFSET_HZ),
        FSK_RATE as u32,
        0,
    );
    let events = run_chain(
        &[
            ("psk_demod", json!({"symbol_rate_baud": FSK_BAUD})),
            ("clock_recover", json!({"symbol_rate_baud": FSK_BAUD})),
            ("slicer", json!({"constellation": "bpsk"})),
        ],
        &iq,
        FSK_RATE,
    )
    .await
    .expect("the chain runs");
    let e = align_errors(&recovered_bits(&events), &want);
    assert_eq!(e, 0, "{e} bit errors");
}

/// Chip rate of the OOK remotes in the fixtures: about 313 us a chip.
const REMOTE_BAUD: u32 = 3_200;

/// The meta of fixture `name` (`tests/data/<name>.sigmf-meta`) and the codes its
/// annotation says the window holds (`zeta:expected.codes`, hex), or `None` with the
/// reason printed when the file is absent: the real remote's captures are the
/// operator's and are not published; the synthesised `ook-ev1527-synth-*` always are.
fn fixture(name: &str) -> Option<(std::path::PathBuf, Vec<u32>)> {
    let meta = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(format!("{name}.sigmf-meta"));
    if !meta.exists() {
        eprintln!("skipped: {name}: a private capture, not in this checkout");
        return None;
    }
    let json: Value = serde_json::from_slice(&std::fs::read(&meta).expect("meta")).expect("json");
    let codes = json["annotations"][0]["zeta:expected"]["codes"]
        .as_array()
        .unwrap_or_else(|| panic!("{name}: no zeta:expected.codes in its annotation"))
        .iter()
        .map(|c| {
            let c = c.as_str().expect("a hex code");
            u32::from_str_radix(c.trim_start_matches("0x"), 16).expect("a hex code")
        })
        .collect();
    Some((meta, codes))
}
/// Chips of one EV1527 symbol: a 0 is `1000`, a 1 is `1110`.
const EV1527_SYMBOL_CHIPS: usize = 4;
/// Data bits of one EV1527 frame; the sync pulse makes its 25th symbol.
const EV1527_BITS: usize = 24;
/// Off chips that end a frame: the sync's silence is 31 chips, a bit's at most 3.
const EV1527_SYNC_ZERO_CHIPS: usize = 20;

/// The codes of every whole EV1527 frame in `chips`: split at runs of at least
/// [`EV1527_SYNC_ZERO_CHIPS`] zeros, each group cut into 4-chip symbols (`1000` is 0,
/// `1110` is 1, anything else spoils the group), and a group of exactly
/// [`EV1527_BITS`] + 1 symbols (the bits, then the sync pulse) read as its first 24 bits,
/// MSB first.
fn ev1527_codes(chips: &[u8]) -> Vec<u32> {
    let mut groups: Vec<Vec<u8>> = Vec::new();
    let (mut cur, mut zeros) = (Vec::new(), 0usize);
    for &c in chips {
        if c == 0 {
            zeros += 1;
            if zeros == EV1527_SYNC_ZERO_CHIPS && !cur.is_empty() {
                groups.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if zeros < EV1527_SYNC_ZERO_CHIPS && !cur.is_empty() {
            cur.extend(std::iter::repeat_n(0, zeros));
        }
        zeros = 0;
        cur.push(c);
    }
    groups
        .into_iter()
        .filter_map(|mut g| {
            // A group ends on its last on chip (the sync pulse): pad it to a symbol.
            g.resize(g.len().next_multiple_of(EV1527_SYMBOL_CHIPS), 0);
            let symbols: Option<Vec<u32>> = g
                .chunks(EV1527_SYMBOL_CHIPS)
                .map(|s| match s {
                    [1, 0, 0, 0] => Some(0),
                    [1, 1, 1, 0] => Some(1),
                    _ => None,
                })
                .collect();
            let symbols = symbols?;
            (symbols.len() == EV1527_BITS + 1).then(|| {
                symbols[..EV1527_BITS]
                    .iter()
                    .fold(0u32, |acc, &b| (acc << 1) | b)
            })
        })
        .collect()
}

/// The stages `bits_start` builds for `plan`, without its `capture_in` (the harness
/// feeds the IQ) and `bit_view` (the slicer's `bits` events are what is read), in graph
/// order.
fn plan_stages(plan: &zeta_core::bits::BitsPlan) -> Vec<(String, Value)> {
    zeta_core::bits::bits_graph(plan)
        .nodes
        .into_iter()
        .filter(|n| !matches!(n.kind.as_str(), "capture_in" | "bit_view"))
        .map(|n| (n.kind, n.params))
        .collect()
}

/// The chips and EV1527 codes that the chain `bits_start` builds recovers from the
/// real-remote fixture `name` (`tests/data/<name>.sigmf-meta`, cf32 at 51 200 Hz centred
/// on the remote): the plan is made from the fixture's own record and feature values,
/// centred on the capture, OOK at [`REMOTE_BAUD`].
async fn remote_codes(meta: &std::path::Path) -> (String, Vec<u32>) {
    remote_codes_with(meta, true).await.1
}

/// The plan, chips and EV1527 codes the chain `bits_start` builds recovers from the
/// real-remote fixture `name`: as [`remote_codes`], the plan made from the fixture's own
/// record and features, its measured centre kept as recorded unless `centred` (then the
/// capture's own, so the chain does not shift).
async fn remote_codes_with(
    meta: &std::path::Path,
    centred: bool,
) -> (zeta_core::bits::BitsPlan, (String, Vec<u32>)) {
    remote_codes_from(meta, Features::Recorded { centred }).await
}

/// Where [`remote_codes_from`] takes the plan's features from.
enum Features {
    /// The fixture's recorded features, OOK at [`REMOTE_BAUD`] given; the capture's
    /// centre in place of the measured one when `centred`.
    Recorded { centred: bool },
    /// Measured afresh as the node's burst capture does (`measure_band` over the
    /// capture's band), the modulation from the layer-1 label `measure_selection` gives
    /// the same band, nothing given: a bare `bits_start`.
    Remeasured,
}

/// The plan, chips and EV1527 codes the chain `bits_start` builds recovers from the
/// real-remote fixture `name`, its plan made from the fixture's record and `features`.
async fn remote_codes_from(
    meta: &std::path::Path,
    features: Features,
) -> (zeta_core::bits::BitsPlan, (String, Vec<u32>)) {
    let meta = meta.to_path_buf();
    let name = meta
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("a fixture name")
        .to_string();
    let (m, iq) = zeta_core::capture::read_cf32(&meta).expect("fixture");
    let rate = m.sample_rate_hz.0;
    let (features, label, modulation, baud) = match features {
        Features::Recorded { centred } => {
            let mut f: zeta_core::classify::FeatureRecord =
                serde_json::from_value(m.annotations[0]["zeta:features"].clone())
                    .expect("features");
            if centred {
                f.center_hz = m.center_hz;
            }
            let ook = Some(zeta_core::bits::Modulation::Ook);
            (f, None, ook, Some(zeta_core::types::Baud(REMOTE_BAUD)))
        }
        Features::Remeasured => {
            let a = &m.annotations[0];
            let edge = |k: &str| a[k].as_u64().expect("annotation edge");
            let band = edge("core:freq_upper_edge") - edge("core:freq_lower_edge");
            let f = zeta_dsp::measure::measure_band(&iq, rate, m.center_hz.0, band);
            let (_, l) = zeta_dsp::measure::measure_selection(
                &iq,
                rate,
                m.center_hz.0,
                zeta_core::classify::Band {
                    offset_hz: 0,
                    bandwidth_hz: zeta_core::types::Hz(band),
                },
            )
            .expect("a measurement");
            (f, Some(l.name), None, None)
        }
    };
    let record = zeta_core::bursts::CaptureRecord {
        id: name,
        data_path: meta.with_extension("sigmf-data"),
        meta_path: meta,
        source: SourceId("s".into()),
        set: JobId("set".into()),
        center_hz: m.center_hz,
        bandwidth_hz: features.bandwidth_hz,
        sample_rate_hz: m.sample_rate_hz,
        start_ns: m.start.0,
        end_ns: m.start.0 + iq.len() as u64 * NS_PER_S / rate,
        bytes: 0,
        features: serde_json::to_value(&features).expect("features"),
        signal: None,
        job: JobId("set".into()),
        watch: None,
        allocations: json!([]),
        pinned: false,
        written_ns: 0,
    };
    let plan = zeta_core::bits::bits_plan(
        &[record],
        &features,
        label.as_deref(),
        modulation,
        baud,
        None,
    )
    .expect("plan");
    let stages = plan_stages(&plan);
    let stages: Vec<(&str, Value)> = stages
        .iter()
        .map(|(k, p)| (k.as_str(), p.clone()))
        .collect();
    let events = run_chain_at(&stages, &iq, rate, m.center_hz.0)
        .await
        .expect("the chain runs");
    let chips = recovered_bits(&events);
    let shown: String = chips.iter().map(|c| char::from(b'0' + c)).collect();
    (plan, (shown, ev1527_codes(&chips)))
}

/// A real EV1527-style remote (433.882 MHz, about 30 dB SNR, 3200 chips/s) captured on
/// the air: every whole frame that the chain `bits_start` builds recovers reads the
/// remote's code (its annotation's `zeta:expected`). The captures are private: without
/// them the test says so and passes; the synthesised remote below runs everywhere.
#[tokio::test]
async fn a_real_ook_remote_decodes_to_its_code() {
    for (name, least) in [("ook-remote-433882k-1", 3), ("ook-remote-433882k-2", 2)] {
        let Some((meta, want)) = fixture(name) else {
            continue;
        };
        let (chips, codes) = remote_codes(&meta).await;
        assert!(
            codes.len() >= least && codes.iter().all(|c| want.contains(c)),
            "{name}: codes {codes:06x?} from {} chips {chips}",
            chips.len()
        );
    }
}

/// Whole frames each B210 window holds, read by pulse width: 12, one allowed lost at a
/// window edge (the chain reads 12 and 13).
const B210_LEAST_FRAMES: usize = 11;

/// The B210 windows at 61 440 Hz (another button of the same remote), their measured
/// centre 1110 Hz off the capture's and 40 kHz wide: the plan `bits_start` makes from
/// their own features shifts through a `channel` stage, and every whole frame of each
/// window still reads the code. Private captures: skipped without them.
#[tokio::test]
async fn b210_windows_decode_every_whole_frame() {
    for name in ["ook-remote-b210-1", "ook-remote-b210-2"] {
        let Some((meta, want)) = fixture(name) else {
            continue;
        };
        let (plan, (chips, codes)) = remote_codes_with(&meta, false).await;
        assert!(plan.channel_rate_hz.is_some(), "{name}: no shift: {plan:?}");
        assert!(
            codes.len() >= B210_LEAST_FRAMES && codes.iter().all(|c| want.contains(c)),
            "{name}: {} codes {codes:06x?} from {} chips {chips}",
            codes.len(),
            chips.len()
        );
    }
}

/// Whole frames the long sparse windows of the weak HackRF remote
/// (`ook-remote-hackrf-sparse-*`: one press of two buttons, both codes in each
/// annotation) decode at least, from their own re-measured features with nothing given:
/// the 431 ms window holds about 10 frames, the chain reads 9; the 471 ms one about 11,
/// the chain reads 8. One fewer each is allowed.
const SPARSE_LEAST_FRAMES: [(&str, usize); 2] = [
    ("ook-remote-hackrf-sparse-1", 8),
    ("ook-remote-hackrf-sparse-3", 7),
];

/// The 431 ms and 471 ms windows of the weak remote, their features measured afresh
/// (OOK at about 3200 Bd; the 471 ms one's band, 56 580 Hz of mostly noise, clamped to
/// the capture): the chain a bare `bits_start` builds decodes whole frames, every one of
/// them one of the remote's codes. Private captures: skipped without them.
#[tokio::test]
async fn a_weak_remote_decodes_from_its_own_measured_features() {
    for (name, least) in SPARSE_LEAST_FRAMES {
        let Some((meta, want)) = fixture(name) else {
            continue;
        };
        let (plan, (chips, codes)) = remote_codes_from(&meta, Features::Remeasured).await;
        eprintln!("{name}: {} codes {codes:06x?} plan {plan:?}", codes.len());
        assert_eq!(
            plan.modulation,
            zeta_core::bits::Modulation::Ook,
            "{plan:?}"
        );
        assert!(
            codes.len() >= least && codes.iter().all(|c| want.contains(c)),
            "{name}: {} codes {codes:06x?} from {} chips {chips}",
            codes.len(),
            chips.len()
        );
    }
}

/// The public synthesised remotes (`gen_ook`: an invented ID, two buttons, 8 frames
/// each; edges biased as a real fob's read, the second window weaker, 1.9 kHz off the
/// centre and opening on 20 ms of silence).
const SYNTH_REMOTES: [&str; 2] = ["ook-ev1527-synth-1", "ook-ev1527-synth-2"];
/// Frames each synthesised window holds.
const SYNTH_FRAMES: usize = 8;

/// The synthesised remotes through the three plans the real ones take (the recorded
/// features centred, as recorded, and measured afresh with nothing given): every
/// whole frame reads the window's code, and at most one of the 8 is lost (the chain
/// reads 7 of them on each window and plan).
#[tokio::test]
async fn a_synthesised_ook_remote_decodes_every_frame_to_its_code() {
    for name in SYNTH_REMOTES {
        let (meta, want) = fixture(name).expect("a public fixture");
        assert_eq!(want.len(), 1, "{name}: one code a window");
        for (how, features) in [
            ("centred", Features::Recorded { centred: true }),
            ("recorded", Features::Recorded { centred: false }),
            ("remeasured", Features::Remeasured),
        ] {
            let (plan, (chips, codes)) = remote_codes_from(&meta, features).await;
            eprintln!("{name} {how}: {} whole frames", codes.len());
            assert_eq!(
                plan.modulation,
                zeta_core::bits::Modulation::Ook,
                "{name} {how}: {plan:?}"
            );
            assert!(
                codes.len() >= SYNTH_FRAMES - 1 && codes.iter().all(|c| want.contains(c)),
                "{name} {how}: {} codes {codes:06x?} from {} chips {chips}",
                codes.len(),
                chips.len()
            );
        }
    }
}

/// The committed synthesised fixtures are `gen_ook`'s output: the same samples (to
/// float rounding across platforms), the same expected codes.
#[test]
fn ev1527_fixtures_match_their_generator() {
    let dir = tempfile::tempdir().unwrap();
    for f in zeta_dsp::fixtures::ev1527_fixtures() {
        let fresh = zeta_dsp::fixtures::write_ev1527_fixture(dir.path(), &f);
        let (committed, _) = fixture(f.stem).expect("a public fixture");
        let (cm, ciq) = zeta_core::capture::read_cf32(&committed).unwrap();
        let (fm, fiq) = zeta_core::capture::read_cf32(&fresh).unwrap();
        assert_eq!(
            (cm.sample_rate_hz, cm.center_hz, cm.start.0),
            (fm.sample_rate_hz, fm.center_hz, fm.start.0),
            "{}",
            f.stem
        );
        assert_eq!(
            cm.annotations[0]["zeta:expected"], fm.annotations[0]["zeta:expected"],
            "{}",
            f.stem
        );
        assert_eq!(ciq.len(), fiq.len(), "{}", f.stem);
        let worst = ciq
            .iter()
            .zip(&fiq)
            .map(|(a, b)| (a - b).norm())
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-4, "{}: samples differ by {worst}", f.stem);
    }
}

/// A `framing` spec: 8 frames of 32 pseudo-random bytes behind a 16-bit sync with a
/// CRC-16/CCITT, fsk at 4800 Bd, `framing_extra` merged over the framing.
fn framing_spec(framing_extra: Value) -> SynthSpec {
    let mut framing = json!({"sync": {"hex": "2dd4", "bits": 16}, "length": {"fixed_bits": 272}, "crc": "crc16-ccitt"});
    for (k, v) in framing_extra.as_object().expect("an object") {
        framing[k] = v.clone();
    }
    let spec = json!({"family": "framing", "modulation": "fsk", "symbol_rate_baud": FSK_BAUD,
        "deviation_hz": 2400.0, "frames": 8, "body_bytes": 32, "seed": SEED,
        "framing": framing, "repeat": 1});
    serde_json::from_value(spec).expect("a framing spec")
}

/// The `frame` events of the chain over `spec` at 20 dB, resampled and demodulated as
/// the fsk chain is, framed by the spec's own framing.
async fn framing_events(spec: &SynthSpec) -> Vec<Value> {
    let SynthSpec::Framing { framing, .. } = spec else {
        panic!("a framing spec")
    };
    let mut iq = synthesise(spec, &impairments(20.0, OFFSET_HZ), IN_RATE, 0);
    // The resampler keeps its last symbol back (see `RESAMPLE_TAIL_SYMBOLS`), which would
    // cost the last frame its CRC: a capture runs on past a burst, so end in silence.
    iq.resize(iq.len() + IN_RATE as usize / 100, Complex::new(0.0, 0.0));
    run_chain(
        &[
            ("resample", json!({"out_rate_hz": FSK_RATE})),
            (
                "fsk_demod",
                json!({"symbol_rate_baud": FSK_BAUD, "order": 2}),
            ),
            ("clock_recover", json!({"symbol_rate_baud": FSK_BAUD})),
            ("slicer", json!({"levels": 2})),
            (
                "frame_decode",
                serde_json::to_value(framing).expect("framing"),
            ),
        ],
        &iq,
        IN_RATE as u64,
    )
    .await
    .expect("the chain runs")
    .into_iter()
    .filter(|e| e["protocol"] == "frame")
    .collect()
}

/// `bytes` as lowercase hex.
fn hex_of_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
async fn framing_family_decodes_to_crc_clean_frames() {
    let spec = framing_spec(json!({}));
    let SynthSpec::Framing { framing, .. } = &spec else {
        unreachable!()
    };
    let planted = zeta_dsp::synth::framing_bits(&spec).expect("planted bits");
    let (want, _) = zeta_dsp::framing::decode_frames(&planted, framing);
    let events = framing_events(&spec).await;
    let good = events
        .iter()
        .filter(|e| {
            e["crc_ok"] == true
                && want.iter().any(|w| {
                    e["bytes"].as_str().expect("hex")[..64] == hex_of_bytes(&w.bytes[..32])
                })
        })
        .count();
    assert!(
        good >= 7,
        "{good} of 8 frames crc-clean and planted: {events:?}"
    );
}

#[tokio::test]
async fn framing_family_round_trips_manchester_and_pn9() {
    let spec = framing_spec(json!({"encoding": "manchester", "whitening": "pn9"}));
    let events = framing_events(&spec).await;
    let good = events.iter().filter(|e| e["crc_ok"] == true).count();
    assert!(good >= 7, "{good} of 8 frames crc_ok: {events:?}");
}
