//! Burst gating and the cyclostationary symbol rate of layer 1: where in a
//! window the signal is on, what is measured of it, and the symbol-rate lines
//! found over its runs.

use std::borrow::Cow;

use num_complex::Complex;
use rustfft::FftPlanner;
use zeta_core::types::Baud;

use super::{FLOOR_PERCENTILE, LINE_DB, LOCAL_BINS, MIN_LINE_HZ, SIZE, envelope_cv, median};
use crate::fft::{CpuFft, FftBackend, PowerSum, Window};

/// Burst gating: the envelope is the mean power over chunks of this many ms ...
pub(crate) const BURST_ENVELOPE_MS: u64 = 1;
/// ... a chunk is on when it stands more than this many dB over the
/// `FLOOR_PERCENTILE`th-percentile chunk ...
pub(crate) const BURST_ON_DB: f64 = 6.0;
/// ... on-runs closer than this many ms are merged ...
pub(crate) const BURST_MERGE_MS: u64 = 2;
/// ... runs shorter than this many ms are dropped ...
const BURST_MIN_MS: u64 = 1;
/// ... and a window on for less than this fraction is measured over its runs only ...
pub(crate) const BURST_DUTY_MAX: f32 = 0.5;
/// ... unless its runs repeat faster than this many ms: shorter, denser on-off
/// runs are keying, not bursts.
pub(super) const BURST_MIN_PERIOD_MS: f32 = 20.0;
/// The gate needs at least this many on-runs ...
const BURST_MIN_BURSTS: usize = 2;
/// ... or one run at least this many ms long: a lone shorter run is an outlier
/// chunk of noise, not a burst.
const BURST_MIN_SINGLE_MS: f32 = 3.0;
/// The gate also needs the off chunks to be noise: the `FLOOR_PERCENTILE`th-percentile
/// chunk power at most this many dB over the window's in-band noise floor (the
/// `FLOOR_PERCENTILE`th-percentile bin of its spectrum, as a per-sample power), so two
/// clicks over a continuous signal, whose off chunks are the signal, never gate it.
pub(crate) const BURST_OFF_NEAR_FLOOR_DB: f64 = 6.0;
/// FFT size of the cyclostationary line search: a line grows with the
/// transform length and the noise continuum does not.
const CYCLO_SIZE: usize = 4096;

/// The on-runs of a window ([`burst_runs`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Bursts {
    /// Sample ranges `[start, end)` the signal is on, in order.
    pub runs: Vec<(usize, usize)>,
    /// On samples over all samples (1 for one run spanning the window).
    pub duty: f32,
    /// Median run length, ms; `None` without runs.
    pub burst_ms: Option<f32>,
    /// Median start-to-start interval of the runs, ms; `None` under two runs.
    pub period_ms: Option<f32>,
    /// The off level: the `FLOOR_PERCENTILE`th-percentile chunk power (linear, per
    /// sample) the chunks were compared with; 0 for an empty window.
    pub off_power: f64,
}

/// The on-runs of `iq`: the mean power of each `BURST_ENVELOPE_MS` chunk is
/// compared with the `FLOOR_PERCENTILE`th-percentile chunk, a chunk more than
/// `BURST_ON_DB` over it is on, runs closer than `BURST_MERGE_MS` are merged
/// and runs shorter than `BURST_MIN_MS` dropped. When no chunk stands out the
/// window is one run spanning it (a continuous signal, whose floor is the
/// signal itself, or plain noise). An empty window has no runs.
pub fn burst_runs(iq: &[Complex<f32>], rate_hz: u64) -> Bursts {
    let ms = |samples: usize| (samples as f64 * 1_000.0 / rate_hz.max(1) as f64) as f32;
    let chunk = ((rate_hz * BURST_ENVELOPE_MS / 1_000) as usize).max(1);
    let power: Vec<f64> = iq
        .chunks(chunk)
        .map(|c| c.iter().map(|s| s.norm_sqr() as f64).sum::<f64>() / c.len() as f64)
        .collect();
    let floor = {
        let mut p = power.clone();
        p.sort_by(f64::total_cmp);
        p.get(p.len() * FLOOR_PERCENTILE / 100)
            .copied()
            .unwrap_or(0.0)
    };
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let merge = (rate_hz * BURST_MERGE_MS / 1_000) as usize;
    for (i, &p) in power.iter().enumerate() {
        // A zero floor holds every chunk with power on; 0/0 is not on.
        let is_on = 10.0 * (p / floor).log10() > BURST_ON_DB;
        if !is_on {
            continue;
        }
        let (start, end) = (i * chunk, ((i + 1) * chunk).min(iq.len()));
        match runs.last_mut() {
            Some(last) if start - last.1 < merge => last.1 = end,
            _ => runs.push((start, end)),
        }
    }
    let min = ((rate_hz * BURST_MIN_MS / 1_000) as usize).max(1);
    runs.retain(|&(s, e)| e - s >= min);
    if runs.is_empty() && !iq.is_empty() {
        runs.push((0, iq.len()));
    }
    let on: usize = runs.iter().map(|&(s, e)| e - s).sum();
    let lengths: Vec<f32> = runs.iter().map(|&(s, e)| ms(e - s)).collect();
    let periods: Vec<f32> = runs.windows(2).map(|w| ms(w[1].0 - w[0].0)).collect();
    Bursts {
        duty: if iq.is_empty() {
            0.0
        } else {
            on as f32 / iq.len() as f32
        },
        burst_ms: (!lengths.is_empty()).then(|| median(&lengths)),
        period_ms: (!periods.is_empty()).then(|| median(&periods)),
        off_power: floor,
        runs,
    }
}

/// The noise floor of `iq` as a per-sample power, from the band `bandwidth_hz` wide about
/// the stream's centre (offset 0: a channelised stream, as `measure_band` reads it): the
/// `FLOOR_PERCENTILE`th-percentile in-band bin of a `SIZE`-point spectrum stands for every
/// in-band bin, the out-of-band bins count as they are, and the sum over the whole
/// spectrum scales the window's mean power to it. A channel's stopband (most of the bins
/// when the band is narrow against the rate) thus never reads as the floor, and on an
/// unfiltered stream the out-of-band noise counts like the chunks that hold it. `None` for a
/// window shorter than one transform.
pub(crate) fn noise_floor_power(
    iq: &[Complex<f32>],
    rate_hz: u64,
    bandwidth_hz: u64,
) -> Option<f64> {
    let ps = CpuFft::new().power_sum(iq, SIZE, Window::Hann);
    if ps.chunks == 0 || iq.is_empty() || rate_hz == 0 {
        return None;
    }
    let total: f64 = ps.sum.iter().sum();
    if total <= 0.0 {
        return None;
    }
    let bin_hz = rate_hz as f64 / SIZE as f64;
    let half = ((bandwidth_hz as f64 / 2.0 / bin_hz).floor() as usize).min(SIZE / 2);
    let (lo, hi) = (SIZE / 2 - half, (SIZE / 2 + half + 1).min(SIZE));
    let mut inband = ps.sum[lo..hi].to_vec();
    inband.sort_by(f64::total_cmp);
    let floor_bin = inband[inband.len() * FLOOR_PERCENTILE / 100];
    let outside: f64 = ps.sum[..lo].iter().chain(&ps.sum[hi..]).sum();
    let mean = iq.iter().map(|s| s.norm_sqr() as f64).sum::<f64>() / iq.len() as f64;
    Some(mean * (floor_bin * inband.len() as f64 + outside) / total)
}

/// Whether the off chunks of `b` sit near the noise floor of `iq` in its band
/// (`BURST_OFF_NEAR_FLOOR_DB`); true when the floor cannot be measured.
fn off_is_noise(iq: &[Complex<f32>], rate_hz: u64, bandwidth_hz: u64, b: &Bursts) -> bool {
    match noise_floor_power(iq, rate_hz, bandwidth_hz) {
        Some(floor) if floor > 0.0 && b.off_power > 0.0 => {
            10.0 * (b.off_power / floor).log10() <= BURST_OFF_NEAR_FLOOR_DB
        }
        _ => true,
    }
}

/// `n`-point Hann window value at `i`.
pub(super) fn hann(i: usize, n: usize) -> f32 {
    if n > 1 {
        0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (n - 1) as f32).cos()
    } else {
        1.0
    }
}

/// The on-time of `iq`: each run of `b` Hann-windowed (so the joins add no
/// splatter) and the runs concatenated, zero-padded to `SIZE` samples when
/// shorter.
pub fn on_time(iq: &[Complex<f32>], b: &Bursts) -> Vec<Complex<f32>> {
    let mut out: Vec<Complex<f32>> = Vec::with_capacity(SIZE);
    for &(s, e) in &b.runs {
        let n = e - s;
        out.extend(iq[s..e].iter().enumerate().map(|(i, &x)| x * hann(i, n)));
    }
    if out.len() < SIZE {
        out.resize(SIZE, Complex::new(0.0, 0.0));
    }
    out
}

/// What layer 1 measures of a window ([`gated_view`]).
pub(super) struct View<'a> {
    /// The window's runs.
    pub bursts: Bursts,
    /// The samples the spectrum, SNR and occupied band are taken over: the
    /// on-time when gated, else the whole window.
    pub on: Cow<'a, [Complex<f32>]>,
    /// Whether the window was measured over its on-time.
    pub gated: bool,
    /// Envelope CV of the measured samples as received (the runs' Hann
    /// windows would read as amplitude keying).
    pub cv: f32,
    /// The samples the symbol-rate searches run over: each run as received
    /// when gated, else the whole window.
    pub runs: Vec<&'a [Complex<f32>]>,
}

/// What layer 1 measures of `iq`: its runs ([`burst_runs`]); the on-time
/// ([`on_time`]) when the duty is under `BURST_DUTY_MAX` and the runs repeat no
/// faster than `BURST_MIN_PERIOD_MS` (denser runs are keying), and there are
/// at least `BURST_MIN_BURSTS` runs or one of at least `BURST_MIN_SINGLE_MS`
/// (a lone shorter run is an outlier chunk), and the off chunks are noise
/// (within `BURST_OFF_NEAR_FLOOR_DB` of the floor in the `bandwidth_hz` band
/// about the centre, [`noise_floor_power`]: impulses over a continuous signal
/// are not bursts); else the whole window.
pub(super) fn gated_view(iq: &[Complex<f32>], rate_hz: u64, bandwidth_hz: u64) -> View<'_> {
    let bursts = burst_runs(iq, rate_hz);
    let sparse = bursts.period_ms.is_none_or(|p| p >= BURST_MIN_PERIOD_MS);
    let enough = bursts.runs.len() >= BURST_MIN_BURSTS
        || (bursts.runs.len() == 1 && bursts.burst_ms.is_some_and(|ms| ms >= BURST_MIN_SINGLE_MS));
    if bursts.duty < BURST_DUTY_MAX
        && sparse
        && enough
        && off_is_noise(iq, rate_hz, bandwidth_hz, &bursts)
    {
        let runs: Vec<&[Complex<f32>]> = bursts.runs.iter().map(|&(s, e)| &iq[s..e]).collect();
        let raw: Vec<Complex<f32>> = runs.iter().flat_map(|r| r.iter()).copied().collect();
        View {
            cv: envelope_cv(&raw),
            on: on_time(iq, &bursts).into(),
            gated: true,
            runs,
            bursts,
        }
    } else {
        View {
            cv: envelope_cv(iq),
            on: iq.into(),
            gated: false,
            runs: vec![iq],
            bursts,
        }
    }
}

/// Positive-frequency half of the `size`-bin power spectrum, in dB, of the
/// real `runs` (each demeaned), averaged over them: a run of at least `size`
/// samples adds its Hann-windowed `size`-sample chunks, a shorter one is
/// Hann-windowed over its own length and zero-padded to `size`. `None` when
/// no run holds a sample.
pub(super) fn averaged_half_spectrum(runs: &[Vec<f32>], size: usize) -> Option<Vec<f32>> {
    let fft = CpuFft::new();
    let mut acc = PowerSum::new(size);
    for x in runs.iter().filter(|x| !x.is_empty()) {
        let mean = x.iter().map(|&v| v as f64).sum::<f64>() / x.len() as f64;
        let mut c: Vec<Complex<f32>> = x
            .iter()
            .map(|&v| Complex::new((v as f64 - mean) as f32, 0.0))
            .collect();
        if c.len() >= size {
            acc.add(&fft.power_sum(&c, size, Window::Hann));
        } else {
            let n = c.len();
            c.iter_mut().enumerate().for_each(|(i, v)| *v *= hann(i, n));
            c.resize(size, Complex::new(0.0, 0.0));
            acc.add(&fft.power_sum(&c, size, Window::Rect));
        }
    }
    (acc.chunks > 0).then(|| acc.db()[size / 2..].to_vec())
}

/// Cyclostationary symbol rate of `runs` (each band-passed to the occupied
/// band, [`bandpass`]: noise outside it buries the lines): the stronger of two
/// candidate lines between `MIN_LINE_HZ` and `max_hz` ([`strongest_line`] of
/// each `CYCLO_SIZE`-point spectrum averaged over the runs), the one standing
/// further over its local median: (a) the spectrum of `|x|²`, where shaped
/// linear modulation (PSK, ASK) has its symbol-rate line, and (b) the
/// spectrum of the rectified envelope derivative `| |x[n]| - |x[n-1]| |`,
/// where rectangular on-off keying, whose `|x|²` has a null at the baud, has
/// an edge line. A constant-envelope signal has neither line, so the caller
/// asks only when the envelope moves.
pub(super) fn cyclo_symbol_rate(
    runs: &[Vec<Complex<f32>>],
    rate_hz: u64,
    max_hz: f64,
) -> Option<Baud> {
    let bin_hz = rate_hz as f64 / CYCLO_SIZE as f64;
    let lo = (MIN_LINE_HZ / bin_hz).ceil() as usize;
    let hi = (max_hz / bin_hz).floor() as usize + 1;
    let power: Vec<Vec<f32>> = runs
        .iter()
        .map(|r| r.iter().map(|s| s.norm_sqr()).collect())
        .collect();
    let edges: Vec<Vec<f32>> = runs
        .iter()
        .map(|r| {
            r.windows(2)
                .map(|w| (w[1].norm() - w[0].norm()).abs())
                .collect()
        })
        .collect();
    [power, edges]
        .iter()
        .filter_map(|x| strongest_line(&averaged_half_spectrum(x, CYCLO_SIZE)?, bin_hz, lo, hi))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(hz, _)| Baud(hz.round() as u32))
}

/// `iq` with everything outside `offset_hz ± bandwidth_hz / 2` removed by FFT.
pub(super) fn bandpass(
    iq: &[Complex<f32>],
    rate_hz: u64,
    offset_hz: f64,
    bandwidth_hz: f64,
) -> Vec<Complex<f32>> {
    let n = iq.len();
    if n == 0 {
        return Vec::new();
    }
    let mut planner = FftPlanner::<f32>::new();
    let mut buf = iq.to_vec();
    planner.plan_fft_forward(n).process(&mut buf);
    let bin_hz = rate_hz as f64 / n as f64;
    for (k, b) in buf.iter_mut().enumerate() {
        let f = if 2 * k <= n {
            k as f64
        } else {
            k as f64 - n as f64
        } * bin_hz;
        if (f - offset_hz).abs() > bandwidth_hz / 2.0 {
            *b = Complex::new(0.0, 0.0);
        }
    }
    planner.plan_fft_inverse(n).process(&mut buf);
    let scale = 1.0 / n as f32;
    buf.iter().map(|c| c * scale).collect()
}

/// The strongest local maximum of the half spectrum `db` in bins `lo..hi`
/// that stands `LINE_DB` above the median of `db[lo..]` and also `LINE_DB`
/// above the median of its own neighbourhood (`LOCAL_BINS` either side, its
/// main lobe excluded), so a coloured noise hump is not mistaken for a line:
/// its frequency (bin `k` is `k · bin_hz`) refined by a parabola, and its
/// margin over that local median in dB.
pub(super) fn strongest_line(db: &[f32], bin_hz: f64, lo: usize, hi: usize) -> Option<(f64, f32)> {
    let lo = lo.max(1);
    let hi = hi.min(db.len());
    if lo + 1 >= hi {
        return None;
    }
    let global = median(&db[lo..]);
    let local = |k: usize| {
        let near: Vec<f32> = (k.saturating_sub(LOCAL_BINS)..(k + LOCAL_BINS + 1).min(db.len()))
            .filter(|&j| j.abs_diff(k) > 2)
            .map(|j| db[j])
            .collect();
        median(&near)
    };
    let k = (lo..hi)
        .filter(|&k| {
            let is_max = db[k] > db[k - 1] && db.get(k + 1).is_none_or(|&n| db[k] >= n);
            is_max && db[k] - global >= LINE_DB && db[k] - local(k) >= LINE_DB
        })
        .max_by(|&a, &b| db[a].total_cmp(&db[b]))?;
    Some(((k as f64 + parabola(db, k)) * bin_hz, db[k] - local(k)))
}

/// Parabolic offset, in bins (±0.5), of the peak at `k` of `db`.
fn parabola(db: &[f32], k: usize) -> f64 {
    if k == 0 || k + 1 >= db.len() {
        return 0.0;
    }
    let (a, b, c) = (db[k - 1] as f64, db[k] as f64, db[k + 1] as f64);
    let den = a - 2.0 * b + c;
    if den < 0.0 {
        (0.5 * (a - c) / den).clamp(-0.5, 0.5)
    } else {
        0.0
    }
}
