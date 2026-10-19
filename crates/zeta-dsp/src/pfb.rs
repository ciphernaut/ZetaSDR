//! Streaming 2x-oversampled polyphase filter bank: splits a wideband IQ stream
//! into `bins` equally spaced channels, each at `2 x rate / bins`.

use std::f64::consts::PI;
use std::sync::Arc;

use num_complex::Complex;
use rustfft::{Fft, FftPlanner};

/// Narrowest bin width the bank aims for.
pub const BIN_MIN_HZ: u64 = 750_000;
/// Fewest bins a bank uses.
pub const MIN_BINS: usize = 8;
/// Most bins a bank uses.
pub const MAX_BINS: usize = 256;
/// Prototype filter length in multiples of `bins`.
const TAPS_PER_BIN: usize = 16;
/// Prototype cutoff (half-amplitude point) in bin widths from the bin centre.
/// Past the half-bin crossover, so a channel centred on a bin boundary with a
/// half-width up to a quarter bin sits in the flat passband of the nearer bin.
const CUTOFF_BINS: f64 = 0.75;

/// Bin count for a source at `rate_hz`: the largest power of two with
/// `rate_hz / bins >= BIN_MIN_HZ`, clamped to `MIN_BINS..=MAX_BINS`.
pub fn bins_for(rate_hz: u64) -> usize {
    let mut bins = 1usize;
    while bins < MAX_BINS && rate_hz / (2 * bins as u64) >= BIN_MIN_HZ {
        bins *= 2;
    }
    bins.clamp(MIN_BINS, MAX_BINS)
}

/// Blackman-windowed sinc prototype low-pass: `len` taps, cutoff `fc`
/// cycles/sample, unity DC gain.
///
/// Blackman for its deep stopband: with the 0.75-bin cutoff and `16 x bins`
/// taps, a tone reaches no bin whose span (centre +/- one bin width) excludes
/// it by more than -80 dB, so nothing aliases into a bin's stream. Bins
/// overlap: a tone inside a neighbour's span passes there at its true
/// frequency, and the channel's fine filter removes it.
fn prototype(len: usize, fc: f64) -> Vec<f32> {
    let mid = (len - 1) as f64 / 2.0;
    let m = (len - 1) as f64;
    let h: Vec<f64> = (0..len)
        .map(|i| {
            let t = i as f64 - mid;
            let sinc = if t == 0.0 {
                2.0 * fc
            } else {
                (2.0 * PI * fc * t).sin() / (PI * t)
            };
            let x = 2.0 * PI * i as f64 / m;
            sinc * (0.42 - 0.5 * x.cos() + 0.08 * (2.0 * x).cos())
        })
        .collect();
    let sum: f64 = h.iter().sum();
    h.iter().map(|v| (v / sum) as f32).collect()
}

/// A streaming 2x-oversampled polyphase channeliser.
///
/// Bin `k` (natural order, lowest frequency first) is the input mixed down by
/// `(k - bins/2) x rate / bins`, low-pass filtered by the prototype and
/// decimated by `bins / 2`. Input carries across [`PfbBank::push`] calls, so
/// splitting the input differently gives the same outputs.
pub struct PfbBank {
    rate_hz: u64,
    bins: usize,
    /// Prototype taps reversed, so a hop is a dot product with the newest
    /// `taps.len()` input samples in time order.
    /// Each reversed tap twice (`[t, t]`), matching interleaved re/im input.
    rev_taps: Vec<f32>,
    /// The last `taps.len() - 1` consumed samples, then unconsumed input.
    /// Interleaved re/im, so the filter loop is plain `f32` multiply-adds
    /// the compiler vectorises.
    buf: Vec<f32>,
    /// Hops produced so far, modulo 2 (selects the commutator rotation).
    hop_parity: usize,
    fft: Arc<dyn Fft<f32>>,
    /// Per-push transform buffer, `bins` samples per hop.
    rows: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// Per-hop branch accumulator, `bins` samples interleaved re/im, reused
    /// across hops.
    acc: Vec<f32>,
}

impl PfbBank {
    /// A bank of `bins` channels over a source at `rate_hz`. Panics unless
    /// `bins` is a power of two of at least 2.
    pub fn new(rate_hz: u64, bins: usize) -> PfbBank {
        assert!(
            bins >= 2 && bins.is_power_of_two(),
            "PfbBank: bins must be a power of two >= 2, got {bins}"
        );
        let len = TAPS_PER_BIN * bins;
        let mut rev_taps = prototype(len, CUTOFF_BINS / bins as f64);
        rev_taps.reverse();
        let rev_taps: Vec<f32> = rev_taps.iter().flat_map(|&t| [t, t]).collect();
        // Inverse transform: bin k sums branch m with exp(+j 2 pi k m / bins).
        let fft = FftPlanner::new().plan_fft_inverse(bins);
        let scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
        PfbBank {
            rate_hz,
            bins,
            rev_taps,
            buf: vec![0.0; 2 * (len - 1)],
            hop_parity: 0,
            fft,
            rows: Vec::new(),
            scratch,
            acc: vec![0.0; 2 * bins],
        }
    }

    /// Sample rate of every bin's stream: `2 x rate / bins`.
    pub fn bin_rate_hz(&self) -> u64 {
        2 * self.rate_hz / self.bins as u64
    }

    /// Centre of bin `k` relative to the source centre: `(k - bins/2) x rate / bins`.
    pub fn bin_offset_hz(&self, k: usize) -> i64 {
        let rel = k as i128 - (self.bins / 2) as i128;
        (rel * self.rate_hz as i128 / self.bins as i128) as i64
    }

    /// Number of bins.
    pub fn bins(&self) -> usize {
        self.bins
    }

    /// Prototype filter length in input samples (`16 x bins`). Bin output
    /// `h` is produced with input `h x bins/2 + bins/2 - 1` as its newest
    /// sample; its group delay is `(taps_len - 1) / 2` input samples.
    pub fn taps_len(&self) -> usize {
        self.rev_taps.len() / 2
    }

    /// Feed `iq`; returns one Vec per bin (natural order) holding the samples
    /// produced by this call: one per complete hop of `bins / 2` inputs, with
    /// a trailing partial hop kept for the next call.
    pub fn push(&mut self, iq: &[Complex<f32>]) -> Vec<Vec<Complex<f32>>> {
        let all: Vec<usize> = (0..self.bins).collect();
        self.push_bins(iq, &all)
    }

    /// As [`PfbBank::push`], but returns only the bins in `active` (natural
    /// order indices, each `< bins`), one Vec per entry in `active`'s order.
    /// Every bin is still filtered (the transform needs all branches); only
    /// the gather is skipped for the others.
    pub fn push_bins(&mut self, iq: &[Complex<f32>], active: &[usize]) -> Vec<Vec<Complex<f32>>> {
        let m = self.bins;
        let mask = m - 1;
        let hop = m / 2;
        let len = self.taps_len();
        let tw = 2 * m;
        self.buf.extend(iq.iter().flat_map(|c| [c.re, c.im]));
        let hops = (self.buf.len() / 2 + 1 - len) / hop;
        // Every hop's rotated branch outputs, one `m`-sample row per hop, then
        // transformed in one batched call.
        self.rows.clear();
        self.rows.resize(hops * m, Complex::default());
        let acc = &mut self.acc;
        for (h, row) in self.rows.chunks_exact_mut(m).enumerate() {
            // Window of the newest `len` samples ending at this hop's last input.
            let start = h * hop + hop - 1;
            let win = &self.buf[2 * start..2 * (start + len)];
            // acc[r] sums reversed taps j = c*m + r. Tap index j in time order
            // is n = len - 1 - j samples back from the newest, i.e. polyphase
            // branch b = m - 1 - r.
            mac(acc, &self.rev_taps, win, tw);
            // Commutator rotation: the newest input index is t*hop + hop - 1;
            // rotating the branches left by that index mod m gives each bin the
            // absolute-time mixing phase, so odd bins are not frequency-flipped.
            let shift = self.hop_parity * hop + hop - 1;
            for (i, v) in row.iter_mut().enumerate() {
                let r = mask - ((i + shift) & mask);
                *v = Complex::new(acc[2 * r], acc[2 * r + 1]);
            }
            self.hop_parity ^= 1;
        }
        if hops > 0 {
            self.fft
                .process_with_scratch(&mut self.rows, &mut self.scratch);
        }
        self.buf.drain(..2 * hops * hop);
        // fftshift: natural bin k is transform bin (k + m/2) mod m.
        active
            .iter()
            .map(|&k| {
                let col = (k + hop) & mask;
                self.rows.iter().skip(col).step_by(m).copied().collect()
            })
            .collect()
    }

    /// The straightforward implementation [`PfbBank::push`] replaced, kept as
    /// the reference its outputs are compared against.
    #[cfg(test)]
    fn push_reference(&mut self, iq: &[Complex<f32>]) -> Vec<Vec<Complex<f32>>> {
        let m = self.bins;
        let hop = m / 2;
        let len = self.taps_len();
        self.buf.extend(iq.iter().flat_map(|c| [c.re, c.im]));
        let hops = (self.buf.len() / 2 + 1 - len) / hop;
        self.rows.clear();
        self.rows.resize(hops * m, Complex::default());
        let acc = &mut self.acc;
        for (h, row) in self.rows.chunks_exact_mut(m).enumerate() {
            let start = h * hop + hop - 1;
            let win = &self.buf[2 * start..2 * (start + len)];
            acc.fill(0.0);
            for (tc, xc) in self
                .rev_taps
                .chunks_exact(2 * m)
                .zip(win.chunks_exact(2 * m))
            {
                for ((a, &t), &x) in acc.iter_mut().zip(tc).zip(xc) {
                    *a += x * t;
                }
            }
            let shift = (self.hop_parity * hop + hop - 1) % m;
            for (i, v) in row.iter_mut().enumerate() {
                let r = m - 1 - (i + shift) % m;
                *v = Complex::new(acc[2 * r], acc[2 * r + 1]);
            }
            self.hop_parity ^= 1;
        }
        if hops > 0 {
            self.fft
                .process_with_scratch(&mut self.rows, &mut self.scratch);
        }
        self.buf.drain(..2 * hops * hop);
        (0..m)
            .map(|k| {
                let col = (k + hop) % m;
                self.rows.iter().skip(col).step_by(m).copied().collect()
            })
            .collect()
    }
}

/// Floats summed per register block in [`mac`].
const MAC_BLOCK: usize = 16;

/// `acc[i] = sum over rows c of win[c*tw + i] * taps[c*tw + i]`, for `i < tw`.
///
/// Register-blocked: each 16-float slice of `acc` is summed across every tap
/// row in a local array (registers) and stored once, instead of loading and
/// storing the whole `acc` once per row. The per-element summation order is
/// unchanged (row 0 first), so results are bit-identical to the plain loop.
fn mac(acc: &mut [f32], taps: &[f32], win: &[f32], tw: usize) {
    let full = tw - tw % MAC_BLOCK;
    for ob in (0..full).step_by(MAC_BLOCK) {
        let mut a = [0f32; MAC_BLOCK];
        for (tc, xc) in taps.chunks_exact(tw).zip(win.chunks_exact(tw)) {
            let t: &[f32; MAC_BLOCK] = tc[ob..ob + MAC_BLOCK].try_into().unwrap();
            let x: &[f32; MAC_BLOCK] = xc[ob..ob + MAC_BLOCK].try_into().unwrap();
            for i in 0..MAC_BLOCK {
                a[i] += x[i] * t[i];
            }
        }
        acc[ob..ob + MAC_BLOCK].copy_from_slice(&a);
    }
    // Fewer than 8 bins: a short tail the blocks do not cover.
    if full < tw {
        acc[full..tw].fill(0.0);
        for (tc, xc) in taps.chunks_exact(tw).zip(win.chunks_exact(tw)) {
            for ((a, &t), &x) in acc[full..tw].iter_mut().zip(&tc[full..]).zip(&xc[full..]) {
                *a += x * t;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic broadband test signal: two tones plus LCG noise.
    fn input(n: usize) -> Vec<Complex<f32>> {
        let mut seed = 12345u32;
        (0..n)
            .map(|i| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
                let p1 = 2.0 * PI * 0.0731 * i as f64;
                let p2 = -2.0 * PI * 0.3117 * i as f64;
                Complex::new(
                    (p1.cos() + 0.3 * p2.cos()) as f32 + 0.1 * noise,
                    (p1.sin() + 0.3 * p2.sin()) as f32 - 0.1 * noise,
                )
            })
            .collect()
    }

    #[test]
    fn push_matches_the_reference_path() {
        for bins in [2, 4, 8, 32, 256] {
            let x = input(40 * bins * TAPS_PER_BIN + 17);
            let mut new = PfbBank::new(24_000_000, bins);
            let mut old = PfbBank::new(24_000_000, bins);
            // Uneven blocks, so hops straddle pushes and parity alternates.
            let mut got: Vec<Vec<Complex<f32>>> = vec![Vec::new(); bins];
            let mut want: Vec<Vec<Complex<f32>>> = vec![Vec::new(); bins];
            for c in x.chunks(3 * bins + 5) {
                for (g, o) in got.iter_mut().zip(new.push(c)) {
                    g.extend(o);
                }
                for (w, o) in want.iter_mut().zip(old.push_reference(c)) {
                    w.extend(o);
                }
            }
            assert!(!got[0].is_empty());
            for (k, (g, w)) in got.iter().zip(&want).enumerate() {
                assert_eq!(g.len(), w.len(), "bins {bins} bin {k}");
                for (a, b) in g.iter().zip(w) {
                    assert!((a - b).norm() <= 1e-6, "bins {bins} bin {k}: {a} vs {b}");
                }
            }
        }
    }

    #[test]
    fn push_bins_returns_only_the_active_bins_in_order() {
        let bins = 32;
        let x = input(20 * bins * TAPS_PER_BIN);
        let mut all = PfbBank::new(24_000_000, bins);
        let mut some = PfbBank::new(24_000_000, bins);
        let active = [27, 3, 20];
        for c in x.chunks(4096) {
            let a = all.push(c);
            let s = some.push_bins(c, &active);
            assert_eq!(s.len(), active.len());
            for (o, &k) in s.iter().zip(&active) {
                assert_eq!(o, &a[k], "bin {k}");
            }
        }
    }
}
