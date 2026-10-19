//! Streaming rational resampler: upsample by L, low-pass, keep every Mth.

use crate::util::lowpass_taps;

/// Low-pass cutoff as a fraction of the lower of the two rates.
const CUTOFF: f64 = 0.45;

/// Prototype taps per unit of `max(L, M)`.
const TAPS_PER_FACTOR: u64 = 8;

/// Fewest prototype taps: small factors (96k→48k gives 17 by the formula)
/// would otherwise leave a transition band too wide to stop aliasing.
const MIN_TAPS: u64 = 63;

/// Largest `max(L, M)` accepted, so at most 800 001 prototype taps (the
/// 63-tap floor only applies below `max(L, M) = 8`). Coprime rates (a
/// prime source rate, say) otherwise reduce to factors in the millions and a
/// prototype of tens of megabytes.
pub const MAX_FACTOR: u64 = 100_000;

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Streaming resampler from `in_rate_hz` to `out_rate_hz` by the reduced
/// ratio L/M. The prototype is a Hann-windowed sinc low-pass at
/// `0.45 × min(in, out)` designed at the upsampled rate `in × L`, with
/// `max(8 × max(L, M) + 1, 63)` taps (odd either way) and gain L. Evaluation is polyphase: output `m`
/// sits at upsampled index `m × M`, uses phase `p = m × M mod L` and the input
/// samples ending at `m × M / L`, so the zero-stuffed signal never exists.
/// Input history and the output phase carry across [`Resampler::process`]
/// calls, so any block split gives the same output as one call.
#[derive(Debug, Clone)]
pub struct Resampler {
    l: u64,
    m: u64,
    /// `phases[p][j] = L × h[p + j L]`, zero-padded to a common length.
    phases: Vec<Vec<f32>>,
    /// Last `phases[0].len() - 1` input samples, oldest first (the
    /// `taps.len() - 1` upsampled samples the FIR spans, minus the stuffed zeros).
    history: Vec<f32>,
    /// Upsampled index of the next output, relative to the next block's first sample.
    next: u64,
}

impl Resampler {
    /// Resampler from `in_rate_hz` to `out_rate_hz`; both must be at least 1
    /// and the reduced ratio's larger term at most [`MAX_FACTOR`].
    pub fn new(in_rate_hz: u64, out_rate_hz: u64) -> Result<Resampler, String> {
        if in_rate_hz == 0 || out_rate_hz == 0 {
            return Err(format!(
                "resampler rates must be >= 1, got {in_rate_hz} -> {out_rate_hz}"
            ));
        }
        let g = gcd(in_rate_hz, out_rate_hz);
        let (l, m) = (out_rate_hz / g, in_rate_hz / g);
        if l == 1 && m == 1 {
            return Ok(Resampler {
                l,
                m,
                phases: Vec::new(),
                history: Vec::new(),
                next: 0,
            });
        }
        if l.max(m) > MAX_FACTOR {
            return Err(format!(
                "resampler {in_rate_hz} -> {out_rate_hz} Hz reduces to {l}/{m}, beyond the {MAX_FACTOR} limit"
            ));
        }
        let n = (TAPS_PER_FACTOR * l.max(m) + 1).max(MIN_TAPS) as usize;
        let cutoff = CUTOFF * in_rate_hz.min(out_rate_hz) as f64;
        let h = lowpass_taps(cutoff, in_rate_hz as f64 * l as f64, n);
        let lu = l as usize;
        let k = n.div_ceil(lu);
        let gain = l as f32;
        let phases: Vec<Vec<f32>> = (0..lu)
            .map(|p| {
                (0..k)
                    .map(|j| h.get(p + j * lu).map_or(0.0, |&t| t * gain))
                    .collect()
            })
            .collect();
        Ok(Resampler {
            l,
            m,
            history: vec![0.0; k - 1],
            phases,
            next: 0,
        })
    }

    /// The reduced ratio `(L, M)`: output rate / input rate = L / M.
    pub fn ratio(&self) -> (u64, u64) {
        (self.l, self.m)
    }

    /// Resample the next block of input. Over a stream of N inputs fed to a
    /// fresh resampler the outputs total `ceil(N × L / M)`; a single block's
    /// count varies by one because `next` carries the remainder between calls.
    pub fn process(&mut self, x: &[f32]) -> Vec<f32> {
        if self.l == 1 && self.m == 1 {
            return x.to_vec();
        }
        let h = self.history.len();
        let mut ext = Vec::with_capacity(h + x.len());
        ext.extend_from_slice(&self.history);
        ext.extend_from_slice(x);
        let end = x.len() as u64 * self.l;
        let mut out = Vec::with_capacity(((end.saturating_sub(self.next)) / self.m + 1) as usize);
        let mut t = self.next;
        while t < end {
            let i = (t / self.l) as usize;
            let taps = &self.phases[(t % self.l) as usize];
            // ext[h + i] is x[i]; tap j multiplies x[i - j].
            let acc = taps
                .iter()
                .zip(ext[..=h + i].iter().rev())
                .fold(0.0f32, |acc, (&c, &s)| acc + c * s);
            out.push(acc);
            t += self.m;
        }
        self.next = t - end;
        if h > 0 {
            self.history.copy_from_slice(&ext[ext.len() - h..]);
        }
        out
    }
}
