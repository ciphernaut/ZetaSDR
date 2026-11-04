//! FSK order and tone spacing, PSK order and on-off keying's two envelope
//! levels, for layer 1. Like the symbol rate, the estimates run over the raw
//! runs ([`super::burst::View::runs`]), never the Hann-windowed on-time: a
//! window's edges are not keying.

use num_complex::Complex;
use rustfft::FftPlanner;
use zeta_core::types::Hz;

use super::burst::{BURST_MIN_PERIOD_MS, averaged_half_spectrum, bandpass, burst_runs, hann};
use super::{FSK_SEP_HZ, LOCAL_BINS, MIN_LINE_HZ, SIZE, discriminator, median};
use crate::fft::{CpuFft, FftBackend, PowerSum, Window};

/// FSK: a cluster of the instantaneous-frequency histogram holds at least
/// this share of the on-time samples (as do OOK's two envelope levels).
const FSK_CLUSTER_MIN_WEIGHT: f64 = 0.1;
/// FSK: the instantaneous-frequency histogram spans the occupied band in
/// this many bins.
const FSK_HIST_BINS: usize = 64;
/// FSK: a line of the instantaneous frequency's spectrum is that bin and
/// this many either side (the Hann main lobe).
const FSK_LINE_BINS: usize = 2;
/// FSK: when one line holds more than this share of the instantaneous
/// frequency's power, the frequency is swept by a tone (FM by a test tone,
/// or AFSK audio under FM), whose two turning points are not tones (measured:
/// an FM test tone 0.998, AFSK under FM 0.41-0.46, random 2- and 4-FSK
/// 0.16-0.22).
const FSK_TONE_SHARE_MAX: f64 = 0.3;
/// FSK: only the samples whose envelope reaches this fraction of the
/// `FSK_ON_PERCENTILE`th-percentile envelope are histogrammed. FSK's envelope
/// is constant, so this keeps its every sample; on-off keying's off samples are
/// noise, whose instantaneous frequency spreads across the band, and with them
/// a real OOK remote's carrier read as two clusters 2.5-8 kHz apart.
const FSK_ON_FRACTION: f32 = 0.5;
/// FSK: the envelope percentile `FSK_ON_FRACTION` is taken of.
const FSK_ON_PERCENTILE: usize = 90;
/// Histograms are smoothed by a mean over this many bins.
const SMOOTH_BINS: usize = 3;
/// PSK: the orders tried by the m-th power test, smallest first.
const PSK_ORDERS: [u8; 3] = [2, 4, 8];
/// PSK: the line of `x^m` stands this many dB over its local median.
const PSK_LINE_DB: f32 = 10.0;
/// OOK: the envelope histogram spans 0 to the envelope's `OOK_TOP_PERCENTILE`th
/// percentile in this many bins.
const OOK_HIST_BINS: usize = 32;
/// OOK: the envelope level the histogram's top bin reaches.
const OOK_TOP_PERCENTILE: usize = 99;
/// OOK: the lower envelope level is off, below this fraction of the upper
/// (an AM tone's envelope also has two modes, at its troughs and crests:
/// 1/3 of the crest at 50 % modulation).
const OOK_OFF_MAX: f32 = 0.25;

/// Clusters of `hist` (`total` samples, those outside it included): the
/// local maxima of its `SMOOTH_BINS` mean, each holding the samples of its
/// basin (from the lowest point between it and the previous maximum to the
/// lowest before the next), kept when that is at least `min_weight` of
/// `total`; each as (bin, weight), in bin order.
fn clusters(hist: &[f64], total: usize, min_weight: f64) -> Vec<(usize, f64)> {
    let n = hist.len();
    if n == 0 || total == 0 {
        return Vec::new();
    }
    let h = SMOOTH_BINS / 2;
    let smooth: Vec<f64> = (0..n)
        .map(|i| {
            let (a, b) = (i.saturating_sub(h), (i + h + 1).min(n));
            hist[a..b].iter().sum::<f64>() / (b - a) as f64
        })
        .collect();
    let maxima: Vec<usize> = (0..n)
        .filter(|&i| {
            (i == 0 || smooth[i] > smooth[i - 1]) && (i + 1 == n || smooth[i] >= smooth[i + 1])
        })
        .filter(|&i| smooth[i] > 0.0)
        .collect();
    let valley = |a: usize, b: usize| {
        (a..=b)
            .min_by(|&i, &j| smooth[i].total_cmp(&smooth[j]))
            .unwrap_or(a)
    };
    maxima
        .iter()
        .enumerate()
        .filter_map(|(k, &m)| {
            let lo = if k == 0 {
                0
            } else {
                valley(maxima[k - 1], m) + 1
            };
            let hi = maxima.get(k + 1).map_or(n - 1, |&next| valley(m, next));
            let w = hist[lo..=hi].iter().sum::<f64>() / total as f64;
            (w >= min_weight).then_some((m, w))
        })
        .collect()
}

/// FSK order and tone spacing of `runs`: each run band-passed to the occupied
/// band `offset_hz ± bandwidth_hz / 2`, its instantaneous frequency
/// (discriminator) where the signal is on (the envelope at least
/// `FSK_ON_FRACTION` of its `FSK_ON_PERCENTILE`th percentile over the runs)
/// histogrammed across that band in `FSK_HIST_BINS` bins, and the histogram's
/// [`clusters`] holding at least `FSK_CLUSTER_MIN_WEIGHT` of the samples
/// counted. Two or four clusters give the order and the
/// median distance between adjacent ones; any other count is `None`. So is a
/// spacing, or an occupied band, under `FSK_SEP_HZ.0`: a tone pair closer
/// than the two-peak rule's minimum is not an FSK order.
pub(super) fn fsk_clusters(
    runs: &[&[Complex<f32>]],
    rate_hz: u64,
    (offset_hz, bandwidth_hz): (f64, u64),
) -> Option<(u8, Hz)> {
    let bw = bandwidth_hz as f64;
    if bw < FSK_SEP_HZ.0 || rate_hz == 0 {
        return None;
    }
    let band: Vec<Vec<Complex<f32>>> = runs
        .iter()
        .map(|r| bandpass(r, rate_hz, offset_hz, bw))
        .collect();
    fsk_clusters_in(&band, rate_hz, (offset_hz, bandwidth_hz))
}

/// [`fsk_clusters`] of `band`, runs already band-passed to `offset_hz ± bandwidth_hz / 2`.
fn fsk_clusters_in(
    band: &[Vec<Complex<f32>>],
    rate_hz: u64,
    (offset_hz, bandwidth_hz): (f64, u64),
) -> Option<(u8, Hz)> {
    let bw = bandwidth_hz as f64;
    if bw < FSK_SEP_HZ.0 || rate_hz == 0 {
        return None;
    }
    let bin = bw / FSK_HIST_BINS as f64;
    let mut hist = vec![0.0f64; FSK_HIST_BINS];
    let mut total = 0usize;
    let env: Vec<f32> = band.iter().flatten().map(|s| s.norm()).collect();
    let on = FSK_ON_FRACTION * super::percentile(&env, FSK_ON_PERCENTILE);
    // Each run's discriminator, its samples where the signal is off (either sample it
    // spans under `on`) held at the run's mean on-frequency: they take no part in the
    // histogram, and in the tone share's spectrum they add no power off its DC.
    let freqs: Vec<Vec<f32>> = band
        .iter()
        .map(|x| {
            let d = discriminator(x);
            let is_on: Vec<bool> = x
                .windows(2)
                .map(|w| w[0].norm().min(w[1].norm()) >= on)
                .collect();
            let (sum, n) = d
                .iter()
                .zip(&is_on)
                .filter(|(_, o)| **o)
                .fold((0.0f64, 0usize), |(s, n), (v, _)| (s + *v as f64, n + 1));
            let mean = (sum / n.max(1) as f64) as f32;
            d.iter()
                .zip(&is_on)
                .map(|(&v, &o)| if o { v } else { mean })
                .collect()
        })
        .collect();
    let on_samples = band.iter().zip(&freqs).flat_map(|(x, d)| {
        d.iter()
            .zip(x.windows(2))
            .filter(|(_, w)| w[0].norm().min(w[1].norm()) >= on)
            .map(|(d, _)| d)
    });
    for d in on_samples {
        total += 1;
        let f = *d as f64 * rate_hz as f64 / std::f64::consts::TAU - offset_hz + bw / 2.0;
        if (0.0..bw).contains(&f) {
            hist[((f / bin) as usize).min(FSK_HIST_BINS - 1)] += 1.0;
        }
    }
    let share = tone_share(&freqs, rate_hz, bw / 2.0);
    if share.is_some_and(|s| s > FSK_TONE_SHARE_MAX) {
        return None;
    }
    let c = clusters(&hist, total, FSK_CLUSTER_MIN_WEIGHT);
    if c.len() != 2 && c.len() != 4 {
        return None;
    }
    let gaps: Vec<f32> = c
        .windows(2)
        .map(|w| ((w[1].0 - w[0].0) as f64 * bin) as f32)
        .collect();
    let spacing = median(&gaps) as f64;
    (spacing >= FSK_SEP_HZ.0).then(|| (c.len() as u8, Hz(spacing.round() as u64)))
}

/// Share of the power of the instantaneous frequency `freqs` (one per run, its off
/// samples held at the run's mean on-frequency by [`fsk_clusters`];
/// its spectrum averaged over them) between `MIN_LINE_HZ` and `max_hz` that
/// its strongest line (that bin and `FSK_LINE_BINS` either side) holds.
fn tone_share(freqs: &[Vec<f32>], rate_hz: u64, max_hz: f64) -> Option<f64> {
    let db = averaged_half_spectrum(freqs, SIZE)?;
    let bin_hz = rate_hz as f64 / SIZE as f64;
    let lo = (MIN_LINE_HZ / bin_hz).ceil() as usize;
    let hi = ((max_hz / bin_hz) as usize).min(db.len() - 1);
    if lo >= hi {
        return None;
    }
    let p: Vec<f64> = db.iter().map(|&v| 10f64.powf(v as f64 / 10.0)).collect();
    let total: f64 = p[lo..=hi].iter().sum();
    let k = (lo..=hi).max_by(|&a, &b| p[a].total_cmp(&p[b]))?;
    let line: f64 = p[k.saturating_sub(FSK_LINE_BINS).max(lo)..=(k + FSK_LINE_BINS).min(hi)]
        .iter()
        .sum();
    (total > 0.0).then(|| line / total)
}

/// PSK order of `runs` by the m-th power test: each run band-passed to the
/// occupied band `offset_hz ± bandwidth_hz / 2` and raised to the power `m`,
/// which folds an `m`-point constellation onto one phase, so `x^m` carries a
/// carrier line at `m × offset_hz` (its strongest bin within `m` bins of it
/// in the `SIZE`-point spectrum averaged over the runs) standing `PSK_LINE_DB`
/// over its local median (`LOCAL_BINS` either side, its main lobe excluded).
/// The smallest `m` of `PSK_ORDERS` with that line is the order. `None` when
/// none has one, or when `x` itself has such a line anywhere in the occupied
/// band: a carrier that is not suppressed (AM, OOK, FM) or discrete tones,
/// whose powers have lines wherever their sums fall.
pub(super) fn psk_order(
    runs: &[&[Complex<f32>]],
    rate_hz: u64,
    (offset_hz, bandwidth_hz): (f64, u64),
) -> Option<u8> {
    let band: Vec<Vec<Complex<f32>>> = runs
        .iter()
        .map(|r| bandpass(r, rate_hz, offset_hz, bandwidth_hz as f64))
        .collect();
    let bin_hz = rate_hz as f64 / SIZE as f64;
    let fft = CpuFft::new();
    // The strongest margin over its local median of the bins within `reach`
    // of m × offset in the spectrum of x^m.
    let line_db = |m: u8, reach: isize| {
        let mut acc = PowerSum::new(SIZE);
        for x in band.iter().filter(|x| !x.is_empty()) {
            let mut y: Vec<Complex<f32>> = x.iter().map(|s| s.powi(i32::from(m))).collect();
            if y.len() >= SIZE {
                acc.add(&fft.power_sum(&y, SIZE, Window::Hann));
            } else {
                let n = y.len();
                y.iter_mut().enumerate().for_each(|(i, v)| *v *= hann(i, n));
                y.resize(SIZE, Complex::new(0.0, 0.0));
                acc.add(&fft.power_sum(&y, SIZE, Window::Rect));
            }
        }
        if acc.chunks == 0 {
            return f32::NEG_INFINITY;
        }
        let db = acc.db();
        // The fftshifted bin of m × offset, aliased into the spectrum.
        let centre = (f64::from(m) * offset_hz / bin_hz).round() as isize + (SIZE / 2) as isize;
        (centre - reach..=centre + reach)
            .map(|j| j.rem_euclid(SIZE as isize) as usize)
            .map(|k| {
                let near: Vec<f32> = (k.saturating_sub(LOCAL_BINS)..(k + LOCAL_BINS + 1).min(SIZE))
                    .filter(|&j| j.abs_diff(k) > 2)
                    .map(|j| db[j])
                    .collect();
                db[k] - median(&near)
            })
            .fold(f32::NEG_INFINITY, f32::max)
    };
    let half_band = (bandwidth_hz as f64 / 2.0 / bin_hz) as isize;
    let l1 = line_db(1, half_band.max(1));
    if l1 >= PSK_LINE_DB {
        return None;
    }
    PSK_ORDERS.into_iter().find(|&m| {
        let l = line_db(m, isize::from(m));
        l >= PSK_LINE_DB
    })
}

/// Whether the envelope `|x|` of `runs` sits at two levels: its histogram
/// from 0 to the `OOK_TOP_PERCENTILE`th percentile in `OOK_HIST_BINS` bins
/// has exactly two [`clusters`] of at least `FSK_CLUSTER_MIN_WEIGHT` each, the
/// lower under `OOK_OFF_MAX` of the upper. The caller passes the burst runs
/// ([`envelope_runs`]), so the gaps between bursts are not an off level.
pub(super) fn two_levels(runs: &[&[Complex<f32>]]) -> bool {
    let env: Vec<f32> = runs
        .iter()
        .flat_map(|r| r.iter())
        .map(|s| s.norm())
        .collect();
    let top = super::percentile(&env, OOK_TOP_PERCENTILE);
    if top <= 0.0 {
        return false;
    }
    let mut hist = vec![0.0f64; OOK_HIST_BINS];
    for &e in &env {
        let b = (e / top * OOK_HIST_BINS as f32) as usize;
        hist[b.min(OOK_HIST_BINS - 1)] += 1.0;
    }
    let c = clusters(&hist, env.len(), FSK_CLUSTER_MIN_WEIGHT);
    // The off level, below the on level's `OOK_OFF_MAX`.
    c.len() == 2 && (c[0].0 as f32 + 0.5) < OOK_OFF_MAX * (c[1].0 as f32 + 0.5)
}

/// The samples whose envelope is read for its levels: the window's burst runs
/// ([`burst_runs`]) when they repeat no faster than `BURST_MIN_PERIOD_MS`
/// (bursts, whose gaps are not keying, whatever the duty), else the whole
/// window (one run, or dense on-off runs, which are the keying itself).
pub(super) fn envelope_runs(iq: &[Complex<f32>], rate_hz: u64) -> Vec<&[Complex<f32>]> {
    let b = burst_runs(iq, rate_hz);
    if b.period_ms.is_some_and(|p| p >= BURST_MIN_PERIOD_MS) {
        b.runs.iter().map(|&(s, e)| &iq[s..e]).collect()
    } else {
        vec![iq]
    }
}

/// OOK chip rate: an edge turns the keying on above this fraction of the way from the
/// envelope's off level to its on level ([`envelope_levels`]) ...
const OOK_EDGE_ON: f32 = 0.6;
/// ... and off below this fraction (the hysteresis between the two keeps
/// noise riding an edge from cutting a run).
const OOK_EDGE_OFF: f32 = 0.4;
/// OOK chip rate: the 2-medians of the envelope's two levels start from these
/// percentiles (the off level from the low one; the on level from the high one, so
/// an on level under a tenth of the window still starts on it) ...
const OOK_LEVEL_SEED_PERCENTILES: (usize, usize) = (10, OOK_TOP_PERCENTILE);
/// ... and are refined this many times.
const OOK_LEVEL_ITERATIONS: usize = 8;
/// OOK chip rate: the seed unit is this percentile of the run lengths. The
/// shortest runs are one chip and in pulse-width keying they are common (a 0 is
/// one chip on, a 1 one chip off), so the 10th percentile is one, past a
/// stray short run.
const OOK_UNIT_SEED_PERCENTILE: usize = 10;
/// OOK chip rate: runs of more than this many units are not fitted (a frame's
/// sync silence, the gaps between frames): they hold no timing finer than a
/// short run does.
const OOK_UNIT_MAX_MULTIPLE: f64 = 8.0;
/// OOK chip rate: a run fits a multiple of the unit within this many units.
const OOK_UNIT_TOLERANCE: f64 = 0.25;
/// OOK chip rate: at least this share of the fitted runs fit ...
const OOK_UNIT_MIN_SHARE: f64 = 0.8;
/// ... and at least this many runs fit.
const OOK_UNIT_MIN_RUNS: usize = 16;
/// OOK chip rate: a unit shorter than this many samples is not timing.
const OOK_UNIT_MIN_SAMPLES: f64 = 2.0;
/// OOK chip rate: refinements of the unit by its least-squares fit.
const OOK_UNIT_ITERATIONS: usize = 4;
/// OOK chip rate: the runs of at most `OOK_UNIT_MAX_MULTIPLE` units must be at least
/// this share of all runs. A half or a third of the chip fits every run of a few chips,
/// but pushes the longer ones past the bound.
const OOK_UNIT_MIN_FITTED_SHARE: f64 = 0.5;
/// OOK chip rate: the multiples of the converged unit tried as the chip, the largest
/// that still fits kept: a seed on a half or a third of the chip converges there, and
/// every run is then a whole multiple of it as of the chip. One and a half: a weak
/// remote's pulses, narrowed by the band and the noise by a third of a chip, read its
/// 1:3 and 3:1 bits as 1:5 and 4:2 of two thirds of the chip, which fit too.
const OOK_UNIT_MULTIPLES: [f64; 3] = [1.5, 2.0, 3.0];
/// OOK chip rate: the edge bias, how much every on run reads short and every off run
/// long (the band's rise time and the edge thresholds), is fitted with the unit up to
/// this many units either way. Measured on a weak HackRF remote in a 6.6 kHz band: a
/// sixth to a third of a chip.
const OOK_UNIT_MAX_BIAS: f64 = 0.3;
/// OOK chip rate: when the occupied band holds no fit, the band is narrowed by this
/// factor and tried again ...
const OOK_UNIT_BAND_STEP: f64 = 0.5;
/// ... down to this width, in Hz. A weak signal's occupied band, measured over a
/// short window, reaches far into the noise (a 71 ms window of a remote read 37 kHz
/// for about 7), and the noise in it cuts every pulse.
const OOK_UNIT_MIN_BAND_HZ: f64 = 2_000.0;
/// ... and to at least this fraction of the occupied band (three halvings). The weak
/// remote's 471 ms window fitted at an eighth of its 56 580 Hz measured band; narrower,
/// a burst train's envelope fits on its own (a synthesised Mode S train measured 49 kHz
/// wide fitted 2005 Bd at a sixteenth).
const OOK_UNIT_MIN_BAND_FRACTION: f64 = 0.125;
/// OOK chip rate: the off runs must stand at least this many dB under the on runs in a
/// band twice as wide as the fit's. A band that holds one tone of an FSK signal keys on
/// and off with its symbols, but the wider band holds the other tone in the off runs.
const OOK_OFF_DB: f64 = 6.0;

/// OOK chip rate: a chip rate over this many times the band it was measured in is not
/// keying the band can carry (on-off keying's main lobe spans twice its rate): a PSK
/// signal's envelope dips at its phase changes, and in a 3 kHz band 2400 Bd BPSK read
/// 4068.
const OOK_UNIT_MAX_RATE_PER_HZ: f64 = 1.0;

/// One on or off run of the keyed envelope: `[start, end)` in its slice.
#[derive(Clone, Copy, Debug, PartialEq)]
struct KeyRun {
    /// The slice of the window's runs it lies in.
    slice: usize,
    start: usize,
    end: usize,
    on: bool,
}

impl KeyRun {
    fn length(&self) -> f64 {
        (self.end - self.start) as f64
    }
}

/// The off and on levels of the envelope `env`: a 2-medians seeded at the
/// `OOK_LEVEL_SEED_PERCENTILES` and refined up to `OOK_LEVEL_ITERATIONS` times (until
/// it stops moving), each level the median of the samples on its side of their
/// midpoint. `None` when the two meet.
fn envelope_levels(env: &[f32]) -> Option<(f32, f32)> {
    let (mut lo, mut hi) = (
        super::percentile(env, OOK_LEVEL_SEED_PERCENTILES.0),
        super::percentile(env, OOK_LEVEL_SEED_PERCENTILES.1),
    );
    let (mut below, mut above) = (Vec::with_capacity(env.len()), Vec::with_capacity(env.len()));
    for _ in 0..OOK_LEVEL_ITERATIONS {
        let mid = 0.5 * (lo + hi);
        below.clear();
        above.clear();
        for &e in env {
            if e < mid {
                below.push(e)
            } else {
                above.push(e)
            }
        }
        if below.is_empty() || above.is_empty() {
            return None;
        }
        let next = (upper_median(&mut below), upper_median(&mut above));
        if next == (lo, hi) {
            break;
        }
        (lo, hi) = next;
    }
    (hi > lo).then_some((lo, hi))
}

/// The upper median of `v` (as [`median`] reads it), reordering it: a selection, not a
/// sort.
fn upper_median(v: &mut [f32]) -> f32 {
    let k = v.len() / 2;
    *v.select_nth_unstable_by(k, f32::total_cmp).1
}

/// The runs' spectra, taken once, from which every band [`ook_unit_rate`] tries is cut
/// at a decimated rate: one forward transform a run (each zero-padded to a whole number
/// of `pad` samples), then for each band a small inverse transform of the bins inside
/// it, moved to baseband, at the capture rate over a divisor of `pad`
/// ([`Spectra::decim_for`]).
struct Spectra {
    bins: Vec<Vec<Complex<f32>>>,
    /// Each run's length before padding.
    lens: Vec<usize>,
    rate_hz: u64,
    /// The coarsest decimation: the largest divisor of the rate that keeps
    /// `OOK_BAND_OVERSAMPLE` times the narrowest band tried.
    pad: usize,
}

impl Spectra {
    fn new(planner: &mut FftPlanner<f32>, runs: &[&[Complex<f32>]], rate_hz: u64) -> Self {
        let least = (OOK_BAND_OVERSAMPLE * OOK_UNIT_MIN_BAND_HZ).ceil() as u64;
        let pad = (rate_hz / zeta_core::ops::channel_rate_hz(rate_hz, least.min(rate_hz))).max(1)
            as usize;
        let bins = runs
            .iter()
            .map(|r| {
                let mut buf = r.to_vec();
                buf.resize(r.len().next_multiple_of(pad), Complex::new(0.0, 0.0));
                if !buf.is_empty() {
                    planner.plan_fft_forward(buf.len()).process(&mut buf);
                }
                buf
            })
            .collect();
        Spectra {
            bins,
            lens: runs.iter().map(|r| r.len()).collect(),
            rate_hz,
            pad,
        }
    }

    /// The decimation a band `bandwidth_hz` wide is cut at: the largest divisor of `pad`
    /// keeping a rate of at least `OOK_BAND_OVERSAMPLE` times the band (twice the band,
    /// [`off_under_on_db`]'s, then fits it twice over).
    fn decim_for(&self, bandwidth_hz: f64) -> usize {
        (1..=self.pad)
            .rev()
            .find(|&d| {
                self.pad.is_multiple_of(d)
                    && self.rate_hz as f64 / d as f64 >= OOK_BAND_OVERSAMPLE * bandwidth_hz
            })
            .unwrap_or(1)
    }

    /// The rate a band is cut at with `decim`.
    fn out_rate_hz(&self, decim: usize) -> u64 {
        self.rate_hz / decim as u64
    }

    /// Each run's band `offset_hz ± bandwidth_hz / 2`, moved to baseband at
    /// [`Self::out_rate_hz`] of `decim` (a divisor of `pad`): `len / decim` samples,
    /// sample `j` the band-limited run at sample `j · decim` (the band at most that rate
    /// wide).
    fn band(
        &self,
        planner: &mut FftPlanner<f32>,
        offset_hz: f64,
        bandwidth_hz: f64,
        decim: usize,
    ) -> Vec<Vec<Complex<f32>>> {
        self.bins
            .iter()
            .zip(&self.lens)
            .map(|(x, &len)| {
                let n = x.len();
                let m = n / decim;
                if m == 0 {
                    return Vec::new();
                }
                let bin_hz = self.rate_hz as f64 / n as f64;
                let centre = (offset_hz / bin_hz).round() as i64;
                let half = (bandwidth_hz / 2.0 / bin_hz).floor() as i64;
                let mut y = vec![Complex::new(0.0f32, 0.0); m];
                // Only the bins inside the band: signed bin `centre - half ..= centre + half`.
                for signed in (centre - half)..=(centre + half) {
                    if (signed as f64 * bin_hz - offset_hz).abs() > bandwidth_hz / 2.0 {
                        continue;
                    }
                    let k = signed.rem_euclid(n as i64) as usize;
                    y[(signed - centre).rem_euclid(m as i64) as usize] += x[k];
                }
                planner.plan_fft_inverse(m).process(&mut y);
                let scale = 1.0 / n as f32;
                y.truncate(len / decim);
                y.iter_mut().for_each(|v| *v *= scale);
                y
            })
            .collect()
    }
}

/// OOK chip rate: each band is cut at a rate of at least this many times its width (the
/// capture rate over a whole divisor), so a 1 s window at 2.4 Msps is worked at tens of
/// kHz, twice the band still fits for the off-run checks, and a chip in the narrowest
/// band spans several samples.
const OOK_BAND_OVERSAMPLE: f64 = 4.0;

/// What [`ook_unit_rate`] fitted, with what the label's veto reads of it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct OokFit {
    pub baud: zeta_core::types::Baud,
    /// The band the fit was made in, Hz.
    pub bandwidth_hz: f64,
    /// How many dB the fitted off runs (those of at most `OOK_UNIT_MAX_MULTIPLE` units)
    /// stand over the noise floor in twice that band ([`off_over_noise_db`]); `None`
    /// when it cannot be measured.
    pub off_over_noise_db: Option<f64>,
    /// Whether twice that band carries FSK tones ([`fsk_clusters_in`]): a narrow band
    /// holding one tone of an FSK signal keys with its symbols.
    pub wide_tones: bool,
}

/// The chip rate of on-off keying in `runs` from its run lengths: each band cut from
/// one transform of the runs at a decimated rate ([`Spectra`]); [`ook_unit_rate_in`] over
/// the occupied band `offset_hz ± bandwidth_hz / 2`, else over it narrowed by
/// `OOK_UNIT_BAND_STEP` at a time down to `OOK_UNIT_MIN_BAND_HZ` and
/// `OOK_UNIT_MIN_BAND_FRACTION` of it, the first that fits,
/// and none when the occupied band carries FSK tones ([`fsk_clusters_in`], taken once,
/// when a band fits; a narrower band may hold one tone, which keys on and off with the
/// symbols). The cyclostationary lines of pulse-width keying stand at its bit rate and
/// harmonics (an EV1527 remote, a bit of four chips, read 1598 Bd for 3200), while its
/// run lengths are whole chips.
pub(super) fn ook_unit_rate(
    runs: &[&[Complex<f32>]],
    rate_hz: u64,
    (offset_hz, bandwidth_hz): (f64, u64),
) -> Option<OokFit> {
    let occupied = bandwidth_hz as f64;
    if rate_hz == 0 || occupied <= 0.0 {
        return None;
    }
    let mut planner = FftPlanner::<f32>::new();
    let spectra = Spectra::new(&mut planner, runs, rate_hz);
    let narrowest = OOK_UNIT_MIN_BAND_HZ.max(occupied * OOK_UNIT_MIN_BAND_FRACTION);
    let bands = std::iter::successors(Some(occupied), |&b| {
        Some(b * OOK_UNIT_BAND_STEP).filter(|&n| n >= narrowest)
    });
    for (i, bw) in bands.enumerate() {
        let (fit, tones) = ook_unit_rate_in(&spectra, &mut planner, offset_hz, bw);
        if let Some(f) = fit {
            // The occupied band's tones: the fitted band's own when it is that band.
            let occupied_tones = if i == 0 {
                tones
            } else {
                let d = spectra.decim_for(occupied);
                let band = spectra.band(&mut planner, offset_hz, occupied, d);
                fsk_clusters_in(&band, spectra.out_rate_hz(d), (0.0, bandwidth_hz)).is_some()
            };
            return (!occupied_tones).then_some(f);
        }
    }
    None
}

/// [`ook_unit_rate`] in one band: each run's band `offset_hz ± bandwidth_hz / 2` (from
/// `spectra`, at baseband), and when its envelope sits at two levels with the lower off
/// ([`two_levels`]) and it carries no FSK tones ([`fsk_clusters_in`]: a dense train of
/// FSK packets is two envelope levels too), cut into on and off runs at the `OOK_EDGE_ON`
/// / `OOK_EDGE_OFF` fractions between its two levels ([`envelope_levels`]; the first and
/// last run of each slice, cut by the window, dropped), and the unit found by
/// [`chip_unit`]. The fit also needs the off runs `OOK_OFF_DB` under the on runs in
/// twice the band ([`off_under_on_db`]), and a rate of at most
/// `OOK_UNIT_MAX_RATE_PER_HZ` times the band. With the fit, whether the band carries
/// FSK tones (taken only of a two-level band).
fn ook_unit_rate_in(
    spectra: &Spectra,
    planner: &mut FftPlanner<f32>,
    offset_hz: f64,
    bandwidth_hz: f64,
) -> (Option<OokFit>, bool) {
    let decim = spectra.decim_for(bandwidth_hz);
    let rate_hz = spectra.out_rate_hz(decim);
    let band = spectra.band(planner, offset_hz, bandwidth_hz, decim);
    let slices: Vec<&[Complex<f32>]> = band.iter().map(|b| b.as_slice()).collect();
    if !two_levels(&slices) {
        return (None, false);
    }
    if fsk_clusters_in(&band, rate_hz, (0.0, bandwidth_hz.round() as u64)).is_some() {
        return (None, true);
    }
    let env: Vec<f32> = band.iter().flatten().map(|s| s.norm()).collect();
    let Some((lo, hi)) = envelope_levels(&env) else {
        return (None, false);
    };
    let (on_at, off_at) = (lo + OOK_EDGE_ON * (hi - lo), lo + OOK_EDGE_OFF * (hi - lo));
    let mut keyed: Vec<KeyRun> = Vec::new();
    for (slice, x) in band.iter().enumerate() {
        let mut state = x.first().is_some_and(|s| s.norm() >= on_at);
        let mut edges: Vec<(usize, bool)> = Vec::new();
        for (i, s) in x.iter().enumerate() {
            let e = s.norm();
            if (!state && e >= on_at) || (state && e < off_at) {
                state = !state;
                edges.push((i, state));
            }
        }
        keyed.extend(edges.windows(2).map(|w| KeyRun {
            slice,
            start: w[0].0,
            end: w[1].0,
            on: w[0].1,
        }));
    }
    let lengths: Vec<(f64, bool)> = keyed.iter().map(|r| (r.length(), r.on)).collect();
    let Some((unit, _)) = chip_unit(&lengths, None) else {
        return (None, false);
    };
    let baud = rate_hz as f64 / unit;
    if baud > OOK_UNIT_MAX_RATE_PER_HZ * bandwidth_hz {
        return (None, false);
    }
    let wider = (2.0 * bandwidth_hz).min(rate_hz as f64);
    let wide = spectra.band(planner, offset_hz, wider, decim);
    let off_db = off_under_on_db(&wide, &keyed);
    if off_db < OOK_OFF_DB {
        return (None, false);
    }
    // What the veto reads ([`OokFit`]), over the runs of at most `OOK_UNIT_MAX_MULTIPLE`
    // units (a burst train's gaps are not keying).
    let fitted: Vec<KeyRun> = keyed
        .iter()
        .copied()
        .filter(|r| r.length() <= OOK_UNIT_MAX_MULTIPLE * unit)
        .collect();
    let fit = OokFit {
        baud: zeta_core::types::Baud(baud.round() as u32),
        bandwidth_hz,
        off_over_noise_db: off_over_noise_db(&wide, &fitted, rate_hz, wider),
        wide_tones: fsk_clusters_in(&wide, rate_hz, (0.0, wider.round() as u64)).is_some(),
    };
    (Some(fit), false)
}

/// OOK veto: the noise floor's spectrum is averaged over segments of at most this many
/// samples ...
const OOK_NOISE_SEGMENT: usize = 1_024;
/// ... and at least this many (shorter runs are zero-padded to it).
const OOK_NOISE_MIN_SEGMENT: usize = 64;

/// How many dB the off runs of `fitted` stand over the noise floor of `band` (the runs
/// cut `bandwidth_hz` wide at baseband, `rate_hz`): their mean power against the
/// `FLOOR_PERCENTILE`th-percentile in-band bin of the band's averaged spectrum standing
/// for every in-band bin. `None` when no off run or no spectrum.
fn off_over_noise_db(
    band: &[Vec<Complex<f32>>],
    fitted: &[KeyRun],
    rate_hz: u64,
    bandwidth_hz: f64,
) -> Option<f64> {
    let longest = band.iter().map(Vec::len).max()?;
    let size = OOK_NOISE_SEGMENT
        .min(longest.next_power_of_two() / 2)
        .max(OOK_NOISE_MIN_SEGMENT);
    let fft = CpuFft::new();
    let mut acc = PowerSum::new(size);
    for x in band.iter().filter(|x| !x.is_empty()) {
        if x.len() >= size {
            acc.add(&fft.power_sum(x, size, Window::Hann));
        } else {
            let mut y: Vec<Complex<f32>> = x
                .iter()
                .enumerate()
                .map(|(i, &v)| v * hann(i, x.len()))
                .collect();
            y.resize(size, Complex::new(0.0, 0.0));
            acc.add(&fft.power_sum(&y, size, Window::Rect));
        }
    }
    if acc.chunks == 0 {
        return None;
    }
    let bin_hz = rate_hz as f64 / size as f64;
    let half = ((bandwidth_hz / 2.0 / bin_hz) as usize).clamp(1, size / 2 - 1);
    let mut inband: Vec<f64> = acc.sum[size / 2 - half..=size / 2 + half].to_vec();
    let total: f64 = inband.iter().sum();
    inband.sort_by(f64::total_cmp);
    let floor_bin = inband[inband.len() * super::FLOOR_PERCENTILE / 100];
    let samples = band.iter().map(Vec::len).sum::<usize>();
    let mean = band
        .iter()
        .flatten()
        .map(|v| v.norm_sqr() as f64)
        .sum::<f64>()
        / samples.max(1) as f64;
    let noise = mean * floor_bin * inband.len() as f64 / total;
    let (sum, n) = fitted
        .iter()
        .filter(|r| !r.on)
        .flat_map(|r| &band[r.slice][r.start..r.end])
        .fold((0.0f64, 0usize), |(s, n), x| {
            (s + x.norm_sqr() as f64, n + 1)
        });
    (n > 0 && noise > 0.0 && total > 0.0).then(|| 10.0 * (sum / n as f64 / noise).log10())
}

/// How many dB the off runs of `keyed` stand under its on runs in `band` (the runs cut
/// in a wider band, sample for sample): their mean powers there compared; infinite for
/// silent off runs.
fn off_under_on_db(band: &[Vec<Complex<f32>>], keyed: &[KeyRun]) -> f64 {
    let mean = |on: bool| {
        let (sum, n) = keyed
            .iter()
            .filter(|r| r.on == on)
            .flat_map(|r| &band[r.slice][r.start..r.end])
            .fold((0.0f64, 0usize), |(s, n), x| {
                (s + x.norm_sqr() as f64, n + 1)
            });
        sum / n.max(1) as f64
    };
    let (on, off) = (mean(true), mean(false));
    if off <= 0.0 {
        f64::INFINITY
    } else {
        10.0 * (on / off).log10()
    }
}

/// Run `(length, on)` read as `k` units: `(length ± bias) / unit`, an on run lengthened
/// and an off run shortened by `bias` samples.
fn units(&(l, on): &(f64, bool), unit: f64, bias: f64) -> f64 {
    let sign = if on { 1.0 } else { -1.0 };
    (l + sign * bias) / unit
}

/// Whether `(unit, bias)` fits `runs`: of the runs within `OOK_UNIT_MAX_MULTIPLE` units
/// (at least `OOK_UNIT_MIN_FITTED_SHARE` of all), at least `OOK_UNIT_MIN_RUNS`, and
/// `OOK_UNIT_MIN_SHARE` of them, lie within `OOK_UNIT_TOLERANCE` of a whole multiple
/// of at least 1 (a run under half a unit fits none), each read with the bias
/// ([`units`]).
fn unit_fits(runs: &[(f64, bool)], (unit, bias): (f64, f64)) -> bool {
    if unit < OOK_UNIT_MIN_SAMPLES {
        return false;
    }
    let within: Vec<f64> = runs
        .iter()
        .map(|r| units(r, unit, bias))
        .filter(|&r| r.round() <= OOK_UNIT_MAX_MULTIPLE)
        .collect();
    let close = within
        .iter()
        .filter(|&&r| r.round() >= 1.0 && (r - r.round()).abs() <= OOK_UNIT_TOLERANCE)
        .count();
    within.len() as f64 >= OOK_UNIT_MIN_FITTED_SHARE * runs.len() as f64
        && close >= OOK_UNIT_MIN_RUNS
        && close as f64 >= OOK_UNIT_MIN_SHARE * within.len() as f64
}

/// `unit` refined `OOK_UNIT_ITERATIONS` times as the least-squares unit of the runs of
/// 1 to `OOK_UNIT_MAX_MULTIPLE` units. Without `with_bias` the unit alone (sum of lengths
/// over sum of nearest multiples, the bias 0): an edge threshold that shortens every on
/// run and lengthens every off run by the same time cancels between them when the runs
/// pair up. With it the unit and the bias together (each run `k · unit ∓ bias`, the bias
/// held to `OOK_UNIT_MAX_BIAS` units), from the unit alone's assignment of multiples.
/// `None` when the unit falls under `OOK_UNIT_MIN_SAMPLES` or no run fits.
fn refine_unit(runs: &[(f64, bool)], mut unit: f64, with_bias: bool) -> Option<(f64, f64)> {
    let mut bias = 0.0;
    for _ in 0..OOK_UNIT_ITERATIONS {
        if unit < OOK_UNIT_MIN_SAMPLES {
            return None;
        }
        // Sums over the fitted runs of k², k·s, k·l, s·l and l, k and their count, s
        // the run's sign (+1 on, -1 off).
        let (mut kk, mut ks, mut kl, mut sl, mut sum_l, mut sum_k, mut n) =
            (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        for r in runs {
            let k = units(r, unit, bias).round();
            if !(1.0..=OOK_UNIT_MAX_MULTIPLE).contains(&k) {
                continue;
            }
            let (l, s) = (r.0, if r.1 { 1.0 } else { -1.0 });
            (kk, ks, kl, sl) = (kk + k * k, ks + k * s, kl + k * l, sl + s * l);
            (sum_l, sum_k, n) = (sum_l + l, sum_k + k, n + 1.0);
        }
        if sum_k == 0.0 {
            return None;
        }
        // Minimising Σ (l + s·bias - k·unit)²: [Σk², -Σks; -Σks, n] [unit; bias] =
        // [Σkl; -Σsl].
        let det = n * kk - ks * ks;
        if with_bias && det.abs() > f64::EPSILON * n * kk {
            unit = (n * kl - ks * sl) / det;
            bias = ((ks * kl - kk * sl) / det).clamp(
                -OOK_UNIT_MAX_BIAS * unit.abs(),
                OOK_UNIT_MAX_BIAS * unit.abs(),
            );
        } else {
            unit = sum_l / sum_k;
            bias = 0.0;
        }
    }
    (unit >= OOK_UNIT_MIN_SAMPLES).then_some((unit, bias))
}

/// The chip, in samples, and the edge bias of the on and off `runs` (`(length, on)`):
/// from `seed` (else the `OOK_UNIT_SEED_PERCENTILE`th-percentile run) refined alone
/// ([`refine_unit`]), then it and each of `OOK_UNIT_MULTIPLES` of it refined alone and
/// then with the bias, the largest unit that fits ([`unit_fits`]) kept; `None` with
/// fewer than `OOK_UNIT_MIN_RUNS` runs or when none fits.
fn chip_unit(runs: &[(f64, bool)], seed: Option<f64>) -> Option<(f64, f64)> {
    if runs.len() < OOK_UNIT_MIN_RUNS {
        return None;
    }
    let seed = seed.unwrap_or_else(|| {
        let mut sorted: Vec<f64> = runs.iter().map(|r| r.0).collect();
        sorted.sort_by(f64::total_cmp);
        sorted[(sorted.len() - 1) * OOK_UNIT_SEED_PERCENTILE / 100]
    });
    let (base, _) = refine_unit(runs, seed, false)?;
    std::iter::once(1.0)
        .chain(OOK_UNIT_MULTIPLES)
        .filter_map(|m| refine_unit(runs, m * base, false))
        .flat_map(|alone| [Some(alone), refine_unit(runs, alone.0, true)])
        .flatten()
        .filter(|&fit| unit_fits(runs, fit))
        .max_by(|a, b| a.0.total_cmp(&b.0))
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use num_complex::Complex;

    use super::{chip_unit, ook_unit_rate};
    use crate::fixtures::{Impairments, SynthSpec, am_modulate, awgn, speech_like, synthesise};

    const RATE: u64 = 48_000;
    /// The EV1527 remote's chip, in samples at 61 440 Hz (3200 chips/s).
    const CHIP: f64 = 19.2;
    /// The edge threshold's bias on a real remote: on runs read this much of a chip
    /// short and off runs as much long (280 us pulses for a 313 us chip).
    const BIAS: f64 = 0.1;
    /// How far a found unit or rate may stray from the chip's, as a fraction.
    const TOLERANCE: f64 = 0.02;
    /// 240 non-repeating bits.
    const BITS: &str = "14ed58c6d3ff2d75a5650138a8d447f00ec47321d420c5d08bec3850735c";

    fn lcg(seed: u64) -> impl FnMut() -> u64 {
        let mut s = seed;
        move || {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            s >> 33
        }
    }

    /// The on and off runs, `(length in samples, on)`, of `frames` EV1527 frames (24
    /// bits, a 0 one chip on and three off, a 1 three on and one off, then the sync pulse
    /// and 31 chips of silence), each on run `bias` chips short and each off run as much
    /// long, and jittered by up to a sample.
    fn ev1527_runs(frames: usize, bias: f64) -> Vec<(f64, bool)> {
        const CODE: u32 = <button-code>;
        let mut r = lcg(0xe715);
        let mut jitter = move || (r() % 3) as f64 - 1.0;
        let mut out = Vec::new();
        for _ in 0..frames {
            let mut chips: Vec<(f64, f64)> = (0..24)
                .rev()
                .map(|b| {
                    if (CODE >> b) & 1 == 1 {
                        (3.0, 1.0)
                    } else {
                        (1.0, 3.0)
                    }
                })
                .collect();
            chips.push((1.0, 31.0));
            for (on, off) in chips {
                out.push(((on - bias) * CHIP + jitter(), true));
                out.push(((off + bias) * CHIP + jitter(), false));
            }
        }
        out
    }

    /// The runs of NRZ OOK of `bits` random bits, one chip a bit.
    fn nrz_runs(bits: usize) -> Vec<(f64, bool)> {
        let mut r = lcg(0x4e52);
        let b: Vec<u64> = (0..bits).map(|_| r() & 1).collect();
        let mut out = Vec::new();
        let mut n = 1.0;
        for w in b.windows(2) {
            if w[0] == w[1] {
                n += 1.0;
            } else {
                out.push((n * CHIP, w[0] == 1));
                n = 1.0;
            }
        }
        out
    }

    fn near_chip(u: Option<(f64, f64)>) -> bool {
        u.is_some_and(|(u, _)| (u - CHIP).abs() <= CHIP * TOLERANCE)
    }

    /// Seeded on half and a third of the chip (where the least-squares fit alone
    /// converges, every short run a whole multiple of it), the search still returns the
    /// chip: its multiples are tried and the largest that fits kept.
    #[test]
    fn the_chip_of_pwm_and_nrz_runs_is_found_from_any_seed() {
        for (what, runs) in [("pwm", ev1527_runs(12, BIAS)), ("nrz", nrz_runs(400))] {
            for seed in [None, Some(CHIP / 2.0), Some(CHIP / 3.0)] {
                let u = chip_unit(&runs, seed);
                assert!(near_chip(u), "{what} from {seed:?}: {u:?}");
            }
        }
    }

    /// A weak remote in a narrow band reads its pulses a third of a chip short and its
    /// gaps as much long (the HackRF windows `ook-remote-hackrf-sparse-*` in 6.6 kHz):
    /// the bias is fitted with the unit, and the chip, not two thirds of it, is found.
    #[test]
    fn the_chip_of_pwm_runs_is_found_under_a_third_of_a_chip_of_edge_bias() {
        let runs = ev1527_runs(12, 0.33);
        let u = chip_unit(&runs, None);
        assert!(near_chip(u), "{u:?}");
        assert!(
            u.is_some_and(|(_, b)| (b - 0.33 * CHIP).abs() <= 0.1 * CHIP),
            "{u:?}"
        );
    }

    /// `iq` measured over the whole band at [`RATE`].
    fn rate_of(iq: &[Complex<f32>]) -> Option<u32> {
        ook_unit_rate(&[iq], RATE, (0.0, RATE)).map(|f| f.baud.0)
    }

    fn noisy(mut iq: Vec<Complex<f32>>, seed: u64) -> Vec<Complex<f32>> {
        awgn(&mut iq, 20.0, seed);
        iq
    }

    fn within(r: Option<u32>, baud: u32) -> bool {
        r.is_some_and(|r| (r as f64 - baud as f64).abs() <= baud as f64 * TOLERANCE)
    }

    /// Envelope keying of `runs` (on first, alternating), amplitude 1.
    fn keyed(runs: &[f64]) -> Vec<Complex<f32>> {
        let mut out = Vec::new();
        for (i, &l) in runs.iter().enumerate() {
            let a = if i % 2 == 0 { 1.0 } else { 0.0 };
            out.extend(std::iter::repeat_n(
                Complex::new(a, 0.0),
                l.round() as usize,
            ));
        }
        out
    }

    #[test]
    fn ook_unit_rate_reads_the_chip_of_pwm_and_nrz_keying() {
        // The EV1527 runs at 48 kHz: a chip of 15 samples, 3200 Bd.
        let pwm: Vec<f64> = ev1527_runs(12, BIAS)
            .iter()
            .map(|(l, _)| l * 15.0 / CHIP)
            .collect();
        let r = rate_of(&noisy(keyed(&pwm), 1));
        assert!(within(r, 3_200), "pwm {r:?}");
        let spec = SynthSpec::Ook {
            symbol_rate_baud: 2_400,
            bits: BITS.into(),
            repeat: 4,
            gap_ms: 0,
        };
        let iq = synthesise(&spec, &Impairments::default(), RATE as u32, 0);
        let r = rate_of(&noisy(iq, 2));
        assert!(within(r, 2_400), "nrz {r:?}");
    }

    #[test]
    fn ook_unit_rate_reads_nothing_on_other_signals() {
        let synth = |spec: SynthSpec| synthesise(&spec, &Impairments::default(), RATE as u32, 0);
        let fsk = |repeat, gap_ms| {
            synth(SynthSpec::Fsk {
                symbol_rate_baud: 4_800,
                deviation_hz: 2_400.0,
                order: 2,
                bits: BITS.into(),
                repeat,
                gap_ms,
            })
        };
        let bpsk = synth(SynthSpec::Psk {
            symbol_rate_baud: 4_800,
            order: 2,
            bits: BITS.into(),
            repeat: 4,
            gap_ms: 0,
        });
        let mut r = lcg(0x0153);
        let mut u = move || (r() as f64 / (1u64 << 31) as f64 - 0.5) as f32;
        let noise: Vec<Complex<f32>> = (0..RATE).map(|_| Complex::new(u(), u())).collect();
        let am = am_modulate(
            &speech_like(RATE as u32, 1.0),
            RATE as u32,
            RATE as u32,
            1.0,
            0.0,
        );
        // 50 ms packets of 2-FSK every 60 ms: two envelope levels, and every run a
        // whole multiple of the 10 ms gap.
        for (what, iq) in [
            ("2-fsk", noisy(fsk(4, 0), 3)),
            ("bpsk", noisy(bpsk, 4)),
            ("fsk packet train", noisy(fsk(16, 10), 5)),
            ("noise", noise),
            ("am voice", noisy(am, 7)),
        ] {
            assert_eq!(rate_of(&iq), None, "{what}");
        }
    }
}
