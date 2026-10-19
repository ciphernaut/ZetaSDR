//! FFT stage v2: rows at a fixed rate of signal time, power-averaged and quantised.

use std::sync::Arc;

use num_complex::Complex;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::port::IqBlock;
use zeta_core::stage::{Stage, StageContext};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};
use zeta_dsp::fft::{CpuFft, FftStage, Window, power_spectrum_db};

const CENTER_HZ: u64 = 100_000_000;

fn prov() -> Provenance {
    Provenance {
        source: SourceId("s".into()),
        job: None,
        channel: None,
        span: Span {
            start_ns: Timestamp(0),
            end_ns: Timestamp(1),
        },
    }
}

/// A tone at `freq_hz` (amplitude 0.5) plus uniform noise, `len` samples from absolute
/// sample `start`.
fn signal(start: u64, len: usize, freq_hz: f64, rate_hz: f64, seed: &mut u64) -> Vec<Complex<f32>> {
    let mut rnd = move || {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((*seed >> 40) as f32 / (1u64 << 24) as f32) - 0.5
    };
    (0..len)
        .map(|i| {
            let ph = 2.0
                * std::f64::consts::PI
                * (((start + i as u64) as f64) * freq_hz / rate_hz).fract();
            let (s, c) = ph.sin_cos();
            Complex::new(0.5 * c as f32 + 0.01 * rnd(), 0.5 * s as f32 + 0.01 * rnd())
        })
        .collect()
}

/// Runs the fft stage over `blocks` (each `(t0 sample index, samples)`) at `rate`; the events.
async fn run_stage(
    params: serde_json::Value,
    rate: u64,
    blocks: Vec<(u64, Vec<Complex<f32>>)>,
) -> Vec<(Span, serde_json::Value)> {
    let bus = Arc::new(StreamBus::new(4096));
    let out = bus.register(StreamKind::Event, prov());
    let mut rx_out = bus.subscribe(&out).unwrap();
    let (tx, rx_in) = broadcast::channel(blocks.len() + 1);
    for (start, samples) in blocks {
        tx.send(Frame::new(
            StreamId::new(),
            prov(),
            Payload::Iq(IqBlock {
                t0: Timestamp((start as u128 * 1_000_000_000 / rate as u128) as u64),
                sample_rate_hz: Hz(rate),
                center_hz: Hz(CENTER_HZ),
                samples: samples.into(),
            }),
        ))
        .unwrap();
    }
    drop(tx);
    FftStage::new(Arc::new(CpuFft::new()))
        .run(
            params,
            vec![rx_in],
            vec![out],
            bus,
            CancellationToken::new(),
            StageContext::new(SourceId("s".into())),
        )
        .await
        .unwrap();
    let mut evs = Vec::new();
    while let Ok(f) = rx_out.try_recv() {
        let Payload::Event(ev) = f.payload else {
            panic!("expected an event")
        };
        evs.push((f.provenance.span, ev));
    }
    evs
}

#[tokio::test]
async fn rows_are_emitted_at_rows_per_s_regardless_of_block_rate() {
    const RATE: u64 = 1_000_000;
    const BLOCK: usize = 1024;
    let n_blocks = (2 * RATE as usize).div_ceil(BLOCK);
    let mut seed = 7;
    let blocks: Vec<_> = (0..n_blocks)
        .map(|i| {
            let start = (i * BLOCK) as u64;
            (
                start,
                signal(start, BLOCK, 100_000.0, RATE as f64, &mut seed),
            )
        })
        .collect();
    let evs = run_stage(
        serde_json::json!({"size": 1024, "window": "hann", "rows_per_s": 20}),
        RATE,
        blocks,
    )
    .await;
    assert!((39..=41).contains(&evs.len()), "{} rows", evs.len());
    let mut last_end = 0;
    for (span, ev) in &evs {
        assert_eq!(ev["q"].as_array().unwrap().len(), 1024);
        assert!(
            ev["q"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v.as_u64().is_some_and(|q| q <= 255))
        );
        assert!(ev["floor_db"].as_f64().is_some(), "{ev}");
        assert_eq!(ev["step_db"].as_f64(), Some(0.5));
        assert_eq!(ev["rows_per_s"], 20);
        assert_eq!(ev["size"], 1024);
        assert_eq!(ev["center_hz"], CENTER_HZ);
        assert_eq!(ev["bin_hz"].as_f64(), Some(RATE as f64 / 1024.0));
        assert!(ev.get("db").is_none(), "v2 rows carry no db key");
        // Each row spans about 50 ms of signal time, rows abut.
        let dur = span.end_ns.0 - span.start_ns.0;
        assert!(
            (45_000_000..=55_000_000).contains(&dur),
            "row span {dur} ns"
        );
        assert!(span.start_ns.0 >= last_end, "rows overlap");
        last_end = span.end_ns.0;
    }
    // The tone stays at its bin: 100 kHz above centre.
    let q: Vec<u64> = evs[0].1["q"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect();
    let argmax = (0..q.len()).max_by_key(|&i| q[i]).unwrap();
    assert!(
        (argmax as i64 - (512 + 102)).abs() <= 1,
        "peak bin {argmax}"
    );
}

#[tokio::test]
async fn quantised_row_reconstructs_within_half_a_step() {
    // 1.024 Msps at 20 rows/s: one row is exactly 50 chunks of 1024.
    const RATE: u64 = 1_024_000;
    const ROW: usize = 51_200;
    let mut seed = 11;
    let x = signal(0, ROW, 250_000.0, RATE as f64, &mut seed);
    let want = power_spectrum_db(&x, 1024, Window::Hann);
    let evs = run_stage(
        serde_json::json!({"size": 1024, "window": "hann", "rows_per_s": 20}),
        RATE,
        vec![(0, x)],
    )
    .await;
    assert_eq!(evs.len(), 1);
    let ev = &evs[0].1;
    let floor = ev["floor_db"].as_f64().unwrap();
    let step = ev["step_db"].as_f64().unwrap();
    let q = ev["q"].as_array().unwrap();
    let mut inside = 0;
    let mut max_err = 0.0f64;
    for (w, q) in want.iter().zip(q) {
        let w = *w as f64;
        if w < floor || w > floor + 255.0 * step {
            continue;
        }
        inside += 1;
        let got = floor + q.as_u64().unwrap() as f64 * step;
        max_err = max_err.max((got - w).abs());
    }
    assert!(inside >= 1000, "{inside} bins inside the dynamic range");
    assert!(max_err <= 0.25 + 1e-3, "max error {max_err} dB");
}

#[tokio::test]
async fn lagged_input_drops_the_partial_row_and_reanchors() {
    // Current-thread runtime: the stage runs only while this task awaits, so sending more
    // than CAP frames without awaiting makes its receiver lag.
    const RATE: u64 = 1_000_000;
    const BLOCK: usize = 1024;
    const CAP: usize = 64; // a power of two: tokio keeps exactly CAP frames
    const ROW_NS: u64 = 50_000_000;
    const GAP_START: u64 = 5_000_000; // first sample after the gap (5 s)
    let ns = |sample: u64| (sample as u128 * 1_000_000_000 / RATE as u128) as u64;
    let bus = Arc::new(StreamBus::new(4096));
    let out = bus.register(StreamKind::Event, prov());
    let mut rx_out = bus.subscribe(&out).unwrap();
    let (tx, rx_in) = broadcast::channel(CAP);
    let o2 = out.clone();
    let h = tokio::spawn(async move {
        FftStage::new(Arc::new(CpuFft::new()))
            .run(
                serde_json::json!({"size": 1024, "window": "hann", "rows_per_s": 20}),
                vec![rx_in],
                vec![o2],
                bus,
                CancellationToken::new(),
                StageContext::new(SourceId("s".into())),
            )
            .await
    });
    let mut seed = 3;
    let mut send = |start: u64| {
        tx.send(Frame::new(
            StreamId::new(),
            prov(),
            Payload::Iq(IqBlock {
                t0: Timestamp(ns(start)),
                sample_rate_hz: Hz(RATE),
                center_hz: Hz(CENTER_HZ),
                samples: signal(start, BLOCK, 100_000.0, RATE as f64, &mut seed).into(),
            }),
        ))
        .unwrap();
    };
    let drain = || tokio::time::sleep(std::time::Duration::from_millis(30));
    // 0.725 s: 14 rows close, a partial row is left open.
    let before = 725_000usize.div_ceil(BLOCK);
    for i in 0..before {
        send((i * BLOCK) as u64);
        if i % 16 == 15 {
            drain().await;
        }
    }
    drain().await;
    // Lost frames, then the first CAP blocks after the gap, all while the stage is idle:
    // it sees Lagged(5) and then exactly the CAP post-gap blocks.
    for i in 0..5 {
        send(1_000_000 + (i * BLOCK) as u64);
    }
    let after = 1_000_000usize.div_ceil(BLOCK);
    for i in 0..after {
        send(GAP_START + (i * BLOCK) as u64);
        if i + 1 >= CAP && (i + 1 - CAP).is_multiple_of(16) {
            drain().await;
        }
    }
    drop(tx);
    h.await.unwrap().unwrap();
    let mut spans = Vec::new();
    while let Ok(f) = rx_out.try_recv() {
        spans.push(f.provenance.span);
    }
    let gap_ns = ns(GAP_START);
    assert!(
        spans
            .iter()
            .all(|s| s.end_ns.0 <= ns(before as u64 * BLOCK as u64) || s.start_ns.0 >= gap_ns),
        "a row spans the gap: {spans:?}"
    );
    let pre = spans.iter().filter(|s| s.end_ns.0 < gap_ns).count();
    assert_eq!(pre, 14, "rows before the gap");
    let post: Vec<_> = spans.iter().filter(|s| s.start_ns.0 >= gap_ns).collect();
    assert_eq!(post.len(), 20, "rows after the gap: {post:?}");
    for (k, s) in post.iter().enumerate() {
        let want = gap_ns + k as u64 * ROW_NS;
        let off = s.start_ns.0.abs_diff(want);
        assert!(
            off <= ns(BLOCK as u64),
            "row {k} starts {off} ns off the new cadence: {s:?}"
        );
    }
    assert_eq!(
        post[0].start_ns.0, gap_ns,
        "the first row after the gap starts at its t0"
    );
}
