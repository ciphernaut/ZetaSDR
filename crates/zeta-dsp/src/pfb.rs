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
        let m = self.bins;
        let hop = m / 2;
        let len = self.taps_len();
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
            // Commutator rotation: the newest input index is t*hop + hop - 1;
            // rotating the branches left by that index mod m gives each bin the
            // absolute-time mixing phase, so odd bins are not frequency-flipped.
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
        // fftshift: natural bin k is transform bin (k + m/2) mod m.
        (0..m)
            .map(|k| {
                let col = (k + hop) % m;
                self.rows.iter().skip(col).step_by(m).copied().collect()
            })
            .collect()
    }
}
