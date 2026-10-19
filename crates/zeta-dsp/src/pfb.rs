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
const TAPS_PER_BIN: usize = 8;

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
/// Blackman rather than Hann: at `8 x bins` taps a Hann prototype leaves a
/// tone 0.87 bin widths from a neighbouring bin's centre only ~55 dB down
/// (that bin's 2x-oversampled stream passes it), while Blackman keeps every
/// other bin more than 75 dB down at the cost of slightly more passband droop.
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
    rev_taps: Vec<f32>,
    /// The last `taps.len() - 1` consumed samples, then unconsumed input.
    buf: Vec<Complex<f32>>,
    /// Hops produced so far, modulo 2 (selects the commutator rotation).
    hop_parity: usize,
    fft: Arc<dyn Fft<f32>>,
    /// Per-push transform buffer, `bins` samples per hop.
    rows: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// Per-hop branch accumulator, `bins` samples, reused across hops.
    acc: Vec<Complex<f32>>,
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
        let mut rev_taps = prototype(len, 1.0 / (2.0 * bins as f64));
        rev_taps.reverse();
        // Inverse transform: bin k sums branch m with exp(+j 2 pi k m / bins).
        let fft = FftPlanner::new().plan_fft_inverse(bins);
        let scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
        PfbBank {
            rate_hz,
            bins,
            rev_taps,
            buf: vec![Complex::default(); len - 1],
            hop_parity: 0,
            fft,
            rows: Vec::new(),
            scratch,
            acc: vec![Complex::default(); bins],
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

    /// Feed `iq`; returns one Vec per bin (natural order) holding the samples
    /// produced by this call: one per complete hop of `bins / 2` inputs, with
    /// a trailing partial hop kept for the next call.
    pub fn push(&mut self, iq: &[Complex<f32>]) -> Vec<Vec<Complex<f32>>> {
        let m = self.bins;
        let hop = m / 2;
        let len = self.rev_taps.len();
        self.buf.extend_from_slice(iq);
        let hops = (self.buf.len() + 1 - len) / hop;
        // Every hop's rotated branch outputs, one `m`-sample row per hop, then
        // transformed in one batched call.
        self.rows.clear();
        self.rows.resize(hops * m, Complex::default());
        let acc = &mut self.acc;
        for (h, row) in self.rows.chunks_exact_mut(m).enumerate() {
            // Window of the newest `len` samples ending at this hop's last input.
            let start = h * hop + hop - 1;
            let win = &self.buf[start..start + len];
            // acc[r] sums reversed taps j = c*m + r. Tap index j in time order
            // is n = len - 1 - j samples back from the newest, i.e. polyphase
            // branch b = m - 1 - r.
            acc.fill(Complex::default());
            for (tc, xc) in self.rev_taps.chunks_exact(m).zip(win.chunks_exact(m)) {
                for ((a, &t), &x) in acc.iter_mut().zip(tc).zip(xc) {
                    *a += x * t;
                }
            }
            // Commutator rotation: the newest input index is t*hop + hop - 1;
            // rotating the branches left by that index mod m gives each bin the
            // absolute-time mixing phase, so odd bins are not frequency-flipped.
            let shift = (self.hop_parity * hop + hop - 1) % m;
            for (i, v) in row.iter_mut().enumerate() {
                *v = acc[m - 1 - (i + shift) % m];
            }
            self.hop_parity ^= 1;
        }
        if hops > 0 {
            self.fft
                .process_with_scratch(&mut self.rows, &mut self.scratch);
        }
        self.buf.drain(..hops * hop);
        // fftshift: natural bin k is transform bin (k + m/2) mod m.
        (0..m)
            .map(|k| {
                let col = (k + hop) % m;
                self.rows.iter().skip(col).step_by(m).copied().collect()
            })
            .collect()
    }
}
