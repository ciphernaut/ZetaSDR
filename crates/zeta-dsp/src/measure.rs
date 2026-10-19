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
use zeta_core::classify::{Band, Classifier, FeatureRecord, Label};
use zeta_core::ops::channel_rate_hz;
use zeta_core::stage::{Backend, Stage, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Baud, Hz, Provenance, StreamId, StreamKind, Timestamp};

use crate::channel::extract_offset;
use crate::fft::{Window, power_spectrum_db};
use crate::{Input, frame_span, run_map};

/// FFT size of every spectrum taken here.
const SIZE: usize = 1024;
/// Occupied band: bins at or above the noise floor plus this many dB.
const OCCUPIED_DB: f32 = 6.0;
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
/// FM needs discriminator audio (band-limited to half the occupied band) with
/// an RMS deviation above this; below it a constant-envelope signal is a carrier.
const FM_MIN_DEV_HZ: f64 = 100.0;
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
/// least this, with a carrier line and less RMS phase deviation than cv.
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
const FSK_SEP_HZ: (f64, f64) = (500.0, 3_000.0);

/// Noise floor of [`measure_band`]: this percentile of the in-band bins. A
/// signal may fill more than half of a channel (APRS: Carson ~10.4 kHz in
/// 12.5 kHz), so the median would be signal, not noise.
const FLOOR_PERCENTILE: usize = 15;

/// The `pct`th percentile of `v` (the value at index `len · pct / 100` when
/// sorted); 0 when empty.
fn percentile(v: &[f32], pct: usize) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(f32::total_cmp);
    s[(s.len() * pct / 100).min(s.len() - 1)]
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
    let n = x.len();
    if n == 0 {
        return Vec::new();
    }
    let mut planner = FftPlanner::<f32>::new();
    let mut buf: Vec<Complex<f32>> = x.iter().map(|&v| Complex::new(v, 0.0)).collect();
    planner.plan_fft_forward(n).process(&mut buf);
    let max_bin = (max_hz * n as f64 / rate_hz as f64).floor() as usize;
    for (k, b) in buf.iter_mut().enumerate().skip(1) {
        if k > max_bin {
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

/// Strongest line (a local maximum) of the keying signal between
/// `MIN_LINE_HZ` and `rate / 2` (half the occupied band on the FM path) that stands `LINE_DB` above that range's median and also `LINE_DB` above
/// the median of its own neighbourhood (`LOCAL_BINS` either side, its main
/// lobe excluded), so a coloured noise hump is not mistaken for a line. The
/// frequency is refined by a parabola through the peak bin and its neighbours.
///
/// On an HDLC flag-only stretch (e.g. an APRS preamble of 0x7E flags) the
/// keying repeats once per 8-bit flag, so the strongest line is the byte rate
/// (150 Bd for 1200 bit/s), not the bit rate; the full APRS packet reports 1200.
fn symbol_rate(iq: &[Complex<f32>], rate_hz: u64, cv: f32, bandwidth_hz: u64) -> Option<Baud> {
    let db = real_spectrum(&keying_signal(iq, rate_hz, cv, bandwidth_hz))?;
    let bin_hz = rate_hz as f64 / SIZE as f64;
    let lo = (MIN_LINE_HZ / bin_hz).ceil() as usize;
    // Under FM the keying cannot be faster than the highest modulating
    // frequency, which Carson's rule puts at half the occupied band.
    let hi = if cv < FM_MAX_CV {
        ((bandwidth_hz as f64 / 2.0 / bin_hz) as usize + 1).min(db.len())
    } else {
        db.len()
    };
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
    let delta = if k + 1 < db.len() {
        let (a, b, c) = (db[k - 1] as f64, db[k] as f64, db[k + 1] as f64);
        let den = a - 2.0 * b + c;
        if den < 0.0 {
            (0.5 * (a - c) / den).clamp(-0.5, 0.5)
        } else {
            0.0
        }
    } else {
        0.0
    };
    Some(Baud(((k as f64 + delta) * bin_hz).round() as u32))
}

/// Measure one IQ block centred at `block_center_hz`.
///
/// From the `SIZE`-bin Hann power spectrum: noise floor = 15th-percentile bin;
/// `snr_db` = peak − floor; occupied band = the run of bins around the peak at
/// or above floor + 6 dB, giving `bandwidth_hz` (run length × bin width, at
/// least one bin) and `center_hz` (block centre plus the run's centre offset,
/// clamped at 0). `burst` compares 10 ms chunk powers; `symbol_rate_baud` is
/// the keying line of [`keying_signal`].
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
    if iq.is_empty() || rate_hz == 0 {
        return FeatureRecord {
            bandwidth_hz: Hz(0),
            center_hz: Hz(block_center_hz),
            symbol_rate_baud: None,
            snr_db: 0.0,
            burst: false,
        };
    }
    let bin_hz = rate_hz as f64 / SIZE as f64;
    let half_bins = ((bandwidth_hz as f64 / 2.0 / bin_hz).floor() as usize).min(SIZE / 2);
    let first = SIZE / 2 - half_bins;
    let db = &spectrum(iq)[first..(SIZE / 2 + half_bins + 1).min(SIZE)];
    let floor = percentile(db, FLOOR_PERCENTILE);
    let peak = argmax(db);
    let gap = (GAP_HZ / bin_hz).floor() as usize;
    let (lo, hi) = occupied(db, peak, floor + OCCUPIED_DB, gap);
    let (lo, hi) = (lo + first, hi + first);
    let bins = (hi - lo + 1) as f64;
    let offset = ((lo + hi) as f64 / 2.0 - (SIZE / 2) as f64) * bin_hz;
    let center = (block_center_hz as i64)
        .saturating_add(offset.round() as i64)
        .max(0);
    let cv = envelope_cv(iq);
    let bandwidth_hz = (bins * bin_hz).round().max(bin_hz.round()) as u64;
    FeatureRecord {
        bandwidth_hz: Hz(bandwidth_hz),
        center_hz: Hz(center as u64),
        symbol_rate_baud: symbol_rate(iq, rate_hz, cv, bandwidth_hz),
        snr_db: db[peak] - floor,
        burst: is_burst(iq, rate_hz),
    }
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

/// Conservative layer-1 label for a measured block. Rules, first match wins:
///
/// 1. `"noise"` (0.5) when `snr_db < 6`.
/// 2. `"fsk"` (0.6) when the power spectrum has exactly two dominant peaks
///    inside the occupied band 500–3000 Hz apart and within 6 dB of each
///    other, or, for a constant-envelope signal (CV below 0.2) with a keying
///    line, when its discriminator audio does (AFSK on FM).
/// 3. `"wfm"` (0.9) when the occupied band is 40–250 kHz and
///    [`pilot_line_db`] finds a 19 kHz pilot at least 10 dB above its
///    region, with no envelope or deviation gate (multipath and noise ripple
///    a real station's envelope; the pilot is the stronger evidence).
///    Otherwise `"wfm"` (0.6) when the occupied band is 60–250 kHz, the
///    envelope CV is below 0.45 and the discriminator audio deviates by more
///    than 5 kHz RMS.
/// 4. `"am"` (0.6) when a carrier line sits at the block centre (the
///    strongest of the centre bin and its two neighbours at least 6 dB above
///    the mean power of the bins 1–5 kHz either side) and the envelope CV is
///    at least 0.25.
/// 5. `"ssb"` (0.6) when there is no such centre line, the envelope CV is at
///    least 0.25, the occupied band is 1.5–4 kHz and at least 80 % of its
///    power lies on one side of centre; the evidence names `usb` (above) or
///    `lsb` (below) and the dial frequency [`ssb_dial_hz`] gives.
/// 6. `"am"` (0.6) for low-index AM: the same centre line, an envelope CV
///    of at least 0.1 both whole-band and within half the occupied band (so
///    wideband noise on a bare carrier does not pass), and an RMS phase
///    deviation of the discriminator audio (radians per sample, band-limited
///    as for `fm`) below the CV, so the envelope moves more than the phase.
/// 7. `"fm"` (0.6) when the envelope CV is below 0.2, the band is at most
///    25 kHz and the discriminator audio deviates by more than 100 Hz RMS;
///    `"carrier"` (0.7) under the same gate when it does not.
/// 8. Otherwise `"unknown"` (0.5).
pub fn layer1_label(f: &FeatureRecord, iq: &[Complex<f32>], rate_hz: u64) -> Label {
    let cv = envelope_cv(iq);
    let (name, confidence, reason) = classify_block(f, iq, rate_hz, cv);
    let baud = f
        .symbol_rate_baud
        .map_or_else(|| "none".to_string(), |b| format!("{} Bd", b.0));
    Label {
        layer: 1,
        name: name.into(),
        confidence,
        evidence: format!(
            "{reason}; snr_db {:.1} dB, bandwidth_hz {} Hz, symbol_rate_baud {baud}, cv {cv:.3} (envelope std/mean)",
            f.snr_db, f.bandwidth_hz.0
        ),
    }
}

/// RMS frequency deviation, in Hz, of the discriminator audio with its mean
/// (the carrier's offset from centre) removed, band-limited to half the
/// occupied band.
fn audio_rms_hz(iq: &[Complex<f32>], rate_hz: u64, bandwidth_hz: u64) -> f64 {
    let mut d = discriminator(iq);
    let mean = d.iter().map(|&v| v as f64).sum::<f64>() / d.len().max(1) as f64;
    d.iter_mut().for_each(|v| *v -= mean as f32);
    let audio = analytic(&d, rate_hz, bandwidth_hz as f64 / 2.0);
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

fn classify_block(
    f: &FeatureRecord,
    iq: &[Complex<f32>],
    rate_hz: u64,
    cv: f32,
) -> (&'static str, f32, String) {
    if f.snr_db < NOISE_SNR_DB || iq.is_empty() || rate_hz == 0 {
        return ("noise", 0.5, format!("snr below {NOISE_SNR_DB} dB"));
    }
    let bin_hz = rate_hz as f64 / SIZE as f64;
    let db = spectrum(iq);
    let floor = median(&db);
    let gap = (GAP_HZ / bin_hz).floor() as usize;
    let (lo, hi) = occupied(&db, argmax(&db), floor + OCCUPIED_DB, gap);
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
        let dev = audio_rms_hz(iq, rate_hz, bw);
        if dev > WFM_MIN_DEV_HZ {
            return (
                "wfm",
                0.6,
                format!("wideband constant envelope, rms deviation {dev:.0} Hz"),
            );
        }
    }
    if cv >= AM_MIN_CV {
        let carrier_db = centre_line_db(&db, bin_hz);
        if carrier_db >= OCCUPIED_DB {
            return (
                "am",
                0.6,
                format!("carrier line {carrier_db:.1} dB above the sidebands, cv {cv:.3}"),
            );
        }
        if (SSB_BW_HZ.0..=SSB_BW_HZ.1).contains(&f.bandwidth_hz.0) {
            let power = |k: usize| 10f64.powf(db[k] as f64 / 10.0);
            let below: f64 = (lo..=hi).filter(|&k| k < SIZE / 2).map(power).sum();
            let above: f64 = (lo..=hi).filter(|&k| k > SIZE / 2).map(power).sum();
            let total: f64 = (lo..=hi).map(power).sum();
            for (side, p) in [("usb", above), ("lsb", below)] {
                if total > 0.0 && p / total >= SSB_ONE_SIDE {
                    return (
                        "ssb",
                        0.6,
                        format!(
                            "{side}: no carrier line ({carrier_db:.1} dB), {:.0} % of the occupied power on one side, cv {cv:.3}, dial \u{2248} {} Hz ({side})",
                            100.0 * p / total,
                            ssb_dial_hz(f.center_hz.0, side == "usb")
                        ),
                    );
                }
            }
        }
    }
    if cv >= AM_LOW_MIN_CV {
        let carrier_db = centre_line_db(&db, bin_hz);
        if carrier_db >= OCCUPIED_DB {
            let phase_rms = (audio_rms_hz(iq, rate_hz, bw) * 2.0 * std::f64::consts::PI
                / rate_hz as f64) as f32;
            let band_cv = envelope_band_cv(iq, rate_hz, bw);
            if phase_rms < cv && band_cv >= AM_LOW_MIN_CV {
                return (
                    "am",
                    0.6,
                    format!(
                        "carrier line {carrier_db:.1} dB above the sidebands, cv {cv:.3} ({band_cv:.3} in band), phase rms {phase_rms:.4} rad below cv"
                    ),
                );
            }
        }
    }
    if cv < FM_MAX_CV && f.bandwidth_hz.0 <= FM_MAX_BW_HZ {
        let dev = audio_rms_hz(iq, rate_hz, f.bandwidth_hz.0);
        if dev < FM_MIN_DEV_HZ {
            return (
                "carrier",
                0.7,
                format!(
                    "constant envelope, narrow band, no discriminator audio (rms deviation {dev:.0} Hz)"
                ),
            );
        }
        return (
            "fm",
            0.6,
            format!("constant envelope, narrow band, rms deviation {dev:.0} Hz"),
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

/// Accumulates IQ until it holds at least one second (`rate_hz` samples),
/// then emits one Event for that window: its [`FeatureRecord`] fields plus
/// `"label"`, its [`layer1_label`], with the provenance span covering the
/// window. A change of sample rate or centre frequency, or a discontinuity in
/// time (a block whose `t0` is more than one sample away from where the window
/// expects it, e.g. after the stage lagged and frames were skipped), discards
/// the partial window without an event and starts a new window from that
/// block, so a window never stitches across a gap; a partial window left when
/// the input closes is not measured.
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
    ) -> Result<(), StageError> {
        let p: MeasureParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("measure params: {e}")))?;
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        // Window under construction: first frame's provenance, t0, rate, centre.
        let mut window: Option<(Provenance, Timestamp, u64, u64)> = None;
        let mut buf: Vec<Complex<f32>> = Vec::new();
        run_map(inputs, bus, cancel, move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    window = None;
                    buf.clear();
                    return Ok(Vec::new());
                }
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
            if gap
                || window
                    .as_ref()
                    .is_some_and(|w| (w.2, w.3) != (rate, center))
            {
                window = None;
                buf.clear();
            }
            let (prov, t0, _, _) = window
                .get_or_insert_with(|| (frame.provenance.clone(), block.t0, rate, center))
                .clone();
            buf.extend_from_slice(&block.samples);
            if rate == 0 || (buf.len() as u64) < rate {
                return Ok(Vec::new());
            }
            let f = measure_band(&buf, rate, center, p.bandwidth_hz.unwrap_or(rate));
            let label = layer1_label(&f, &buf, rate);
            let mut ev = serde_json::to_value(&f)
                .map_err(|e| StageError::Failed(format!("measure: {e}")))?;
            ev["label"] = serde_json::to_value(&label)
                .map_err(|e| StageError::Failed(format!("measure: {e}")))?;
            let mut prov = prov;
            prov.span = frame_span(t0, buf.len(), rate);
            window = None;
            buf.clear();
            Ok(vec![Frame::new(out.clone(), prov, Payload::Event(ev))])
        })
        .await
    }
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
    if band.is_whole(Hz(rate_hz)) {
        let f = measure(iq, rate_hz, center_hz);
        let l = layer1_label(&f, iq, rate_hz);
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
    let f = measure_band(&chan, out_rate, band_center, bw);
    let l = layer1_label(&f, &chan, out_rate);
    Some((f, l))
}

/// Layer-1 classifier: [`measure_selection`] on the requested band; upstream
/// features are not used, the band is measured afresh.
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
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::fixtures::{aprs_packet_iq, awgn, nbfm_modulate, tone};

    const RATE: u64 = 48_000;
    const CENTER: u64 = 144_800_000;
    const BIN_HZ: f64 = RATE as f64 / 1024.0;

    fn show(what: &str, f: &FeatureRecord, l: &Label) {
        eprintln!("{what}: {f:?} -> {} ({})", l.name, l.evidence);
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
        // Local-maximum guard: the on/off gating leaves a 1/f skirt, not a line.
        assert_eq!(f.symbol_rate_baud, None);
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

    #[tokio::test]
    async fn measure_stage_discards_window_on_gap() {
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
        let (tx, rx_in) = broadcast::channel(32);
        let h = tokio::spawn(async move {
            MeasureStage::new()
                .run(
                    serde_json::json!({"bandwidth_hz": null}),
                    vec![rx_in],
                    vec![out],
                    bus,
                    CancellationToken::new(),
                )
                .await
        });
        let mut iq = vec![Complex::new(0.0f32, 0.0); 16 * 4_800];
        awgn(&mut iq, 0.0, 2);
        // 5 contiguous 100 ms blocks from 1 s, then a block 1 s late, then
        // 10 contiguous blocks after it.
        let pre_gap = 1_000_000_000u64;
        let post_gap = pre_gap + 5 * 100_000_000 + 1_000_000_000;
        let t0s = (0..5)
            .map(|i| pre_gap + i * 100_000_000)
            .chain((0..11).map(|i| post_gap + i * 100_000_000));
        for (block, t0) in iq.chunks(4_800).zip(t0s) {
            tx.send(Frame::new(
                StreamId::new(),
                prov.clone(),
                Payload::Iq(IqBlock {
                    t0: Timestamp(t0),
                    sample_rate_hz: Hz(RATE),
                    center_hz: Hz(CENTER),
                    samples: block.to_vec().into(),
                }),
            ))
            .unwrap();
        }
        let f = rx_out.recv().await.unwrap();
        assert_eq!(f.provenance.span.start_ns, Timestamp(post_gap));
        assert_eq!(
            f.provenance.span.end_ns,
            Timestamp(post_gap + 1_000_000_000)
        );
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
    }

    #[tokio::test]
    async fn measure_stage_discards_window_on_lag() {
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
        let (tx, rx_in) = broadcast::channel(8);
        let h = tokio::spawn(async move {
            MeasureStage::new()
                .run(
                    serde_json::json!({"bandwidth_hz": null}),
                    vec![rx_in],
                    vec![out],
                    bus,
                    CancellationToken::new(),
                )
                .await
        });
        let mut iq = vec![Complex::new(0.0f32, 0.0); 14 * 4_800];
        awgn(&mut iq, 0.0, 3);
        let mk = |i: u64, block: &[Complex<f32>]| {
            Frame::new(
                StreamId::new(),
                prov.clone(),
                Payload::Iq(IqBlock {
                    t0: Timestamp(1_000_000_000 + i * 100_000_000),
                    sample_rate_hz: Hz(RATE),
                    center_hz: Hz(CENTER),
                    samples: block.to_vec().into(),
                }),
            )
        };
        let blocks: Vec<_> = iq.chunks(4_800).collect();
        // 0.5 s held, then a lag, then 0.8 s more with contiguous timestamps:
        // 1.3 s in total, but only if the lag did not discard the window.
        for (i, b) in blocks.iter().enumerate().take(5) {
            tx.send(mk(i as u64, b)).unwrap();
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        tx.send(mk(5, blocks[5])).unwrap(); // overwritten
        for i in 5..13 {
            tx.send(mk(i, blocks[i as usize])).unwrap();
        }
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
        assert!(rx_out.try_recv().is_err(), "no event spans the lag");
    }
}
