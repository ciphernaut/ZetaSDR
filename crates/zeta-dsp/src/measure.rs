//! Layer-1 classical measurement: a numeric feature record per IQ block and a
//! conservative label derived from it.

use std::sync::Arc;

use async_trait::async_trait;
use num_complex::Complex;
use rustfft::FftPlanner;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::classify::{
    Band, Classifier, FeatureRecord, Label, Measurement, RateLine, SsbFeature, Tried,
};
use zeta_core::knowledge::Allocation;
use zeta_core::ops::{MEASURE_DC_MASK_HZ, Sideband, channel_rate_hz};
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Baud, Hz, Provenance, StreamId, StreamKind, Timestamp};

mod burst;
mod order;

pub(crate) use burst::{
    BURST_DUTY_MAX, BURST_ENVELOPE_MS, BURST_MERGE_MS, BURST_ON_DB, noise_floor_power,
};
pub use burst::{Bursts, burst_runs, on_time};
use burst::{
    View, averaged_half_spectrum, bandpass, cyclo_symbol_rate, gated_view, strongest_line,
};
use order::{OokFit, envelope_runs, fsk_clusters, ook_unit_rate, psk_order, two_levels};

use crate::channel::extract_offset;
use crate::fft::{Window, power_spectrum_db};
use crate::{Input, frame_span, run_map};

/// FFT size of every spectrum taken here.
const SIZE: usize = 1024;
/// Occupied band: bins at or above the noise floor plus this many dB.
const OCCUPIED_DB: f32 = 6.0;
/// [`power_width_hz`] counts only bins at least this many dB over the noise floor: the
/// averaged noise bins stay under it, so the share is the signal's, not the box's noise.
const POWER_GATE_DB: f32 = 3.0;
/// The share of the excess power [`power_width_hz`] holds.
const POWER_SHARE: f64 = 0.99;
/// Below-threshold gaps of at most this width inside the occupied band are
/// bridged, so the discrete sidebands of an FM tone count as one band.
const GAP_HZ: f64 = 1_500.0;
/// `burst` when the loudest 10 ms chunk exceeds the median chunk power by this ratio.
const BURST_RATIO: f64 = 3.0;
/// A line (symbol rate, or the 19 kHz broadcast pilot) must stand this far
/// above the median of its spectrum.
const LINE_DB: f32 = 10.0;
/// Neighbourhood half-width, in bins, for a symbol-rate line's local median.
const LOCAL_BINS: usize = 16;
/// Audio below this fraction of its RMS is treated as silent on the FM keying path.
const SILENT_AUDIO: f32 = 0.25;
/// Lowest symbol rate searched.
const MIN_LINE_HZ: f64 = 50.0;
/// Below this SNR a block is labelled noise.
const NOISE_SNR_DB: f32 = 6.0;
/// FM needs discriminator audio (from `FM_MIN_AUDIO_HZ` to half the occupied band, its
/// drift taken out) whose RMS deviation beyond the envelope's motion is above this
/// ([`Motion::over_hz`]); below it a constant-envelope signal is a carrier. Where the noise
/// could click, FM needs more (`CLICK_MARGIN`, `WEAK_FM_MOTION`), and what lies between is
/// `unknown`.
/// Measured over the carrier where it is up, on generators:
/// - a bare carrier, keyed or steady, and one drifting linearly by up to 3 kHz in 2 s, or
///   warming up by 1 kHz with a 0.2 s time constant, at 15 and 36 dB: at most 1 Hz over
///   the envelope (the phase alone up to 28 Hz at a 3 kHz drift);
/// - AM at m 0.1 from 3 to 30 dB: 0 Hz over the envelope (the phase alone up to 47 Hz at
///   6 dB in 12.5 kHz, as the noise drives it in the sidebands' band);
/// - the keyed-over generator's quiet passage (audio at a tenth, 62 Hz RMS as sent at
///   2.5 kHz deviation), at 15 dB in 12.5 kHz: 48-63 Hz over the envelope, 48 on a 0.3 s
///   key-down, the phase alone 74-80 Hz.
///
/// A drift faster than that bends above `FM_MIN_AUDIO_HZ` and counts as deviation.
const FM_MIN_DEV_HZ: f64 = 40.0;
/// The discriminator's deviation counts a phase step only where the envelope is at least
/// this fraction of its median ([`audio_rms_hz`]).
const DISC_MIN_ENVELOPE: f32 = 0.25;
/// The discriminator's deviation is read from this audio frequency up ([`audio_rms_hz`]):
/// a carrier's drift bends slower, while sub-audible tone squelch, from 67 Hz, and speech
/// are above it.
const FM_MIN_AUDIO_HZ: f64 = 50.0;
/// FM: envelope coefficient of variation below this ...
const FM_MAX_CV: f32 = 0.2;
/// ... and occupied bandwidth at most this.
const FM_MAX_BW_HZ: u64 = 25_000;
/// Wideband FM with a 19 kHz pilot: occupied band from this (up to
/// `WFM_MAX_BW_HZ`); narrowband FM (at most `FM_MAX_BW_HZ`) cannot reach it.
const WFM_PILOT_MIN_BW_HZ: u64 = 40_000;
/// Wideband (broadcast) FM without a pilot: occupied band from this ...
const WFM_MIN_BW_HZ: u64 = 60_000;
/// ... to this, ...
const WFM_MAX_BW_HZ: u64 = 250_000;
/// ... envelope coefficient of variation below this (looser than `FM_MAX_CV`:
/// a wide band at a modest rate collects more noise, and multipath on a real
/// station ripples the envelope; one measured 0.33-0.36) ...
const WFM_MAX_CV: f32 = 0.45;
/// ... and discriminator audio deviating by more than this RMS.
const WFM_MIN_DEV_HZ: f64 = 5_000.0;
/// Broadcast FM stereo pilot frequency ...
const PILOT_HZ: f64 = 19_000.0;
/// ... and the half-width of the window it is searched in.
const PILOT_TOL_HZ: f64 = 300.0;
/// The pilot's line is measured against the median of the discriminator audio
/// spectrum over this range, the pilot window excluded.
const PILOT_REGION_HZ: (f64, f64) = (15_000.0, 23_000.0);
/// AM and SSB: envelope coefficient of variation at least this.
const AM_MIN_CV: f32 = 0.25;
/// Low-index AM (below `AM_MIN_CV`): envelope coefficient of variation at
/// least this, with a carrier line and the envelope moving more than the phase, in Hz
/// (`MOTION_MARGIN`).
const AM_LOW_MIN_CV: f32 = 0.1;
/// AM carrier line: the sideband level is the mean power of the bins this far
/// from the centre, either side.
const AM_SIDEBAND_HZ: (f64, f64) = (1_000.0, 5_000.0);
/// SSB: occupied band range ...
const SSB_BW_HZ: (u64, u64) = (1_500, 4_000);
/// ... and the least fraction of the occupied-band power on one side of centre.
const SSB_ONE_SIDE: f64 = 0.8;
/// FSK: the two tones are within this many dB of each other, and every other
/// peak is at least this far below the weaker tone.
const FSK_PEAK_DB: f32 = 6.0;
/// FSK: tone separation range.
pub(super) const FSK_SEP_HZ: (f64, f64) = (500.0, 3_000.0);

/// Noise floor of [`measure_band`]: this percentile of the in-band bins. A
/// signal may fill more than half of a channel (APRS: Carson ~10.4 kHz in
/// 12.5 kHz), so the median would be signal, not noise.
pub(crate) const FLOOR_PERCENTILE: usize = 15;

/// The `pct`th percentile of `v` (the value at index `len · pct / 100` when
/// sorted); 0 when empty.
pub(crate) fn percentile(v: &[f32], pct: usize) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    let k = (s.len() * pct / 100).min(s.len() - 1);
    // The k-th smallest, as a sort would place it: a selection, not a sort.
    *s.select_nth_unstable_by(k, f32::total_cmp).1
}

/// Median of `v` (upper median for an even length); 0 when empty.
fn median(v: &[f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(f32::total_cmp);
    s[s.len() / 2]
}

/// `SIZE`-bin Hann spectrum of `iq` (fftshifted dB); a block shorter than
/// `SIZE` is zero-padded to one chunk.
fn spectrum(iq: &[Complex<f32>]) -> Vec<f32> {
    if iq.len() >= SIZE {
        power_spectrum_db(iq, SIZE, Window::Hann)
    } else {
        let mut x = iq.to_vec();
        x.resize(SIZE, Complex::new(0.0, 0.0));
        power_spectrum_db(&x, SIZE, Window::Hann)
    }
}

/// Index of the largest value (first on ties).
fn argmax(v: &[f32]) -> usize {
    v.iter()
        .enumerate()
        .fold(
            (0, f32::NEG_INFINITY),
            |b, (i, &x)| if x > b.1 { (i, x) } else { b },
        )
        .0
}

/// Inclusive bin range of the occupied band around `peak`: the contiguous run
/// at or above `threshold`, bridging gaps of at most `gap` bins.
fn occupied(db: &[f32], peak: usize, threshold: f32, gap: usize) -> (usize, usize) {
    let extend = |mut i: usize, step: isize| {
        let mut j = i as isize;
        let mut below = 0;
        loop {
            j += step;
            if j < 0 || j as usize >= db.len() {
                return i;
            }
            if db[j as usize] >= threshold {
                i = j as usize;
                below = 0;
            } else {
                below += 1;
                if below > gap {
                    return i;
                }
            }
        }
    };
    (extend(peak, -1), extend(peak, 1))
}

/// The width (Hz) that holds [`POWER_SHARE`] of the excess power over the noise floor `floor` (dB)
/// of the band around `peak` in `db` (`bin_hz` per bin): the run of bins around the peak at
/// least [`POWER_GATE_DB`] over the floor (gaps of up to `gap` bins bridged, so a separate
/// neighbour is not counted), trimmed of the first and last half of the remaining share of
/// its power. At a low SNR the occupied band (6 dB over the floor) can be only a carrier line;
/// the sidebands under that threshold still hold most of the power. `None` when no bin
/// clears the gate.
fn power_width_hz(db: &[f32], peak: usize, floor: f32, gap: usize, bin_hz: f64) -> Option<u64> {
    let gate = floor + POWER_GATE_DB;
    if db.get(peak).is_none_or(|&v| v < gate) {
        return None;
    }
    let (a, b) = occupied(db, peak, gate, gap);
    let noise = 10f64.powf(floor as f64 / 10.0);
    let excess: Vec<f64> = db[a..=b]
        .iter()
        .map(|&v| {
            if v >= gate {
                (10f64.powf(v as f64 / 10.0) - noise).max(0.0)
            } else {
                0.0
            }
        })
        .collect();
    let total: f64 = excess.iter().sum();
    if total <= 0.0 {
        return None;
    }
    let tail = total * (1.0 - POWER_SHARE) / 2.0;
    let mut acc = 0.0;
    let lo = excess
        .iter()
        .position(|&e| {
            acc += e;
            acc > tail
        })
        .unwrap_or(0);
    let mut acc = 0.0;
    let hi = excess
        .iter()
        .rposition(|&e| {
            acc += e;
            acc > tail
        })
        .unwrap_or(excess.len() - 1);
    Some(((hi - lo + 1) as f64 * bin_hz).round().max(bin_hz.round()) as u64)
}

/// Coefficient of variation (std / mean) of the envelope `|x|`; 0 for an
/// empty or all-zero block.
pub fn envelope_cv(iq: &[Complex<f32>]) -> f32 {
    if iq.is_empty() {
        return 0.0;
    }
    let n = iq.len() as f64;
    let mean = iq.iter().map(|s| s.norm() as f64).sum::<f64>() / n;
    if mean <= 0.0 {
        return 0.0;
    }
    let var = iq
        .iter()
        .map(|s| (s.norm() as f64 - mean).powi(2))
        .sum::<f64>()
        / n;
    (var.sqrt() / mean) as f32
}

/// Whether the loudest 10 ms chunk of `iq` is more than `BURST_RATIO` times
/// the median chunk power (the median is the idle level whenever the signal
/// is on for under half the block). Fewer than two chunks is never a burst.
fn is_burst(iq: &[Complex<f32>], rate_hz: u64) -> bool {
    let chunk = ((rate_hz / 100) as usize).max(1);
    let mut powers: Vec<f64> = iq
        .chunks_exact(chunk)
        .map(|c| c.iter().map(|s| s.norm_sqr() as f64).sum::<f64>() / chunk as f64)
        .collect();
    if powers.len() < 2 {
        return false;
    }
    powers.sort_by(f64::total_cmp);
    let median = powers[powers.len() / 2];
    let max = powers[powers.len() - 1];
    median > 0.0 && max / median > BURST_RATIO
}

/// Phase-difference discriminator in radians per sample (`arg(x[n] x*[n-1])`);
/// `len - 1` samples.
fn discriminator(x: &[Complex<f32>]) -> Vec<f32> {
    x.windows(2)
        .map(|w| {
            let v = (w[1] * w[0].conj()).arg();
            if v.is_finite() { v } else { 0.0 }
        })
        .collect()
}

/// Analytic signal of real `x` band-limited to `max_hz` at `rate_hz`
/// (negative frequencies and everything above `max_hz` removed by FFT).
fn analytic(x: &[f32], rate_hz: u64, max_hz: f64) -> Vec<Complex<f32>> {
    band_analytic(x, rate_hz, 0.0, max_hz)
}

/// [`analytic`] with everything under `min_hz` removed too (the DC bin is kept).
fn band_analytic(x: &[f32], rate_hz: u64, min_hz: f64, max_hz: f64) -> Vec<Complex<f32>> {
    let n = x.len();
    if n == 0 {
        return Vec::new();
    }
    let mut planner = FftPlanner::<f32>::new();
    let mut buf: Vec<Complex<f32>> = x.iter().map(|&v| Complex::new(v, 0.0)).collect();
    planner.plan_fft_forward(n).process(&mut buf);
    let max_bin = (max_hz * n as f64 / rate_hz as f64).floor() as usize;
    let min_bin = (min_hz * n as f64 / rate_hz as f64).ceil() as usize;
    for (k, b) in buf.iter_mut().enumerate().skip(1) {
        if k > max_bin || k < min_bin {
            *b = Complex::new(0.0, 0.0);
        } else if 2 * k < n {
            *b *= 2.0;
        } else if 2 * k > n {
            *b = Complex::new(0.0, 0.0);
        }
    }
    planner.plan_fft_inverse(n).process(&mut buf);
    let scale = 1.0 / n as f32;
    buf.iter().map(|c| c * scale).collect()
}

/// The signal whose spectrum carries the keying rate.
///
/// The brief's recipe is the squared envelope, which carries amplitude keying
/// (OOK/ASK, bursts). A constant-envelope (FM-family) signal has a flat
/// envelope, and for AFSK-on-FM even the squared discriminator output shows
/// only lines at twice the tones (measured: 4453 Hz on the APRS fixture, not
/// 1200). So when the envelope CV is below `FM_MAX_CV` the keying is taken
/// from the instantaneous frequency of the demodulated audio: discriminate,
/// form the audio's analytic signal, discriminate again, smooth over
/// `rate / 4800` samples, and rectify the sample-to-sample change, which
/// pulses at every tone transition and so has a line at the symbol rate.
/// The audio is band-limited to half the occupied band (Carson) before the
/// second discriminator, and near-silent audio contributes nothing.
/// Direct (RF) FSK keying is not recovered by this path.
fn keying_signal(iq: &[Complex<f32>], rate_hz: u64, cv: f32, bandwidth_hz: u64) -> Vec<f32> {
    if cv >= FM_MAX_CV {
        return iq.iter().map(|s| s.norm_sqr()).collect();
    }
    let audio = analytic(&discriminator(iq), rate_hz, bandwidth_hz as f64 / 2.0);
    let tone_freq = discriminator(&audio);
    // Where the audio is near silent its frequency is meaningless; hold it.
    let rms = (audio.iter().map(|a| a.norm_sqr() as f64).sum::<f64>() / audio.len().max(1) as f64)
        .sqrt() as f32;
    let audible: Vec<bool> = audio
        .windows(2)
        .map(|w| w[0].norm().min(w[1].norm()) >= SILENT_AUDIO * rms)
        .collect();
    let m = ((rate_hz / 4_800) as usize).max(1);
    // Moving average over `m` samples by running sum (f64, so long blocks do
    // not drift); one output per full window, as `windows(m)` gives.
    let mut smooth = Vec::with_capacity((tone_freq.len() + 1).saturating_sub(m));
    let mut sum = 0.0f64;
    for (i, &v) in tone_freq.iter().enumerate() {
        sum += v as f64;
        if i >= m {
            sum -= tone_freq[i - m] as f64;
        }
        if i + 1 >= m {
            smooth.push((sum / m as f64) as f32);
        }
    }
    smooth
        .windows(2)
        .zip(&audible)
        .map(|(w, &on)| if on { (w[1] - w[0]).abs() } else { 0.0 })
        .collect()
}

/// Positive-frequency half of the `SIZE`-bin spectrum of demeaned real `x`
/// (bin `k` is `k · rate / SIZE`); `None` when `x` is shorter than `SIZE`.
fn real_spectrum(x: &[f32]) -> Option<Vec<f32>> {
    if x.len() < SIZE {
        return None;
    }
    let mean = x.iter().map(|&v| v as f64).sum::<f64>() / x.len() as f64;
    let c: Vec<Complex<f32>> = x
        .iter()
        .map(|&v| Complex::new((v as f64 - mean) as f32, 0.0))
        .collect();
    Some(power_spectrum_db(&c, SIZE, Window::Hann)[SIZE / 2..].to_vec())
}

/// Strongest line (a local maximum) of the keying signal of `runs` (its
/// `SIZE`-point spectrum averaged over them, [`averaged_half_spectrum`]) between
/// `MIN_LINE_HZ` and `rate / 2` (half the occupied band on the FM path) that stands `LINE_DB` above that range's median and also `LINE_DB` above
/// the median of its own neighbourhood (`LOCAL_BINS` either side, its main
/// lobe excluded), so a coloured noise hump is not mistaken for a line. The
/// frequency is refined by a parabola through the peak bin and its neighbours.
///
/// On an HDLC flag-only stretch (e.g. an APRS preamble of 0x7E flags) the
/// keying repeats once per 8-bit flag, so the strongest line is the byte rate
/// (150 Bd for 1200 bit/s), not the bit rate; the full APRS packet reports 1200.
fn symbol_rate(runs: &[&[Complex<f32>]], rate_hz: u64, cv: f32, bandwidth_hz: u64) -> Option<Baud> {
    let keying: Vec<Vec<f32>> = runs
        .iter()
        .map(|r| keying_signal(r, rate_hz, cv, bandwidth_hz))
        .collect();
    let db = averaged_half_spectrum(&keying, SIZE)?;
    let bin_hz = rate_hz as f64 / SIZE as f64;
    let lo = (MIN_LINE_HZ / bin_hz).ceil() as usize;
    // Under FM the keying cannot be faster than the highest modulating
    // frequency, which Carson's rule puts at half the occupied band.
    let hi = if cv < FM_MAX_CV {
        ((bandwidth_hz as f64 / 2.0 / bin_hz) as usize + 1).min(db.len())
    } else {
        db.len()
    };
    strongest_line(&db, bin_hz, lo, hi).map(|(hz, _)| Baud(hz.round() as u32))
}

/// Measure one IQ block centred at `block_center_hz`.
///
/// From the `SIZE`-bin Hann power spectrum: noise floor = 15th-percentile bin;
/// `snr_db` = peak − floor; occupied band = the run of bins around the peak at
/// or above floor + 6 dB, giving `bandwidth_hz` (run length × bin width, at
/// least one bin) and `center_hz` (block centre plus the run's centre offset,
/// clamped at 0). See [`measure_band_masked`] for the burst gating, `duty`
/// and the symbol rate.
pub fn measure(iq: &[Complex<f32>], rate_hz: u64, block_center_hz: u64) -> FeatureRecord {
    measure_band(iq, rate_hz, block_center_hz, rate_hz)
}

/// [`measure`] restricted to the central `bandwidth_hz` of the block: floor,
/// peak and occupied run use only the bins within `± bandwidth_hz / 2` of the
/// centre (at least one bin), so the skirts of a channel filter wider than its
/// band are not measured as occupied.
pub fn measure_band(
    iq: &[Complex<f32>],
    rate_hz: u64,
    block_center_hz: u64,
    bandwidth_hz: u64,
) -> FeatureRecord {
    measure_band_masked(iq, rate_hz, block_center_hz, bandwidth_hz, None)
}

/// [`measure_band`] with the receiver's DC spike masked: `dc = Some((dc_hz,
/// half_width_hz))` masks the in-band bins whose centre lies within
/// `half_width_hz` of `dc_hz - block_center_hz` (the DC's offset in the block).
/// The floor is the 15th percentile of the unmasked in-band bins; the masked bins
/// are then set to that floor, so they are neither the peak nor occupied. When
/// the mask covers every in-band bin, or `dc_hz` lies outside the band, it is not
/// applied and the result is [`measure_band`]'s. Nor is it when the DC sits at the
/// band's centre (within one bin of `block_center_hz`): the radio was tuned for
/// this band, its signal sits on the DC, and masking would hide what is measured.
///
/// Burst gating: the window's on-runs ([`burst_runs`], the off part found from either
/// side, so `duty` is the on share) give `duty`, `bursts`, `burst_ms` and `period_ms`.
/// A carrier only a few dB over the noise, which single chunks do not split from it, is
/// found by its two levels over `BURST_LEVEL_MS` (`level_runs`), and the runs
/// are then those.
/// When the window holds an off part of noise ([`gated_view`]: on under half the window,
/// or in runs of at least `BURST_LONG_RUN_MS` at any duty) and the runs repeat no faster
/// than `BURST_MIN_PERIOD_MS` (denser runs are keying), the spectrum, SNR and occupied
/// band are taken over the on-time ([`on_time`]), the symbol-rate searches over each run
/// as received, the label reads those runs ([`layer1_label`]), and `burst` is set;
/// otherwise the whole window is measured and `burst` compares 10 ms chunk powers.
/// `symbol_rate_baud` is, when the envelope moves, on-off keying's run-length
/// unit ([`ook_unit_rate`]: a two-level envelope whose runs are whole chips)
/// or else the cyclostationary line ([`cyclo_symbol_rate`]); otherwise, or when
/// neither finds one, the keying line of [`keying_signal`].
///
/// Order: `order` and `tone_spacing_hz` are the FSK tones of the
/// instantaneous-frequency clusters over the runs
/// ([`order::fsk_clusters`]), else of the two-peak rule on the measured
/// spectrum (order 2); without tones and with a symbol rate, `order` is the
/// PSK order of the m-th power test ([`order::psk_order`]) and
/// `tone_spacing_hz` is `None`. AFSK under FM carries no order: its tones are
/// audio, not RF.
pub fn measure_band_masked(
    iq: &[Complex<f32>],
    rate_hz: u64,
    block_center_hz: u64,
    bandwidth_hz: u64,
    dc: Option<(u64, u64)>,
) -> FeatureRecord {
    measure_band_noted(iq, rate_hz, block_center_hz, bandwidth_hz, dc).0
}

/// [`measure_band_masked`] with the [`RateNote`] it measured and the [`View`] it
/// measured over, which its label reads ([`layer1_label_of`]).
fn measure_band_noted(
    iq: &[Complex<f32>],
    rate_hz: u64,
    block_center_hz: u64,
    bandwidth_hz: u64,
    dc: Option<(u64, u64)>,
) -> (FeatureRecord, RateNote, View<'_>) {
    let v = gated_view(iq, rate_hz, bandwidth_hz);
    if iq.is_empty() || rate_hz == 0 {
        let record = FeatureRecord {
            bandwidth_hz: Hz(0),
            center_hz: Hz(block_center_hz),
            symbol_rate_baud: None,
            snr_db: 0.0,
            burst: false,
            duty: None,
            bursts: None,
            burst_ms: None,
            period_ms: None,
            order: None,
            tone_spacing_hz: None,
            symbol_rate_line: None,
            power99_hz: None,
            audio_rms_hz: None,
            ssb: None,
        };
        let note = RateNote { power99_hz: None };
        return (record, note, v);
    }
    let full = iq;
    let iq = &v.on[..];
    let bin_hz = rate_hz as f64 / SIZE as f64;
    let half_bins = ((bandwidth_hz as f64 / 2.0 / bin_hz).floor() as usize).min(SIZE / 2);
    let first = SIZE / 2 - half_bins;
    let all_db = spectrum(iq);
    let mut db = all_db[first..(SIZE / 2 + half_bins + 1).min(SIZE)].to_vec();
    // The in-band bins within the mask of the DC's offset in the block, when the
    // DC lies inside the band and off its centre (a capture centred on the band
    // holds the measured signal on the DC).
    let masked: Vec<bool> = match dc {
        Some((dc_hz, half_width_hz)) => {
            let dc_off = dc_hz as f64 - block_center_hz as f64;
            let edge = (half_bins as f64 + 0.5) * bin_hz;
            let inside = dc_off.abs() <= edge && dc_off.abs() > bin_hz;
            (0..db.len())
                .map(|i| {
                    let off = ((i + first) as f64 - (SIZE / 2) as f64) * bin_hz;
                    inside && (off - dc_off).abs() <= half_width_hz as f64
                })
                .collect()
        }
        None => vec![false; db.len()],
    };
    let unmasked: Vec<f32> = db
        .iter()
        .zip(&masked)
        .filter(|(_, m)| !**m)
        .map(|(v, _)| *v)
        .collect();
    let floor = if unmasked.is_empty() {
        // The mask covers the whole band: not applied.
        percentile(&db, FLOOR_PERCENTILE)
    } else {
        let floor = percentile(&unmasked, FLOOR_PERCENTILE);
        for (v, m) in db.iter_mut().zip(&masked) {
            if *m {
                *v = floor;
            }
        }
        floor
    };
    let db = &db[..];
    let peak = argmax(db);
    let gap = (GAP_HZ / bin_hz).floor() as usize;
    let (lo, hi) = occupied(db, peak, floor + OCCUPIED_DB, gap);
    let noise = noise_beside(&all_db, first, db, (lo, hi), floor + OCCUPIED_DB, bin_hz);
    let (lo, hi) = (lo + first, hi + first);
    let bins = (hi - lo + 1) as f64;
    let offset = ((lo + hi) as f64 / 2.0 - (SIZE / 2) as f64) * bin_hz;
    let center = (block_center_hz as i64)
        .saturating_add(offset.round() as i64)
        .max(0);
    let bandwidth_hz = (bins * bin_hz).round().max(bin_hz.round()) as u64;
    // On-off keying's chip from its run lengths, unless the veto finds the signal is
    // not keyed on and off ([`ook_veto`]): then the rate and order as for any other.
    let fit = ook_fit(&v, full, rate_hz, (offset, bandwidth_hz))
        .filter(|f| !ook_veto(f, &v, rate_hz, (offset, bandwidth_hz)));
    let rate = match fit {
        Some(f) => Some((f.baud, RateLine::Runs)),
        None => rate_of(&v, rate_hz, (offset, bandwidth_hz)),
    };
    // FSK order and spacing from the instantaneous-frequency clusters, else the
    // two-peak rule on the measured spectrum; PSK order only without tones. Neither
    // for a chip rate read from on-off keying's runs: that fit found two envelope
    // levels and no tones in the band it fitted (and the veto no PSK order), and the
    // measured band of a weak keyed signal is mostly noise, whose instantaneous
    // frequency reads as tones.
    let snr_db = db[peak] - floor;
    let keyed = fit.is_some();
    let fsk = (!keyed)
        .then(|| {
            fsk_clusters(&v.runs, rate_hz, (offset, bandwidth_hz)).or_else(|| {
                let (a, b) = (lo - first, hi - first);
                tone_pair(db, (a, b), floor + OCCUPIED_DB, bin_hz, true)
                    .map(|(a, b)| (2, Hz((b - a).round() as u64)))
            })
        })
        .flatten();
    let psk = match (fsk, rate) {
        (None, Some(_)) if !keyed => psk_order(&v.runs, rate_hz, (offset, bandwidth_hz)),
        _ => None,
    };
    let b = &v.bursts;
    let record = FeatureRecord {
        bandwidth_hz: Hz(bandwidth_hz),
        center_hz: Hz(center as u64),
        symbol_rate_baud: rate.map(|(r, _)| r),
        snr_db,
        burst: v.gated || is_burst(full, rate_hz),
        duty: Some(b.duty),
        bursts: Some(b.runs.len() as u32),
        burst_ms: b.burst_ms,
        period_ms: b.period_ms,
        order: fsk.map(|(o, _)| o).or(psk),
        tone_spacing_hz: fsk.map(|(_, s)| s),
        symbol_rate_line: rate.map(|(_, line)| line),
        power99_hz: None,
        audio_rms_hz: None,
        ssb: None,
    };
    let note = RateNote {
        power99_hz: power_width_hz(db, peak, floor, gap, bin_hz),
    };
    let mut v = v;
    v.band = Some(OccupiedBand {
        offset_hz: offset,
        noise,
    });
    (record, note, v)
}

/// What a measurement found of the occupied band in the samples it measured, for the
/// label read from them ([`View::band`]).
#[derive(Clone, Copy, Debug, PartialEq)]
struct OccupiedBand {
    /// The occupied band's centre, Hz from the stream's.
    offset_hz: f64,
    /// The noise beside it in the samples ([`noise_beside`]).
    noise: Option<Noise>,
}

/// The noise of measured samples, as seen beside their signal ([`noise_beside`]).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Noise {
    /// The signal's power over the noise's in the samples, a ratio.
    snr: f64,
    /// The width the noise would fill at its level beside the signal, Hz: its power over
    /// its density there.
    width_hz: f64,
}

/// The noise in the samples whose `SIZE`-bin spectrum is `all` (dB, `bin_hz` a bin) and the
/// signal's power over it: what the label takes the noise's share of the envelope's motion
/// ([`rice_cv`]) and of the phase's ([`click_hz2`]) from. `band` is the measured band's
/// bins, `all[first..]` with any mask applied, `run` its occupied run in it and `threshold`
/// the level a bin is occupied from.
/// - The noise bins are the band's bins outside the run and under the threshold: noise
///   seen beside the signal. Their mean power stands for every bin of the band, the ones
///   the signal covers too.
/// - The bins outside the band (a channel filter's skirts and stopband) count as they
///   are, as the samples hold them.
/// - The signal is what the spectrum holds beyond that.
///
/// So a line or sideband standing clear of the run is not counted as noise, and a signal's
/// tail under the threshold is: the noise is never understated by the signal's own
/// spectrum. `None` when the band holds no noise bin (a signal filling it: its own
/// spectrum is not a noise floor) or no signal is left.
fn noise_beside(
    all: &[f32],
    first: usize,
    band: &[f32],
    run: (usize, usize),
    threshold: f32,
    bin_hz: f64,
) -> Option<Noise> {
    let power = |db: f32| 10f64.powf(f64::from(db) / 10.0);
    let (sum, n) = band
        .iter()
        .enumerate()
        .filter(|&(k, &db)| (k < run.0 || k > run.1) && db < threshold)
        .fold((0.0f64, 0usize), |(s, n), (_, &db)| (s + power(db), n + 1));
    if n == 0 {
        return None;
    }
    let outside: f64 = all[..first]
        .iter()
        .chain(&all[first + band.len()..])
        .map(|&db| power(db))
        .sum();
    let noise = sum / n as f64 * band.len() as f64 + outside;
    let total: f64 = band.iter().map(|&db| power(db)).sum::<f64>() + outside;
    (sum > 0.0 && total > noise).then(|| Noise {
        snr: (total - noise) / noise,
        width_hz: noise / (sum / n as f64) * bin_hz,
    })
}

/// The complementary error function, to 1.5e-7 (Abramowitz and Stegun 7.1.26), for `x` of 0
/// or more.
fn erfc(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    poly * (-x * x).exp()
}

/// The mean square, Hz^2, that the noise's clicks alone put into a discriminator's audio
/// from `FM_MIN_AUDIO_HZ` to `audio_max_hz`, for a carrier `snr` (a ratio) over flat noise
/// `width_hz` wide (Rice's click model). Under about 10 dB the noise now and then carries
/// the carrier's phasor round the origin: a step of a whole turn, an impulse of one cycle
/// in the frequency. They come `r erfc(sqrt(snr))` times a second, `r = width / sqrt(12)`
/// the band's radius of gyration, so they lay a flat `2 r erfc(sqrt(snr))` Hz^2 per Hz under
/// the audio, which the envelope's motion does not share. Counting every click, it is an
/// upper bound: [`held_steps`] takes out the ones through a deep fade.
fn click_hz2(snr: f64, width_hz: f64, audio_max_hz: f64) -> f64 {
    let rate = width_hz / 12f64.sqrt() * erfc(snr.max(0.0).sqrt());
    2.0 * rate * (audio_max_hz - FM_MIN_AUDIO_HZ).max(0.0)
}

/// The envelope CV of a steady carrier in complex Gaussian noise `snr` (a power ratio)
/// under it: the Rice distribution's. With `K = snr`, the envelope's mean square is
/// `N (1 + K)` and its mean `sqrt(pi N / 4) L(K)`, where
/// `L(K) = exp(-K/2) ((1 + K) I0(K/2) + K I1(K/2))`, so
/// `cv^2 = 4 (1 + K) / (pi L(K)^2) - 1`: 0.523 for noise alone (Rayleigh), 0.32 at 6 dB,
/// `1 / sqrt(2 K)` when the carrier is strong.
fn rice_cv(snr: f64) -> f64 {
    let k = snr.max(0.0);
    if k > 40.0 {
        return (1.0 / (2.0 * k)).sqrt();
    }
    // I0 and I1 at x = K/2 by their power series, which converge for every x.
    let q = k * k / 16.0;
    let (mut i0, mut i1, mut term) = (1.0f64, 1.0f64, 1.0f64);
    for j in 1..200 {
        term *= q / (j * j) as f64;
        i0 += term;
        i1 += term / (j + 1) as f64;
        if term < 1e-17 * i0 {
            break;
        }
    }
    let i1 = i1 * k / 4.0;
    let l = (-k / 2.0).exp() * ((1.0 + k) * i0 + k * i1);
    (4.0 * (1.0 + k) / (std::f64::consts::PI * l * l) - 1.0)
        .max(0.0)
        .sqrt()
}

/// The envelope CV `cv` of samples with the share a steady carrier's noise gives it taken
/// out, in quadrature ([`rice_cv`], at `NOISE_SLACK_DB` over `snr`, so sooner too little
/// than too much): how far the envelope moves beyond what the noise in the samples
/// explains. `cv` itself without an `snr`.
fn cv_over_noise(cv: f32, snr: Option<f64>) -> f32 {
    let noise = snr.map_or(0.0, |snr| rice_cv(snr * 10f64.powf(NOISE_SLACK_DB / 10.0)));
    (f64::from(cv).powi(2) - noise * noise).max(0.0).sqrt() as f32
}

/// On-off keying's chip rate from its run lengths ([`ook_unit_rate`]) of the occupied
/// band `offset_hz ± bandwidth_hz / 2`, when the envelope moves (`cv` at least
/// `FM_MAX_CV`): over the view's rate samples ([`View::runs`]) and, when the view is
/// gated on under `OOK_REFIT_MAX_DUTY` of the window, over the whole window `full` (the
/// gate cuts its runs from the window's power, whole-band: a weak keyed signal's
/// loudest chunks gate as bursts a millisecond long, and the keying's off runs lie
/// between them; a gate that holds more found real bursts).
fn ook_fit(v: &View, full: &[Complex<f32>], rate_hz: u64, band: (f64, u64)) -> Option<OokFit> {
    if v.cv < FM_MAX_CV {
        return None;
    }
    ook_unit_rate(&v.runs, rate_hz, band).or_else(|| {
        (v.gated && v.bursts.duty < OOK_REFIT_MAX_DUTY)
            .then(|| ook_unit_rate(&[full], rate_hz, band))
            .flatten()
    })
}

/// A gated view whose runs hold less than this share of the window is fitted over the
/// whole window too ([`ook_fit`]): the weak HackRF remote's gate held 1 % of its 471 ms
/// window, 1 ms chunks of a keyed train; real bursts (FSK or PSK packets 100 ms apart)
/// held 4-24 %, and refitting them cost a full window per measure.
const OOK_REFIT_MAX_DUTY: f32 = 0.05;

/// Whether a run-length fit is vetoed, its rate and label left to the other rules: the
/// fitted off runs stand more than `OOK_OFF_MAX_OVER_NOISE_DB` over the noise floor (or
/// it cannot be measured), twice the fitted band carries FSK tones, or the measured band
/// carries a PSK order ([`psk_order`]: OOK's carrier line rules one out).
fn ook_veto(fit: &OokFit, v: &View, rate_hz: u64, band: (f64, u64)) -> bool {
    fit.off_over_noise_db
        .is_none_or(|db| db > OOK_OFF_MAX_OVER_NOISE_DB)
        || fit.wide_tones
        || psk_order(&v.runs, rate_hz, band).is_some()
}

/// On-off keying's off level is the noise: the fitted off runs stand at most this many
/// dB over the band's noise floor. Measured: the real remotes' 1.1-5.3 dB, a PSK burst's
/// envelope dips, which are signal, 15 dB at 8 dB SNR and 28-29 dB at 30 dB. The envelope
/// CV inside the on runs did not tell them apart (0.15-0.21 on both).
const OOK_OFF_MAX_OVER_NOISE_DB: f64 = 8.0;

/// The symbol rate of the view's rate samples ([`View::runs`]: each run as received
/// when gated, else the whole window) and the line it came from, when no run-length fit
/// stands ([`ook_fit`]): when the envelope moves (`cv` at least `FM_MAX_CV`)
/// [`cyclo_symbol_rate`] of the occupied band `offset_hz ± bandwidth_hz / 2` (from the
/// block centre, band-passed per run), up to its width (`cyclo`); else, or when it finds
/// none, the [`symbol_rate`] keying line (`keying`).
fn rate_of(
    v: &View,
    rate_hz: u64,
    (offset_hz, bandwidth_hz): (f64, u64),
) -> Option<(Baud, RateLine)> {
    let bw = bandwidth_hz as f64;
    let cyclo = || {
        let runs: Vec<Vec<Complex<f32>>> = v
            .runs
            .iter()
            .map(|r| bandpass(r, rate_hz, offset_hz, bw))
            .collect();
        cyclo_symbol_rate(&runs, rate_hz, bw)
    };
    let moves = v.cv >= FM_MAX_CV;
    moves
        .then(cyclo)
        .flatten()
        .map(|r| (r, RateLine::Cyclo))
        .or_else(|| {
            symbol_rate(&v.runs, rate_hz, v.cv, bandwidth_hz).map(|r| (r, RateLine::Keying))
        })
}

/// What layer 1 measured of a record beyond its fields, for the extras `classify`
/// answers ([`carry_extras`]).
#[derive(Clone, Copy, Debug, PartialEq)]
struct RateNote {
    /// The width holding [`POWER_SHARE`] of the excess power (over the noise floor) of the
    /// band around the peak: see [`power_width_hz`]. `None` for an empty block.
    pub power99_hz: Option<u64>,
}

/// Frequencies of the two tones when `db[lo..=hi]` holds two tone peaks: local
/// maxima at or above `threshold`, the strongest two within `FSK_PEAK_DB` of
/// each other and separated by `FSK_SEP_HZ`; with `dominant`, every other peak
/// must also be at least `FSK_PEAK_DB` below the weaker one.
fn tone_pair(
    db: &[f32],
    (lo, hi): (usize, usize),
    threshold: f32,
    bin_hz: f64,
    dominant: bool,
) -> Option<(f64, f64)> {
    let mut peaks: Vec<(usize, f32)> = (lo.max(1)..=hi.min(db.len().saturating_sub(2)))
        .filter(|&i| db[i] >= threshold && db[i] > db[i - 1] && db[i] >= db[i + 1])
        .map(|i| (i, db[i]))
        .collect();
    peaks.sort_by(|a, b| b.1.total_cmp(&a.1));
    let (&(i1, p1), &(i2, p2)) = (peaks.first()?, peaks.get(1)?);
    let sep = i1.abs_diff(i2) as f64 * bin_hz;
    let others_low = !dominant || peaks.get(2).is_none_or(|&(_, p3)| p3 <= p2 - FSK_PEAK_DB);
    (p1 - p2 <= FSK_PEAK_DB && (FSK_SEP_HZ.0..=FSK_SEP_HZ.1).contains(&sep) && others_low)
        .then_some((i1.min(i2) as f64 * bin_hz, i1.max(i2) as f64 * bin_hz))
}

/// Conservative layer-1 label for a measured block, read from the view its record was
/// measured over (the on-time as received when the window is gated, else the whole
/// window; [`measure_band_masked`]), so the label and the record describe the same
/// samples. The view's samples ("the samples" below) give every spectrum, carrier line,
/// discriminator audio and envelope level. A key-up, a key-down or a drop-out is gated to
/// the carrier where it is up, so the key edge is never read as an envelope level.
///
/// "The CV" the rules read is the samples' envelope CV with the noise's share taken out
/// ([`cv_over_noise`]): the measurement finds the signal's power over the noise's in the
/// samples ([`noise_beside`]), a steady carrier in that noise has a known envelope CV
/// ([`rice_cv`]: 0.32 at 6 dB, 0.17 at 12), and what the envelope moves beyond it, in
/// quadrature, is the signal's own. So a weak FM over or bare carrier has a constant
/// envelope, as a strong one has, and weak AM still moves it. With no noise seen beside
/// the signal (its occupied run fills the band) the CV is the samples' as it is.
///
/// Rules, first match wins:
///
/// 1. `"noise"` (0.5) when `snr_db < 6`.
/// 2. `"ook"` (0.6) when the record's symbol rate is on-off keying's chip from its run
///    lengths (`symbol_rate_line` `runs`: two envelope levels, the lower off, no FSK
///    tones, runs of whole chips and a flat on level in the band it was fitted in, and
///    no PSK order; [`measure_band_masked`]) and it carries no order.
/// 3. `"fsk"` (0.6) when the record carries an FSK `order` and
///    `tone_spacing_hz` ([`measure_band_masked`]), or the samples' power spectrum has
///    exactly two dominant peaks inside its occupied band 500–3000 Hz apart and within
///    6 dB of each other, or, for a constant envelope (the CV below 0.2) with a keying
///    line, when the samples' discriminator audio does (AFSK on FM).
///
/// A signal weaker than the noise in its samples has the noise's envelope
/// (`ENVELOPE_MIN_SNR`): rules 4 and 7 to 10 do not read it, and what rules 5 and 6 leave is
/// `"unknown"`.
///
/// 4. `"ook"` (0.6) when the CV is at least 0.25, the record has a symbol rate and no
///    PSK order, and inside the samples the envelope sits at two levels, the lower one
///    off ([`order::two_levels`]): over the view's runs when gated, else over the
///    window's burst runs ([`order::envelope_runs`]: the gaps between bursts are not an
///    off level), whose envelope CV is still at least 0.25. A window gated into one or
///    two long runs of a modulated carrier holds one level, so it is never `ook`: tested
///    on keyed FM and keyed AM (m 1) overs.
/// 5. `"wfm"` (0.9) when the occupied band is 40–250 kHz and
///    [`pilot_line_db`] of the samples finds a 19 kHz pilot at least 10 dB above its
///    region, with no envelope or deviation gate (multipath and noise ripple
///    a real station's envelope; the pilot is the stronger evidence).
///    Otherwise `"wfm"` (0.6) when the occupied band is 60–250 kHz, the CV is below 0.45
///    and the discriminator audio deviates by more than 5 kHz RMS.
/// 6. `"psk"` (0.6) when the record carries a PSK `order` (no tone spacing)
///    and a symbol rate. It precedes `am`: the carrier-line test of `am`
///    reads a narrow PSK main lobe as a carrier (15.9 dB on 2400 Bd BPSK),
///    while the PSK order requires no line in `x` itself.
/// 7. `"am"` (0.6) when a carrier line sits at the centre of the samples' spectrum (the
///    strongest of the centre bin and its two neighbours at least 6 dB above
///    the mean power of the bins 1–5 kHz either side) and the CV is at least 0.25. But
///    when the discriminator audio moves `WEAK_FM_MOTION` times as far as the envelope
///    and 40 Hz beyond it ([`motion_hz`]), the moving envelope is a fading
///    angle-modulated carrier's, its audio in the phase: `"unknown"`, not AM. (An FM over
///    fading as a Rayleigh path at 30 Hz of Doppler has a CV of 0.3 to 0.4 beyond the
///    noise's where it is up.)
/// 8. `"ssb"` (0.6) when there is no such centre line, the CV is at least 0.25, the
///    occupied band is 1.5–4 kHz and at least 80 % of the samples' occupied power lies
///    on one side of centre; the evidence names `usb` (above) or `lsb` (below) and the
///    dial frequency [`ssb_dial_hz`] gives.
/// 9. `"am"` (0.6) for low-index AM: the same centre line, a CV of at least 0.1 and an
///    envelope CV of at least 0.1 within half the occupied band (so wideband noise on a
///    bare carrier does not pass), and the envelope's motion at least `MOTION_MARGIN`
///    (1.25) times the discriminator audio's RMS deviation measured alike, both in Hz
///    ([`motion_hz`]), so the envelope moves more than the phase whatever the rate.
/// 10. `"fm"` (0.6) when the CV is below 0.2, the band is at most 25 kHz and the
///     discriminator audio of the occupied band, drift taken out, deviates beyond the
///     envelope's motion in the same band ([`Motion::over_hz`]: noise moves both alike,
///     AM only the envelope) by more than a floor: 40 Hz RMS, raised in quadrature by
///     `CLICK_MARGIN` times what the noise's clicks could leave there ([`click_hz2`]:
///     nothing from about 10 dB up), and, where the band the phase is read from holds the
///     carrier under 10 dB over its noise, the phase moves at least `WEAK_FM_MOTION` (1.6)
///     times as far as the envelope. `"carrier"` (0.7) under the same gate when the
///     deviation beyond the envelope's motion is under the 40 Hz: a keyed carrier with no
///     modulation where it is up, or one that drifts, reads `carrier`, its keying in
///     `duty`, `bursts` and `burst_ms`. Deviation over the 40 Hz that does not stand clear
///     of the noise is not decided: `"unknown"`.
/// 11. Otherwise `"unknown"` (0.5).
///
/// What that decides on generators (`fixtures::weak_cases`, 2 s, three noise seeds, the
/// SNR inside a 12.5 or 25 kHz channel):
/// - FM of voice-like audio (rule 10) and of the keyed-over generator's speech (`fm` or,
///   by rule 3, `fsk`) reads so from 3 dB, whole or keyed; at 0 dB `unknown`;
/// - a bare carrier reads `carrier` from 3 dB, at 0 dB `carrier` or `unknown`;
/// - AM reads `am` from 6 dB (a tone at index 0.2 or 0.5, speech at 0.9, over flat noise
///   and over noise sloping 6 dB across the channel), at index 0.5 from 1 dB; under 6 dB
///   the others read `am`, `carrier` or `unknown`, and none reads `fm` down to 1 dB.
///
/// The evidence quotes the samples' envelope CV once, what of it is beyond the noise's at
/// the SNR found, and how much was read: `over <on> s of <window> s`.
/// This builds the view [`measure`] takes of the whole block `iq`; `classify` labels
/// with the view its own measurement took ([`measure_selection`]).
pub fn layer1_label(f: &FeatureRecord, iq: &[Complex<f32>], rate_hz: u64) -> Label {
    let mut v = gated_view(iq, rate_hz, rate_hz);
    if !iq.is_empty() && rate_hz > 0 {
        // The occupied band as `measure` finds it over the whole block.
        let db = spectrum(&v.on);
        let floor = percentile(&db, FLOOR_PERCENTILE);
        let bin_hz = rate_hz as f64 / SIZE as f64;
        let gap = (GAP_HZ / bin_hz).floor() as usize;
        let (lo, hi) = occupied(&db, argmax(&db), floor + OCCUPIED_DB, gap);
        v.band = Some(OccupiedBand {
            offset_hz: ((lo + hi) as f64 / 2.0 - (SIZE / 2) as f64) * bin_hz,
            noise: noise_beside(&db, 0, &db, (lo, hi), floor + OCCUPIED_DB, bin_hz),
        });
    }
    layer1_label_of(f, &v, iq.len(), rate_hz)
}

/// [`layer1_label`] of `f` read from `v`, the view of a window of `window` samples that
/// [`measure_band_noted`] measured `f` over.
fn layer1_label_of(f: &FeatureRecord, v: &View, window: usize, rate_hz: u64) -> Label {
    let (name, confidence, reason) = classify_block(f, v, rate_hz);
    let baud = f.symbol_rate_baud.map_or_else(
        || "none".to_string(),
        |b| match f.symbol_rate_line {
            Some(line) => format!("{} Bd ({})", b.0, line.name()),
            None => format!("{} Bd", b.0),
        },
    );
    let duty = f
        .duty
        .map_or_else(|| "none".to_string(), |d| format!("{d:.3}"));
    let secs = |n: usize| n as f64 / rate_hz.max(1) as f64;
    Label {
        layer: 1,
        name: name.into(),
        confidence,
        evidence: format!(
            "{reason}; snr_db {:.1} dB, bandwidth_hz {} Hz, symbol_rate_baud {baud}, duty {duty}, cv {:.3} (envelope std/mean{}) over {:.2} s of {:.2} s",
            f.snr_db,
            f.bandwidth_hz.0,
            v.cv,
            match v.band.and_then(|b| b.noise) {
                Some(Noise { snr, .. }) => format!(
                    ", {:.3} beyond the noise's at {:.1} dB",
                    cv_over_noise(v.cv, Some(snr)),
                    10.0 * snr.log10()
                ),
                None => String::new(),
            },
            secs(v.received.len()),
            secs(window)
        ),
    }
}

/// RMS frequency deviation, in Hz, of the discriminator audio of `iq`, from
/// `FM_MIN_AUDIO_HZ` to half the occupied band, with its linear trend removed: the
/// mean is the carrier's offset from centre and the slope its drift (a transmitter
/// warming up, a free-running oscillator, a chirping key-down), neither of them
/// modulation, and what drift is left bends slower than `FM_MIN_AUDIO_HZ`. A phase
/// step counts as no deviation where either sample's envelope is under
/// `DISC_MIN_ENVELOPE` of the median envelope (there the phase is the noise's: a key
/// edge, a channel filter's first samples, and a few such steps of up to pi radians
/// read a bare carrier as FM), and at each of `joins`, the samples where `iq` joins one
/// run to the next ([`view_joins`]).
fn audio_rms_hz(iq: &[Complex<f32>], rate_hz: u64, bandwidth_hz: u64, joins: &[usize]) -> f64 {
    let held = held_steps(iq, joins);
    steps_rms_hz(discriminator(iq), &held, rate_hz, bandwidth_hz)
}

/// [`motion_hz`] reads the phase and the envelope of the occupied band's samples: of a band
/// this many times the occupied width about its centre. The channel's noise beside the
/// signal is then not in the phase it reads: under about 10 dB of it a discriminator's
/// output is the noise's (its clicks, each a whole turn, put a flat floor under the audio
/// that the envelope does not share), and a bare carrier 2.8 dB over the noise of a 25 kHz
/// channel (the keyed-over generator at 0 dB over its whole 48 kS/s) read 73 Hz over the
/// envelope. A band twice the occupied one keeps the sidebands
/// under the occupied threshold either side.
const MOTION_BAND: f64 = 2.0;
/// The steps within this many periods of [`MOTION_BAND`]'s width of a cut in the samples
/// (the window's ends, a join of two runs) count as no motion: the band's edge rings there.
const MOTION_RING: f64 = 4.0;

/// The rules that read the envelope (4 and 7 to 10) need the signal at least this many
/// times the noise in its samples, in power. As the signal sinks under the noise the
/// envelope's CV goes to the noise's 0.523 whatever the signal is, and what it holds beyond
/// the noise's share shrinks by about `snr / (1 + snr)` in variance: at 0 dB half is left,
/// and under it "no more than the noise explains" says nothing about the signal.
const ENVELOPE_MIN_SNR: f64 = 1.0;
/// Rule 9 takes the envelope to move more than the phase only when it moves at least this
/// many times as far, in Hz. Noise alone moves both alike, and the ratio of the two as
/// measured spreads by `1 / sqrt(2 W T)` of itself over a band `W` and a time `T`: 4 % over
/// the 1 kHz and 0.3 s of a narrow, short read, so 1.25 is six of those spreads. On
/// generators 3 to 12 dB over the noise of a 12.5 or 25 kHz channel
/// (`fixtures::weak_cases`), the envelope's motion over the phase's:
/// - AM of a tone at index 0.2: 1.37 to 4.5; at 0.5 and 3 dB: 1.85 to 3.0;
/// - AM of speech at 0.9: 1.41 to 3.65, but 1.0 to 1.07 at 3 dB in 12.5 kHz, where it is
///   not told from a carrier;
/// - a bare carrier: 0.76 to 1.02 (the phase a little ahead); FM speech: 0.24 to 0.42.
const MOTION_MARGIN: f64 = 1.25;
/// The noise's share of the envelope CV is taken at an SNR this many dB over the one found
/// ([`cv_over_noise`]), so the share errs toward too little: [`noise_beside`] reads noise
/// that is not there (a signal's tail under the threshold) sooner than it misses some, and
/// a floor that is not flat (a neighbour's skirt) is not the same under the carrier as
/// beside it. Taking too much erases weak AM (an index of 0.2 is a CV of 0.14 over a noise
/// share of 0.4 at 3 dB), taking too little leaves a constant envelope some CV:
/// - at 1 dB a steady carrier is left 0.14 at 3 dB, 0.13 at 6 and 0.08 at 12, under rule
///   10's 0.2 with the SNR read another 0.5 dB over, and an index of 0.2 keeps rule 9's 0.1
///   from 3 dB with the SNR read 1 dB under (`rice_cv`'s unit test);
/// - on generators (`fixtures::weak_cases`): at 0 dB of slack a tone at index 0.2 over
///   noise sloping 6 dB across the channel reads `carrier` 3 times of 6 at 3 dB, at 1 dB
///   none; at 2 dB FM speech at 3 dB reads `unknown` 4 times of 36.
const NOISE_SLACK_DB: f64 = 1.0;
/// The FM threshold, as a ratio of the carrier to the noise in the band its phase is read
/// from (10 dB): from here up the noise's clicks are a few a minute or none.
const FM_THRESHOLD_SNR: f64 = 10.0;
/// Under the FM threshold rule 10 reads `fm` only when the phase moves at least this many
/// times as far as the envelope, in Hz. There the envelope is no longer a faithful witness
/// of the noise: AM whose troughs dip into the noise clicks at them, more than a steady
/// carrier's model says. On generators 1 to 12 dB over the channel's noise
/// (`fixtures::weak_cases`), the phase's motion over the envelope's where rule 10 reads
/// them: AM (a tone, speech, flat and sloping noise) at most 1.34 and a bare carrier at
/// most 1.3; FM of voice-like audio at least 2.6 and of the keyed-over generator's speech
/// at least 2.4, but for 1.98 on an over with a drop-out at 3 dB in a channel twice its
/// width. 1.6 lies midway between 1.34 and 1.98.
const WEAK_FM_MOTION: f64 = 1.6;
/// Rule 10's deviation floor rises by this many times the mean square the noise's clicks
/// could leave over the envelope ([`click_hz2`]): deviation counts when it is at least
/// twice that, in Hz. On generators (`fixtures::weak_cases`):
/// - FM of voice-like audio at 3 dB reads 3.6 to 38 times the clicks' RMS over the
///   envelope (213 to 1134 Hz against 9 to 315), so it clears a margin of up to 9, and at
///   16 one case of 18 is lost;
/// - a bare carrier reads 0 to 2 Hz at every level, the clicks' share nothing in the
///   narrow band its phase is read from;
/// - weak AM is kept out of `fm` by `WEAK_FM_MOTION`, at a margin of 0, 1 or 4 alike;
///   without that rule a margin of 4 lets one case read `fm` (speech at index 0.9 over
///   sloping noise at 1 dB, 2.4 times the clicks' RMS) and 9 lets none.
///
/// No generator case distinguishes a margin from 0 to 4 once `WEAK_FM_MOTION` is in
/// force. It is kept as the click model's bound: the motion rule compares the phase with
/// the envelope, and clicks are the one part of the noise's phase the envelope does not
/// show, so without the bound a carrier whose noise clicks under a still envelope has
/// nothing against it but the 40 Hz.
const CLICK_MARGIN: f64 = 4.0;

/// How much the phase and the envelope of the samples move, both in Hz ([`motion_hz`]).
#[derive(Clone, Copy)]
struct Motion {
    /// [`audio_rms_hz`]: the discriminator audio's RMS deviation, Hz.
    phase_hz: f64,
    /// The same measure of the log envelope's steps, Hz.
    envelope_hz: f64,
}

impl Motion {
    /// The phase's deviation beyond the envelope's, in quadrature: what modulates the
    /// frequency, since additive noise moves both alike.
    fn over_hz(&self) -> f64 {
        (self.phase_hz.powi(2) - self.envelope_hz.powi(2))
            .max(0.0)
            .sqrt()
    }
}

/// [`audio_rms_hz`] of `iq` and the same measure of its envelope's motion: the steps of
/// the log envelope, `ln |x[n+1]| - ln |x[n]|`, read as if they were phase steps, held,
/// detrended and band-limited alike. Additive noise on a carrier moves its log envelope
/// and its phase alike, AM moves only the envelope and FM only the phase. Both are in
/// Hz, so comparing them does not depend on the rate.
///
/// With `offset_hz`, the occupied band's centre from the stream's, both are read from the
/// samples band-passed to `MOTION_BAND` times `bandwidth_hz` about it (when that is
/// narrower than the stream), and the steps the band's edge rings in count as none
/// (`MOTION_RING`).
fn motion_hz(
    iq: &[Complex<f32>],
    rate_hz: u64,
    bandwidth_hz: u64,
    joins: &[usize],
    offset_hz: Option<f64>,
) -> Motion {
    // The pre-detection band: the occupied band and half as much again either side.
    let width = MOTION_BAND * bandwidth_hz as f64;
    let narrowed = offset_hz
        .filter(|_| width < rate_hz as f64)
        .map(|off| bandpass(iq, rate_hz, off, width));
    let mut held = held_steps(narrowed.as_deref().unwrap_or(iq), joins);
    if let Some(x) = &narrowed {
        // The band's edge rings for a few of its periods about every cut in the samples:
        // the window's ends (the transform is circular) and each join.
        let ring = (MOTION_RING * rate_hz as f64 / width).ceil() as usize;
        let n = held.len();
        for &j in [0, x.len()].iter().chain(joins) {
            for h in &mut held[j.saturating_sub(ring).min(n)..(j + ring).min(n)] {
                *h = true;
            }
        }
    }
    let iq = narrowed.as_deref().unwrap_or(iq);
    let phase_hz = steps_rms_hz(discriminator(iq), &held, rate_hz, bandwidth_hz);
    let log_env: Vec<f32> = iq
        .windows(2)
        .zip(&held)
        .map(|(w, &h)| {
            let (a, b) = (w[0].norm(), w[1].norm());
            if h || a <= 0.0 || b <= 0.0 {
                0.0
            } else {
                (b / a).ln()
            }
        })
        .collect();
    Motion {
        phase_hz,
        envelope_hz: steps_rms_hz(log_env, &held, rate_hz, bandwidth_hz),
    }
}

/// The phase steps of `iq` that count as no deviation ([`audio_rms_hz`]): where either
/// sample's envelope is under `DISC_MIN_ENVELOPE` of the median envelope, and the step
/// into each of `joins`.
fn held_steps(iq: &[Complex<f32>], joins: &[usize]) -> Vec<bool> {
    let env: Vec<f32> = iq.iter().map(|s| s.norm()).collect();
    let low = DISC_MIN_ENVELOPE * median(&env);
    let mut held: Vec<bool> = env.windows(2).map(|w| w[0].min(w[1]) < low).collect();
    for &j in joins {
        if let Some(h) = j.checked_sub(1).and_then(|i| held.get_mut(i)) {
            *h = true;
        }
    }
    held
}

/// RMS, in Hz, of per-sample steps `d` (radians per sample) with the `held` ones zeroed
/// and the least-squares line through the others taken out, from `FM_MIN_AUDIO_HZ` to
/// half of `bandwidth_hz` ([`band_analytic`]).
fn steps_rms_hz(mut d: Vec<f32>, held: &[bool], rate_hz: u64, bandwidth_hz: u64) -> f64 {
    let (mut n, mut st, mut sv, mut stt, mut stv) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
    for (i, (&v, _)) in d.iter().zip(held).enumerate().filter(|(_, (_, h))| !**h) {
        let (t, v) = (i as f64, v as f64);
        (n, st, sv, stt, stv) = (n + 1.0, st + t, sv + v, stt + t * t, stv + t * v);
    }
    let den = n * stt - st * st;
    let slope = if den > 0.0 {
        (n * stv - st * sv) / den
    } else {
        0.0
    };
    let mean = if n > 0.0 { (sv - slope * st) / n } else { 0.0 };
    for (i, (v, &h)) in d.iter_mut().zip(held).enumerate() {
        *v = if h {
            0.0
        } else {
            (*v as f64 - mean - slope * i as f64) as f32
        };
    }
    let audio = band_analytic(&d, rate_hz, FM_MIN_AUDIO_HZ, bandwidth_hz as f64 / 2.0);
    if audio.is_empty() {
        return 0.0;
    }
    let ms = audio.iter().map(|a| (a.re as f64).powi(2)).sum::<f64>() / audio.len() as f64;
    ms.sqrt() * rate_hz as f64 / (2.0 * std::f64::consts::PI)
}

/// Envelope coefficient of variation band-limited like [`audio_rms_hz`]:
/// the RMS of the envelope's variation within half the occupied band over
/// its mean. Wideband noise on a bare carrier raises [`envelope_cv`] but not
/// this; an AM tone inside the band raises both.
fn envelope_band_cv(iq: &[Complex<f32>], rate_hz: u64, bandwidth_hz: u64) -> f32 {
    let mut e: Vec<f32> = iq.iter().map(|s| s.norm()).collect();
    let mean = e.iter().map(|&v| v as f64).sum::<f64>() / e.len().max(1) as f64;
    if mean <= 0.0 {
        return 0.0;
    }
    e.iter_mut().for_each(|v| *v -= mean as f32);
    let audio = analytic(&e, rate_hz, bandwidth_hz as f64 / 2.0);
    let ms = audio.iter().map(|a| (a.re as f64).powi(2)).sum::<f64>() / audio.len().max(1) as f64;
    (ms.sqrt() / mean) as f32
}

/// Strength, in dB, of a 19 kHz broadcast stereo pilot in the discriminator
/// audio of `iq`: the largest bin of the audio's `SIZE`-bin spectrum within
/// `PILOT_HZ ± PILOT_TOL_HZ` minus the median of the bins from 15 to 23 kHz
/// outside that window. `None` when `rate_hz` is below
/// `2 × (PILOT_HZ + PILOT_TOL_HZ)` (the window is not below Nyquist) or the
/// block is shorter than `SIZE + 1` samples.
pub fn pilot_line_db(iq: &[Complex<f32>], rate_hz: u64) -> Option<f32> {
    if (rate_hz as f64) < 2.0 * (PILOT_HZ + PILOT_TOL_HZ) {
        return None;
    }
    let db = real_spectrum(&discriminator(iq))?;
    let bin_hz = rate_hz as f64 / SIZE as f64;
    let last = db.len() - 1;
    let bin = |hz: f64| ((hz / bin_hz).round() as usize).min(last);
    let (p_lo, p_hi) = (
        ((PILOT_HZ - PILOT_TOL_HZ) / bin_hz).ceil() as usize,
        (((PILOT_HZ + PILOT_TOL_HZ) / bin_hz).floor() as usize).min(last),
    );
    // A window narrower than one bin still holds the bin nearest the pilot.
    let (p_lo, p_hi) = if p_lo > p_hi {
        (bin(PILOT_HZ), bin(PILOT_HZ))
    } else {
        (p_lo, p_hi)
    };
    let line = db[p_lo..=p_hi]
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    let region: Vec<f32> = (bin(PILOT_REGION_HZ.0)..=bin(PILOT_REGION_HZ.1))
        .filter(|k| !(p_lo..=p_hi).contains(k))
        .map(|k| db[k])
        .collect();
    if region.is_empty() {
        return None;
    }
    Some(line - median(&region))
}

/// dB by which the strongest of the centre bin of `db` (a `SIZE`-bin
/// fftshifted spectrum) and its two neighbours exceeds the mean power of the
/// bins `AM_SIDEBAND_HZ` from the centre, either side. AM detection
/// therefore assumes the carrier sits at the block centre.
fn centre_line_db(db: &[f32], bin_hz: f64) -> f32 {
    let c = SIZE / 2;
    let centre = db[c - 1].max(db[c]).max(db[c + 1]);
    let lo = ((AM_SIDEBAND_HZ.0 / bin_hz).ceil() as usize).max(2);
    let hi = ((AM_SIDEBAND_HZ.1 / bin_hz).floor() as usize).min(c - 1);
    if lo > hi {
        return 0.0;
    }
    let (sum, n) = (lo..=hi)
        .flat_map(|k| [db[c - k], db[c + k]])
        .fold((0.0f64, 0usize), |(s, n), v| {
            (s + 10f64.powf(v as f64 / 10.0), n + 1)
        });
    centre - (10.0 * (sum / n as f64).log10()) as f32
}

/// Offset, in Hz, of an SSB voice band's centre from its suppressed carrier
/// (the dial frequency).
const SSB_DIAL_OFFSET_HZ: u64 = 1_500;

/// The dial frequency of an SSB signal whose occupied band is centred at
/// `center_hz`: [`SSB_DIAL_OFFSET_HZ`] below it for `usb`, above it for lsb.
pub fn ssb_dial_hz(center_hz: u64, usb: bool) -> u64 {
    if usb {
        center_hz.saturating_sub(SSB_DIAL_OFFSET_HZ)
    } else {
        center_hz.saturating_add(SSB_DIAL_OFFSET_HZ)
    }
}

/// The side (`usb` or `lsb`) holding at least `SSB_ONE_SIDE` of the occupied power
/// `lo..=hi` of the spectrum `db`, with that share, when `f`'s band is an SSB voice
/// band's width.
fn ssb_side(
    f: &FeatureRecord,
    db: &[f32],
    (lo, hi): (usize, usize),
) -> Option<(&'static str, f64)> {
    if !(SSB_BW_HZ.0..=SSB_BW_HZ.1).contains(&f.bandwidth_hz.0) {
        return None;
    }
    let power = |k: usize| 10f64.powf(db[k] as f64 / 10.0);
    let below: f64 = (lo..=hi).filter(|&k| k < SIZE / 2).map(power).sum();
    let above: f64 = (lo..=hi).filter(|&k| k > SIZE / 2).map(power).sum();
    let total: f64 = (lo..=hi).map(power).sum();
    [("usb", above), ("lsb", below)]
        .into_iter()
        .find(|&(_, p)| total > 0.0 && p / total >= SSB_ONE_SIDE)
        .map(|(side, p)| (side, p / total))
}

/// The samples of `v.received` where one run is joined to the next: the start of each
/// run after the first when gated, none for a whole window.
fn view_joins(v: &View) -> Vec<usize> {
    if !v.gated {
        return Vec::new();
    }
    v.runs
        .iter()
        .scan(0, |at, r| {
            *at += r.len();
            Some(*at)
        })
        .take(v.runs.len().saturating_sub(1))
        .collect()
}

/// Fill `f`'s measured extras (the 99 % power width from `note`, the discriminator
/// audio's RMS deviation, the SSB side and dial) for the record `classify` answers,
/// over the samples of the view `v` its label read. The label's rules are untouched.
fn carry_extras(f: &mut FeatureRecord, note: &RateNote, label: &Label, v: &View, rate_hz: u64) {
    f.power99_hz = note.power99_hz.map(Hz);
    let iq = &v.received[..];
    if iq.is_empty() || rate_hz == 0 {
        return;
    }
    f.audio_rms_hz = Some(Hz(
        audio_rms_hz(iq, rate_hz, f.bandwidth_hz.0, &view_joins(v)).round() as u64,
    ));
    if label.name == "ssb" {
        let db = spectrum(iq);
        let floor = median(&db);
        let gap = (GAP_HZ / (rate_hz as f64 / SIZE as f64)).floor() as usize;
        let (lo, hi) = occupied(&db, argmax(&db), floor + OCCUPIED_DB, gap);
        f.ssb = ssb_side(f, &db, (lo, hi)).map(|(side, _)| SsbFeature {
            sideband: if side == "usb" {
                Sideband::Usb
            } else {
                Sideband::Lsb
            },
            dial_hz: Hz(ssb_dial_hz(f.center_hz.0, side == "usb")),
        });
    }
}

/// [`layer1_label`]'s rules over `f` and the view `v` it was measured over (its samples
/// as received and its CV); `runs` when `f`'s symbol rate is on-off keying's chip from
/// its run lengths ([`RateLine::Runs`]) by `f`'s line.
fn classify_block(f: &FeatureRecord, v: &View, rate_hz: u64) -> (&'static str, f32, String) {
    let iq = &v.received[..];
    // The CV the rules read: the envelope's motion beyond the noise's share of it.
    let noise = v.band.and_then(|b| b.noise);
    let cv = cv_over_noise(v.cv, noise.map(|n| n.snr));
    let joins = view_joins(v);
    let runs = f.symbol_rate_line == Some(RateLine::Runs);
    if f.snr_db < NOISE_SNR_DB || iq.is_empty() || rate_hz == 0 {
        return ("noise", 0.5, format!("snr below {NOISE_SNR_DB} dB"));
    }
    if runs && f.order.is_none() && f.symbol_rate_baud.is_some() {
        return (
            "ook",
            0.6,
            "two envelope levels whose on and off runs are whole chips".into(),
        );
    }
    let bin_hz = rate_hz as f64 / SIZE as f64;
    let db = spectrum(iq);
    let floor = median(&db);
    let gap = (GAP_HZ / bin_hz).floor() as usize;
    let (lo, hi) = occupied(&db, argmax(&db), floor + OCCUPIED_DB, gap);
    if let (Some(order), Some(Hz(spacing))) = (f.order, f.tone_spacing_hz) {
        return ("fsk", 0.6, format!("{order} rf tones {spacing} Hz apart"));
    }
    if let Some((a, b)) = tone_pair(&db, (lo, hi), floor + OCCUPIED_DB, bin_hz, true) {
        return ("fsk", 0.6, format!("two rf tones {:.0} Hz apart", b - a));
    }
    if cv < FM_MAX_CV {
        // AFSK under FM: two audio tones below the Carson limit (half the
        // occupied band) and a keying line.
        let audio = real_spectrum(&discriminator(iq)).filter(|_| f.symbol_rate_baud.is_some());
        if let Some(audio) = audio {
            let lo = (MIN_LINE_HZ / bin_hz).ceil() as usize;
            let hi = ((f.bandwidth_hz.0 as f64 / 2.0 / bin_hz) as usize).min(audio.len() - 1);
            if lo < hi {
                let floor = median(&audio[lo..=hi]);
                if let Some((a, b)) =
                    tone_pair(&audio, (lo, hi), floor + OCCUPIED_DB, bin_hz, false)
                {
                    return (
                        "fsk",
                        0.6,
                        format!("audio tones {a:.0} Hz and {b:.0} Hz under fm with a keying line"),
                    );
                }
            }
        }
    }
    // Under the noise in its samples a signal has the noise's envelope.
    let faint = noise.is_some_and(|n| n.snr < ENVELOPE_MIN_SNR);
    // Two levels inside the samples: a gated view's runs are its samples, and its CV
    // theirs; a whole window's are its burst runs.
    let keyed_runs = || {
        if v.gated {
            return two_levels(&v.runs);
        }
        let runs = envelope_runs(iq, rate_hz);
        let on: Vec<Complex<f32>> = runs.iter().flat_map(|r| r.iter()).copied().collect();
        envelope_cv(&on) >= AM_MIN_CV && two_levels(&runs)
    };
    if !faint
        && cv >= AM_MIN_CV
        && f.order.is_none()
        && f.symbol_rate_baud.is_some()
        && keyed_runs()
    {
        return (
            "ook",
            0.6,
            "two envelope levels with a symbol-rate line".into(),
        );
    }
    let bw = f.bandwidth_hz.0;
    if (WFM_PILOT_MIN_BW_HZ..=WFM_MAX_BW_HZ).contains(&bw)
        && let Some(d) = pilot_line_db(iq, rate_hz).filter(|&d| d >= LINE_DB)
    {
        return (
            "wfm",
            0.9,
            format!("19 kHz pilot +{d:.1} dB, wideband {bw} Hz"),
        );
    }
    if (WFM_MIN_BW_HZ..=WFM_MAX_BW_HZ).contains(&bw) && cv < WFM_MAX_CV {
        let dev = audio_rms_hz(iq, rate_hz, bw, &joins);
        if dev > WFM_MIN_DEV_HZ {
            return (
                "wfm",
                0.6,
                format!("wideband constant envelope, rms deviation {dev:.0} Hz"),
            );
        }
    }
    if let (Some(m), None, Some(_)) = (f.order, f.tone_spacing_hz, f.symbol_rate_baud) {
        return (
            "psk",
            0.6,
            format!("a line at {m}\u{d7} the centre in x^{m}, none in x"),
        );
    }
    if faint {
        return (
            "unknown",
            0.5,
            "no layer-1 rule matched: weaker than the noise in its samples, whose envelope it has"
                .into(),
        );
    }
    // The phase's motion against the envelope's, once for rules 7, 9 and 10.
    let mut motion = None;
    let mut motion = || {
        *motion
            .get_or_insert_with(|| motion_hz(iq, rate_hz, bw, &joins, v.band.map(|b| b.offset_hz)))
    };
    if cv >= AM_MIN_CV {
        let carrier_db = centre_line_db(&db, bin_hz);
        if carrier_db >= OCCUPIED_DB {
            // An envelope that moves under a phase moving far more is a fading or keyed
            // angle-modulated carrier, not AM: its audio is in the phase.
            let m = motion();
            if m.over_hz() >= FM_MIN_DEV_HZ && m.phase_hz >= WEAK_FM_MOTION * m.envelope_hz {
                return (
                    "unknown",
                    0.5,
                    format!(
                        "no layer-1 rule matched: carrier line {carrier_db:.1} dB above the sidebands and a moving envelope, but the phase's {:.0} Hz moves over {WEAK_FM_MOTION} times the envelope's {:.0} Hz",
                        m.phase_hz, m.envelope_hz
                    ),
                );
            }
            return (
                "am",
                0.6,
                format!("carrier line {carrier_db:.1} dB above the sidebands"),
            );
        }
        if let Some((side, share)) = ssb_side(f, &db, (lo, hi)) {
            return (
                "ssb",
                0.6,
                format!(
                    "{side}: no carrier line ({carrier_db:.1} dB), {:.0} % of the occupied power on one side, dial \u{2248} {} Hz ({side})",
                    100.0 * share,
                    ssb_dial_hz(f.center_hz.0, side == "usb")
                ),
            );
        }
    }
    if cv >= AM_LOW_MIN_CV {
        let carrier_db = centre_line_db(&db, bin_hz);
        if carrier_db >= OCCUPIED_DB {
            let m = motion();
            let band_cv = envelope_band_cv(iq, rate_hz, bw);
            if MOTION_MARGIN * m.phase_hz < m.envelope_hz && band_cv >= AM_LOW_MIN_CV {
                return (
                    "am",
                    0.6,
                    format!(
                        "carrier line {carrier_db:.1} dB above the sidebands, envelope {band_cv:.3} in band, phase rms {:.0} Hz below the envelope's {:.0} Hz",
                        m.phase_hz, m.envelope_hz
                    ),
                );
            }
        }
    }
    if cv < FM_MAX_CV && f.bandwidth_hz.0 <= FM_MAX_BW_HZ {
        let m = motion();
        let (dev, over) = (m.phase_hz, m.over_hz());
        // What the noise's clicks alone would put over the envelope, in the band the
        // phase was read in.
        // The noise in the band the phase was read in, and what its clicks alone would
        // put over the envelope there.
        let read = noise.map(|n| {
            let width = (MOTION_BAND * bw as f64).min(n.width_hz);
            (n.snr * n.width_hz / width, width)
        });
        let clicks = read.map_or(0.0, |(snr, width)| click_hz2(snr, width, bw as f64 / 2.0));
        let floor = (FM_MIN_DEV_HZ.powi(2) + CLICK_MARGIN * clicks).sqrt();
        if over < FM_MIN_DEV_HZ {
            return (
                "carrier",
                0.7,
                format!(
                    "constant envelope, narrow band, no discriminator audio (rms deviation {dev:.0} Hz, {over:.0} Hz over the envelope's)"
                ),
            );
        }
        // Under the FM threshold the phase must also out-move the envelope clearly.
        let under_threshold = read.is_some_and(|(snr, _)| snr < FM_THRESHOLD_SNR);
        let clear = !under_threshold || m.phase_hz >= WEAK_FM_MOTION * m.envelope_hz;
        if over >= floor && clear {
            return (
                "fm",
                0.6,
                format!(
                    "constant envelope, narrow band, rms deviation {dev:.0} Hz ({over:.0} Hz over the envelope's)"
                ),
            );
        }
        return (
            "unknown",
            0.5,
            format!(
                "no layer-1 rule matched: constant envelope, narrow band, rms deviation {dev:.0} Hz, but its {over:.0} Hz over the envelope's {:.0} Hz does not stand clear of this noise (a floor of {floor:.0} Hz)",
                m.envelope_hz
            ),
        );
    }
    ("unknown", 0.5, "no layer-1 rule matched".into())
}

/// `measure` parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct MeasureParams {
    /// Measure only the central `bandwidth_hz` of each block (see
    /// [`measure_band`]); `null` measures the whole block.
    #[serde(deserialize_with = "nullable_u64")]
    #[schemars(required, schema_with = "nullable_bandwidth")]
    pub bandwidth_hz: Option<u64>,
}

/// Deserialize a required `Option<u64>`: a missing key is an error, `null` is `None`.
fn nullable_u64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    <Option<u64> as Deserialize>::deserialize(d)
}

/// Schema of the required, nullable `bandwidth_hz`: `["integer","null"]`.
fn nullable_bandwidth(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["integer", "null"],
        "format": "uint64",
        "minimum": 0,
        "description": "Measure only the central bandwidth_hz of each block; null measures the whole block."
    })
}

/// A window cut short is emitted as `partial: true` when it holds at least
/// this many seconds of samples (and at least [`SIZE`] samples).
pub const MIN_PARTIAL_S: f64 = 0.25;

/// Accumulates IQ until it holds at least one second (`rate_hz` samples),
/// then emits one Event for that window: its [`FeatureRecord`] fields plus
/// `"label"`, its [`layer1_label`], and `"partial": false`, with the
/// provenance span covering the window. A change of sample rate or centre
/// frequency, a discontinuity in time (a block whose `t0` is more than one
/// sample away from where the window expects it) or a lag of the input cuts
/// the window short: it is emitted with `"partial": true` and a span of just
/// its samples when it holds at least [`MIN_PARTIAL_S`] of them (and
/// [`SIZE`] samples), else discarded, and a new window starts from the next
/// block, so a window never stitches across a gap. Each discard and partial is
/// noted to the job as `{"measure": {"discarded", "partial", "last", "dc_mask_hz"}}`
/// (`last`: `gap`, `lag` or `retune`; counts cumulative for the run), and once
/// when the stage starts with no cut yet. A window left when the input closes is
/// not measured. With a `dc_hz` in its context (a local hardware receiver) the
/// bins within [`MEASURE_DC_MASK_HZ`] of it take no part in the floor, the peak
/// or the occupied band (see [`measure_band_masked`]); `dc_mask_hz` is that
/// half-width, 0 without one. Each event also carries `power99_hz`: the width
/// holding 99 % of the excess power of the bins around the peak that are at
/// least [`POWER_GATE_DB`] (3 dB) over the noise floor (see [`power_width_hz`]).
/// Sidebands that stay under that gate are not counted, so for a signal weaker
/// than that it is only the carrier line, narrow like the occupied width: a
/// reader treats a `power99_hz` under the kind's minimum as no better than it.
pub struct MeasureStage {
    spec: StageSpec,
}

impl MeasureStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: "measure",
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Event],
                params_schema: schemars::schema_for!(MeasureParams),
            },
        }
    }
}

impl Default for MeasureStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for MeasureStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    async fn run(
        &self,
        params: serde_json::Value,
        inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        ctx: StageContext,
    ) -> Result<(), StageError> {
        let p: MeasureParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("measure params: {e}")))?;
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        // Window under construction: first frame's provenance, t0, rate, centre.
        let mut window: Option<(Provenance, Timestamp, u64, u64)> = None;
        let mut buf: Vec<Complex<f32>> = Vec::new();
        let (mut discarded, mut partials) = (0u64, 0u64);
        // The receiver's DC, masked on a local hardware source only.
        let dc = ctx.dc_hz.map(|hz| (hz, MEASURE_DC_MASK_HZ));
        let dc_mask_hz = dc.map_or(0, |d| d.1);
        ctx.note(serde_json::json!({"measure": {
            "discarded": 0, "partial": 0, "last": null, "dc_mask_hz": dc_mask_hz}}));
        run_map(self.spec().kind, inputs, bus, cancel, move |input| {
            // Cut the window short for `why`: a partial event, or a discard.
            let mut cut = |window: &mut Option<(Provenance, Timestamp, u64, u64)>,
                           buf: &mut Vec<Complex<f32>>,
                           why: &str|
             -> Result<Vec<Frame>, StageError> {
                let taken = window.take();
                let held = std::mem::take(buf);
                let Some((prov, t0, rate, center)) = taken else {
                    return Ok(Vec::new());
                };
                let enough =
                    held.len() >= (rate as f64 * MIN_PARTIAL_S) as usize && held.len() >= SIZE;
                let event = if enough {
                    partials += 1;
                    Some(window_event(
                        &held, &p, &out, prov, t0, rate, center, dc, true,
                    )?)
                } else {
                    discarded += 1;
                    None
                };
                ctx.note(serde_json::json!({"measure": {
                    "discarded": discarded, "partial": partials, "last": why,
                    "dc_mask_hz": dc_mask_hz}}));
                Ok(event.into_iter().collect())
            };
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => return cut(&mut window, &mut buf, "lag"),
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed("measure stage expects iq frames".into()));
            };
            let (rate, center) = (block.sample_rate_hz.0, block.center_hz.0);
            // Where the next block should start: the window's t0 plus the
            // samples held, within one sample (the ring's gap rule).
            let gap = window.as_ref().is_some_and(|w| {
                let expected =
                    w.1.0 as u128 + buf.len() as u128 * 1_000_000_000 / rate.max(1) as u128;
                let tolerance = 1_000_000_000u128.div_ceil(rate.max(1) as u128);
                (block.t0.0 as u128).abs_diff(expected) > tolerance
            });
            let retune = window
                .as_ref()
                .is_some_and(|w| (w.2, w.3) != (rate, center));
            let mut events = if retune {
                cut(&mut window, &mut buf, "retune")?
            } else if gap {
                cut(&mut window, &mut buf, "gap")?
            } else {
                Vec::new()
            };
            let (prov, t0, _, _) = window
                .get_or_insert_with(|| (frame.provenance.clone(), block.t0, rate, center))
                .clone();
            buf.extend_from_slice(&block.samples);
            if rate == 0 || (buf.len() as u64) < rate {
                return Ok(events);
            }
            events.push(window_event(
                &buf, &p, &out, prov, t0, rate, center, dc, false,
            )?);
            window = None;
            buf.clear();
            Ok(events)
        })
        .await
    }
}

/// The event for the window `buf` (first sample at `t0`): measured band,
/// layer-1 label and `partial`, spanning exactly its samples.
#[allow(clippy::too_many_arguments)]
fn window_event(
    buf: &[Complex<f32>],
    p: &MeasureParams,
    out: &StreamId,
    mut prov: Provenance,
    t0: Timestamp,
    rate: u64,
    center: u64,
    dc: Option<(u64, u64)>,
    partial: bool,
) -> Result<Frame, StageError> {
    let (f, note, v) = measure_band_noted(buf, rate, center, p.bandwidth_hz.unwrap_or(rate), dc);
    let label = layer1_label_of(&f, &v, buf.len(), rate);
    let mut ev =
        serde_json::to_value(&f).map_err(|e| StageError::Failed(format!("measure: {e}")))?;
    ev["label"] =
        serde_json::to_value(&label).map_err(|e| StageError::Failed(format!("measure: {e}")))?;
    ev["partial"] = partial.into();
    ev["power99_hz"] = note.power99_hz.into();
    prov.span = frame_span(t0, buf.len(), rate);
    Ok(Frame::new(out.clone(), prov, Payload::Event(ev)))
}

#[cfg(test)]
thread_local! {
    /// [`measure_selection`] calls on this thread, for tests that count them.
    pub(crate) static MEASURE_SELECTION_CALLS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// [`measure`] and [`layer1_label`] of `band` of `iq`, a block sampled at
/// `rate_hz` and centred at `center_hz`: the whole block as it is, any other
/// band extracted at [`channel_rate_hz`] and measured with [`measure_band`]
/// around its own centre (`center_hz` plus the band's offset). `None` when the
/// band cannot be extracted.
pub fn measure_selection(
    iq: &[Complex<f32>],
    rate_hz: u64,
    center_hz: u64,
    band: Band,
) -> Option<(FeatureRecord, Label)> {
    #[cfg(test)]
    MEASURE_SELECTION_CALLS.with(|c| c.set(c.get() + 1));
    if band.is_whole(Hz(rate_hz)) {
        let (mut f, note, v) = measure_band_noted(iq, rate_hz, center_hz, rate_hz, None);
        let l = layer1_label_of(&f, &v, iq.len(), rate_hz);
        carry_extras(&mut f, &note, &l, &v, rate_hz);
        return Some((f, l));
    }
    let bw = band.bandwidth_hz.0;
    let (chan, out_rate) = extract_offset(
        iq,
        rate_hz,
        band.offset_hz,
        bw,
        channel_rate_hz(rate_hz, bw),
    )
    .ok()?;
    let band_center = (center_hz as i64).saturating_add(band.offset_hz).max(0) as u64;
    let (mut f, note, v) = measure_band_noted(&chan, out_rate, band_center, bw, None);
    let l = layer1_label_of(&f, &v, chan.len(), out_rate);
    carry_extras(&mut f, &note, &l, &v, out_rate);
    Some((f, l))
}

/// Layer-1 classifier: [`measure_selection`] on the requested band; upstream
/// features are not used, the band is measured afresh. Through
/// [`Classifier::classify_measured`] it hands the measurement and its label on
/// to later layers.
pub struct Layer1Classifier;

#[async_trait]
impl Classifier for Layer1Classifier {
    fn name(&self) -> &str {
        "layer1-classical"
    }
    fn layer(&self) -> u8 {
        1
    }
    async fn classify(
        &self,
        iq: &[Complex<f32>],
        sample_rate_hz: Hz,
        center_hz: Hz,
        band: Band,
        _features: Option<&FeatureRecord>,
    ) -> Vec<Label> {
        measure_selection(iq, sample_rate_hz.0, center_hz.0, band)
            .map(|(_, l)| l)
            .into_iter()
            .collect()
    }
    async fn classify_measured(
        &self,
        iq: &[Complex<f32>],
        sample_rate_hz: Hz,
        center_hz: Hz,
        band: Band,
        _features: Option<&FeatureRecord>,
        _measured: Option<&Measurement>,
        _allocations: &[Allocation],
    ) -> (Vec<Label>, Option<Measurement>, Vec<Tried>) {
        match measure_selection(iq, sample_rate_hz.0, center_hz.0, band) {
            Some((features, label)) => (
                vec![label.clone()],
                Some(Measurement { features, label }),
                Vec::new(),
            ),
            None => (Vec::new(), None, Vec::new()),
        }
    }
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use super::burst::{BURST_DUTY_MAX, BURST_MIN_PERIOD_MS};
    use super::*;
    use crate::fixtures::{
        Impairments, SynthSpec, aprs_packet_iq, awgn, nbfm_modulate, synthesise, tone,
    };

    const RATE: u64 = 48_000;
    const CENTER: u64 = 144_800_000;
    const BIN_HZ: f64 = RATE as f64 / 1024.0;
    const WIDE_RATE: u64 = 192_000;

    /// The public [`super::layer1_label`] of `f`, asserted to name what production's
    /// path ([`measure_selection`] over the whole block at [`CENTER`]) names whenever
    /// `f` is that path's record: the tests below run on both.
    fn layer1_label(f: &FeatureRecord, iq: &[Complex<f32>], rate_hz: u64) -> Label {
        let l = super::layer1_label(f, iq, rate_hz);
        let whole = Band {
            offset_hz: 0,
            bandwidth_hz: Hz(rate_hz),
        };
        if let Some((pf, pl)) = measure_selection(iq, rate_hz, CENTER, whole)
            && pf == *f
        {
            assert_eq!(
                pl.name, l.name,
                "production names {pl:?}, layer1_label {l:?}"
            );
        }
        l
    }

    fn show(what: &str, f: &FeatureRecord, l: &Label) {
        eprintln!("{what}: {f:?} -> {} ({})", l.name, l.evidence);
    }

    /// 240 non-repeating bits (an LCG's top nibbles), so no pattern line
    /// competes with the symbol rate.
    const LCG_BITS: &str = "14ed58c6d3ff2d75a5650138a8d447f00ec47321d420c5d08bec3850735c";

    #[test]
    fn a_1_percent_duty_burst_reports_duty_period_and_band() {
        // 5 ms burst (24 bits at 4800 Bd) every 1000 ms, 3 bursts = 2 s of signal
        let spec = SynthSpec::Fsk {
            symbol_rate_baud: 4800,
            deviation_hz: 4000.0,
            order: 2,
            bits: LCG_BITS[..6].into(),
            repeat: 3,
            gap_ms: 995,
        };
        let iq = synthesise(
            &spec,
            &Impairments {
                snr_db: Some(20.0),
                ..Default::default()
            },
            48_000,
            0,
        );
        let f = measure_band(&iq, 48_000, CENTER, 20_000);
        eprintln!("1 % duty burst: {f:?}");
        let duty = f.duty.unwrap();
        // 3 × 5 ms on in 2005 ms
        assert!((0.005..=0.010).contains(&duty), "duty {duty}");
        assert_eq!(f.bursts, Some(3));
        assert!((980.0..=1020.0).contains(&f.period_ms.unwrap()));
        assert!((4.0..=7.0).contains(&f.burst_ms.unwrap()));
        assert!(f.burst);
        // Carson for 4800 Bd at 4 kHz deviation: 2·(4000 + 4800) = 17.6 kHz
        assert!(
            (15_000..=21_000).contains(&f.bandwidth_hz.0),
            "bw {}",
            f.bandwidth_hz.0
        );
        // A 24-bit burst has too few symbols to owe a rate, but no window edge may read as one.
        assert!(
            f.symbol_rate_baud.is_none_or(|Baud(b)| b >= 1000),
            "no sub-kHz window-edge artefact: {:?}",
            f.symbol_rate_baud
        );
    }

    #[test]
    fn a_single_5_ms_burst_in_a_window_is_gated() {
        let spec = SynthSpec::Fsk {
            symbol_rate_baud: 4800,
            deviation_hz: 4000.0,
            order: 2,
            bits: LCG_BITS[..6].into(),
            repeat: 1,
            gap_ms: 50,
        };
        let mut iq = synthesise(&spec, &Impairments::default(), 48_000, 0);
        iq.resize(48_000, Complex::new(0.0, 0.0));
        awgn(&mut iq, 20.0, 17);
        let v = gated_view(&iq, 48_000, 48_000);
        eprintln!("single burst: {:?}", v.bursts);
        assert_eq!(v.bursts.runs.len(), 1);
        assert!(v.gated);
    }

    #[test]
    fn two_clicks_over_a_continuous_signal_do_not_gate() {
        let audio = tone(48_000, 48_000, 1_000.0, 1.0);
        let mut clean = nbfm_modulate(&audio, 48_000, RATE as u32, 2_500.0, 0.0);
        awgn(&mut clean, 20.0, 3);
        // Two 1 ms clicks 10 dB over the signal.
        let mut iq = clean.clone();
        let ms = (RATE / 1_000) as usize;
        for at in [300, 700] {
            iq[at * ms..(at + 1) * ms]
                .iter_mut()
                .for_each(|s| *s *= 10f32.sqrt());
        }
        let v = gated_view(&iq, RATE, 20_000);
        eprintln!("clicks: {:?}", v.bursts);
        assert!(!v.gated, "{:?}", v.bursts);
        let f = measure_band(&iq, RATE, CENTER, 20_000);
        let plain = measure_band(&clean, RATE, CENTER, 20_000);
        show("clicks", &f, &layer1_label(&f, &iq, RATE));
        let tol = 2 * BIN_HZ as u64;
        assert!(
            f.bandwidth_hz.0.abs_diff(plain.bandwidth_hz.0) <= tol,
            "{} vs {}",
            f.bandwidth_hz.0,
            plain.bandwidth_hz.0
        );
        assert_eq!(
            layer1_label(&f, &iq, RATE).name,
            layer1_label(&plain, &clean, RATE).name
        );
    }

    #[test]
    fn channel_filtered_noise_still_lets_a_burst_gate() {
        // The 1 % duty fixture as a channel stage hands it on: low-passed to a 12.5 kHz
        // band at 48 kHz, so most of the spectrum is stopband, not noise.
        let spec = SynthSpec::Fsk {
            symbol_rate_baud: 4800,
            deviation_hz: 4000.0,
            order: 2,
            bits: LCG_BITS[..6].into(),
            repeat: 3,
            gap_ms: 995,
        };
        let raw = synthesise(
            &spec,
            &Impairments {
                snr_db: Some(20.0),
                ..Default::default()
            },
            48_000,
            0,
        );
        let (iq, _) = crate::channel::extract_offset(&raw, RATE, 0, 12_500, RATE).unwrap();
        // The floor of the band, not of the stopband: plain filtered noise reads near 0 dB
        // over it (98.6 dB with the whole spectrum's 15th-percentile bin).
        let mut w = vec![Complex::new(0.0f32, 0.0); 2 * RATE as usize];
        awgn(&mut w, 0.0, 41);
        let (noise, _) = crate::channel::extract_offset(&w, RATE, 0, 12_500, RATE).unwrap();
        let mean = noise.iter().map(|s| s.norm_sqr() as f64).sum::<f64>() / noise.len() as f64;
        let ratio = 10.0 * (mean / noise_floor_power(&noise, RATE, 12_500).unwrap()).log10();
        eprintln!("filtered noise mean/floor: {ratio:.1} dB");
        assert!(ratio.abs() < 3.0, "{ratio}");
        let v = gated_view(&iq, RATE, 12_500);
        eprintln!("filtered bursts: {:?}", v.bursts);
        assert!(v.gated, "{:?}", v.bursts);
        let f = measure_band(&iq, RATE, CENTER, 12_500);
        assert!(f.burst, "{f:?}");
        assert_eq!(f.bursts, Some(3), "{f:?}");
        assert!(f.duty.unwrap() < 0.02, "{f:?}");
    }

    #[test]
    fn a_lone_1_ms_outlier_chunk_is_not_gated() {
        for rate in [8_000u64, 48_000] {
            let mut clean = vec![Complex::new(0.0f32, 0.0); rate as usize];
            awgn(&mut clean, 0.0, 23);
            // One 1 ms chunk of the noise 8 dB up: the lone outlier a quiet
            // capture can hold.
            let mut iq = clean.clone();
            let ms = (rate / 1_000) as usize;
            iq[500 * ms..501 * ms].iter_mut().for_each(|s| *s *= 2.5);
            let v = gated_view(&iq, rate, rate);
            eprintln!("noise at {rate} Hz: {:?}", v.bursts);
            assert_eq!(v.bursts.runs.len(), 1, "{rate} Hz: {:?}", v.bursts);
            assert!(v.bursts.burst_ms.is_some_and(|ms| ms < 3.0));
            assert!(!v.gated, "{rate} Hz: {:?}", v.bursts);
            let f = measure_band(&iq, rate, CENTER, rate);
            let l = layer1_label(&f, &iq, rate);
            eprintln!("noise at {rate} Hz: {f:?} -> {}", l.name);
            assert!(!f.burst, "{rate} Hz: {f:?}");
            // Measured whole: labelled as the same noise without the outlier.
            let plain = measure_band(&clean, rate, CENTER, rate);
            assert_eq!(l.name, layer1_label(&plain, &clean, rate).name);
            if rate == 48_000 {
                assert_eq!(l.name, "noise", "{f:?}");
            }
        }
    }

    #[test]
    fn a_bursty_psk_link_reports_its_symbol_rate() {
        // 100 ms bursts of 240 random bits every 1050 ms
        let spec = SynthSpec::Psk {
            symbol_rate_baud: 2400,
            order: 2,
            bits: LCG_BITS.into(),
            repeat: 3,
            gap_ms: 950,
        };
        let iq = synthesise(
            &spec,
            &Impairments {
                snr_db: Some(20.0),
                ..Default::default()
            },
            48_000,
            0,
        );
        let f = measure_band(&iq, 48_000, CENTER, 20_000);
        eprintln!("bursty psk: {f:?}");
        let duty = f.duty.unwrap();
        assert!(duty < 0.5, "duty {duty}");
        let baud = f.symbol_rate_baud.expect("a rate").0 as f64;
        assert!((baud - 2400.0).abs() <= 2400.0 * 0.02, "baud {baud}");
    }

    #[test]
    fn a_bursty_fsk_link_reports_its_symbol_rate() {
        // 50 ms bursts of 240 random bits every 1000 ms
        let spec = SynthSpec::Fsk {
            symbol_rate_baud: 4800,
            deviation_hz: 4000.0,
            order: 2,
            bits: LCG_BITS.into(),
            repeat: 3,
            gap_ms: 950,
        };
        let iq = synthesise(
            &spec,
            &Impairments {
                snr_db: Some(20.0),
                ..Default::default()
            },
            48_000,
            0,
        );
        let f = measure_band(&iq, 48_000, CENTER, 20_000);
        eprintln!("bursty fsk: {f:?}");
        let duty = f.duty.unwrap();
        assert!(duty < 0.5, "duty {duty}");
        let baud = f.symbol_rate_baud.expect("a rate").0 as f64;
        assert!((baud - 4800.0).abs() <= 4800.0 * 0.04, "baud {baud}");
    }

    #[test]
    fn dense_on_off_keying_is_not_gated() {
        let spec = SynthSpec::Ook {
            symbol_rate_baud: 1200,
            bits: LCG_BITS.into(),
            repeat: 2,
            gap_ms: 300,
        };
        let imp = Impairments {
            snr_db: Some(10.0),
            offset_hz: 500.0,
            ..Default::default()
        };
        let iq = synthesise(&spec, &imp, 48_000, 0);
        let b = burst_runs(&iq, 48_000);
        eprintln!(
            "ook runs: {} duty {} period {:?}",
            b.runs.len(),
            b.duty,
            b.period_ms
        );
        // Two 200 ms transmissions 300 ms apart: on for under half the window,
        // but the symbols' on-off runs repeat within a few ms, which is keying,
        // not bursts, so the window is measured whole.
        assert!(b.duty < BURST_DUTY_MAX, "duty {}", b.duty);
        assert!(b.period_ms.is_some_and(|p| p < BURST_MIN_PERIOD_MS));
        let v = gated_view(&iq, 48_000, 48_000);
        assert!(!v.gated);
        assert_eq!(v.on.len(), iq.len());
        let f = measure_band(&iq, 48_000, CENTER, 20_000);
        eprintln!("ook: {f:?}");
        assert_eq!(f.bursts, Some(b.runs.len() as u32));
        assert_eq!(f.period_ms, b.period_ms);
        // Band and rate as for a continuous signal: the sinc main lobe at
        // least, and the keying line.
        assert!(f.bandwidth_hz.0 >= 2_400, "bw {}", f.bandwidth_hz.0);
        let baud = f.symbol_rate_baud.expect("a rate").0 as f64;
        assert!((baud - 1200.0).abs() <= 1200.0 * 0.02, "baud {baud}");
    }

    #[test]
    fn the_cyclostationary_line_finds_psk_and_ook_rates() {
        for spec in [
            SynthSpec::Psk {
                symbol_rate_baud: 2400,
                order: 2,
                bits: LCG_BITS.into(),
                repeat: 1,
                gap_ms: 50,
            },
            SynthSpec::Ook {
                symbol_rate_baud: 1200,
                bits: LCG_BITS.into(),
                repeat: 1,
                gap_ms: 50,
            },
        ] {
            let iq = synthesise(
                &spec,
                &Impairments {
                    snr_db: Some(10.0),
                    offset_hz: 500.0,
                    ppm: 20.0,
                    ..Default::default()
                },
                48_000,
                0,
            );
            let f = measure_band(&iq, 48_000, CENTER, 20_000);
            eprintln!("{spec:?}: {f:?}");
            let want = match spec {
                SynthSpec::Psk {
                    symbol_rate_baud, ..
                }
                | SynthSpec::Ook {
                    symbol_rate_baud, ..
                } => symbol_rate_baud,
                _ => unreachable!(),
            };
            let got = f.symbol_rate_baud.expect("a rate").0 as f64;
            assert!(
                (got - want as f64).abs() <= want as f64 * 0.02,
                "{spec:?}: {got}"
            );
        }
    }

    /// A keyed NBFM over at [`RATE`] for 2 s: speech-like audio at 2.5 kHz deviation, the
    /// carrier up over `on` (a reversed pair is a hole), `snr_db` over the noise.
    fn over(on: (f64, f64), audio: f32, snr_db: f32) -> Vec<Complex<f32>> {
        crate::fixtures::keyed_over(&crate::fixtures::KeyedOver {
            rate_hz: RATE as u32,
            seconds: 2.0,
            deviation_hz: 2_500.0,
            audio,
            on,
            snr_db,
            seed: 29,
        })
    }

    #[test]
    fn a_hole_in_a_carrier_is_an_off_run() {
        // The carrier is up but for 100 ms in the middle of 2 s: the 15th-percentile chunk
        // is the carrier, and the off part lies under it.
        for snr_db in [36.0, 15.0] {
            let iq = over((0.525, 0.475), 1.0, snr_db);
            let b = burst_runs(&iq, RATE);
            eprintln!("hole at {snr_db} dB: {b:?}");
            assert_eq!(b.runs.len(), 2, "{b:?}");
            assert!((b.duty - 0.95).abs() <= 0.005, "duty {}", b.duty);
            let n = iq.len() as f64;
            let ms = RATE as f64 / 1_000.0;
            let (gap_from, gap_to) = (b.runs[0].1 as f64, b.runs[1].0 as f64);
            assert!((gap_from - 0.475 * n).abs() <= 2.0 * ms, "{b:?}");
            assert!((gap_to - 0.525 * n).abs() <= 2.0 * ms, "{b:?}");
            // The off level is the noise, not the carrier.
            assert!(b.off_power < 0.1, "{b:?}");
        }
    }

    #[test]
    fn a_window_of_one_level_is_one_run() {
        let carrier = over((0.0, 1.0), 1.0, 15.0);
        let mut noise = vec![Complex::new(0.0f32, 0.0); 2 * RATE as usize];
        awgn(&mut noise, 0.0, 31);
        // A window shorter than one transform holds one level too.
        let short = &carrier[..SIZE / 2];
        for (what, iq) in [
            ("carrier", &carrier[..]),
            ("noise", &noise[..]),
            ("short", short),
        ] {
            let b = burst_runs(iq, RATE);
            eprintln!("{what}: {b:?}");
            assert_eq!(b.runs, vec![(0, iq.len())], "{what}");
            assert_eq!(b.duty, 1.0, "{what}");
            let v = gated_view(iq, RATE, 12_500);
            assert!(!v.gated, "{what}");
            assert_eq!(v.on.len(), iq.len(), "{what}");
        }
    }

    #[test]
    fn a_keyed_window_is_gated_at_any_duty() {
        // Key-up and key-down at shares over and under half, so the off part is found from
        // either side of the 15th-percentile chunk.
        for d in [0.97, 0.75, 0.4] {
            for (kind, on) in [("up", (1.0 - d, 1.0)), ("down", (0.0, d))] {
                for snr_db in [36.0f32, 15.0] {
                    let iq = over(on, 1.0, snr_db);
                    let v = gated_view(&iq, RATE, 12_500);
                    let what = format!("key-{kind} d {d} at {snr_db} dB");
                    eprintln!("{what}: {:?}", v.bursts);
                    assert!(v.gated, "{what}: {:?}", v.bursts);
                    assert!(
                        (f64::from(v.bursts.duty) - d).abs() <= 0.005,
                        "{what}: duty {}",
                        v.bursts.duty
                    );
                    // No 1 ms chunk of the on-time is noise: each stands `BURST_ON_DB` over it.
                    let noise = 10f64.powf(-f64::from(snr_db) / 10.0);
                    let chunk = (RATE / 1_000) as usize;
                    for &(s, e) in &v.bursts.runs {
                        for c in iq[s..e].chunks(chunk) {
                            let p =
                                c.iter().map(|x| x.norm_sqr() as f64).sum::<f64>() / c.len() as f64;
                            assert!(
                                10.0 * (p / noise).log10() > BURST_ON_DB,
                                "{what}: a noise chunk in the on-time ({s}..{e})"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_train_of_31_ms_frames_at_duty_0_77_is_not_gated() {
        // `BURST_LONG_RUN_MS` is for a carrier keyed up or down, whose runs are hundreds
        // of ms: an OOK remote's train of frames over half the window is keying, measured
        // whole, so its rate is the run-length chip and not the frames' line.
        let fx = &crate::fixtures::ev1527_fixtures()[0];
        let iq = fx.remote.iq(fx.rate_hz);
        let rate = u64::from(fx.rate_hz);
        let v = gated_view(&iq, rate, fx.band_hz);
        assert!(v.bursts.duty >= BURST_DUTY_MAX, "{:?}", v.bursts);
        assert!(
            v.bursts
                .burst_ms
                .is_some_and(|ms| (25.0..super::burst::BURST_LONG_RUN_MS).contains(&ms)),
            "{:?}",
            v.bursts
        );
        assert!(!v.gated, "{:?}", v.bursts);
        assert_eq!(v.received.len(), iq.len());
    }

    #[test]
    fn a_continuous_signal_measures_as_before() {
        // The fixture of measure_nbfm_tone_reports_band_and_fm (a noiseless
        // 5 kHz-deviation tone fills the whole 20 kHz band at floor + 6 dB).
        let audio = tone(48_000, 48_000, 1_000.0, 1.0);
        let mut iq = nbfm_modulate(&audio, 48_000, RATE as u32, 2_500.0, 0.0);
        awgn(&mut iq, 20.0, 3);
        let f = measure_band(&iq, RATE, CENTER, 20_000);
        assert_eq!(f.duty, Some(1.0));
        assert_eq!(f.bursts, Some(1));
        assert_eq!(f.period_ms, None);
        assert!(!f.burst);
        // the same band and label the existing measure_nbfm_tone_reports_band_and_fm asserts
        assert!((8_000..=16_000).contains(&f.bandwidth_hz.0));
    }

    #[test]
    fn measure_bandwidth_param_is_required_nullable() {
        let parse = |v| serde_json::from_value::<MeasureParams>(v);
        assert!(parse(serde_json::json!({})).is_err());
        assert_eq!(
            parse(serde_json::json!({"bandwidth_hz": null}))
                .unwrap()
                .bandwidth_hz,
            None
        );
        assert_eq!(
            parse(serde_json::json!({"bandwidth_hz": 12_500}))
                .unwrap()
                .bandwidth_hz,
            Some(12_500)
        );
    }

    #[tokio::test]
    async fn layer1_labels_aprs_fsk_in_12k5_band() {
        for offset in [0i64, 12_000] {
            let mut iq = aprs_packet_iq("VK3ABC", "APRS", ">hi", RATE as u32, offset as f64);
            awgn(&mut iq, 20.0, 5);
            let band = Band {
                offset_hz: offset,
                bandwidth_hz: Hz(12_500),
            };
            let chan = extract_offset(&iq, RATE, offset, 12_500, RATE).unwrap().0;
            let f = measure_band(&chan, RATE, CENTER, 12_500);
            eprintln!("offset {offset}: {f:?}");
            let labels = Layer1Classifier
                .classify(&iq, Hz(RATE), Hz(CENTER), band, None)
                .await;
            eprintln!("offset {offset}: {labels:?}");
            assert_eq!(labels.len(), 1, "{labels:?}");
            assert_eq!(labels[0].name, "fsk", "offset {offset}: {labels:?}");
            let baud = f.symbol_rate_baud.expect("a keying line").0;
            assert!(baud.abs_diff(1_200) <= 60, "offset {offset}: {baud} Bd");
        }
    }

    #[test]
    fn measure_noise_is_noise() {
        let mut iq = vec![Complex::new(0.0f32, 0.0); RATE as usize];
        awgn(&mut iq, 0.0, 11);
        let f = measure(&iq, RATE, CENTER);
        let l = layer1_label(&f, &iq, RATE);
        show("noise", &f, &l);
        assert!(!f.burst);
        assert!(f.snr_db < 3.0, "snr_db {}", f.snr_db);
        assert_eq!(f.symbol_rate_baud, None);
        // Noise histograms show spurious clusters; the record carries no order.
        assert_eq!(f.order, None);
        assert_eq!(f.tone_spacing_hz, None);
        assert_eq!(l.name, "noise");
        assert_eq!(l.layer, 1);
    }

    #[test]
    fn measure_nbfm_tone_reports_band_and_fm() {
        let audio = tone(48_000, 48_000, 1_000.0, 1.0);
        let mut iq = nbfm_modulate(&audio, 48_000, RATE as u32, 2_500.0, 0.0);
        awgn(&mut iq, 20.0, 3);
        let f = measure(&iq, RATE, CENTER);
        let l = layer1_label(&f, &iq, RATE);
        show("nbfm", &f, &l);
        assert!(
            (5_000..=12_000).contains(&f.bandwidth_hz.0),
            "bandwidth_hz {}",
            f.bandwidth_hz.0
        );
        assert!(f.snr_db > 20.0, "snr_db {}", f.snr_db);
        assert!(!f.burst);
        // Half-band and local-median guards: a pure FM tone has no keying line.
        assert_eq!(f.symbol_rate_baud, None);
        assert_eq!(l.name, "fm");
    }

    /// A weak FM voice signal as the receiver sees it at about 10 dB: a carrier line `carrier`
    /// times the noise's own amplitude, voice sidebands 300-3000 Hz either side of it
    /// `sideband_db` over the noise's level, and white noise. The occupied band (6 dB over the
    /// floor) is the carrier line alone, the sidebands hold most of the power.
    fn weak_fm_voice(carrier: f32, sideband_db: f32) -> Vec<Complex<f32>> {
        use rustfft::FftPlanner;
        let n = RATE as usize;
        let mut state = 99u64;
        let mut gauss = move || {
            let mut uniform = || {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                ((state >> 11) + 1) as f64 / (1u64 << 53) as f64
            };
            let (u, v) = (uniform(), uniform());
            let r = (-2.0 * u.ln()).sqrt() / std::f64::consts::SQRT_2;
            Complex::new(
                (r * (std::f64::consts::TAU * v).cos()) as f32,
                (r * (std::f64::consts::TAU * v).sin()) as f32,
            )
        };
        let noise: Vec<Complex<f32>> = (0..n).map(|_| gauss()).collect();
        let mut side: Vec<Complex<f32>> = (0..n).map(|_| gauss()).collect();
        let mut planner = FftPlanner::<f32>::new();
        planner.plan_fft_forward(n).process(&mut side);
        for (k, b) in side.iter_mut().enumerate() {
            let hz = if k <= n / 2 { k } else { n - k };
            if !(300..=3_000).contains(&hz) {
                *b = Complex::new(0.0, 0.0);
            }
        }
        planner.plan_fft_inverse(n).process(&mut side);
        let k = 10f32.powf(sideband_db / 20.0) / n as f32;
        noise
            .iter()
            .zip(&side)
            .map(|(w, s)| w + s * k + Complex::new(carrier, 0.0))
            .collect()
    }

    #[test]
    fn a_weak_voice_signals_power_width_is_its_sidebands_not_its_carrier_line() {
        let iq = weak_fm_voice(0.1, 1.0);
        let (f, note, _) = measure_band_noted(&iq, RATE, CENTER, RATE, None);
        assert!(f.bandwidth_hz.0 < 1_000, "occupied {}", f.bandwidth_hz.0);
        let w = note.power99_hz.expect("a power width");
        assert!((4_000..=14_000).contains(&w), "power99_hz {w}");
    }

    #[tokio::test]
    async fn the_measure_event_carries_the_power_width() {
        let blocks = tone_blocks(10);
        let (events, _) = run_stage(32, None, |tx, prov| {
            Box::pin(async move {
                for (i, b) in blocks.iter().enumerate() {
                    tx.send(iq_frame(&prov, 1_000_000_000 + i as u64 * 100_000_000, b))
                        .unwrap();
                }
            })
        })
        .await;
        let Payload::Event(ev) = &events.last().expect("an event").payload else {
            panic!("event")
        };
        let w = ev["power99_hz"].as_u64().expect("power99_hz in the event");
        assert!(w <= ev["bandwidth_hz"].as_u64().unwrap().max(1_000), "{ev}");
    }

    #[test]
    fn sidebands_under_the_gate_leave_only_a_narrow_power_width() {
        // The limit of `power99_hz`, stated honestly: the same signal as the test above with
        // its sidebands 6 dB under the noise's own level (about 2 dB over the floor at best,
        // under the 3 dB gate): the margin of that test is thin, a few dB of sideband level
        // between 6000 Hz and a width as narrow as the occupied band's. Here only the carrier
        // line is counted, so a reader must treat a width under the kind's minimum as untrusted.
        let iq = weak_fm_voice(0.1, -6.0);
        let (f, note, _) = measure_band_noted(&iq, RATE, CENTER, RATE, None);
        let w = note.power99_hz.expect("a power width");
        eprintln!("under the gate: occupied {} power99 {w}", f.bandwidth_hz.0);
        assert!(f.bandwidth_hz.0 < 1_000, "occupied {}", f.bandwidth_hz.0);
        // Measured 1500 Hz: noise bins over the gate bridged to the carrier, nowhere near the
        // 5 kHz the page's `fm_demod` floor asks of a trusted width.
        assert!(w < 2_500, "power99_hz {w}");
    }

    #[test]
    fn a_clean_narrow_tone_has_a_narrow_power_width() {
        let mut iq: Vec<Complex<f32>> = (0..48_000)
            .map(|i| {
                Complex::from_polar(1.0, std::f32::consts::TAU * 1_000.0 * i as f32 / 48_000.0)
            })
            .collect();
        awgn(&mut iq, 30.0, 3);
        let (_, note, _) = measure_band_noted(&iq, RATE, CENTER, RATE, None);
        let w = note.power99_hz.expect("a power width");
        assert!(w <= 600, "power99_hz {w}");
    }

    #[test]
    fn measure_afsk_packet_reports_fsk_and_1200_baud() {
        let silence = RATE as usize / 2;
        let clean = aprs_packet_iq(
            "N0CALL",
            "APRS",
            "!4903.50N/07201.75W-Test",
            RATE as u32,
            0.0,
        );
        // Active part: the packet without the 0.5 s of audio silence either side.
        let mut active = clean[silence..clean.len() - silence].to_vec();
        awgn(&mut active, 20.0, 5);
        let f = measure(&active, RATE, CENTER);
        let l = layer1_label(&f, &active, RATE);
        show("afsk active", &f, &l);
        let baud = f.symbol_rate_baud.expect("a symbol-rate line").0;
        assert!((1_140..=1_260).contains(&baud), "symbol_rate_baud {baud}");
        assert_eq!(l.name, "fsk");
        // Whole fixture with the carrier keyed only for the packet. The audio
        // silence of `aprs_packet_iq` is an unmodulated carrier (flat
        // envelope, so never a burst), so model the transmitter as off for
        // those 0.5 s either side and leave only receiver noise.
        let mut keyed = clean.clone();
        let n = keyed.len();
        keyed[..silence].fill(Complex::new(0.0, 0.0));
        keyed[n - silence..].fill(Complex::new(0.0, 0.0));
        awgn(&mut keyed, 20.0, 9);
        let f = measure(&keyed, RATE, CENTER);
        show("afsk keyed whole", &f, &layer1_label(&f, &keyed, RATE));
        assert!(f.burst);
        // Burst-gated: the packet is on for about a third of the window, so it
        // is measured over its on-time and the keying line is found again.
        assert_eq!(f.bursts, Some(1));
        let baud = f.symbol_rate_baud.expect("a symbol-rate line").0;
        assert!((1_140..=1_260).contains(&baud), "symbol_rate_baud {baud}");
    }

    #[test]
    fn measure_center_offset_tracked() {
        let mut iq = crate::util::nco(RATE as usize, 5_000.0, RATE as f64);
        awgn(&mut iq, 20.0, 13);
        let f = measure(&iq, RATE, CENTER);
        let l = layer1_label(&f, &iq, RATE);
        show("tone +5k", &f, &l);
        assert_eq!(l.name, "carrier");
        // Half-band guard: discriminator noise of a bare carrier is not keying.
        assert_eq!(f.symbol_rate_baud, None);
        let err = f.center_hz.0 as f64 - (CENTER + 5_000) as f64;
        assert!(err.abs() <= BIN_HZ, "center_hz {} err {err}", f.center_hz.0);
    }

    #[tokio::test]
    async fn measure_stage_noise_in_production_blocks_is_noise() {
        use zeta_core::port::IqBlock;
        use zeta_core::types::{Provenance, SourceId, Span, Timestamp};
        let prov = Provenance {
            source: SourceId("s".into()),
            job: None,
            channel: None,
            span: Span {
                start_ns: Timestamp(0),
                end_ns: Timestamp(1),
            },
        };
        let bus = Arc::new(StreamBus::new(8));
        let out = bus.register(StreamKind::Event, prov.clone());
        let mut rx_out = bus.subscribe(&out).unwrap();
        let (tx, rx_in) = broadcast::channel(16);
        let h = tokio::spawn(async move {
            MeasureStage::new()
                .run(
                    serde_json::json!({"bandwidth_hz": null}),
                    vec![rx_in],
                    vec![out],
                    bus,
                    CancellationToken::new(),
                    zeta_core::stage::StageContext::new(zeta_core::types::SourceId("s".into())),
                )
                .await
        });
        // Review Focus 6 at the production block size: 4800-sample blocks of
        // pure noise, one event once a full second has accumulated.
        let mut iq = vec![Complex::new(0.0f32, 0.0); RATE as usize];
        awgn(&mut iq, 0.0, 1);
        for (i, block) in iq.chunks(4_800).enumerate() {
            tx.send(Frame::new(
                StreamId::new(),
                prov.clone(),
                Payload::Iq(IqBlock {
                    t0: Timestamp(1_000_000_000 + i as u64 * 100_000_000),
                    sample_rate_hz: Hz(RATE),
                    center_hz: Hz(CENTER),
                    samples: block.to_vec().into(),
                }),
            ))
            .unwrap();
        }
        let f = rx_out.recv().await.unwrap();
        assert_eq!(f.provenance.span.start_ns, Timestamp(1_000_000_000));
        assert_eq!(f.provenance.span.end_ns, Timestamp(2_000_000_000));
        let Payload::Event(ev) = &f.payload else {
            panic!("expected an event")
        };
        let mut ev = ev.clone();
        let label: Label = serde_json::from_value(ev["label"].take()).unwrap();
        ev.as_object_mut().unwrap().remove("label");
        assert_eq!(ev["partial"], false);
        ev.as_object_mut().unwrap().remove("partial");
        assert!(ev["power99_hz"].is_number() || ev["power99_hz"].is_null());
        ev.as_object_mut().unwrap().remove("power99_hz");
        assert_eq!(ev["symbol_rate_baud"], serde_json::Value::Null);
        let rec: FeatureRecord = serde_json::from_value(ev).unwrap();
        eprintln!(
            "stage noise: {rec:?} -> {} ({})",
            label.name, label.evidence
        );
        assert!(rec.snr_db < 3.0, "snr_db {}", rec.snr_db);
        assert!(!rec.burst);
        assert_eq!(rec.symbol_rate_baud, None);
        assert_eq!(label.layer, 1);
        assert_eq!(label.name, "noise");
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
    }

    /// Run a measure stage over `sends` (a script of frames and pauses) with a
    /// broadcast input of `cap`; its events and the notes it made.
    async fn run_stage(
        cap: usize,
        dc_hz: Option<u64>,
        script: impl FnOnce(broadcast::Sender<Frame>, Provenance) -> futures_script::Script,
    ) -> (Vec<Frame>, Vec<serde_json::Value>) {
        use zeta_core::types::{SourceId, Span, Timestamp};
        let prov = Provenance {
            source: SourceId("s".into()),
            job: None,
            channel: None,
            span: Span {
                start_ns: Timestamp(0),
                end_ns: Timestamp(1),
            },
        };
        let bus = Arc::new(StreamBus::new(16));
        let out = bus.register(StreamKind::Event, prov.clone());
        let mut rx_out = bus.subscribe(&out).unwrap();
        let (tx, rx_in) = broadcast::channel(cap);
        let notes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = notes.clone();
        let ctx = zeta_core::stage::StageContext::new(SourceId("s".into()))
            .with_dc_hz(dc_hz)
            .with_note(Arc::new(move |v| sink.lock().unwrap().push(v)));
        let h = tokio::spawn(async move {
            MeasureStage::new()
                .run(
                    serde_json::json!({"bandwidth_hz": null}),
                    vec![rx_in],
                    vec![out],
                    bus,
                    CancellationToken::new(),
                    ctx,
                )
                .await
        });
        script(tx, prov).await;
        assert!(matches!(h.await.unwrap(), Ok(())));
        let mut events = Vec::new();
        while let Ok(f) = rx_out.try_recv() {
            events.push(f);
        }
        let notes = notes.lock().unwrap().clone();
        (events, notes)
    }

    mod futures_script {
        pub type Script = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
    }

    fn iq_frame(prov: &Provenance, t0: u64, block: &[Complex<f32>]) -> Frame {
        iq_frame_at(prov, t0, RATE, block)
    }

    fn iq_frame_at(prov: &Provenance, t0: u64, rate: u64, block: &[Complex<f32>]) -> Frame {
        use zeta_core::port::IqBlock;
        use zeta_core::types::Timestamp;
        Frame::new(
            StreamId::new(),
            prov.clone(),
            Payload::Iq(IqBlock {
                t0: Timestamp(t0),
                sample_rate_hz: Hz(rate),
                center_hz: Hz(CENTER),
                samples: block.to_vec().into(),
            }),
        )
    }

    fn tone_blocks(n: usize) -> Vec<Vec<Complex<f32>>> {
        let mut iq = crate::util::nco(n * 4_800, 5_000.0, RATE as f64);
        awgn(&mut iq, 20.0, 2);
        iq.chunks(4_800).map(|c| c.to_vec()).collect()
    }

    fn span_ns(f: &Frame) -> u64 {
        f.provenance.span.end_ns.0 - f.provenance.span.start_ns.0
    }

    #[tokio::test]
    async fn measure_stage_emits_a_partial_on_gap() {
        let blocks = tone_blocks(16);
        let pre = 1_000_000_000u64;
        // 5 blocks (0.5 s), a 10 ms gap, then 10 blocks (1 s).
        let post = pre + 500_000_000 + 10_000_000;
        let (events, notes) = run_stage(32, None, |tx, prov| {
            Box::pin(async move {
                for (i, b) in blocks.iter().take(5).enumerate() {
                    tx.send(iq_frame(&prov, pre + i as u64 * 100_000_000, b))
                        .unwrap();
                }
                for (i, b) in blocks.iter().skip(5).take(10).enumerate() {
                    tx.send(iq_frame(&prov, post + i as u64 * 100_000_000, b))
                        .unwrap();
                }
            })
        })
        .await;
        assert_eq!(events.len(), 2);
        let Payload::Event(first) = &events[0].payload else {
            panic!("event")
        };
        assert_eq!(first["partial"], true);
        assert!(span_ns(&events[0]).abs_diff(500_000_000) <= 100_000_000);
        let Payload::Event(second) = &events[1].payload else {
            panic!("event")
        };
        assert_eq!(second["partial"], false);
        assert_eq!(events[1].provenance.span.start_ns.0, post);
        assert_eq!(span_ns(&events[1]), 1_000_000_000);
        assert_eq!(
            notes.last().unwrap(),
            &serde_json::json!({"measure": {"discarded": 0, "partial": 1, "last": "gap", "dc_mask_hz": 0}})
        );
    }

    #[tokio::test]
    async fn measure_stage_drops_a_short_remainder_on_lag() {
        let blocks = tone_blocks(10);
        let (events, notes) = run_stage(8, None, |tx, prov| {
            Box::pin(async move {
                tx.send(iq_frame(&prov, 1_000_000_000, &blocks[0])).unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                // Nine sends into a queue of 8: the receiver lags by one.
                for (i, b) in blocks.iter().skip(1).take(9).enumerate() {
                    tx.send(iq_frame(&prov, 2_000_000_000 + i as u64 * 100_000_000, b))
                        .unwrap();
                }
            })
        })
        .await;
        assert!(events.is_empty(), "0.1 s is under MIN_PARTIAL_S");
        assert_eq!(
            notes.last().unwrap(),
            &serde_json::json!({"measure": {"discarded": 1, "partial": 0, "last": "lag", "dc_mask_hz": 0}})
        );
    }

    #[tokio::test]
    async fn a_partial_window_spans_its_samples() {
        let blocks = tone_blocks(4);
        // 0.3 s, then a block 1 s late: the 0.3 s remainder is a partial.
        let (events, _) = run_stage(16, None, |tx, prov| {
            Box::pin(async move {
                for (i, b) in blocks.iter().take(3).enumerate() {
                    tx.send(iq_frame(&prov, 1_000_000_000 + i as u64 * 100_000_000, b))
                        .unwrap();
                }
                tx.send(iq_frame(&prov, 3_000_000_000, &blocks[3])).unwrap();
            })
        })
        .await;
        assert_eq!(events.len(), 1);
        assert_eq!(span_ns(&events[0]), 300_000_000);
        assert_eq!(events[0].provenance.span.start_ns.0, 1_000_000_000);
        let Payload::Event(ev) = &events[0].payload else {
            panic!("event")
        };
        assert_eq!(ev["partial"], true);
        assert!(ev["snr_db"].as_f64().is_some());
    }

    /// [`measure_selection`] calls made by `f` on this thread.
    async fn measures<T>(f: impl std::future::Future<Output = T>) -> (T, usize) {
        let before = MEASURE_SELECTION_CALLS.with(|c| c.get());
        let out = f.await;
        (out, MEASURE_SELECTION_CALLS.with(|c| c.get()) - before)
    }

    #[tokio::test]
    async fn run_all_measures_the_band_once_for_layers_1_and_2() {
        use crate::catalogue::CatalogueClassifier;
        use crate::fixtures::wfm_modulate;
        use zeta_core::classify::ClassifierSet;

        let audio = tone(48_000, 48_000, 1_000.0, 0.9);
        let mut iq = wfm_modulate(&audio, 48_000, 240_000, 75_000.0, 0.0, true);
        awgn(&mut iq, 20.0, 21);
        let band = Band::whole(Hz(240_000));
        let (center, rate) = (Hz(104_900_000), Hz(240_000));

        // Alone, each classifier measures the band itself.
        let (_, alone) =
            measures(CatalogueClassifier.classify(&iq, rate, center, band, None)).await;
        assert_eq!(alone, 1);

        let mut set = ClassifierSet::default();
        set.register(Arc::new(CatalogueClassifier));
        set.register(Arc::new(Layer1Classifier));
        let (c, n) = measures(set.run_all(&iq, rate, center, band, None, vec![])).await;
        let labels = c.labels;
        eprintln!("run_all: {n} measure(s) -> {labels:?}");
        assert_eq!(n, 1, "the band is measured once per run_all");
        assert_eq!(labels[0].layer, 1);
        assert_eq!(labels[0].name, "wfm", "{labels:?}");
        assert!(
            labels
                .iter()
                .any(|l| l.layer == 2 && l.name == "FM Broadcast Radio"),
            "{labels:?}"
        );
    }

    /// A tone 40 kHz above centre at 0.1 under a DC spike (a constant 0.5) and 20 dB noise.
    fn spike_and_tone(n: usize) -> Vec<Complex<f32>> {
        let mut iq: Vec<Complex<f32>> = crate::util::nco(n, 40_000.0, WIDE_RATE as f64)
            .into_iter()
            .map(|c| c * 0.1 + Complex::new(0.5, 0.0))
            .collect();
        awgn(&mut iq, 20.0, 7);
        iq
    }

    /// A tone 20 kHz above the block centre at 0.1 under a DC spike 20 kHz below it
    /// (a 0.5 tone there: the receiver's DC in a block centred off it) and 20 dB noise.
    fn offset_spike_and_tone(n: usize) -> Vec<Complex<f32>> {
        let tone = crate::util::nco(n, 20_000.0, WIDE_RATE as f64);
        let spike = crate::util::nco(n, -20_000.0, WIDE_RATE as f64);
        let mut iq: Vec<Complex<f32>> = tone
            .into_iter()
            .zip(spike)
            .map(|(t, d)| t * 0.1 + d * 0.5)
            .collect();
        awgn(&mut iq, 20.0, 7);
        iq
    }

    #[test]
    fn measure_band_masked_ignores_the_dc_spike() {
        // The block is centred at CENTER + 20 kHz, the receiver's DC at CENTER.
        let iq = offset_spike_and_tone(WIDE_RATE as usize);
        let block = CENTER + 20_000;
        let bin = WIDE_RATE as f64 / SIZE as f64; // 187.5 Hz
        let plain = measure_band(&iq, WIDE_RATE, block, WIDE_RATE);
        assert!(
            (plain.center_hz.0 as f64 - CENTER as f64).abs() <= bin,
            "{plain:?}"
        );
        let masked = measure_band_masked(&iq, WIDE_RATE, block, WIDE_RATE, Some((CENTER, 25_000)));
        assert!(
            (masked.center_hz.0 as f64 - (CENTER + 40_000) as f64).abs() <= 2.0 * bin,
            "{masked:?}"
        );
        assert!(masked.snr_db > 10.0, "{masked:?}");
        assert_eq!(
            measure_band_masked(&iq, WIDE_RATE, block, WIDE_RATE, None),
            plain
        );
    }

    #[test]
    fn measure_band_masked_with_the_dc_at_the_band_centre_measures_as_today() {
        // A capture centred on the band: the DC is the band's centre, so the mask
        // would hide the signal the radio was tuned for; it is not applied.
        let iq = spike_and_tone(WIDE_RATE as usize);
        assert_eq!(
            measure_band_masked(&iq, WIDE_RATE, CENTER, WIDE_RATE, Some((CENTER, 25_000))),
            measure_band(&iq, WIDE_RATE, CENTER, WIDE_RATE)
        );
    }

    #[test]
    fn measure_band_masked_with_the_mask_over_the_whole_band_measures_as_today() {
        // A 12.5 kHz band around the DC: every in-band bin is within 25 kHz of it.
        let iq = spike_and_tone(WIDE_RATE as usize);
        let plain = measure_band(&iq, WIDE_RATE, CENTER, 12_500);
        assert_eq!(
            measure_band_masked(&iq, WIDE_RATE, CENTER, 12_500, Some((CENTER, 25_000))),
            plain
        );
        // A DC outside the band: not applied either.
        assert_eq!(
            measure_band_masked(
                &iq,
                WIDE_RATE,
                CENTER,
                WIDE_RATE,
                Some((CENTER + 500_000, 25_000))
            ),
            measure_band(&iq, WIDE_RATE, CENTER, WIDE_RATE)
        );
    }

    /// One full second of [`offset_spike_and_tone`] at [`WIDE_RATE`] (blocks centred
    /// at `CENTER`: the spike at `CENTER - 20_000`, the tone at `CENTER + 20_000`) through a measure stage
    /// whose context carries `dc_hz`: the one event and the notes.
    async fn spike_second(dc_hz: Option<u64>) -> (serde_json::Value, Vec<serde_json::Value>) {
        let iq = offset_spike_and_tone(WIDE_RATE as usize);
        let (events, notes) = run_stage(32, dc_hz, |tx, prov| {
            Box::pin(async move {
                let block = WIDE_RATE as usize / 10;
                for (i, b) in iq.chunks(block).enumerate() {
                    tx.send(iq_frame_at(
                        &prov,
                        1_000_000_000 + i as u64 * 100_000_000,
                        WIDE_RATE,
                        b,
                    ))
                    .unwrap();
                }
            })
        })
        .await;
        assert_eq!(events.len(), 1, "one full second");
        let Payload::Event(ev) = &events[0].payload else {
            panic!("event")
        };
        assert_eq!(ev["partial"], false);
        (ev.clone(), notes)
    }

    #[tokio::test]
    async fn measure_stage_masks_the_dc_only_with_a_dc_hz_in_its_context() {
        let bin = WIDE_RATE as f64 / SIZE as f64;
        let (ev, notes) = spike_second(Some(CENTER - 20_000)).await;
        let center = ev["center_hz"].as_u64().unwrap() as f64;
        assert!(
            (center - (CENTER + 20_000) as f64).abs() <= 2.0 * bin,
            "{ev}"
        );
        assert_eq!(
            notes[0],
            serde_json::json!({"measure": {"discarded": 0, "partial": 0, "last": null, "dc_mask_hz": 25_000}})
        );
        let (ev, notes) = spike_second(None).await;
        let center = ev["center_hz"].as_u64().unwrap() as f64;
        assert!((center - (CENTER - 20_000) as f64).abs() <= bin, "{ev}");
        assert_eq!(notes[0]["measure"]["dc_mask_hz"], 0);
    }

    /// `n` hex characters of non-repeating bits (the top nibble of each step
    /// of a 32-bit LCG), for fixtures longer than [`LCG_BITS`].
    fn lcg_hex(n: usize) -> String {
        let mut x: u32 = 0x2545_f491;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                char::from_digit(x >> 28, 16).unwrap()
            })
            .collect()
    }

    fn at_20_db(spec: &SynthSpec) -> Vec<Complex<f32>> {
        let imp = Impairments {
            snr_db: Some(20.0),
            ..Default::default()
        };
        synthesise(spec, &imp, 48_000, 0)
    }

    #[test]
    fn fsk_order_and_tone_spacing_are_measured() {
        for (order, dev, want_spacing) in [(2u8, 4000.0, 8000u64), (4u8, 6000.0, 4000u64)] {
            let spec = SynthSpec::Fsk {
                symbol_rate_baud: 2400,
                deviation_hz: dev,
                order,
                bits: lcg_hex(128),
                repeat: 1,
                gap_ms: 50,
            };
            let iq = at_20_db(&spec);
            let f = measure_band(&iq, 48_000, CENTER, 24_000);
            let l = layer1_label(&f, &iq, 48_000);
            show(&format!("{order}-fsk"), &f, &l);
            assert_eq!(l.name, "fsk", "{order}-fsk: {}", l.evidence);
            assert_eq!(f.order, Some(order));
            let s = f.tone_spacing_hz.unwrap().0;
            assert!(
                (s as f64 - want_spacing as f64).abs() < 600.0,
                "spacing {s}"
            );
        }
    }

    #[test]
    fn psk_order_is_the_smallest_power_with_a_line() {
        for order in [2u8, 4u8] {
            let spec = SynthSpec::Psk {
                symbol_rate_baud: 2400,
                order,
                bits: lcg_hex(160),
                repeat: 1,
                gap_ms: 50,
            };
            let iq = at_20_db(&spec);
            let f = measure_band(&iq, 48_000, CENTER, 12_000);
            let l = layer1_label(&f, &iq, 48_000);
            show(&format!("{order}-psk"), &f, &l);
            assert_eq!(l.name, "psk", "{order}-psk: {}", l.evidence);
            assert_eq!(f.order, Some(order));
        }
    }

    #[test]
    fn ook_is_labelled_ook() {
        let spec = SynthSpec::Ook {
            symbol_rate_baud: 1200,
            bits: lcg_hex(120),
            repeat: 1,
            gap_ms: 50,
        };
        let iq = at_20_db(&spec);
        let f = measure_band(&iq, 48_000, CENTER, 8_000);
        let l = layer1_label(&f, &iq, 48_000);
        show("ook", &f, &l);
        assert_eq!(l.name, "ook", "{}", l.evidence);
    }

    #[test]
    fn under_the_noise_snr_the_record_carries_no_order() {
        // Without the 500 Hz minimum spacing these noise windows read order 2
        // or 4 with a tone spacing of a few Hz (seeds 0-3 and 5).
        for seed in 0..6u64 {
            let mut iq = vec![Complex::new(0.0f32, 0.0); RATE as usize];
            awgn(&mut iq, 0.0, seed);
            for bw in [RATE, 12_500] {
                let f = measure_band(&iq, RATE, CENTER, bw);
                assert!(f.snr_db < NOISE_SNR_DB, "seed {seed}: {f:?}");
                assert_eq!(f.order, None, "seed {seed}, {bw} Hz: {f:?}");
                assert_eq!(f.tone_spacing_hz, None, "seed {seed}, {bw} Hz: {f:?}");
            }
        }
    }

    #[test]
    fn a_2_fsk_under_the_noise_snr_carries_no_order() {
        let spec = SynthSpec::Fsk {
            symbol_rate_baud: 2400,
            deviation_hz: 4000.0,
            order: 2,
            bits: lcg_hex(128),
            repeat: 1,
            gap_ms: 50,
        };
        let measured = |snr_db: f32| {
            let imp = Impairments {
                snr_db: Some(snr_db),
                ..Default::default()
            };
            let iq = synthesise(&spec, &imp, 48_000, 0);
            measure_band(&iq, 48_000, CENTER, 24_000)
        };
        let strong = measured(20.0);
        assert_eq!(strong.order, Some(2), "{strong:?}");
        let weak = measured(WEAK_FSK_SNR_DB);
        eprintln!("weak 2-fsk: {weak:?}");
        assert!(
            (2.0..NOISE_SNR_DB).contains(&weak.snr_db),
            "snr_db {}",
            weak.snr_db
        );
        assert_eq!(weak.order, None, "{weak:?}");
        assert_eq!(weak.tone_spacing_hz, None, "{weak:?}");
    }

    /// Fixture SNR (over the whole 48 kHz) that measures a 2-FSK block at about
    /// 4 dB, under `NOISE_SNR_DB`.
    const WEAK_FSK_SNR_DB: f32 = -14.0;

    #[test]
    fn a_constant_envelope_burst_train_is_not_ook() {
        // Three 1 s bursts of 2400 Bd 2-FSK on an alternating pattern (its
        // instantaneous frequency is a tone, so no FSK clusters) 950 ms apart:
        // the gaps between bursts are not an off level.
        let spec = SynthSpec::Fsk {
            symbol_rate_baud: 2400,
            deviation_hz: 2400.0,
            order: 2,
            bits: "pattern:aa".into(),
            repeat: 3,
            gap_ms: 950,
        };
        let iq = at_20_db(&spec);
        let f = measure_band(&iq, 48_000, CENTER, 20_000);
        let l = layer1_label(&f, &iq, 48_000);
        show("fsk burst train", &f, &l);
        assert_ne!(l.name, "ook", "{}", l.evidence);
    }

    #[test]
    fn a_one_bin_band_just_over_the_noise_snr_carries_no_order() {
        // 2-FSK at -12 dB measures just over the noise SNR on a one-bin
        // (47 Hz) band; its histogram's clusters (once order 4, 4 Hz apart)
        // are closer than any FSK tone pair.
        let spec = SynthSpec::Fsk {
            symbol_rate_baud: 2400,
            deviation_hz: 4000.0,
            order: 2,
            bits: lcg_hex(128),
            repeat: 1,
            gap_ms: 50,
        };
        let imp = Impairments {
            snr_db: Some(-12.0),
            ..Default::default()
        };
        let iq = synthesise(&spec, &imp, 48_000, 0);
        let f = measure_band(&iq, 48_000, CENTER, 24_000);
        eprintln!("one-bin 2-fsk: {f:?}");
        assert!(f.snr_db >= NOISE_SNR_DB, "snr_db {}", f.snr_db);
        assert_eq!(f.order, None, "{f:?}");
        assert_eq!(f.tone_spacing_hz, None, "{f:?}");
    }

    /// The veto on its own: fitted over the whole window (as a gate holding under
    /// `OOK_REFIT_MAX_DUTY` would have it, bypassing that skip), patterned PSK bursts and
    /// POCSAG over NBFM at 8 dB do fit run lengths, and each fit is vetoed: the PSK
    /// bursts' off runs are their envelope dips, signal well over the noise, and POCSAG's
    /// narrow band holds one tone of two (FSK tones in twice the band).
    #[test]
    fn the_veto_rejects_the_run_fits_of_psk_bursts_and_nbfm_pocsag() {
        let psk = |order| SynthSpec::Psk {
            symbol_rate_baud: 2_400,
            order,
            bits: "a5c3f0e1a5c3f0e1".into(),
            repeat: 8,
            gap_ms: 100,
        };
        let pocsag = SynthSpec::Pocsag {
            address: 1_234_567,
            function: 3,
            text: "HELLO WORLD THIS IS A PAGE".into(),
            baud: 2_400,
        };
        for (what, spec, snr) in [
            ("bpsk bursts 30 dB", psk(2), 30.0),
            ("qpsk bursts 30 dB", psk(4), 30.0),
            ("qpsk bursts 8 dB", psk(4), 8.0),
            ("pocsag 2400 8 dB", pocsag, 8.0),
        ] {
            let imp = Impairments {
                snr_db: Some(snr),
                ..Default::default()
            };
            let iq = synthesise(&spec, &imp, RATE as u32, 0);
            let f = measure(&iq, RATE, CENTER);
            let v = gated_view(&iq, RATE, RATE);
            let band = (f.center_hz.0 as f64 - CENTER as f64, f.bandwidth_hz.0);
            let fit = ook_unit_rate(&v.runs, RATE, band)
                .or_else(|| ook_unit_rate(&[&iq[..]], RATE, band))
                .unwrap_or_else(|| panic!("{what}: no fit to veto: {f:?}"));
            eprintln!("{what}: {fit:?}");
            assert!(ook_veto(&fit, &v, RATE, band), "{what}: {fit:?}");
        }
    }
    /// The Rice envelope's CV: Rayleigh's `sqrt(4 / pi - 1)` with no carrier, falling to
    /// `1 / sqrt(2 snr)` under a strong one, and between them a value checked against a
    /// carrier in generated noise.
    #[test]
    fn rice_cv_is_the_envelope_cv_of_a_carrier_in_noise() {
        let rayleigh = (4.0 / std::f64::consts::PI - 1.0).sqrt();
        assert!((rice_cv(0.0) - rayleigh).abs() < 1e-9, "{}", rice_cv(0.0));
        for k in [30.0f64, 39.9, 40.1, 100.0, 1_000.0] {
            let asymptote = (1.0 / (2.0 * k)).sqrt();
            let got = rice_cv(k);
            assert!((got / asymptote - 1.0).abs() < 0.02, "snr {k}: {got}");
        }
        for snr_db in [0.0f32, 3.0, 6.0, 9.0, 12.0, 20.0] {
            let mut iq = vec![Complex::new(1.0f32, 0.0); 400_000];
            awgn(&mut iq, snr_db, 5);
            let (measured, model) = (
                f64::from(envelope_cv(&iq)),
                rice_cv(10f64.powf(f64::from(snr_db) / 10.0)),
            );
            eprintln!("{snr_db} dB: measured {measured:.4}, rice {model:.4}");
            assert!((measured - model).abs() < 0.005, "{snr_db} dB");
            // The share is taken `NOISE_SLACK_DB` short, so a little is left: under rule
            // 10's 0.2 at every level, with the SNR read 0.5 dB over too (`noise_beside`
            // reads it within 0.3 dB, and errs the other way).
            let snr = 10f64.powf(f64::from(snr_db) / 10.0);
            let left = cv_over_noise(measured as f32, Some(snr));
            let low = cv_over_noise(measured as f32, Some(snr * 10f64.powf(0.05)));
            eprintln!("{snr_db} dB: {left:.3} left, {low:.3} with the SNR read 0.5 dB over");
            assert!(left < 0.16 && low < FM_MAX_CV, "{snr_db} dB: {left} {low}");
            // From 3 dB, an AM tone at index 0.2 (0.141, of which about snr / (1 + snr)
            // shows in variance) on top keeps rule 9's 0.1 with the SNR read 1 dB under.
            let am = (measured.powi(2) + 0.141f64.powi(2) * snr / (1.0 + snr)).sqrt();
            let kept = cv_over_noise(am as f32, Some(snr * 10f64.powf(-0.1)));
            eprintln!("{snr_db} dB: an index of 0.2 keeps {kept:.3} with the SNR read 1 dB under");
            assert!(snr_db < 3.0 || kept >= AM_LOW_MIN_CV, "{snr_db} dB: {kept}");
        }
        // Without an SNR the CV is as it is.
        assert_eq!(cv_over_noise(0.3, None), 0.3);
    }

    #[test]
    fn erfc_matches_known_values() {
        for (x, want) in [
            (0.0, 1.0),
            (0.5, 0.479_500_1),
            (1.0, 0.157_299_2),
            (2.0, 0.004_677_73),
        ] {
            assert!((erfc(x) - want).abs() < 2e-7, "erfc({x}) = {}", erfc(x));
        }
    }

    /// Rice's click rate: a 12.5 kHz band's radius of gyration is 3.6 kHz, so at 6 dB the
    /// noise clicks 17 times a second and at 12 dB once in two hours.
    #[test]
    fn clicks_fall_away_above_the_fm_threshold() {
        let per_hz =
            |snr_db: f64| click_hz2(10f64.powf(snr_db / 10.0), 12_500.0, 1_050.0) / 2_000.0;
        assert!((per_hz(6.0) - 17.2).abs() < 0.5, "{}", per_hz(6.0));
        assert!(per_hz(12.0) < 1.0 / 7_000.0, "{}", per_hz(12.0));
        assert!(per_hz(3.0) > 100.0, "{}", per_hz(3.0));
        // No audio band, no clicks counted.
        assert_eq!(click_hz2(1.0, 12_500.0, 40.0), 0.0);
    }

    /// The noise is read beside the signal: a carrier in white noise through a channel
    /// reads its SNR within 0.3 dB from 0 to 20 dB, whole or channelised; a signal filling its
    /// band shows no noise, so nothing is taken from its CV.
    #[test]
    fn noise_beside_reads_the_snr_of_the_measured_samples() {
        for snr_db in [0.0f32, 3.0, 6.0, 12.0, 20.0] {
            let mut iq = vec![Complex::new(1.0f32, 0.0); 96_000];
            awgn(&mut iq, snr_db, 9);
            let (_, _, v) = measure_band_noted(&iq, RATE, CENTER, RATE, None);
            let got = 10.0 * v.band.unwrap().noise.unwrap().snr.log10();
            eprintln!("whole {snr_db} dB: {got:.2} dB");
            assert!((got - f64::from(snr_db)).abs() < 0.3, "{snr_db} dB: {got}");
            // In a 12.5 kHz channel the noise is 5.8 dB less.
            let (chan, rate) = extract_offset(&iq, RATE, 0, 12_500, RATE).unwrap();
            let (_, _, v) = measure_band_noted(&chan, rate, CENTER, 12_500, None);
            let noise = v.band.unwrap().noise.unwrap();
            let got = 10.0 * noise.snr.log10();
            eprintln!(
                "channel {snr_db} dB: {got:.2} dB, noise {:.0} Hz wide",
                noise.width_hz
            );
            assert!(
                (got - f64::from(snr_db) - 5.84).abs() < 0.3,
                "{snr_db} dB: {got}"
            );
            assert!(
                (11_000.0..15_000.0).contains(&noise.width_hz),
                "{}",
                noise.width_hz
            );
        }
        // Tones every 400 Hz across the 3 kHz band they are read through, 40 dB over the
        // noise: the occupied run is the band, the bins between its tones are not noise
        // beside it, and its CV is left as it is.
        let mut comb: Vec<Complex<f32>> = (0..96_000)
            .map(|i| {
                (-3..=3)
                    .map(|k| {
                        let turns = f64::from(k) * 400.0 * i as f64 / RATE as f64;
                        let (sn, cs) = (2.0 * std::f64::consts::PI * turns.fract()).sin_cos();
                        Complex::new(cs as f32, sn as f32) * (1.0 + 0.1 * k as f32)
                    })
                    .sum()
            })
            .collect();
        awgn(&mut comb, 40.0, 3);
        let (chan, rate) = extract_offset(&comb, RATE, 0, 3_000, RATE).unwrap();
        let (f, _, v) = measure_band_noted(&chan, rate, CENTER, 3_000, None);
        let noise = v.band.unwrap().noise;
        eprintln!("comb: {f:?}, cv {:.3}, {noise:?}", v.cv);
        assert!(v.cv > 0.4, "{}", v.cv);
        assert!(
            cv_over_noise(v.cv, noise.map(|n| n.snr)) > v.cv - 0.005,
            "{noise:?}"
        );
    }

    /// The slow gate: a carrier 3 dB over a 12.5 kHz channel's noise, up for 60 % of the
    /// window, is found to within 10 ms an edge where single chunks give a duty of 0.2;
    /// noise alone and a steady weak carrier are one level, and a train of 30 ms frames
    /// 9 ms apart stays a train of short runs, ungated.
    #[test]
    fn the_slow_gate_finds_a_weak_keyed_carrier_and_leaves_a_frame_train() {
        use super::burst::level_runs;
        use crate::fixtures::{KeyedOver, keyed_over};
        let weak = |on: (f64, f64), snr_db: f32, seed: u64| {
            let iq = keyed_over(&KeyedOver {
                rate_hz: RATE as u32,
                seconds: 2.0,
                deviation_hz: 2_500.0,
                audio: 0.0,
                on,
                // 3 dB in 12.5 kHz is 2.8 dB under the noise of the whole rate.
                snr_db: snr_db - 5.84,
                seed,
            });
            extract_offset(&iq, RATE, 0, 12_500, RATE).unwrap().0
        };
        for seed in [1, 2, 3] {
            let iq = weak((0.4, 1.0), 3.0, seed);
            let fast = burst_runs(&iq, RATE);
            let slow = level_runs(&iq, RATE).expect("two levels");
            eprintln!(
                "seed {seed}: fast duty {:.2}, slow {:?}",
                fast.duty, slow.runs
            );
            assert!(fast.duty < 0.4, "{}", fast.duty);
            assert_eq!(slow.runs.len(), 1, "{slow:?}");
            let (start, end) = slow.runs[0];
            assert!(start.abs_diff(38_400) <= 480 && end == iq.len(), "{slow:?}");
            let v = gated_view(&iq, RATE, 12_500);
            assert!(
                v.gated && (v.bursts.duty - 0.6).abs() < 0.01,
                "{:?}",
                v.bursts
            );
            // One level: nothing to split.
            let steady = weak((0.0, 1.0), 3.0, seed);
            assert_eq!(level_runs(&steady, RATE), None);
            let noise = weak((0.0, 0.0), 3.0, seed);
            assert_eq!(level_runs(&noise, RATE), None);
        }
        // 30 ms frames of a carrier 9 ms apart, 6 dB over the noise: the average joins
        // them, half an average between them is under the split, and they are cut apart
        // again: no long run, no gate.
        let mut train = vec![Complex::new(0.0f32, 0.0); 96_000];
        awgn(&mut train, 0.0, 4);
        let frame = (RATE as usize * 39) / 1_000;
        for (i, s) in train.iter_mut().enumerate().skip(4_800).take(86_400) {
            if (i - 4_800) % frame < RATE as usize * 30 / 1_000 {
                *s += Complex::new(2.0, 0.0);
            }
        }
        let (train, _) = extract_offset(&train, RATE, 0, 12_500, RATE).unwrap();
        let fast = burst_runs(&train, RATE);
        eprintln!(
            "train: fast duty {:.2}, {} runs",
            fast.duty,
            fast.runs.len()
        );
        let slow = level_runs(&train, RATE);
        assert!(
            slow.as_ref()
                .is_none_or(|b| b.burst_ms.is_some_and(|ms| ms < 40.0)),
            "{slow:?}"
        );
        assert!(!gated_view(&train, RATE, 12_500).gated);
    }

    /// The slow gate where only it can gate, 3 to 6 dB inside a 12.5 kHz channel:
    /// - a train of 30 ms frames 9 ms apart is not joined into one long run (its gaps
    ///   are at the off level), so no long run gates the window;
    /// - a steady carrier with a 30 ms fade to the noise every 250 ms is cut at its
    ///   fades into runs of over 100 ms: gated to where it is up, duty about 0.88.
    #[test]
    fn the_slow_gate_at_3_to_6_db_leaves_a_frame_train_and_cuts_at_deep_fades() {
        use super::burst::level_runs;
        for snr_db in [3.0f64, 4.5, 6.0] {
            // A carrier `snr_db` over the noise of 12.5 kHz: the noise of the whole rate
            // is 5.84 dB more.
            let amp = 10f64.powf((snr_db - 5.84) / 20.0) as f32;
            let shaped = |up: &dyn Fn(usize) -> bool, seed: u64| {
                let mut iq = vec![Complex::new(0.0f32, 0.0); 96_000];
                awgn(&mut iq, 0.0, seed);
                for (_, s) in iq.iter_mut().enumerate().filter(|&(i, _)| up(i)) {
                    *s += Complex::new(amp, 0.0);
                }
                extract_offset(&iq, RATE, 0, 12_500, RATE).unwrap().0
            };
            let ms = |i: usize| i * 1_000 / RATE as usize;
            for seed in [4, 5, 6] {
                let train = shaped(
                    &|i| (4_800..91_200).contains(&i) && ms(i - 4_800) % 39 < 30,
                    seed,
                );
                let fast = burst_runs(&train, RATE);
                let slow = level_runs(&train, RATE);
                let v = gated_view(&train, RATE, 12_500);
                eprintln!(
                    "train {snr_db} dB seed {seed}: fast duty {:.2} in {} runs, slow {:?}, gated {} duty {:.2}",
                    fast.duty,
                    fast.runs.len(),
                    slow.as_ref().map(|b| (b.duty, b.runs.len(), b.burst_ms)),
                    v.gated,
                    v.bursts.duty
                );
                assert!(
                    !v.gated || v.bursts.burst_ms.is_some_and(|ms| ms < 100.0),
                    "{snr_db} dB seed {seed}: {:?}",
                    v.bursts
                );
                let fading = shaped(&|i| ms(i) % 250 >= 30, seed);
                let v = gated_view(&fading, RATE, 12_500);
                eprintln!(
                    "fading {snr_db} dB seed {seed}: gated {}, duty {:.2}, {} runs of {:?} ms",
                    v.gated,
                    v.bursts.duty,
                    v.bursts.runs.len(),
                    v.bursts.burst_ms
                );
                assert!(v.gated, "{snr_db} dB seed {seed}: {:?}", v.bursts);
                assert!((v.bursts.duty - 0.88).abs() < 0.05, "{:?}", v.bursts.duty);
                assert!(v.bursts.runs.len() >= 7, "{:?}", v.bursts.runs.len());
            }
        }
    }
}
