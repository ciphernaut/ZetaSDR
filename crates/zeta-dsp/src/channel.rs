//! Channel stage: shift a channel to baseband, low-pass it and decimate.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::cut::BandCutter;
use zeta_core::ops::channel_rate_hz;
use zeta_core::port::{IqBlock, IqReceiver, Tuning};
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{ChannelId, Hz, Provenance, StreamId, StreamKind, Timestamp};

use crate::util::lowpass_taps;
use crate::video_fm::DropNote;
use crate::worker::run_map_threaded;
use crate::{Input, frame_span, note_lag, run_map};

/// Fewest and most taps of the channel low-pass filter.
const MIN_TAPS: usize = 63;
const MAX_TAPS: usize = 1025;

/// Channel low-pass length for decimation by `decim`: `8 · decim + 1` taps,
/// at least [`MIN_TAPS`] and at most [`MAX_TAPS`]. The transition band of a
/// Hann-windowed sinc is about `4 · rate / taps` wide, so scaling the length
/// with the decimation keeps it near half the output rate and the adjacent
/// band that would alias onto the channel well attenuated; the cap bounds the
/// per-sample cost (decimation by 128 and above all use 1025 taps).
fn taps_for(decim: usize) -> usize {
    decim
        .saturating_mul(8)
        .saturating_add(1)
        .clamp(MIN_TAPS, MAX_TAPS)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChannelError {
    #[error("center_hz {center_hz} with its bandwidth leaves the block {min_hz}..={max_hz}")]
    OutOfBand {
        center_hz: u64,
        min_hz: u64,
        max_hz: u64,
    },
    #[error("out_rate_hz {out_rate_hz} must divide rate_hz {rate_hz} by an integer >= 1")]
    Rate { rate_hz: u64, out_rate_hz: u64 },
    #[error("bandwidth_hz {bandwidth_hz} must be in 1..=out_rate_hz {out_rate_hz}")]
    Bandwidth { bandwidth_hz: u64, out_rate_hz: u64 },
}

/// Streaming state of one channel extraction: NCO phase, FIR history and
/// decimation phase, carried across blocks so block boundaries are seamless.
/// It resets itself when the rate, offset, bandwidth or output rate change.
#[derive(Debug, Default)]
pub struct ChannelState {
    key: Option<(u64, i64, u64, u64)>,
    /// The low-pass taps, last first (the order [`dot`] meets them in a window), [`padded`].
    reversed: Vec<f32>,
    decim: usize,
    /// NCO phase in cycles, in `[0, 1)`.
    phase: f64,
    /// The shifted samples the filter has yet to leave behind (one fewer than its
    /// [`padded`] taps), oldest first.
    history: Vec<Complex<f32>>,
    /// The work buffer of a block (the history, then the block shifted), kept for the next.
    ext: Vec<Complex<f32>>,
    /// Index in the next block of the next sample to output.
    next: usize,
}

impl ChannelState {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Bring a signal at `+offset_hz` to baseband (multiply by `exp(-j2π·offset·t)`),
/// low-pass at `bandwidth_hz / 2` (filter length `8 · decim + 1` taps, 63..=1025) and
/// decimate to `out_rate_hz`, continuing from `state`. `bandwidth_hz` must be
/// in `1..=out_rate_hz`. Returns the samples and the achieved output rate.
pub fn extract_offset_streaming(
    state: &mut ChannelState,
    iq: &[Complex<f32>],
    rate_hz: u64,
    offset_hz: i64,
    bandwidth_hz: u64,
    out_rate_hz: u64,
) -> Result<(Vec<Complex<f32>>, u64), ChannelError> {
    if out_rate_hz == 0 || out_rate_hz > rate_hz || !rate_hz.is_multiple_of(out_rate_hz) {
        return Err(ChannelError::Rate {
            rate_hz,
            out_rate_hz,
        });
    }
    // Wider than the output rate would fold the band's edges onto itself.
    if bandwidth_hz == 0 || bandwidth_hz > out_rate_hz {
        return Err(ChannelError::Bandwidth {
            bandwidth_hz,
            out_rate_hz,
        });
    }
    let key = (rate_hz, offset_hz, bandwidth_hz, out_rate_hz);
    if state.key != Some(key) {
        let decim = (rate_hz / out_rate_hz) as usize;
        let taps = lowpass_taps(bandwidth_hz as f64 / 2.0, rate_hz as f64, taps_for(decim));
        *state = ChannelState {
            key: Some(key),
            history: vec![Complex::new(0.0, 0.0); padded(taps.clone()).len() - 1],
            reversed: padded(taps),
            ext: Vec::new(),
            decim,
            phase: 0.0,
            next: 0,
        };
    }

    // Both halves are written for a wide channel at a radio's full rate (a multiplex cut
    // from 30.72 Msps), where a `sin_cos` a sample and a sum the compiler cannot
    // vectorise ran at under half of real time: the shift is `shift_into`'s phasor
    // recurrence, and each output is a dot product over a forward window.
    let step = -(offset_hz as f64) / rate_hz as f64;
    let h = state.history.len();
    // The buffer is kept between blocks: a fresh one a block was zeroed first, which at a
    // wide rate cost as much as the shift.
    let mut ext = std::mem::take(&mut state.ext);
    ext.resize(h + iq.len(), Complex::new(0.0, 0.0));
    ext[..h].copy_from_slice(&state.history);
    shift_into(iq, &mut state.phase, step, &mut ext[h..]);

    // Output `n` is the sum of `ext[h + n - j] * taps[j]`: the window `ext[n..=n + h]`
    // against the taps reversed.
    let out = filter(&ext, &state.reversed, state.next, state.decim);
    // Outputs cover every index from `next` up to the block end, so this cannot underflow.
    state.next = state.next + out.len() * state.decim - iq.len();
    state.history.copy_from_slice(&ext[ext.len() - h..]);
    state.ext = ext;
    Ok((out, out_rate_hz))
}

/// Floats [`dot`] sums side by side: independent partial sums, alternately of real and
/// imaginary parts, so the loop is one multiply and one add down a vector.
const DOT_LANES: usize = 16;
/// Floats the wide build's [`filter_avx2`] takes a step: four vectors of eight.
const WIDE_STEP: usize = 32;

/// `taps` last first, with zero taps before them (against samples older than the filter
/// reaches) up to a whole number of [`WIDE_STEP`] floats, so the wide build runs in whole
/// steps with nothing left over. The output is the unpadded filter's.
fn padded(taps: Vec<f32>) -> Vec<f32> {
    let whole = WIDE_STEP / 2;
    let mut reversed = vec![0.0; taps.len().next_multiple_of(whole) - taps.len()];
    reversed.extend(taps.into_iter().rev());
    reversed
}

/// `x` as its floats, each sample's real part then its imaginary part.
fn floats(x: &[Complex<f32>]) -> &[f32] {
    // SAFETY: `Complex<f32>` is `repr(C)` of two `f32` (num-complex guarantees the layout
    // of an array of two), so `x` is `2 * x.len()` initialised floats with `f32`'s alignment.
    unsafe { std::slice::from_raw_parts(x.as_ptr().cast::<f32>(), x.len() * 2) }
}

/// The low-pass taps as [`dot`] and [`filter_avx2`] meet them: each written twice, once
/// for a sample's real part and once for its imaginary part.
fn doubled(reversed: &[f32]) -> Vec<f32> {
    reversed.iter().flat_map(|&t| [t, t]).collect()
}

/// The sum of `window[i] * taps[i]`: `window` as its [`floats`] and `doubled` the taps
/// [`doubled`], the two as long as each other. The additions are in [`DOT_LANES`] partial
/// sums, so their order differs from a plain fold's by rounding only.
fn dot(window: &[f32], doubled: &[f32]) -> Complex<f32> {
    let mut acc = [0.0_f32; DOT_LANES];
    let mut ws = window.chunks_exact(DOT_LANES);
    let mut ts = doubled.chunks_exact(DOT_LANES);
    for (w, t) in (&mut ws).zip(&mut ts) {
        for k in 0..DOT_LANES {
            acc[k] += w[k] * t[k];
        }
    }
    for (k, (w, t)) in ws.remainder().iter().zip(ts.remainder()).enumerate() {
        acc[k] += w * t;
    }
    let (mut re, mut im) = (0.0_f32, 0.0_f32);
    for pair in acc.chunks_exact(2) {
        re += pair[0];
        im += pair[1];
    }
    Complex::new(re, im)
}

/// The low-pass over `ext` (the carried history, then a block): an output for every
/// `decim`-th input from `next`, each the [`dot`] of the window ending at that input with
/// the `reversed` taps. The build every CPU runs.
fn filter_base(
    ext: &[Complex<f32>],
    reversed: &[f32],
    next: usize,
    decim: usize,
) -> Vec<Complex<f32>> {
    let h = reversed.len() - 1;
    let doubled = doubled(reversed);
    let samples = floats(ext);
    (next..ext.len() - h)
        .step_by(decim)
        .map(|n| dot(&samples[2 * n..2 * (n + h + 1)], &doubled))
        .collect()
}

/// [`filter_base`] in AVX2 and FMA instructions, for taps of [`padded`] length: four
/// running sums of eight floats, each step a fused multiply-add into every one, so a
/// 64-tap window is four steps. Rounding differs from the baseline's (fused, and summed
/// in another order), by no more than that.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
fn filter_avx2(
    ext: &[Complex<f32>],
    reversed: &[f32],
    next: usize,
    decim: usize,
) -> Vec<Complex<f32>> {
    use std::arch::x86_64::{
        _mm256_add_ps, _mm256_fmadd_ps, _mm256_loadu_ps, _mm256_setzero_ps, _mm256_storeu_ps,
    };
    let h = reversed.len() - 1;
    let doubled = doubled(reversed);
    let samples = floats(ext);
    let len = doubled.len();
    assert!(len.is_multiple_of(WIDE_STEP) && ext.len() > h);
    let end = ext.len() - h;
    let mut out = Vec::with_capacity(end.saturating_sub(next).div_ceil(decim));
    let taps = doubled.as_ptr();
    let mut n = next;
    while n < end {
        // The window of output `n`: `len` floats from `2 * n`, inside `samples` since
        // `n < end` gives `2 * n + len <= 2 * ext.len()`.
        let window = samples[2 * n..2 * n + len].as_ptr();
        let mut sums = [_mm256_setzero_ps(); 4];
        let mut i = 0;
        while i < len {
            for (k, sum) in sums.iter_mut().enumerate() {
                // SAFETY: `i + 8 * k + 8 <= len` (`len` is a multiple of 32 and `i` steps
                // by 32 below it), and `window` and `taps` each point at `len` floats.
                *sum = unsafe {
                    _mm256_fmadd_ps(
                        _mm256_loadu_ps(window.add(i + 8 * k)),
                        _mm256_loadu_ps(taps.add(i + 8 * k)),
                        *sum,
                    )
                };
            }
            i += WIDE_STEP;
        }
        let sum = _mm256_add_ps(
            _mm256_add_ps(sums[0], sums[1]),
            _mm256_add_ps(sums[2], sums[3]),
        );
        let mut lanes = [0.0_f32; 8];
        // SAFETY: `lanes` is eight floats.
        unsafe { _mm256_storeu_ps(lanes.as_mut_ptr(), sum) };
        out.push(Complex::new(
            lanes[0] + lanes[2] + lanes[4] + lanes[6],
            lanes[1] + lanes[3] + lanes[5] + lanes[7],
        ));
        n += decim;
    }
    out
}

/// Whether this CPU runs the AVX2 and FMA builds of the kernels ([`filter`], [`shift_into`]).
/// The binary is built for the baseline CPU, so the wider build is chosen as it runs.
pub fn wide_kernels() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// The low-pass, in the widest build this CPU runs (the wide one needs taps of [`padded`]
/// length, which [`ChannelState`]'s are).
fn filter(ext: &[Complex<f32>], reversed: &[f32], next: usize, decim: usize) -> Vec<Complex<f32>> {
    #[cfg(target_arch = "x86_64")]
    if wide_kernels()
        && (2 * reversed.len()).is_multiple_of(WIDE_STEP)
        && ext.len() >= reversed.len()
    {
        // SAFETY: AVX2 and FMA were detected on this CPU just above.
        return unsafe { filter_avx2(ext, reversed, next, decim) };
    }
    filter_base(ext, reversed, next, decim)
}

/// Whether a band of `bandwidth_hz` cut at `out_rate_hz` from `rate_hz` skips the
/// low-pass: a wide input (at least [`THREADED_RATE_HZ`]), no decimation and a band of at
/// least half the rate, where the filter would cost a 63-tap complex FIR per sample at the
/// full rate (a video job's 20-24 MHz band at 30.72 Msps; a radio's rate rungs can be 1.5x
/// apart, so a band may fill as little as two thirds of its rung). Nothing outside the
/// band can land inside it: the offset is at most `(rate - band) / 2` and a shift without
/// decimation is circular, so out-of-band content stays out of `±band/2` and the stage
/// after (a demodulator filtering its own band) sees it only as the noise it was. Narrower
/// inputs always filter, as they always have.
pub fn skips_filter(rate_hz: u64, bandwidth_hz: u64, out_rate_hz: u64) -> bool {
    rate_hz >= THREADED_RATE_HZ
        && out_rate_hz == rate_hz
        && u128::from(bandwidth_hz) * 2 >= u128::from(rate_hz)
}

/// Samples between exact re-seeds of [`shift_into`]'s phasors.
const SHIFT_SEED: usize = 1024;
/// Phasors [`shift_into`] steps side by side (independent, so the loop vectorises).
const SHIFT_LANES: usize = 8;

/// Multiply `iq` by `exp(j2π·(phase + n·step))` (`phase` and `step` in cycles) into `out`
/// (as long as `iq`) and advance `phase` past the block. A phasor recurrence:
/// [`SHIFT_LANES`] phasors step by `exp(j2π·LANES·step)` side by side, and every
/// [`SHIFT_SEED`] samples they are re-seeded from an exact `sin_cos`, so the error stays near
/// f32 rounding however long the run. Run in the widest build this CPU has ([`wide_kernels`]).
fn shift_into(iq: &[Complex<f32>], phase: &mut f64, step: f64, out: &mut [Complex<f32>]) {
    #[cfg(target_arch = "x86_64")]
    if wide_kernels() {
        // SAFETY: AVX2 and FMA were detected on this CPU just above.
        return unsafe { shift_avx2(iq, phase, step, out) };
    }
    shift_body(iq, phase, step, out)
}

/// [`shift_body`] built for AVX2.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn shift_avx2(iq: &[Complex<f32>], phase: &mut f64, step: f64, out: &mut [Complex<f32>]) {
    shift_body(iq, phase, step, out)
}

/// The body both builds of [`shift_into`] share.
#[inline(always)]
fn shift_body(iq: &[Complex<f32>], phase: &mut f64, step: f64, out: &mut [Complex<f32>]) {
    let at = |cycles: f64| {
        let (s, c) = (std::f64::consts::TAU * cycles.rem_euclid(1.0)).sin_cos();
        Complex::new(c as f32, s as f32)
    };
    let w = at(SHIFT_LANES as f64 * step);
    for (chunk, dst) in iq.chunks(SHIFT_SEED).zip(out.chunks_mut(SHIFT_SEED)) {
        let mut p: [Complex<f32>; SHIFT_LANES] =
            std::array::from_fn(|k| at(*phase + k as f64 * step));
        let mut src = chunk.chunks_exact(SHIFT_LANES);
        let mut dsts = dst.chunks_exact_mut(SHIFT_LANES);
        for (g, d) in (&mut src).zip(&mut dsts) {
            for k in 0..SHIFT_LANES {
                d[k] = g[k] * p[k];
                p[k] *= w;
            }
        }
        for ((x, d), p) in src.remainder().iter().zip(dsts.into_remainder()).zip(&p) {
            *d = x * p;
        }
        *phase = (*phase + chunk.len() as f64 * step).rem_euclid(1.0);
    }
}

/// A block of IQ samples, shared.
pub type Samples = Arc<[Complex<f32>]>;

/// Streaming state of the direct path: the low-pass's ([`ChannelState`]), or the shift's
/// phase when [`skips_filter`].
#[derive(Debug, Default)]
pub struct DirectChannel {
    fir: ChannelState,
    /// `(rate, offset)` the shift's phase belongs to, and the phase in cycles.
    shift: Option<((u64, i64), f64)>,
}

impl DirectChannel {
    pub fn new() -> Self {
        Self::default()
    }
}

/// [`extract_offset_streaming`] on one block, except where [`skips_filter`]: then the
/// block itself at offset 0 (the same samples, not a copy), else the block shifted to
/// baseband with no filter. Returns the samples and the output rate.
pub fn extract_direct(
    state: &mut DirectChannel,
    iq: &Samples,
    rate_hz: u64,
    offset_hz: i64,
    bandwidth_hz: u64,
    out_rate_hz: u64,
) -> Result<(Samples, u64), ChannelError> {
    if !skips_filter(rate_hz, bandwidth_hz, out_rate_hz) || bandwidth_hz > out_rate_hz {
        state.shift = None;
        let (v, r) = extract_offset_streaming(
            &mut state.fir,
            iq,
            rate_hz,
            offset_hz,
            bandwidth_hz,
            out_rate_hz,
        )?;
        return Ok((v.into(), r));
    }
    state.fir = ChannelState::new();
    if offset_hz == 0 {
        state.shift = None;
        return Ok((iq.clone(), out_rate_hz));
    }
    let key = (rate_hz, offset_hz);
    let mut phase = match state.shift {
        Some((k, p)) if k == key => p,
        _ => 0.0,
    };
    let mut out = vec![Complex::new(0.0, 0.0); iq.len()];
    shift_into(
        iq,
        &mut phase,
        -(offset_hz as f64) / rate_hz as f64,
        &mut out,
    );
    state.shift = Some((key, phase));
    Ok((out.into(), out_rate_hz))
}

/// One-shot [`extract_offset_streaming`] from fresh state.
pub fn extract_offset(
    iq: &[Complex<f32>],
    rate_hz: u64,
    offset_hz: i64,
    bandwidth_hz: u64,
    out_rate_hz: u64,
) -> Result<(Vec<Complex<f32>>, u64), ChannelError> {
    let mut state = ChannelState::new();
    extract_offset_streaming(
        &mut state,
        iq,
        rate_hz,
        offset_hz,
        bandwidth_hz,
        out_rate_hz,
    )
}

/// Extract the channel `center_hz ± bandwidth_hz/2` from a block centred on
/// `block_center_hz` at `rate_hz`, at `out_rate_hz`. The channel must lie inside
/// `block_center_hz ± rate_hz/2`.
pub fn extract_channel(
    iq: &[Complex<f32>],
    rate_hz: u64,
    block_center_hz: u64,
    center_hz: u64,
    bandwidth_hz: u64,
    out_rate_hz: u64,
) -> Result<(Vec<Complex<f32>>, u64), ChannelError> {
    check_in_band(rate_hz, block_center_hz, center_hz, bandwidth_hz)?;
    extract_offset(
        iq,
        rate_hz,
        center_hz as i64 - block_center_hz as i64,
        bandwidth_hz,
        out_rate_hz,
    )
}

/// `center ± bw/2` inside `block_center ± rate/2`, compared doubled in i128 so
/// odd values are exact and nothing underflows.
fn check_in_band(
    rate_hz: u64,
    block_center_hz: u64,
    center_hz: u64,
    bandwidth_hz: u64,
) -> Result<(), ChannelError> {
    let (r, bc, c, bw) = (
        rate_hz as i128,
        block_center_hz as i128,
        center_hz as i128,
        bandwidth_hz as i128,
    );
    if 2 * c - bw < 2 * bc - r || 2 * c + bw > 2 * bc + r {
        return Err(ChannelError::OutOfBand {
            center_hz,
            min_hz: block_center_hz.saturating_sub(rate_hz / 2),
            max_hz: block_center_hz.saturating_add(rate_hz / 2),
        });
    }
    Ok(())
}

/// The most a cut decimates in one stage: past it [`taps_for`] stops lengthening the
/// filter, and its transition outgrows a narrow band.
const CUT_STAGE_MAX: u64 = ((MAX_TAPS - 1) / 8) as u64;
/// The least the last stage of a staged cut decimates, where a factor allows: decimating by
/// so little it gets [`MIN_TAPS`], more than [`taps_for`]'s `8 · decim + 1`, so it is
/// sharper against a neighbour than a single stage is.
const CUT_LAST_MIN: u64 = 4;
/// An earlier stage of a staged cut passes this share of its output rate: the band, a
/// [`CUT_LAST_MIN`]th of that rate or less where the factors allow, stays well inside it.
const CUT_PASS_NUM: u64 = 1;
const CUT_PASS_DEN: u64 = 2;

/// The decimations a cut by `decim` runs, first to last, their product `decim`. One stage
/// up to [`CUT_STAGE_MAX`]; past it the last stage takes the least factor of at least
/// [`CUT_LAST_MIN`] (else of at least 2) that leaves a staged remainder, and the stages
/// before it cut the remainder the same way. A `decim` with no such factor is one stage.
fn cut_stages(decim: u64) -> Vec<u64> {
    if decim <= CUT_STAGE_MAX {
        return vec![decim];
    }
    let last = (CUT_LAST_MIN..=CUT_STAGE_MAX)
        .chain(2..CUT_LAST_MIN)
        .find(|d| decim.is_multiple_of(*d));
    match last {
        Some(last) => {
            let mut stages = cut_stages(decim / last);
            stages.push(last);
            stages
        }
        None => vec![decim],
    }
}

/// The node's band cutter ([`BandCutter`]), installed by the binary with
/// [`zeta_core::node::Node::set_cutter`].
///
/// A band is cut at [`channel_rate_hz`], the rate a job's channel runs it at. Up to a
/// decimation of [`CUT_STAGE_MAX`] that is one [`extract_offset`], a low-pass at half the
/// band, as a job's channel stage filters it. Past it one stage would stop sharpening (its
/// filter is at [`MAX_TAPS`]) and let a neighbour tens of kilohertz off fold in only 30
/// to 50 dB down, so the cut runs in stages ([`cut_stages`]): each earlier one decimates to an
/// exact divisor with the band well inside its passband, and the last cuts at half the band.
/// A narrowband cut from a wide block then keeps whatever would fold into the band some
/// 75 dB down. It is not the live path sample for sample (a job on a wide source reads
/// through the channel bank first), but it holds the same band with its neighbours as far
/// down, and its edges where a job's channel puts them.
///
/// Where that rate is the block's own, the cut does not filter: nothing is saved by it, and
/// a shift without decimation is circular, so nothing outside the band lands inside it (the
/// reasoning of [`skips_filter`]). The band centred is then the block itself; one off
/// centre is the block shifted to it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Cutter;

impl BandCutter for Cutter {
    fn cut(
        &self,
        iq: &[Complex<f32>],
        rate_hz: Hz,
        offset_hz: i64,
        bandwidth_hz: Hz,
    ) -> Option<(Vec<Complex<f32>>, Hz)> {
        let (rate, bw) = (rate_hz.0, bandwidth_hz.0);
        // `offset ± bw/2` inside `± rate/2`, doubled in i128 so odd values are exact.
        let reach = 2 * i128::from(offset_hz).abs() + i128::from(bw);
        if rate == 0 || bw == 0 || reach > i128::from(rate) {
            return None;
        }
        let out_rate = channel_rate_hz(rate, bw);
        if out_rate < rate {
            let stages = cut_stages(rate / out_rate);
            let last = stages.len() - 1;
            let (mut samples, mut at, mut offset) = (None::<Vec<Complex<f32>>>, rate, offset_hz);
            for (i, d) in stages.into_iter().enumerate() {
                let next = at / d;
                let band = if i == last {
                    bw
                } else {
                    next * CUT_PASS_NUM / CUT_PASS_DEN
                };
                let input = samples.as_deref().unwrap_or(iq);
                let (v, _) = extract_offset(input, at, offset, band, next).ok()?;
                (samples, at, offset) = (Some(v), next, 0);
            }
            return Some((samples.unwrap_or_default(), Hz(at)));
        }
        if offset_hz == 0 {
            return Some((iq.to_vec(), rate_hz));
        }
        let mut out = vec![Complex::new(0.0, 0.0); iq.len()];
        let mut phase = 0.0;
        shift_into(iq, &mut phase, -(offset_hz as f64) / rate as f64, &mut out);
        Some((out, rate_hz))
    }
}

#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ChannelParams {
    pub center_hz: u64,
    pub bandwidth_hz: u64,
    pub out_rate_hz: u64,
    /// Cut this wide instead (an adjacent-channel guard's three raster steps, the
    /// demodulator filtering the channel itself) when `center_hz ± wide_hz/2` lies inside
    /// the block and `wide_hz <= out_rate_hz`; otherwise `bandwidth_hz`, noted as
    /// `"wide": "off: <why>"` in the job's `channel` context.
    #[serde(default)]
    pub wide_hz: Option<u64>,
}

/// Extracts one channel from an IQ stream: shift, low-pass, decimate.
pub struct ChannelStage {
    spec: StageSpec,
}

impl ChannelStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: "channel",
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Iq],
                params_schema: schemars::schema_for!(ChannelParams),
            },
        }
    }
}

impl Default for ChannelStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for ChannelStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    /// With a bank in `ctx` whose bins are at least `bandwidth_hz` wide and
    /// whose band holds the channel, the stage subscribes to the nearest bin
    /// and filters that bin's stream, provided `out_rate_hz` divides the bin
    /// rate; the job's IQ input is then only watched for its end and band.
    /// Otherwise (no bank, too wide, out of band, or a rate the bin rate does
    /// not divide, as in graphs written for the source rate) it filters the
    /// job's IQ input directly. Either way it notes the path in the job's
    /// context: `{"channel": {"path": "bank", "bin_hz": N}}`, or
    /// `{"channel": {"path": "direct", "reason": R}}` saying why not the bank. A `wide_hz`
    /// is judged first (on the bank's band, or on each block of the direct path) and the
    /// bank's width rule on the band actually cut; a `wide_hz` not cut adds `"wide": "off:
    /// <why>"` to that object. Where the direct path skips the low-pass ([`skips_filter`]:
    /// an input of 10 MHz or more, no decimation, a band of half the rate or more) it adds
    /// `"filter": "none"`. From an input rate of [`THREADED_RATE_HZ`] the direct path runs on a thread
    /// of its own and adds `"dropped": N` once its queue has dropped inputs.
    async fn run(
        &self,
        params: serde_json::Value,
        inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        ctx: StageContext,
    ) -> Result<(), StageError> {
        let p: ChannelParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("channel params: {e}")))?;
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        let mut chan = Channel {
            channel: ChannelId(format!("{}/{}", p.center_hz, p.bandwidth_hz)),
            p,
            out,
            state: DirectChannel::new(),
            unfiltered: false,
            dropped: None,
            path: serde_json::Value::Null,
            within: None,
            wide_off: None,
            ctx: ctx.clone(),
        };
        // Checked before subscribing, so a declined bank keeps no receiver.
        let bin = match &ctx.bank {
            None => Err("no bank".to_string()),
            Some(bank) => {
                let band = bank.band();
                let (bin_hz, cut) = (
                    bank.bin_hz(),
                    chan.wide_cut(band.bandwidth_hz.0, band.center_hz.0).0,
                );
                let p = &chan.p;
                if cut > bin_hz {
                    Err(format!("band {cut} Hz wider than a bin ({bin_hz} Hz)"))
                } else if p.out_rate_hz == 0 || !(2 * bin_hz).is_multiple_of(p.out_rate_hz) {
                    Err(format!(
                        "bin rate {} Hz not a multiple of out_rate_hz {}",
                        2 * bin_hz,
                        p.out_rate_hz
                    ))
                } else {
                    bank.subscribe(p.center_hz, cut)
                        .map(|(bin, _, _)| (bank, bin, band))
                        .map_err(|e| e.to_string())
                }
            }
        };
        let kind = self.spec().kind;
        match bin {
            Ok((bank, bin, band)) => {
                // The wide band is judged on the capture's band, not on the bin's block.
                chan.within = Some((band.bandwidth_hz.0, band.center_hz.0));
                chan.set_path(serde_json::json!({"path": "bank", "bin_hz": bank.bin_hz()}));
                // The job's provenance, as registered for the output stream.
                let base = bus
                    .list()
                    .into_iter()
                    .find(|i| i.id == chan.out)
                    .map(|i| i.provenance)
                    .unwrap_or_else(|| Provenance {
                        source: ctx.source.clone(),
                        job: None,
                        channel: None,
                        span: frame_span(Timestamp(0), 0, 0),
                    });
                return run_bin(kind, chan, base, bank.band(), bin, inputs, bus, cancel).await;
            }
            Err(reason) => chan.set_path(serde_json::json!({"path": "direct", "reason": reason})),
        }
        let mut inputs = inputs;
        if inputs.len() != 1 {
            return Err(StageError::Failed("expected 1 input".into()));
        }
        // The first frame tells the input rate: a wide capture's direct path runs on a
        // thread of its own, a narrower one on the runtime as [`run_map`] runs it.
        let first = loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                r = inputs[0].recv() => match r {
                    Ok(frame) => break frame,
                    Err(broadcast::error::RecvError::Lagged(n)) => note_lag(kind, n),
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            }
        };
        let threaded =
            matches!(&first.payload, Payload::Iq(b) if b.sample_rate_hz.0 >= THREADED_RATE_HZ);
        for f in chan.direct(Input::Frame(first))? {
            bus.publish(f);
        }
        if !threaded {
            return run_map(kind, inputs, bus, cancel, move |input| chan.direct(input)).await;
        }
        run_direct_threaded(kind, chan, inputs, bus, cancel).await
    }
}

/// Input rates from which the direct path runs on a thread of its own (a video job's
/// capture), so a full-rate block holds no runtime worker.
pub const THREADED_RATE_HZ: u64 = 10_000_000;

/// Inputs the direct path's thread may have waiting before the newest is dropped.
pub const DIRECT_QUEUE_DEPTH: usize = 64;

/// The direct path on a thread of its own ([`run_map_threaded`], drop-on-full queue of
/// [`DIRECT_QUEUE_DEPTH`]): the thread's drops are noted in the job's `channel` context
/// as `"dropped": N` when they grow, at most once a second. An error from a block (the
/// capture left the channel) ends the stage with that error.
async fn run_direct_threaded(
    kind: &'static str,
    mut chan: Channel,
    inputs: Vec<broadcast::Receiver<Frame>>,
    bus: Arc<StreamBus>,
    cancel: CancellationToken,
) -> Result<(), StageError> {
    let dropped = Arc::new(AtomicU64::new(0));
    let seen = dropped.clone();
    let failed = Arc::new(Mutex::new(None::<StageError>));
    let fail = failed.clone();
    let stop = cancel.child_token();
    let stopping = stop.clone();
    let mut drops = DropNote::new(Duration::from_secs(1));
    let r = run_map_threaded(
        kind,
        inputs,
        bus,
        stop,
        DIRECT_QUEUE_DEPTH,
        dropped,
        move |input| {
            if stopping.is_cancelled() {
                return Vec::new();
            }
            let n = seen.load(Ordering::Relaxed);
            if drops.due(n, Instant::now()) {
                chan.dropped = Some(n);
                chan.note();
            }
            chan.direct(input).unwrap_or_else(|e| {
                *fail.lock().unwrap_or_else(|p| p.into_inner()) = Some(e);
                stopping.cancel();
                Vec::new()
            })
        },
    )
    .await;
    let failed = failed.lock().unwrap_or_else(|p| p.into_inner()).take();
    match failed {
        Some(e) => Err(e),
        None => r,
    }
}

/// One channel's params, output and streaming state.
struct Channel {
    p: ChannelParams,
    channel: ChannelId,
    out: StreamId,
    state: DirectChannel,
    /// The block is passed through or only shifted, unfiltered ([`skips_filter`]).
    unfiltered: bool,
    /// Inputs the direct path's thread dropped, when it runs on one.
    dropped: Option<u64>,
    /// The path fields of the job's `channel` context, as last noted.
    path: serde_json::Value,
    /// `(rate, center)` of the band `wide_hz` is judged on; `None`: each block's own.
    within: Option<(u64, u64)>,
    /// Why `wide_hz` is not cut, as last noted.
    wide_off: Option<String>,
    ctx: StageContext,
}

impl Channel {
    /// Note `path` as the job's `channel` context, with the wide band's state.
    fn set_path(&mut self, path: serde_json::Value) {
        self.path = path;
        self.note();
    }

    fn note(&self) {
        let mut v = self.path.clone();
        if let Some(o) = v.as_object_mut() {
            if let Some(why) = &self.wide_off {
                o.insert("wide".into(), format!("off: {why}").into());
            }
            if self.unfiltered {
                o.insert("filter".into(), "none".into());
            }
            if let Some(n) = self.dropped {
                o.insert("dropped".into(), n.into());
            }
        }
        self.ctx.note(serde_json::json!({"channel": v}));
    }

    /// The band to cut from a block at `rate` centred on `block_center` (or from
    /// [`Channel::within`]): `wide_hz` when it fits, else `bandwidth_hz`, with why not.
    fn wide_cut(&self, rate: u64, block_center: u64) -> (u64, Option<String>) {
        let p = &self.p;
        let Some(wide) = p.wide_hz else {
            return (p.bandwidth_hz, None);
        };
        let (rate, block_center) = self.within.unwrap_or((rate, block_center));
        let off = if wide > p.out_rate_hz {
            Some(format!("wider than out_rate_hz {}", p.out_rate_hz))
        } else {
            check_in_band(rate, block_center, p.center_hz, wide)
                .err()
                .map(|e| match e {
                    ChannelError::OutOfBand { min_hz, max_hz, .. } => {
                        format!("leaves the block {min_hz}..={max_hz}")
                    }
                    e => e.to_string(),
                })
        };
        match off {
            None => (wide, None),
            off => (p.bandwidth_hz, off),
        }
    }

    /// [`Channel::wide_cut`] for a block, noting a change in why `wide_hz` is not cut.
    fn cut(&mut self, rate: u64, block_center: u64) -> u64 {
        let (cut, off) = self.wide_cut(rate, block_center);
        if off != self.wide_off {
            self.wide_off = off;
            self.note();
        }
        cut
    }

    /// The direct path on one input: a frame's block filtered, or a lost input forgetting
    /// the streaming state.
    fn direct(&mut self, input: Input) -> Result<Vec<Frame>, StageError> {
        let frame = match input {
            Input::Frame(f) => f,
            Input::Lagged { .. } => {
                self.state = DirectChannel::new();
                return Ok(Vec::new());
            }
        };
        let Payload::Iq(block) = &frame.payload else {
            return Err(StageError::Failed("channel stage expects iq frames".into()));
        };
        Ok(self
            .process(block, &frame.provenance)?
            .into_iter()
            .collect())
    }

    /// Filter one input block (the source's, or a bank bin's) into an output
    /// frame with `prov`'s source and job; `None` when it yields no sample.
    fn process(&mut self, block: &IqBlock, prov: &Provenance) -> Result<Option<Frame>, StageError> {
        let rate = block.sample_rate_hz.0;
        let cut = self.cut(rate, block.center_hz.0);
        let p = &self.p;
        check_in_band(rate, block.center_hz.0, p.center_hz, p.bandwidth_hz)
            .map_err(|e| StageError::Failed(e.to_string()))?;
        // Time of the first output sample: the decimation phase carried in.
        let lead = self.state.fir.next;
        let unfiltered = skips_filter(rate, cut, p.out_rate_hz);
        let (samples, out_rate) = extract_direct(
            &mut self.state,
            &block.samples,
            rate,
            p.center_hz as i64 - block.center_hz.0 as i64,
            cut,
            p.out_rate_hz,
        )
        .map_err(|e| StageError::Failed(e.to_string()))?;
        if unfiltered != self.unfiltered {
            self.unfiltered = unfiltered;
            self.note();
        }
        if samples.is_empty() {
            return Ok(None);
        }
        let mut prov = prov.clone();
        prov.channel = Some(self.channel.clone());
        prov.span = frame_span(block.t0, block.samples.len(), rate);
        let t0 = Timestamp(frame_span(block.t0, lead, rate).end_ns.0);
        Ok(Some(Frame::new(
            self.out.clone(),
            prov,
            Payload::Iq(IqBlock {
                t0,
                sample_rate_hz: Hz(out_rate),
                center_hz: Hz(p.center_hz),
                samples,
            }),
        )))
    }
}

/// The bank path: filter the bin stream `bin` of a bank over `band`, ending
/// cleanly when the job's IQ input closes. A lagging bin resets the filter
/// state, counted under `kind` as [`run_map`] counts. The stage fails when a job IQ block shows the capture has left
/// `band` (the node then makes a new bank this stage is not on) or when the
/// bin closes.
#[allow(clippy::too_many_arguments)]
async fn run_bin(
    kind: &'static str,
    mut chan: Channel,
    base: Provenance,
    band: Tuning,
    mut bin: IqReceiver,
    inputs: Vec<broadcast::Receiver<Frame>>,
    bus: Arc<StreamBus>,
    cancel: CancellationToken,
) -> Result<(), StageError> {
    let mut inputs = inputs;
    if inputs.len() != 1 {
        return Err(StageError::Failed("expected 1 input".into()));
    }
    let root = &mut inputs[0];
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(StageError::Cancelled),
            r = bin.recv() => match r {
                Ok(block) => {
                    if let Some(f) = chan.process(&block, &base)? {
                        bus.publish(f);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    note_lag(kind, n);
                    chan.state = DirectChannel::new();
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(StageError::Failed(
                        "channel bank bin closed; restart the job".into(),
                    ));
                }
            },
            r = root.recv() => match r {
                Ok(frame) => {
                    if let Payload::Iq(b) = &frame.payload
                        && (b.center_hz, b.sample_rate_hz) != (band.center_hz, band.bandwidth_hz)
                    {
                        return Err(StageError::Failed(format!(
                            "capture band changed from center {} Hz, rate {} Hz to center {} Hz, rate {} Hz; restart the job",
                            band.center_hz.0, band.bandwidth_hz.0, b.center_hz.0, b.sample_rate_hz.0
                        )));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use zeta_core::port::IqBlock;
    use zeta_core::stream::Payload;
    use zeta_core::types::{ChannelId, Hz, Provenance, SourceId, Span, Timestamp};

    use super::*;

    const RATE: u64 = 48_000;
    const BLOCK_CENTER: u64 = 100_000_000;

    fn tone(len: usize, freq_hz: f64) -> Vec<Complex<f32>> {
        crate::util::nco(len, freq_hz, RATE as f64)
    }

    /// Magnitude of the single DFT bin at `freq_hz`.
    fn bin(x: &[Complex<f32>], freq_hz: f64, rate_hz: f64) -> f64 {
        let (re, im) = x.iter().enumerate().fold((0.0, 0.0), |(re, im), (n, v)| {
            let (sn, cs) = (-2.0 * PI * freq_hz * n as f64 / rate_hz).sin_cos();
            let (vr, vi) = (v.re as f64, v.im as f64);
            (re + vr * cs - vi * sn, im + vr * sn + vi * cs)
        });
        (re * re + im * im).sqrt()
    }

    #[test]
    fn extract_shifts_tone_to_baseband() {
        let iq = tone(9600, 5_000.0);
        let (out, rate) =
            extract_channel(&iq, RATE, BLOCK_CENTER, 100_005_000, 10_000, RATE).unwrap();
        assert_eq!((out.len(), rate), (9600, RATE));
        // Skip the filter start-up; 4800 samples puts 5 kHz exactly on a bin.
        let w = &out[100..4900];
        let dc = bin(w, 0.0, RATE as f64);
        let plus5 = bin(w, 5_000.0, RATE as f64);
        let db = 20.0 * (dc / plus5).log10();
        assert!(db >= 20.0, "dc {dc} vs +5 kHz {plus5}: {db} dB");
    }

    #[test]
    fn extract_rejects_out_of_band_naming_center() {
        let iq = tone(480, 5_000.0);
        let err = extract_channel(&iq, RATE, BLOCK_CENTER, 100_100_000, 10_000, RATE).unwrap_err();
        assert_eq!(
            err,
            ChannelError::OutOfBand {
                center_hz: 100_100_000,
                min_hz: 99_976_000,
                max_hz: 100_024_000,
            }
        );
        assert!(err.to_string().contains("center_hz 100100000"), "{err}");
    }

    #[test]
    fn extract_rejects_non_integer_decimation() {
        let iq = tone(480, 5_000.0);
        let err =
            extract_channel(&iq, RATE, BLOCK_CENTER, 100_005_000, 10_000, 20_000).unwrap_err();
        assert_eq!(
            err,
            ChannelError::Rate {
                rate_hz: RATE,
                out_rate_hz: 20_000,
            }
        );
        assert!(err.to_string().contains("out_rate_hz 20000"), "{err}");
    }

    #[test]
    fn extract_streaming_matches_one_shot_across_blocks() {
        // Block lengths that are not multiples of the decimation exercise the carried state.
        let iq = tone(4801, 5_000.0);
        let (whole, _) = extract_offset(&iq, RATE, 5_000, 10_000, 12_000).unwrap();
        let mut st = ChannelState::new();
        let mut parts = Vec::new();
        for chunk in [&iq[..1001], &iq[1001..1003], &iq[1003..]] {
            let (o, r) =
                extract_offset_streaming(&mut st, chunk, RATE, 5_000, 10_000, 12_000).unwrap();
            assert_eq!(r, 12_000);
            parts.extend(o);
        }
        assert_eq!(parts.len(), whole.len());
        for (a, b) in parts.iter().zip(&whole) {
            assert!((a - b).norm() < 1e-4, "{a} vs {b}");
        }
    }

    /// The extraction as first written: an exact phase and a `sin_cos` for every sample,
    /// and each output a plain sum over the taps. What the fast path must reproduce.
    fn reference_extract(
        iq: &[Complex<f32>],
        rate_hz: u64,
        offset_hz: i64,
        bandwidth_hz: u64,
        out_rate_hz: u64,
    ) -> Vec<Complex<f32>> {
        let decim = (rate_hz / out_rate_hz) as usize;
        let taps = lowpass_taps(bandwidth_hz as f64 / 2.0, rate_hz as f64, taps_for(decim));
        let step = -(offset_hz as f64) / rate_hz as f64;
        let h = taps.len() - 1;
        let mut ext = vec![Complex::new(0.0, 0.0); h];
        ext.extend(iq.iter().enumerate().map(|(k, &x)| {
            let (s, c) = (std::f64::consts::TAU * (k as f64 * step).rem_euclid(1.0)).sin_cos();
            x * Complex::new(c as f32, s as f32)
        }));
        (0..iq.len())
            .step_by(decim)
            .map(|n| {
                taps.iter()
                    .enumerate()
                    .fold(Complex::new(0.0, 0.0), |acc, (j, &t)| {
                        acc + ext[h + n - j] * t
                    })
            })
            .collect()
    }

    /// A wide channel cut from a radio's full rate (a television multiplex from a
    /// 30.72 Msps capture) equals the reference, whatever the block lengths.
    #[test]
    fn a_wide_channel_from_the_full_rate_matches_the_reference() {
        const FULL: u64 = 30_720_000;
        let iq: Vec<Complex<f32>> = (0..40_000)
            .map(|n: u64| {
                let re = ((n * 7919) % 1013) as f32 / 1013.0 - 0.5;
                let im = ((n * 104_729) % 2039) as f32 / 2039.0 - 0.5;
                Complex::new(re, im)
            })
            .collect();
        let want = reference_extract(&iq, FULL, 7_000_000, 7_000_000, 10_240_000);
        let mut st = ChannelState::new();
        let mut got = Vec::new();
        for chunk in [&iq[..16_384], &iq[16_384..16_389], &iq[16_389..]] {
            let (o, r) =
                extract_offset_streaming(&mut st, chunk, FULL, 7_000_000, 7_000_000, 10_240_000)
                    .unwrap();
            assert_eq!(r, 10_240_000);
            got.extend(o);
        }
        assert_eq!(got.len(), want.len());
        let worst = got
            .iter()
            .zip(&want)
            .map(|(a, b)| (a - b).norm())
            .fold(0.0_f32, f32::max);
        assert!(worst < 1e-4, "worst difference {worst}");
    }

    /// The wide builds of the kernels give what the baseline builds give (on a CPU without
    /// them both sides run the same code, and the test says nothing).
    #[test]
    fn the_wide_kernels_match_the_baseline_ones() {
        let iq: Vec<Complex<f32>> = (0..10_007)
            .map(|n: u64| {
                let re = ((n * 7919) % 1013) as f32 / 1013.0 - 0.5;
                let im = ((n * 104_729) % 2039) as f32 / 2039.0 - 0.5;
                Complex::new(re, im)
            })
            .collect();
        let step = -7.0e6 / 30.72e6;
        let (mut wide, mut base) = (
            vec![Complex::default(); iq.len()],
            vec![Complex::default(); iq.len()],
        );
        let (mut wide_phase, mut base_phase) = (0.37, 0.37);
        shift_into(&iq, &mut wide_phase, step, &mut wide);
        shift_body(&iq, &mut base_phase, step, &mut base);
        assert_eq!(wide_phase, base_phase);
        let worst = |a: &[Complex<f32>], b: &[Complex<f32>]| {
            assert_eq!(a.len(), b.len());
            a.iter()
                .zip(b)
                .map(|(a, b)| (a - b).norm())
                .fold(0.0_f32, f32::max)
        };
        assert!(worst(&wide, &base) < 1e-6, "shift: {}", worst(&wide, &base));
        let reversed = padded(lowpass_taps(3.5e6, 30.72e6, 63));
        for (next, decim) in [(0, 1), (2, 3), (1, 64)] {
            let (a, b) = (
                filter(&base, &reversed, next, decim),
                filter_base(&base, &reversed, next, decim),
            );
            assert!(worst(&a, &b) < 1e-5, "filter by {decim}: {}", worst(&a, &b));
        }
    }

    /// How fast the direct path cuts a wide channel from the full rate, against real
    /// time. Not a gate (it measures the host): run it by hand,
    /// `cargo test -p zeta-dsp --release --lib full_rate_speed -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement, run by hand"]
    fn full_rate_speed() {
        const FULL: u64 = 30_720_000;
        let block: Vec<Complex<f32>> = (0..65_536)
            .map(|n| {
                Complex::new(
                    ((n * 31) % 97) as f32 / 97.0 - 0.5,
                    ((n * 17) % 89) as f32 / 89.0 - 0.5,
                )
            })
            .collect();
        let blocks = 100;
        let signal = (blocks * block.len()) as f64 / FULL as f64;
        // The best of a few runs, as a share of real time: the host is rarely idle.
        let best = |f: &mut dyn FnMut()| {
            (0..7)
                .map(|_| {
                    let start = std::time::Instant::now();
                    for _ in 0..blocks {
                        f();
                    }
                    start.elapsed().as_secs_f64()
                })
                .fold(f64::INFINITY, f64::min)
        };
        let mut st = ChannelState::new();
        let whole = best(&mut || {
            std::hint::black_box(
                extract_offset_streaming(&mut st, &block, FULL, 7_000_000, 7_000_000, 10_240_000)
                    .unwrap(),
            );
        });
        println!(
            "the cut: {:.2}x real time (wide kernels: {})",
            signal / whole,
            wide_kernels()
        );
        let reversed = padded(lowpass_taps(3.5e6, FULL as f64, 63));
        let step = -7.0e6 / FULL as f64;
        let mut ext = vec![Complex::new(0.0, 0.0); 63 + block.len()];
        let mut phase = 0.0;
        let shift = best(&mut || shift_into(&block, &mut phase, step, &mut ext[63..]));
        let shift_base = best(&mut || shift_body(&block, &mut phase, step, &mut ext[63..]));
        let fir = best(&mut || {
            std::hint::black_box(filter(&ext, &reversed, 0, 3));
        });
        let fir_base = best(&mut || {
            std::hint::black_box(filter_base(&ext, &reversed, 0, 3));
        });
        println!(
            "shift: {:.1}x real time, baseline {:.1}x; filter by 3: {:.1}x, baseline {:.1}x",
            signal / shift,
            signal / shift_base,
            signal / fir,
            signal / fir_base
        );
    }

    #[test]
    fn wideband_decimation_rejects_adjacent_channel() {
        const WIDE: u64 = 2_400_000;
        let n = WIDE as usize / 5; // 0.2 s
        // Equal-amplitude in-band tone at +1 kHz and adjacent tone at +48 kHz,
        // which aliases onto 0 Hz after decimation to 48 kHz.
        let a = crate::util::nco(n, 1_000.0, WIDE as f64);
        let b = crate::util::nco(n, 48_000.0, WIDE as f64);
        let iq: Vec<Complex<f32>> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
        let (out, rate) = extract_offset(&iq, WIDE, 0, 12_500, 48_000).unwrap();
        assert_eq!(rate, 48_000);
        // Skip the filter start-up; 4800 samples put 0 and 1 kHz exactly on bins.
        let w = &out[200..5_000];
        let wanted = bin(w, 1_000.0, 48_000.0);
        let alias = bin(w, 0.0, 48_000.0);
        let db = 20.0 * (wanted / alias).log10();
        assert!(
            db >= 60.0,
            "+1 kHz {wanted} vs aliased +48 kHz {alias}: {db} dB"
        );
    }

    #[test]
    fn bandwidth_above_out_rate_is_rejected() {
        let iq = tone(480, 5_000.0);
        let err = extract_offset(&iq, RATE, 0, 20_000, 12_000).unwrap_err();
        assert_eq!(
            err,
            ChannelError::Bandwidth {
                bandwidth_hz: 20_000,
                out_rate_hz: 12_000,
            }
        );
        let msg = err.to_string();
        assert!(msg.contains("bandwidth_hz 20000"), "{msg}");
        assert!(msg.contains("out_rate_hz 12000"), "{msg}");
    }

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

    type Run = tokio::task::JoinHandle<Result<(), StageError>>;

    fn spawn(
        params: serde_json::Value,
        cancel: CancellationToken,
    ) -> (
        broadcast::Sender<Frame>,
        StreamId,
        broadcast::Receiver<Frame>,
        Run,
    ) {
        let bus = Arc::new(StreamBus::new(8));
        let out = bus.register(StreamKind::Iq, prov());
        let rx_out = bus.subscribe(&out).unwrap();
        let (tx, rx_in) = broadcast::channel(8);
        let o2 = out.clone();
        let h = tokio::spawn(async move {
            ChannelStage::new()
                .run(
                    params,
                    vec![rx_in],
                    vec![o2],
                    bus,
                    cancel,
                    StageContext::new(SourceId("s".into())),
                )
                .await
        });
        (tx, out, rx_out, h)
    }

    fn iq_frame(samples: Vec<Complex<f32>>) -> Frame {
        Frame::new(
            StreamId::new(),
            prov(),
            Payload::Iq(IqBlock {
                t0: Timestamp(1_000_000_000),
                sample_rate_hz: Hz(RATE),
                center_hz: Hz(BLOCK_CENTER),
                samples: samples.into(),
            }),
        )
    }

    fn params(center_hz: u64) -> serde_json::Value {
        serde_json::json!({"center_hz": center_hz, "bandwidth_hz": 10_000, "out_rate_hz": 12_000})
    }

    #[tokio::test]
    async fn channel_stage_emits_decimated_blocks_with_channel_provenance() {
        let (tx, out, mut rx_out, h) = spawn(params(100_005_000), CancellationToken::new());
        tx.send(iq_frame(tone(4800, 5_000.0))).unwrap();
        let f = rx_out.recv().await.unwrap();
        assert_eq!(f.stream, out);
        let Payload::Iq(b) = &f.payload else {
            panic!("expected an iq payload")
        };
        assert_eq!(b.samples.len(), 1200);
        assert_eq!(b.sample_rate_hz, Hz(12_000));
        assert_eq!(b.center_hz, Hz(100_005_000));
        assert_eq!(b.t0, Timestamp(1_000_000_000));
        assert_eq!(
            f.provenance.channel,
            Some(ChannelId("100005000/10000".into()))
        );
        let sp = &f.provenance.span;
        assert_eq!(sp.start_ns, Timestamp(1_000_000_000));
        assert_eq!(sp.end_ns.0 - sp.start_ns.0, 100_000_000);
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
    }

    #[tokio::test]
    async fn channel_stage_fails_naming_the_field_and_honours_cancel() {
        let (tx, _out, _rx, h) = spawn(params(100_100_000), CancellationToken::new());
        tx.send(iq_frame(tone(480, 5_000.0))).unwrap();
        match h.await.unwrap() {
            Err(StageError::Failed(m)) => assert!(m.contains("center_hz"), "{m}"),
            r => panic!("expected Failed, got {r:?}"),
        }

        let bad = serde_json::json!({"center_hz": 1, "bandwidth_hz": 1});
        let (_tx, _out, _rx, h) = spawn(bad, CancellationToken::new());
        match h.await.unwrap() {
            Err(StageError::Failed(m)) => assert!(m.contains("out_rate_hz"), "{m}"),
            r => panic!("expected Failed, got {r:?}"),
        }

        let cancel = CancellationToken::new();
        let (_tx, _out, _rx, h) = spawn(params(100_005_000), cancel.clone());
        cancel.cancel();
        assert!(matches!(h.await.unwrap(), Err(StageError::Cancelled)));
    }

    #[tokio::test]
    async fn channel_resets_on_lag() {
        let (tx, _out, mut rx_out, h) = spawn(params(100_005_000), CancellationToken::new());
        // Block A leaves FIR history, decimation phase and NCO phase behind.
        tx.send(iq_frame(tone(4801, 4_000.0))).unwrap();
        rx_out.recv().await.unwrap();
        // Capacity 8: one junk frame is overwritten, then B and seven more.
        let b = tone(4800, 6_000.0);
        tx.send(iq_frame(tone(4800, 1_000.0))).unwrap();
        for _ in 0..8 {
            tx.send(iq_frame(b.clone())).unwrap();
        }
        let f = rx_out.recv().await.unwrap();
        let Payload::Iq(got) = &f.payload else {
            panic!("expected an iq payload")
        };
        let (want, _) = extract_offset(&b, RATE, 5_000, 10_000, 12_000).unwrap();
        assert_eq!(got.samples.len(), want.len());
        for (g, w) in got.samples.iter().zip(&want) {
            assert!((g - w).norm() < 1e-5, "{g} vs {w}");
        }
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
    }

    /// Run the stage with `params` over `frames`, noting into a list: its output samples
    /// and the notes.
    async fn run_noted(
        params: serde_json::Value,
        frames: Vec<Frame>,
    ) -> (Vec<Complex<f32>>, Vec<serde_json::Value>) {
        let notes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let n2 = notes.clone();
        let ctx = StageContext::new(SourceId("s".into()))
            .with_note(Arc::new(move |v| n2.lock().unwrap().push(v)));
        let bus = Arc::new(StreamBus::new(1_024));
        let out = bus.register(StreamKind::Iq, prov());
        let mut rx_out = bus.subscribe(&out).unwrap();
        let (tx, rx_in) = broadcast::channel(1_024);
        for f in frames {
            tx.send(f).unwrap();
        }
        drop(tx);
        let r = ChannelStage::new()
            .run(
                params,
                vec![rx_in],
                vec![out],
                bus,
                CancellationToken::new(),
                ctx,
            )
            .await;
        assert!(matches!(r, Ok(())), "{r:?}");
        let mut samples = Vec::new();
        while let Ok(f) = rx_out.try_recv() {
            let Payload::Iq(b) = &f.payload else {
                panic!("expected an iq payload")
            };
            samples.extend_from_slice(&b.samples);
        }
        let notes = notes.lock().unwrap().clone();
        (samples, notes)
    }

    fn frame_at(samples: Vec<Complex<f32>>, rate_hz: u64, center_hz: u64) -> Frame {
        Frame::new(
            StreamId::new(),
            prov(),
            Payload::Iq(IqBlock {
                t0: Timestamp(1_000_000_000),
                sample_rate_hz: Hz(rate_hz),
                center_hz: Hz(center_hz),
                samples: samples.into(),
            }),
        )
    }

    #[tokio::test]
    async fn wide_band_off_the_block_falls_back_and_says_so() {
        // ch1 widened reaches 476 406 250 Hz, below the block's 476 406 750 Hz.
        const WIDE_RATE: u64 = 1_024_000;
        let iq = crate::util::nco(10_240, -493_750.0 + 2_000.0, WIDE_RATE as f64);
        let p = serde_json::json!({
            "center_hz": 476_425_000, "bandwidth_hz": 12_500, "out_rate_hz": 51_200,
        });
        let mut wide = p.clone();
        wide["wide_hz"] = 37_500.into();
        let frames = || vec![frame_at(iq.clone(), WIDE_RATE, 476_918_750)];
        let (plain, _) = run_noted(p, frames()).await;
        let (got, notes) = run_noted(wide, frames()).await;
        assert!(!plain.is_empty());
        assert_eq!(got, plain);
        assert_eq!(
            notes.last().unwrap(),
            &serde_json::json!({"channel": {
                "path": "direct",
                "reason": "no bank",
                "wide": "off: leaves the block 476406750..=477430750",
            }})
        );
    }

    #[tokio::test]
    async fn wide_hz_passes_the_neighbour() {
        // A unit tone 12.5 kHz off the channel centre, at a 256 000 Hz bin rate.
        const BIN_RATE: u64 = 256_000;
        let iq = crate::util::nco(25_600, 12_500.0, BIN_RATE as f64);
        let p = serde_json::json!({
            "center_hz": BLOCK_CENTER, "bandwidth_hz": 12_500, "out_rate_hz": 51_200,
        });
        let mut wide = p.clone();
        wide["wide_hz"] = 37_500.into();
        let power_db = |x: &[Complex<f32>]| {
            let w = &x[100..];
            10.0 * (w.iter().map(|v| v.norm_sqr() as f64).sum::<f64>() / w.len() as f64).log10()
        };
        let frames = || vec![frame_at(iq.clone(), BIN_RATE, BLOCK_CENTER)];
        let (narrow, _) = run_noted(p, frames()).await;
        let (got, notes) = run_noted(wide, frames()).await;
        assert!(power_db(&got).abs() <= 1.0, "{} dB", power_db(&got));
        assert!(power_db(&narrow) <= -10.0, "{} dB", power_db(&narrow));
        assert_eq!(
            notes.last().unwrap(),
            &serde_json::json!({"channel": {"path": "direct", "reason": "no bank"}})
        );
    }

    const VIDEO_RATE: u64 = 30_720_000;

    fn noise_iq(n: usize) -> Vec<Complex<f32>> {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 40) as f32 / (1u64 << 24) as f32 - 0.5
        };
        (0..n).map(|_| Complex::new(next(), next())).collect()
    }

    #[test]
    fn a_wide_band_at_decimation_1_passes_the_block_through() {
        assert!(skips_filter(VIDEO_RATE, 24_000_000, VIDEO_RATE));
        // 20.5-23 MHz on the 30.72 rung and 10.3-11.5 MHz on 15.36 skip too.
        assert!(skips_filter(VIDEO_RATE, 22_000_000, VIDEO_RATE));
        assert!(skips_filter(VIDEO_RATE, 15_360_000, VIDEO_RATE));
        assert!(!skips_filter(VIDEO_RATE, 15_359_999, VIDEO_RATE));
        assert!(skips_filter(15_360_000, 10_300_000, 15_360_000));
        assert!(!skips_filter(VIDEO_RATE, 24_000_000, VIDEO_RATE / 2));
        // A narrow input filters, however wide its band: a 48 kHz band on 48 kHz.
        assert!(!skips_filter(48_000, 48_000, 48_000));
        assert!(!skips_filter(9_999_999, 9_999_999, 9_999_999));
        let iq: Arc<[Complex<f32>]> = noise_iq(65_536).into();
        let mut st = DirectChannel::new();
        let (out, rate) =
            extract_direct(&mut st, &iq, VIDEO_RATE, 0, 24_000_000, VIDEO_RATE).unwrap();
        assert_eq!(rate, VIDEO_RATE);
        assert!(Arc::ptr_eq(&out, &iq), "offset 0 hands the same samples on");
    }

    #[test]
    fn a_wide_band_off_centre_is_shifted_accurately_over_a_million_samples() {
        const N: usize = 1 << 20;
        let offset: i64 = 1_234_567;
        let iq = noise_iq(N);
        let mut st = DirectChannel::new();
        let mut got = Vec::with_capacity(N);
        // Uneven blocks, so the phase carried between blocks is exercised too.
        let mut at = 0;
        for len in [65_536usize, 1, 4_095, 65_537].iter().cycle() {
            if at >= N {
                break;
            }
            let end = (at + len).min(N);
            let block: Arc<[Complex<f32>]> = iq[at..end].into();
            let (out, _) =
                extract_direct(&mut st, &block, VIDEO_RATE, offset, 24_000_000, VIDEO_RATE)
                    .unwrap();
            got.extend_from_slice(&out);
            at = end;
        }
        assert_eq!(got.len(), N);
        let worst = iq
            .iter()
            .zip(&got)
            .enumerate()
            .map(|(n, (x, y))| {
                let cycles = (-(offset as f64) * n as f64 / VIDEO_RATE as f64).rem_euclid(1.0);
                let (s, c) = (2.0 * PI * cycles).sin_cos();
                let want = Complex::new(
                    x.re as f64 * c - x.im as f64 * s,
                    x.re as f64 * s + x.im as f64 * c,
                );
                let d = Complex::new(y.re as f64, y.im as f64) - want;
                d.norm()
            })
            .fold(0.0f64, f64::max);
        assert!(worst < 1e-4, "worst error {worst}");
    }

    #[tokio::test]
    async fn a_video_rate_channel_runs_unfiltered_on_its_thread_and_says_so() {
        let iq = noise_iq(65_536);
        let center = 1_300_000_000;
        let p = serde_json::json!({
            "center_hz": center, "bandwidth_hz": 24_000_000, "out_rate_hz": VIDEO_RATE,
        });
        let frames = (0..3)
            .map(|_| frame_at(iq.clone(), VIDEO_RATE, center))
            .collect();
        let (got, notes) = run_noted(p, frames).await;
        assert_eq!(got.len(), 3 * iq.len());
        assert_eq!(&got[..iq.len()], &iq[..]);
        assert_eq!(
            notes.last().unwrap(),
            &serde_json::json!({"channel": {
                "path": "direct", "reason": "no bank", "filter": "none",
            }})
        );
    }

    #[tokio::test]
    async fn a_narrow_input_with_a_full_band_is_still_filtered() {
        let iq = tone(4_800, 1_000.0);
        let p = serde_json::json!({
            "center_hz": BLOCK_CENTER, "bandwidth_hz": RATE, "out_rate_hz": RATE,
        });
        let (got, notes) = run_noted(p, vec![frame_at(iq.clone(), RATE, BLOCK_CENTER)]).await;
        let (want, _) = extract_offset(&iq, RATE, 0, RATE, RATE).unwrap();
        assert_eq!(got, want);
        assert_ne!(got, iq, "the filter ran");
        assert_eq!(
            notes.last().unwrap(),
            &serde_json::json!({"channel": {"path": "direct", "reason": "no bank"}})
        );
    }

    #[tokio::test]
    async fn a_threaded_channel_fails_when_the_capture_leaves_it() {
        let iq = noise_iq(4_096);
        let p = serde_json::json!({
            "center_hz": 1_300_000_000u64, "bandwidth_hz": 24_000_000, "out_rate_hz": VIDEO_RATE,
        });
        let (tx, _out, _rx, h) = spawn(p, CancellationToken::new());
        tx.send(frame_at(iq.clone(), VIDEO_RATE, 1_300_000_000))
            .unwrap();
        tx.send(frame_at(iq, VIDEO_RATE, 1_400_000_000)).unwrap();
        match h.await.unwrap() {
            Err(StageError::Failed(m)) => assert!(m.contains("center_hz"), "{m}"),
            r => panic!("expected Failed, got {r:?}"),
        }
    }

    const CUT_RATE: u64 = 2_400_000;

    #[test]
    fn a_cut_holds_the_band_at_the_lowest_divisor_rate() {
        // A tone in the band, and one 40 kHz above it: at 48 kS/s unfiltered it would fold
        // to -8 kHz, inside the band.
        let n = 240_000;
        let inside = crate::util::nco(n, 300_000.0, CUT_RATE as f64);
        let above = crate::util::nco(n, 340_000.0, CUT_RATE as f64);
        let iq: Vec<_> = inside.iter().zip(&above).map(|(a, b)| a + b).collect();
        let (out, rate) = Cutter.cut(&iq, Hz(CUT_RATE), 300_000, Hz(12_500)).unwrap();
        assert_eq!(rate, Hz(48_000));
        assert_eq!(out.len(), n / 50);
        // Past the filter's start-up the tone is at 0 Hz, at its own level, alone.
        let settled = &out[50..];
        let mean = settled.iter().sum::<Complex<f32>>() / settled.len() as f32;
        assert!((mean.norm() - 1.0).abs() < 0.01, "the tone's level {mean}");
        let worst = settled
            .iter()
            .map(|v| (v - mean).norm())
            .fold(0.0, f32::max);
        assert!(
            worst < 0.01,
            "the tone moves, or the one above folds in: {worst}"
        );
    }

    /// The level of a unit tone `from_band_hz` off a band `bandwidth_hz` wide, cut from a
    /// block at `rate_hz`, past the filters' start-up (dB, a tone in the band at 0).
    fn cut_level(rate_hz: u64, bandwidth_hz: u64, from_band_hz: f64) -> f64 {
        let offset = 1_000_000i64;
        let iq = crate::util::nco(
            (rate_hz / 50) as usize,
            offset as f64 + from_band_hz,
            rate_hz as f64,
        );
        let (out, _) = Cutter
            .cut(&iq, Hz(rate_hz), offset, Hz(bandwidth_hz))
            .unwrap();
        let settled = &out[out.len() / 4..];
        let power = settled.iter().map(|v| v.norm_sqr() as f64).sum::<f64>() / settled.len() as f64;
        10.0 * power.log10()
    }

    #[test]
    fn a_wide_block_is_cut_in_stages_so_a_neighbour_stays_out() {
        // 20 MS/s to 50 kS/s is a decimation of 400, past one stage's longest filter.
        assert_eq!(cut_stages(400), [100, 4]);
        assert_eq!(cut_stages(1_280), [80, 4, 4]);
        assert_eq!(cut_stages(128), [128]);
        // No factor past one stage: one stage, at the longest filter.
        assert_eq!(cut_stages(131), [131]);
        const WIDE: u64 = 20_000_000;
        let (_, rate) = Cutter
            .cut(&noise_iq(4_000), Hz(WIDE), 0, Hz(12_500))
            .unwrap();
        assert_eq!(rate, Hz(50_000));
        // Tones 40 to 50 kHz off a 12.5 kHz band, folding onto it at 50 kS/s.
        for from in [40_000.0, 45_000.0, 50_000.0, -45_000.0, -50_000.0] {
            let db = cut_level(WIDE, 12_500, from);
            assert!(db < -60.0, "a tone {from} Hz off the band is {db:.1} dB");
        }
        // The band itself passes, its edge where a job's channel puts it.
        assert!(cut_level(WIDE, 12_500, 0.0).abs() < 0.1);
        let edge = cut_level(WIDE, 12_500, 6_250.0);
        assert!((-7.0..-5.0).contains(&edge), "the band's edge {edge:.2} dB");
    }

    #[test]
    fn a_cut_as_wide_as_the_block_is_the_block() {
        let iq = noise_iq(4_800);
        let got = Cutter.cut(&iq, Hz(CUT_RATE), 0, Hz(CUT_RATE)).unwrap();
        assert_eq!(got, (iq, Hz(CUT_RATE)));
    }

    #[test]
    fn a_cut_at_the_blocks_own_rate_moves_the_band_to_the_centre() {
        // 48 kS/s is the lowest rate a band is cut at: a 12.5 kHz band 10 kHz off centre
        // stays at it, shifted to 0 Hz sample for sample.
        let iq = tone(4_800, 10_000.0);
        let (out, rate) = Cutter.cut(&iq, Hz(RATE), 10_000, Hz(12_500)).unwrap();
        assert_eq!((out.len(), rate), (iq.len(), Hz(RATE)));
        let worst = out
            .iter()
            .map(|v| (v - Complex::new(1.0, 0.0)).norm())
            .fold(0.0, f32::max);
        assert!(worst < 1e-3, "the tone is not at 0 Hz: {worst}");
    }

    #[test]
    fn a_band_outside_the_block_cannot_be_cut() {
        let iq = noise_iq(4_800);
        let cut = |offset_hz: i64, bandwidth_hz: u64| {
            Cutter.cut(&iq, Hz(CUT_RATE), offset_hz, Hz(bandwidth_hz))
        };
        // Wholly outside, on either side.
        assert_eq!(cut(1_300_000, 12_500), None);
        assert_eq!(cut(-1_300_000, 12_500), None);
        // Reaching past an edge by one hertz; touching it is inside.
        assert_eq!(cut(1_193_751, 12_500), None);
        assert_eq!(cut(-1_193_751, 12_500), None);
        assert!(cut(1_193_750, 12_500).is_some());
        assert!(cut(-1_193_750, 12_500).is_some());
        // An odd band, its half-hertz edge exact: 12_501 Hz reaches 6_250.5 Hz each side.
        assert!(cut(1_193_749, 12_501).is_some());
        assert_eq!(cut(1_193_750, 12_501), None);
        assert_eq!(cut(-1_193_750, 12_501), None);
        // Wider than the block, or no band at all.
        assert_eq!(cut(0, CUT_RATE + 1), None);
        assert_eq!(cut(0, 0), None);
        assert_eq!(Cutter.cut(&iq, Hz(0), 0, Hz(12_500)), None);
    }
}
