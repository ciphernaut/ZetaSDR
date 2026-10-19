//! Polyphase filter bank: geometry, tone placement and leakage, streaming.

use num_complex::Complex;
use zeta_dsp::fft::{Window, power_spectrum_db};
use zeta_dsp::pfb::{PfbBank, bins_for};

const RATE: u64 = 24_000_000;
const BINS: usize = 32;
const TONE_HZ: f64 = 3_100_000.0;
const BLOCK: usize = 65_536;
/// Bin outputs skipped at the start: the prototype spans 8 x bins inputs,
/// i.e. 16 output hops; skip a comfortable multiple of that.
const SETTLE: usize = 64;

/// `len` samples of a unit tone at `freq_hz` starting at absolute sample `start`.
fn tone_at(start: u64, len: usize, freq_hz: f64, rate_hz: f64) -> Vec<Complex<f32>> {
    let step = freq_hz / rate_hz;
    (0..len)
        .map(|i| {
            let ph = 2.0 * std::f64::consts::PI * (((start + i as u64) as f64) * step).fract();
            let (s, c) = ph.sin_cos();
            Complex::new(c as f32, s as f32)
        })
        .collect()
}

/// Push `lens` consecutive tone blocks; concatenated per-bin outputs.
fn run(bank: &mut PfbBank, lens: &[usize]) -> Vec<Vec<Complex<f32>>> {
    let mut out = vec![Vec::new(); BINS];
    let mut t = 0u64;
    for &n in lens {
        let got = bank.push(&tone_at(t, n, TONE_HZ, RATE as f64));
        assert_eq!(got.len(), BINS);
        for (o, g) in out.iter_mut().zip(got) {
            o.extend(g);
        }
        t += n as u64;
    }
    out
}

#[test]
fn bins_for_follows_the_750k_rule() {
    assert_eq!(bins_for(24_000_000), 32);
    assert_eq!(bins_for(50_000_000), 64);
    assert_eq!(bins_for(2_000_000), 8);
    assert_eq!(bins_for(1_000_000_000), 256);
}

#[test]
fn bank_reports_its_geometry() {
    let bank = PfbBank::new(RATE, BINS);
    assert_eq!(bank.bin_rate_hz(), 1_500_000);
    assert_eq!(bank.bin_offset_hz(0), -12_000_000);
    assert_eq!(bank.bin_offset_hz(16), 0);
    assert_eq!(bank.bin_offset_hz(20), 3_000_000);
    assert_eq!(bank.bin_offset_hz(31), 11_250_000);
}

#[test]
fn a_tone_lands_in_its_bin_and_leaks_60_db_less_elsewhere() {
    // 2 s at 24 Msps pushed in 65536-sample blocks; only per-bin power sums and
    // bin 20's stream are kept, so memory stays small.
    let mut bank = PfbBank::new(RATE, BINS);
    let total = 2 * RATE as usize;
    let mut power = [0.0f64; BINS];
    let mut count = [0usize; BINS];
    let mut seen = [0usize; BINS];
    let mut bin20 = Vec::new();
    let mut t = 0usize;
    while t < total {
        let n = BLOCK.min(total - t);
        let got = bank.push(&tone_at(t as u64, n, TONE_HZ, RATE as f64));
        for (k, g) in got.iter().enumerate() {
            assert_eq!(g.len(), n / (BINS / 2), "bin {k} sample count");
            for s in g {
                if seen[k] >= SETTLE {
                    power[k] += s.norm_sqr() as f64;
                    count[k] += 1;
                }
                seen[k] += 1;
            }
        }
        bin20.extend_from_slice(&got[20][..]);
        t += n;
    }
    assert_eq!(bin20.len(), 2 * 1_500_000);

    // Peak of bin 20's 1.5 Msps stream at +100 kHz.
    let size = 4096;
    let spec = power_spectrum_db(&bin20[SETTLE..], size, Window::Hann);
    let (peak, _) = spec
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .unwrap();
    let res = 1_500_000.0 / size as f64;
    let peak_hz = (peak as f64 - (size / 2) as f64) * res;
    assert!(
        (peak_hz - 100_000.0).abs() <= res,
        "bin 20 peak at {peak_hz} Hz"
    );

    let db: Vec<f64> = (0..BINS)
        .map(|k| 10.0 * (power[k] / count[k] as f64).log10())
        .collect();
    let worst = (0..BINS)
        .filter(|&k| k != 20)
        .map(|k| db[k] - db[20])
        .fold(f64::MIN, f64::max);
    eprintln!(
        "bin 20 power {:.2} dB, worst other bin {worst:.1} dB relative",
        db[20]
    );
    assert!(db[20].abs() < 1.0, "bin 20 power {} dB", db[20]);
    for k in (0..BINS).filter(|&k| k != 20) {
        assert!(
            db[k] - db[20] <= -60.0,
            "bin {k} only {:.1} dB below bin 20",
            db[20] - db[k]
        );
    }
}

#[test]
fn bank_output_is_continuous_across_blocks() {
    // One push versus sixteen of uneven length (several not multiples of
    // bins/2, so partial hops carry across calls).
    let total = 16 * BLOCK;
    let mut whole = PfbBank::new(RATE, BINS);
    let one = run(&mut whole, &[total]);
    let mut lens: Vec<usize> = (0..15).map(|i| BLOCK + 7 * i - 49).collect();
    lens.push(total - lens.iter().sum::<usize>());
    let mut split = PfbBank::new(RATE, BINS);
    let many = run(&mut split, &lens);
    for k in 0..BINS {
        assert_eq!(one[k].len(), total / (BINS / 2));
        assert_eq!(many[k].len(), one[k].len(), "bin {k} length");
        for (i, (a, b)) in one[k].iter().zip(&many[k]).enumerate().skip(SETTLE) {
            assert!((a - b).norm() <= 1e-4, "bin {k} sample {i}: {a} vs {b}");
        }
    }
}

#[test]
fn bank_handles_a_short_final_block() {
    let hop = BINS / 2;
    let mut bank = PfbBank::new(RATE, BINS);
    let x = tone_at(0, 3 * hop, TONE_HZ, RATE as f64);
    let first = bank.push(&x[..hop + 5]);
    assert!(first.iter().all(|b| b.len() == 1));
    let short = bank.push(&x[hop + 5..2 * hop - 1]);
    assert_eq!(short.len(), BINS);
    assert!(short.iter().all(Vec::is_empty), "short push emitted");
    let rest = bank.push(&x[2 * hop - 1..]);
    assert!(rest.iter().all(|b| b.len() == 2));

    let mut ref_bank = PfbBank::new(RATE, BINS);
    let all = ref_bank.push(&x);
    for k in 0..BINS {
        let got: Vec<_> = first[k].iter().chain(&rest[k]).copied().collect();
        assert_eq!(got, all[k], "bin {k}");
    }
}

#[test]
#[cfg_attr(debug_assertions, ignore)]
fn pfb_runs_faster_than_realtime_at_24msps() {
    let block = tone_at(0, BLOCK, TONE_HZ, RATE as f64);
    let mut bank = PfbBank::new(RATE, BINS);
    let blocks = (2 * RATE as usize).div_ceil(BLOCK);
    let start = std::time::Instant::now();
    let mut n = 0usize;
    for _ in 0..blocks {
        n += bank.push(&block)[0].len();
    }
    let el = start.elapsed();
    eprintln!("2 s of 24 Msps through 32 bins in {el:?}");
    assert_eq!(n, blocks * BLOCK / (BINS / 2));
    assert!(el.as_secs_f64() < 2.0, "took {el:?}");
}
