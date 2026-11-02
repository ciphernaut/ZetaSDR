//! FSK order and tone spacing, PSK order and on-off keying's two envelope
//! levels, for layer 1. Like the symbol rate, the estimates run over the raw
//! runs ([`super::burst::View::runs`]), never the Hann-windowed on-time: a
//! window's edges are not keying.

use num_complex::Complex;
use zeta_core::types::Hz;

use super::burst::{BURST_MIN_PERIOD_MS, averaged_half_spectrum, bandpass, burst_runs, hann};
use super::{LOCAL_BINS, MIN_LINE_HZ, SIZE, discriminator, median};
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
/// (discriminator) histogrammed across that band in `FSK_HIST_BINS` bins,
/// and the histogram's [`clusters`] holding at least `FSK_CLUSTER_MIN_WEIGHT`
/// of the samples counted. Two or four clusters give the order and the
/// median distance between adjacent ones; any other count is `None`.
pub(super) fn fsk_clusters(
    runs: &[&[Complex<f32>]],
    rate_hz: u64,
    (offset_hz, bandwidth_hz): (f64, u64),
) -> Option<(u8, Hz)> {
    let bw = bandwidth_hz as f64;
    if bw <= 0.0 || rate_hz == 0 {
        return None;
    }
    let bin = bw / FSK_HIST_BINS as f64;
    let mut hist = vec![0.0f64; FSK_HIST_BINS];
    let mut total = 0usize;
    let freqs: Vec<Vec<f32>> = runs
        .iter()
        .map(|r| discriminator(&bandpass(r, rate_hz, offset_hz, bw)))
        .collect();
    for d in freqs.iter().flatten() {
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
    Some((c.len() as u8, Hz(median(&gaps).round() as u64)))
}

/// Share of the power of the instantaneous frequency `freqs` (one per run;
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
