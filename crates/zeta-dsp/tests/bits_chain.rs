//! Layer-4 chains end to end: synthesised IQ through `resample`, a demodulator,
//! `clock_recover` and `slicer`, the recovered bits checked against the planted ones.
#![cfg(feature = "test-fixtures")]

use std::sync::Arc;

use num_complex::Complex;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::port::IqBlock;
use zeta_core::stage::{Stage, StageContext};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, JobId, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};
use zeta_dsp::synth::{Impairments, SynthSpec, synthesise};

/// Input time per IQ block fed to the chain, in milliseconds.
const BLOCK_MS: u64 = 50;
/// Start of the fed IQ, in ns since the epoch.
const T0_NS: u64 = 1_000_000_000;
/// Frames each bus stream holds before a slow reader lags: more than any chain makes.
const BUS_CAPACITY: usize = 1 << 14;
const NS_PER_S: u64 = 1_000_000_000;
const MS_PER_S: u64 = 1_000;

/// The stage implementing `kind`.
fn stage_of(kind: &str) -> Arc<dyn Stage> {
    match kind {
        "resample" => Arc::new(zeta_dsp::resample::ResampleStage::new()),
        "fsk_demod" => Arc::new(zeta_dsp::fsk::FskDemodStage::new()),
        "ook_demod" => Arc::new(zeta_dsp::ook::OokDemodStage::new()),
        "clock_recover" => Arc::new(zeta_dsp::clock::ClockRecoverStage::new()),
        "slicer" => Arc::new(zeta_dsp::slicer::SlicerStage::new()),
        other => panic!("no stage {other} in the chain harness"),
    }
}

fn prov(span: Span) -> Provenance {
    Provenance {
        source: SourceId("s".into()),
        job: Some(JobId("job".into())),
        channel: None,
        span,
    }
}

/// Run `stages` in order over `iq` at `rate_hz` on a test bus: one stream registered per
/// stage output, each stage reading the previous stage's first output (the first stage
/// the IQ, fed as [`BLOCK_MS`] blocks with contiguous spans). Once the input is fed and
/// dropped every stage runs to its end, its outputs closed behind it; the result is every
/// event payload of the last stage, in order. Every stage flushes at the end except
/// `resample`, so a chain through it loses its last [`RESAMPLE_TAIL_SYMBOLS`] symbol(s):
/// compare such a chain with [`align_errors_tail`].
pub async fn run_chain(stages: &[(&str, Value)], iq: &[Complex<f32>], rate_hz: u64) -> Vec<Value> {
    let bus = Arc::new(StreamBus::new(BUS_CAPACITY));
    let block = (rate_hz * BLOCK_MS / MS_PER_S) as usize;
    let blocks = iq.chunks(block).count();
    let (tx, rx) = broadcast::channel(blocks.max(1));
    let mut input = rx;
    let mut runs = Vec::new();
    let mut last_events = Vec::new();
    let zero = Span {
        start_ns: Timestamp(0),
        end_ns: Timestamp(0),
    };
    for (i, (kind, params)) in stages.iter().enumerate() {
        let stage = stage_of(kind);
        let outputs: Vec<StreamId> = stage
            .spec()
            .outputs
            .iter()
            .map(|&k| bus.register(k, prov(zero.clone())))
            .collect();
        let next = bus.subscribe(&outputs[0]).expect("registered above");
        if i + 1 == stages.len() {
            last_events = stage
                .spec()
                .outputs
                .iter()
                .zip(&outputs)
                .filter(|(k, _)| **k == StreamKind::Event)
                .map(|(_, id)| bus.subscribe(id).expect("registered above"))
                .collect();
        }
        let (bus, params, rx) = (bus.clone(), params.clone(), input);
        runs.push(tokio::spawn(async move {
            let r = stage
                .run(
                    params,
                    vec![rx],
                    outputs.clone(),
                    bus.clone(),
                    CancellationToken::new(),
                    StageContext::new(SourceId("s".into())),
                )
                .await;
            for id in &outputs {
                bus.unregister(id);
            }
            r
        }));
        input = next;
    }
    drop(input);
    let iq_stream = StreamId::new();
    let mut t0 = T0_NS;
    for chunk in iq.chunks(block) {
        let end = t0 + chunk.len() as u64 * NS_PER_S / rate_hz;
        tx.send(Frame::new(
            iq_stream.clone(),
            prov(Span {
                start_ns: Timestamp(t0),
                end_ns: Timestamp(end),
            }),
            Payload::Iq(IqBlock {
                t0: Timestamp(t0),
                sample_rate_hz: Hz(rate_hz),
                center_hz: Hz(0),
                samples: chunk.to_vec().into(),
            }),
        ))
        .expect("the first stage subscribed");
        t0 = end;
    }
    drop(tx);
    for r in runs {
        r.await.expect("stage task").expect("stage run");
    }
    let mut events = Vec::new();
    for mut rx in last_events {
        while let Ok(f) = rx.try_recv() {
            let Payload::Event(v) = f.payload else {
                panic!("an event payload")
            };
            events.push(v);
        }
    }
    events
}

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
    let need = want.len().saturating_sub(tail_bits);
    if got.len() < need {
        return want.len();
    }
    (0..=got.len() - need)
        .map(|off| {
            let n = (got.len() - off).min(want.len());
            let e = got[off..off + n]
                .iter()
                .zip(want)
                .filter(|(g, w)| g != w)
                .count();
            e.min(n - e)
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
    .await;
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
    .await;
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
