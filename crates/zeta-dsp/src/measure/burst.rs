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
/// `FLOOR_PERCENTILE`th-percentile chunk, or, when none does, off when it lies more than
/// this many dB under it ...
pub(crate) const BURST_ON_DB: f64 = 6.0;
/// ... on-runs closer than this many ms are merged ...
pub(crate) const BURST_MERGE_MS: u64 = 2;
/// ... runs shorter than this many ms are dropped ...
const BURST_MIN_MS: u64 = 1;
/// A window on for at least this fraction is measured over its runs only when they are
/// at least `BURST_LONG_RUN_MS` long ([`gated_view`]): shorter runs over half the window
/// are a train of keyed frames, measured whole as keying. `burst_capture` takes a window
/// with no on-edge that is on for at least this fraction as a continuous signal.
pub(crate) const BURST_DUTY_MAX: f32 = 0.5;
/// ... runs (their median) at least this many ms long are a transmission keyed up, keyed
/// down or cut by a drop-out, gated at any duty: the keyed-over generator's shortest run
/// is hundreds of ms, the frames of an OOK remote's train 30 ms and a Mode S train's 20.
pub(super) const BURST_LONG_RUN_MS: f32 = 100.0;
/// A window is measured over its runs only when they repeat no faster than this many ms:
/// shorter, denser on-off runs are keying, not bursts.
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
/// The slow gate ([`level_runs`]) compares the mean power over this many ms about each
/// chunk. The mean of `n` independent noise samples spreads by `1 / sqrt(n)` of itself, and a
/// band `B` wide holds `B` of them a second: at 10 ms a 12.5 kHz channel's noise level
/// spreads by 9 %, and a carrier no stronger than that noise (twice the power while it is
/// up, spreading by 16 % of the noise) stands about 4 of its own spreads over the split
/// between the two levels.
pub(super) const BURST_LEVEL_MS: usize = 10;
/// The slow gate needs the on level at least this many times the off level in power: a
/// carrier as strong as the noise in the stream. Under it the two levels may be one
/// level's spread (the halves of noise spreading by 9 % lie 1.15 apart) or a modulated
/// carrier's (AM at full index has at most 1.5 times its carrier's power), and the window
/// is not split.
pub(super) const BURST_LEVEL_RATIO: f64 = 2.0;
/// FFT size of the cyclostationary line search: a line grows with the
/// transform length and the noise continuum does not.
const CYCLO_SIZE: usize = 4096;

/// The on-runs of a window ([`burst_runs`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Bursts {
    /// Sample ranges `[start, end)` the signal is on, in order.
    pub runs: Vec<(usize, usize)>,
    /// On samples over all samples, whichever level the `FLOOR_PERCENTILE`th-percentile
    /// chunk is on (1 for one run spanning the window).
    pub duty: f32,
    /// Median run length, ms; `None` without runs.
    pub burst_ms: Option<f32>,
    /// Median start-to-start interval of the runs, ms; `None` under two runs.
    pub period_ms: Option<f32>,
    /// The off level (linear, per sample): the `FLOOR_PERCENTILE`th-percentile chunk
    /// power the chunks were compared with, or, when the off part lies under that chunk,
    /// the median power of the off chunks; 0 for an empty window.
    pub off_power: f64,
}

/// The on-runs of `iq`: the mean power of each `BURST_ENVELOPE_MS` chunk is
/// compared with the `FLOOR_PERCENTILE`th-percentile chunk. When the off part is at
/// least that share of the window the chunk is on the off level, and a chunk more than
/// `BURST_ON_DB` over it is on. When none stands over it, the chunk is on the on level
/// (a carrier up for most of the window), and a chunk more than `BURST_ON_DB` under it
/// is off: so the off part is found from either side, and `duty` is the on share in
/// both. Runs closer than `BURST_MERGE_MS` are merged and runs shorter than
/// `BURST_MIN_MS` dropped. When no chunk stands out either way the window is one run
/// spanning it (a continuous signal, or plain noise: one level). An empty window has
/// no runs.
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
    // A zero floor holds every chunk with power on; 0/0 is not on.
    let over = |p: f64| 10.0 * (p / floor).log10() > BURST_ON_DB;
    let under = |p: f64| 10.0 * (floor / p).log10() > BURST_ON_DB;
    // The floor chunk is on the on level when nothing stands over it and something lies
    // under it: the off part is then the chunks under it, and the off level theirs.
    let from_above = !power.iter().any(|&p| over(p)) && power.iter().any(|&p| under(p));
    let off_power = if from_above {
        let mut off: Vec<f64> = power.iter().copied().filter(|&p| under(p)).collect();
        off.sort_by(f64::total_cmp);
        off[off.len() / 2]
    } else {
        floor
    };
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let merge = (rate_hz * BURST_MERGE_MS / 1_000) as usize;
    for (i, &p) in power.iter().enumerate() {
        let is_on = if from_above { !under(p) } else { over(p) };
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
        off_power,
        runs,
    }
}

/// The on-runs of `iq` when its power sits at two levels too close for [`burst_runs`]'s
/// `BURST_ON_DB` (a carrier only a few dB over the noise in the stream, keyed up, keyed
/// down or cut by a drop-out): the chunk powers are averaged over `BURST_LEVEL_MS` about
/// each chunk and split at the geometric mean of the two levels that split leaves (two
/// means, iterated), wherever the levels lie. A chunk over the split is on; runs closer
/// than `BURST_LEVEL_MS` are merged and shorter ones dropped, so an edge is found to about
/// half of it.
///
/// The average hides an off part shorter than itself (the 9 ms between an OOK remote's
/// frames, the off chips inside them), so the runs stand only when they hold none: what
/// `fast` ([`burst_runs`] of the same window) leaves off inside them must be on-level
/// chunks that fell short of its threshold (their mean power over the split), not gaps at
/// the off level.
///
/// `None` when the levels are under `BURST_LEVEL_RATIO` apart (one level), the runs hold
/// an off part, or the window is shorter than two averages.
pub(super) fn level_runs(iq: &[Complex<f32>], rate_hz: u64, fast: &Bursts) -> Option<Bursts> {
    let ms = |samples: usize| (samples as f64 * 1_000.0 / rate_hz.max(1) as f64) as f32;
    let chunk = ((rate_hz * BURST_ENVELOPE_MS / 1_000) as usize).max(1);
    let power: Vec<f64> = iq
        .chunks(chunk)
        .map(|c| c.iter().map(|s| s.norm_sqr() as f64).sum::<f64>() / c.len() as f64)
        .collect();
    let span = BURST_LEVEL_MS / BURST_ENVELOPE_MS as usize;
    if power.len() < 2 * span {
        return None;
    }
    // The mean over `span` chunks centred on each (fewer at the window's ends).
    let mut acc = vec![0.0f64; power.len() + 1];
    for (i, &p) in power.iter().enumerate() {
        acc[i + 1] = acc[i] + p;
    }
    let level: Vec<f64> = (0..power.len())
        .map(|i| {
            let (a, b) = (
                i.saturating_sub(span / 2),
                (i + span - span / 2).min(power.len()),
            );
            (acc[b] - acc[a]) / (b - a) as f64
        })
        .collect();
    let (min, max) = level
        .iter()
        .fold((f64::INFINITY, 0.0f64), |(a, b), &v| (a.min(v), b.max(v)));
    if min <= 0.0 || !max.is_finite() {
        return None;
    }
    let mut split = (min * max).sqrt();
    let (mut off, mut on) = (min, max);
    for _ in 0..32 {
        let mean = |keep: &dyn Fn(f64) -> bool| {
            let (s, n) = level
                .iter()
                .filter(|&&v| keep(v))
                .fold((0.0, 0usize), |(s, n), &v| (s + v, n + 1));
            (n > 0).then(|| s / n as f64)
        };
        (off, on) = (mean(&|v| v <= split)?, mean(&|v| v > split)?);
        let next = (off * on).sqrt();
        if (next - split).abs() <= 1e-6 * split {
            break;
        }
        split = next;
    }
    if on < BURST_LEVEL_RATIO * off {
        return None;
    }
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for (i, _) in level.iter().enumerate().filter(|&(_, &v)| v > split) {
        let (start, end) = (i * chunk, ((i + 1) * chunk).min(iq.len()));
        match runs.last_mut() {
            Some(last) if start - last.1 < span * chunk => last.1 = end,
            _ => runs.push((start, end)),
        }
    }
    runs.retain(|&(s, e)| e - s >= span * chunk);
    if runs.is_empty() {
        return None;
    }
    // What the fast gate leaves off inside these runs: on-level chunks, or an off part?
    let in_any = |runs: &[(usize, usize)], at: usize| runs.iter().any(|&(s, e)| s <= at && at < e);
    let (left, n) = power
        .iter()
        .enumerate()
        .filter(|&(i, _)| in_any(&runs, i * chunk) && !in_any(&fast.runs, i * chunk))
        .fold((0.0, 0usize), |(s, n), (_, &p)| (s + p, n + 1));
    if n > 0 && left / n as f64 <= split {
        return None;
    }
    let on_samples: usize = runs.iter().map(|&(s, e)| e - s).sum();
    let lengths: Vec<f32> = runs.iter().map(|&(s, e)| ms(e - s)).collect();
    let periods: Vec<f32> = runs.windows(2).map(|w| ms(w[1].0 - w[0].0)).collect();
    Some(Bursts {
        duty: on_samples as f32 / iq.len() as f32,
        burst_ms: Some(median(&lengths)),
        period_ms: (!periods.is_empty()).then(|| median(&periods)),
        off_power: off,
        runs,
    })
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
    /// Envelope CV of the measured samples as received ([`View::received`]; the runs'
    /// Hann windows would read as amplitude keying).
    pub cv: f32,
    /// The measured samples as received, which the label reads: the runs joined in
    /// order when gated, else the whole window.
    pub received: Cow<'a, [Complex<f32>]>,
    /// The samples the symbol-rate searches run over: each run as received
    /// when gated, else the whole window.
    pub runs: Vec<&'a [Complex<f32>]>,
    /// What the measurement found of the occupied band in these samples, which the label
    /// reads; `None` until they are measured.
    pub band: Option<super::OccupiedBand>,
}

/// What layer 1 measures of `iq`: its runs ([`burst_runs`]); the on-time
/// ([`on_time`]) when the window holds an off part (a duty under 1) and, at a duty of
/// `BURST_DUTY_MAX` or more, the runs are at least `BURST_LONG_RUN_MS` long (a carrier
/// keyed up, keyed down or with a hole in it, not a train of frames), and the runs
/// repeat no faster than `BURST_MIN_PERIOD_MS` (denser runs are keying), and there are
/// at least `BURST_MIN_BURSTS` runs or one of at least `BURST_MIN_SINGLE_MS`
/// (a lone shorter run is an outlier chunk), and the off chunks are noise
/// (within `BURST_OFF_NEAR_FLOOR_DB` of the floor in the `bandwidth_hz` band
/// about the centre, [`noise_floor_power`]: impulses over a continuous signal
/// are not bursts); else the whole window. The window is also tried for two levels closer
/// than `BURST_ON_DB` ([`level_runs`]: a carrier a few dB over the noise, where single
/// chunks do not tell on from off): when it holds them in runs of at least
/// `BURST_LONG_RUN_MS` with an off part of noise, and those are not the long runs the
/// chunks already gave, they are the window's runs, its `duty` theirs, and the view its
/// on-time over them.
pub(super) fn gated_view(iq: &[Complex<f32>], rate_hz: u64, bandwidth_hz: u64) -> View<'_> {
    let mut bursts = burst_runs(iq, rate_hz);
    let sparse = bursts.period_ms.is_none_or(|p| p >= BURST_MIN_PERIOD_MS);
    let enough = bursts.runs.len() >= BURST_MIN_BURSTS
        || (bursts.runs.len() == 1 && bursts.burst_ms.is_some_and(|ms| ms >= BURST_MIN_SINGLE_MS));
    let long = |b: &Bursts| b.duty < 1.0 && b.burst_ms.is_some_and(|ms| ms >= BURST_LONG_RUN_MS);
    let bursty = bursts.duty < BURST_DUTY_MAX || long(&bursts);
    let mut gated = bursty && sparse && enough && off_is_noise(iq, rate_hz, bandwidth_hz, &bursts);
    // The slow gate's runs, unless the fast gate's long runs already are those runs: the
    // slow ones must leave out more than the half average either end of each could be.
    let on = |b: &Bursts| b.runs.iter().map(|&(s, e)| e - s).sum::<usize>();
    let slack = |b: &Bursts| b.runs.len() * (rate_hz as usize * BURST_LEVEL_MS / 1_000);
    if let Some(slow) = level_runs(iq, rate_hz, &bursts).filter(|b| {
        long(b)
            && !(gated && long(&bursts) && on(b) + slack(b) >= on(&bursts))
            && off_is_noise(iq, rate_hz, bandwidth_hz, b)
    }) {
        (bursts, gated) = (slow, true);
    }
    if gated {
        let runs: Vec<&[Complex<f32>]> = bursts.runs.iter().map(|&(s, e)| &iq[s..e]).collect();
        let raw: Vec<Complex<f32>> = runs.iter().flat_map(|r| r.iter()).copied().collect();
        View {
            cv: envelope_cv(&raw),
            received: raw.into(),
            on: on_time(iq, &bursts).into(),
            gated: true,
            runs,
            bursts,
            band: None,
        }
    } else {
        View {
            cv: envelope_cv(iq),
            received: iq.into(),
            on: iq.into(),
            gated: false,
            runs: vec![iq],
            bursts,
            band: None,
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
