//! The 4-level FSK front end P25 phase 1 and DMR share: IQ in, soft dibits out, each with
//! its sample position, and a frame-sync search in both polarities.
//!
//! The signals (both 4800 symbols/s in a 12.5 kHz channel):
//! - **DMR**, ETSI TS 102 361-1 (V2.5.1): 4800 symbols/s of two bits each (clause 10.2.1);
//!   deviation index 0.27, so ±1944 Hz for the outer symbols and ±648 Hz for the inner, and
//!   the dibit mapping of table 10.3 (clause 10.2.2.1); a square-root raised-cosine filter
//!   split between transmitter and receiver, flat to 1920 Hz and nothing past 2880 Hz, so
//!   roll-off 0.2 (clause 10.2.2.2). The receive filter here is that same root raised
//!   cosine, [`Shape::Rrc`].
//! - **P25 phase 1**, TIA-102.BAAA (the common air interface's C4FM modulation clause):
//!   ±1800 and ±600 Hz with the same dibit mapping; the transmitter's raised-cosine
//!   Nyquist filter (roll-off 0.2) cascaded with the shaping filter
//!   `P(f) = (πf/4800) / sin(πf/4800)`, and the receiver's integrate-and-dump filter over
//!   one symbol, whose `sin(πf/4800) / (πf/4800)` cancels the shaping filter. The receive
//!   filter here is that integrate-and-dump with its response cut past the raised
//!   cosine's band ([`C4FM_RECEIVE_PASS`] to [`C4FM_RECEIVE_STOP`]), where the transmitter
//!   sends nothing and the discriminator's noise is strongest: [`Shape::C4fm`].
//!   [`crate::synth::c4fm_modulate`] is the transmitter.
//!
//! The TIA clause number is not cited: TIA-102.BAAA is not freely published and was not to
//! hand. The constants above are the ones its C4FM clause names.
//!
//! Both map dibits to levels alike ([`DIBIT_LEVELS`]): 01 → +3, 00 → +1, 10 → −1,
//! 11 → −3. The first bit is the sign (1 for negative), the second whether the symbol is an
//! outer one, so an inverted discriminator flips the first bit of every dibit.
//!
//! The chain, per [`Fsk4Demod::feed`]:
//! 1. a channel low-pass at the signal's Carson half-bandwidth plus
//!    [`OFFSET_ALLOWANCE_HZ`], so the discriminator sees the channel's noise and not the
//!    sample rate's;
//! 2. the discriminator ([`nbfm_demod`]), scaled so the outer deviation is ±1, clipped at
//!    ±[`DISCRIMINATOR_HEADROOM`];
//! 3. the matched filter for the shape;
//! 4. per symbol, at an interpolated position: the slice against the tracked levels, then
//!    the level and timing updates;
//! 5. per block of [`SEARCH_SYMBOLS`], a timing search.
//!
//! **Levels.** Two numbers place all four: their centre, which is the carrier offset (the
//! DC), and the outer amplitude, the inner levels held at a third of it as the standards
//! define them. Both are decision-directed: each symbol nudges them by its distance from
//! its level. A run of one symbol, sliced right, sits on its level and moves neither, so
//! the DC removal survives it however long it lasts. The search's fits seed them.
//!
//! **Timing.** A Mueller–Müller detector, one sample per symbol: the last symbol's
//! decision times this sample, less this decision times the last sample. Both shapes end in
//! a raised cosine, so the symbol centres are free of intersymbol interference, and with
//! the decisions right the detector reads zero: no self-noise. Its gain hardly depends on
//! the roll-off, which a squaring or Gardner detector needs, and both shapes here have only
//! 0.2. Each symbol moves the next sample by at most [`MAX_TIMING_STEP`] of a symbol, so
//! [`SoftDibit::at`] steps by a symbol within 4 % while the loop holds.
//!
//! **Acquisition.** The loop pulls in slowly from far off, so a search tries
//! [`SEARCH_SYMBOLS`] symbols at each of 16 phases across a symbol, fits each phase's
//! samples to the levels (least squares on its own decisions) and scores how far the eye
//! has closed. The first block is searched before any symbol is given out, and its best
//! phase and fit start the receiver. After that, each block just given out is searched,
//! and the receiver jumps only when its eye has closed (after a gap, or a new carrier)
//! and another fit does at least twice as well. Such a jump moves [`SoftDibit::at`] by up
//! to half a symbol.

use std::f64::consts::PI;

use num_complex::Complex;

use crate::nbfm::nbfm_demod;
use crate::sps_check;
use crate::synth::{C4FM_ROLLOFF, even_pulse, rrc};
use crate::util::{StreamingComplexFir, StreamingFir, lowpass_taps};

/// The name refusals start with.
pub const FSK4_KIND: &str = "fsk4";
/// The level of each dibit (indexed by the dibit), in thirds of the outer deviation:
/// 00 → +1, 01 → +3, 10 → −1, 11 → −3 (ETSI TS 102 361-1 table 10.3; TIA-102.BAAA alike).
pub const DIBIT_LEVELS: [i8; 4] = [1, 3, -1, -3];
/// The discriminator's range, in outer deviations: room for a carrier offset and the
/// pulse's overshoot, a clip on noise clicks beyond.
pub const DISCRIMINATOR_HEADROOM: u64 = 3;
/// How far off the channel's centre a signal may sit and keep its whole band through the
/// channel low-pass, Hz.
pub const OFFSET_ALLOWANCE_HZ: f64 = 1500.0;
/// Channel low-pass taps per sample-rate-to-cutoff ratio (a transition about a quarter
/// of the cutoff wide).
const CHANNEL_TAPS_PER_RATIO: f64 = 8.0;
/// Where the C4FM receive filter starts to taper, in symbol rates: the raised cosine's
/// band edge, `(1 + 0.2) / 2`.
pub const C4FM_RECEIVE_PASS: f64 = (1.0 + C4FM_ROLLOFF) / 2.0;
/// Where the C4FM receive filter stops, in symbol rates.
pub const C4FM_RECEIVE_STOP: f64 = 0.8;
/// Symbols the receive filter spans.
pub const RECEIVE_SPAN_SYMBOLS: f64 = 12.0;
/// The highest channel cutoff, as a fraction of the sample rate.
const CHANNEL_CUTOFF_MAX: f64 = 0.45;
/// The decision-directed centre's step per symbol, of its error.
const CENTRE_GAIN: f64 = 1.0 / 32.0;
/// The decision-directed amplitude's step per symbol, of its error.
const AMPLITUDE_GAIN: f64 = 1.0 / 64.0;
/// The amplitude's range, in declared outer deviations.
const AMPLITUDE_RANGE: (f64, f64) = (0.25, 4.0);
/// Mean square of the nominal levels (±1, ±1/3), normalising the amplitude's update.
const LEVEL_POWER: f64 = 5.0 / 9.0;
/// The timing loop's gain: fraction of a symbol per unit of detector output.
const TIMING_GAIN: f64 = 0.05;
/// The most one symbol moves the next one's sample, in symbols.
pub const MAX_TIMING_STEP: f64 = 0.04;
/// Symbols in a block, the stretch one timing search looks at.
pub const SEARCH_SYMBOLS: usize = 32;
/// Phases across one symbol a timing search tries.
const SEARCH_PHASES: i32 = 16;
/// The eye closure (mean square distance of the samples from their levels, outer levels
/// at ±1) under which the timing is left to the loop: a third of the way from a level to
/// its threshold.
pub const SEARCH_FLOOR: f64 = 1.0 / 81.0;
/// How much better another phase must close the eye for the search to jump to it.
const SEARCH_RATIO: f64 = 0.5;
/// The fewest phases away a jump goes; nearer is the loop's to correct.
const SEARCH_MIN_JUMP: i32 = 2;

/// The receive filter: matched to the transmitter's pulse.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    /// A root raised cosine of roll-off `alpha` at each end (DMR: 0.2).
    Rrc { alpha: f64 },
    /// P25's C4FM: an integrate-and-dump over one symbol after the transmitter's raised
    /// cosine and inverse-sinc shaping.
    C4fm,
}

/// What a 4FSK signal is: its symbol rate, the deviation of its outer symbols, its pulse.
#[derive(Clone, Copy, Debug)]
pub struct Fsk4Params {
    pub symbol_rate_baud: u32,
    pub deviation_hz: f64,
    pub shape: Shape,
}

/// One received symbol. Dibit to level, for P25 and DMR alike: 01 → +3, 00 → +1, 10 → −1, 11 → −3.
#[derive(Clone, Copy, Debug)]
pub struct SoftDibit {
    /// 0..=3, hard decision.
    pub dibit: u8,
    /// Confidence that each bit (first, second) is 1: 0 certain zero, 255 certain one.
    pub soft: [u8; 2],
    /// Sample position of the symbol's centre in the input stream since the demodulator
    /// was made (the filters' delays taken off).
    pub at: f64,
}

/// The symbol [`crate::synth::rrc_modulate_4fsk`] takes for `dibit` (its `0..=3` run
/// from −3 to +3).
pub fn dibit_symbol(dibit: u8) -> u8 {
    ((DIBIT_LEVELS[usize::from(dibit & 3)] + 3) / 2) as u8
}

/// A streaming 4FSK receiver over one channel's IQ; state carries across
/// [`feed`](Self::feed) calls, so feeding in pieces equals feeding whole.
pub struct Fsk4Demod {
    pub rate_hz: u64,
    deviation_hz: f64,
    /// Samples per symbol.
    sps: f64,
    channel: StreamingComplexFir,
    prev: Complex<f32>,
    matched: StreamingFir,
    /// The channel and matched filters' delay, samples.
    delay: f64,
    /// Matched-filter output from sample `base`, outer deviation at ±1.
    buf: Vec<f32>,
    base: u64,
    /// Where the next symbol's centre is expected, in matched-filter samples.
    next: f64,
    /// Whether the first block's search has set the timing and the levels.
    acquired: bool,
    /// The levels: their centre (the carrier offset) and the outer amplitude, raw units.
    centre: f64,
    amplitude: f64,
    /// -1 when the discriminator is inverted.
    sign: f64,
    /// The last symbol's sample (outer levels at ±1) and decided level, for the timing.
    last: (f64, f64),
    /// Where the next block starts, and with it the next timing search.
    block_at: f64,
}

/// A block of symbol samples fitted to the four levels.
#[derive(Clone, Copy, Debug)]
struct Fit {
    centre: f64,
    amplitude: f64,
    /// Mean square distance from the levels, outer levels at ±1.
    closure: f64,
}

impl Fsk4Demod {
    /// A receiver for IQ at `rate_hz`; refused under [`crate::MIN_SAMPLES_PER_SYMBOL`]
    /// samples per symbol, naming the rate and the baud.
    pub fn new(rate_hz: u64, params: Fsk4Params) -> Result<Self, String> {
        sps_check(FSK4_KIND, rate_hz, params.symbol_rate_baud)?;
        let rate = rate_hz as f64;
        let baud = f64::from(params.symbol_rate_baud);
        let sps = rate / baud;

        let alpha = match params.shape {
            Shape::Rrc { alpha } => alpha,
            Shape::C4fm => C4FM_ROLLOFF,
        };
        let cutoff = (params.deviation_hz + (1.0 + alpha) / 2.0 * baud + OFFSET_ALLOWANCE_HZ)
            .min(CHANNEL_CUTOFF_MAX * rate);
        let n = (CHANNEL_TAPS_PER_RATIO * rate / cutoff).round() as usize | 1;
        let channel = lowpass_taps(cutoff, rate, n);

        let reach = (RECEIVE_SPAN_SYMBOLS / 2.0 * sps).floor() as i64;
        let taps: Vec<f64> = (-reach..=reach)
            .map(|j| match params.shape {
                Shape::Rrc { alpha } => rrc(j as f64 / sps, alpha),
                Shape::C4fm => even_pulse(j as f64 / sps, C4FM_RECEIVE_STOP, c4fm_receive),
            })
            .collect();
        let sum: f64 = taps.iter().sum();
        let delay = (n / 2) as f64 + reach as f64;
        // Half a symbol in, so the search's earliest phase is the first sample.
        let first = delay + sps / 2.0;
        Ok(Self {
            rate_hz,
            deviation_hz: params.deviation_hz,
            sps,
            channel: StreamingComplexFir::new(channel),
            prev: Complex::new(0.0, 0.0),
            matched: StreamingFir::new(taps.iter().map(|t| (t / sum) as f32).collect()),
            delay,
            buf: Vec::new(),
            base: 0,
            next: first,
            acquired: false,
            centre: 0.0,
            amplitude: 1.0,
            sign: 1.0,
            last: (0.0, 0.0),
            block_at: first,
        })
    }

    /// The symbols `iq` completes.
    pub fn feed(&mut self, iq: &[Complex<f32>]) -> Vec<SoftDibit> {
        let headroom = DISCRIMINATOR_HEADROOM as f32;
        let mut freq = nbfm_demod(
            &self.channel.process(iq),
            self.rate_hz,
            (self.deviation_hz * DISCRIMINATOR_HEADROOM as f64).round() as u64,
            &mut self.prev,
        );
        freq.iter_mut().for_each(|x| *x *= headroom);
        self.buf.extend(self.matched.process(&freq));
        let mut out = Vec::new();
        let block = SEARCH_SYMBOLS as f64 * self.sps;
        loop {
            if !self.acquired {
                // Acquisition: the first block, searched before any symbol is given out.
                // The search reaches half a symbol past the block's last symbol.
                if !self.holds(self.next + block) {
                    break;
                }
                self.search(self.next);
                self.block_at = self.next + block - self.sps / 2.0;
            }
            if !self.holds(self.next) {
                break;
            }
            if self.next >= self.block_at {
                // The block just given out, every sample of it held now the next is.
                self.search(self.next - block);
                self.block_at = self.next + block - self.sps / 2.0;
                if !self.holds(self.next) {
                    break;
                }
            }
            out.push(self.symbol());
        }
        // Keep from the next search's earliest phase: half a symbol before the block
        // behind the next symbol.
        let keep = (self.next - block - self.sps / 2.0).floor().max(0.0) as u64;
        let n = (keep.saturating_sub(self.base) as usize).min(self.buf.len());
        self.buf.drain(..n);
        self.base += n as u64;
        out
    }

    /// Set by the framer once sync tells the polarity: `true` slices the discriminator
    /// upside down.
    pub fn invert(&mut self, inverted: bool) {
        self.sign = if inverted { -1.0 } else { 1.0 };
    }

    /// Whether the samples to interpolate at `pos` are held.
    fn holds(&self, pos: f64) -> bool {
        pos.floor() as u64 + 1 < self.base + self.buf.len() as u64
    }

    /// The timing search over the block of symbols from `from`: each of [`SEARCH_PHASES`]
    /// phases across a symbol, its samples fitted to the levels. The first block takes the
    /// best phase and its fit outright (acquisition). A later block acts only when the eye
    /// the tracked levels see has closed past [`SEARCH_FLOOR`] and the best fit closes it
    /// by [`SEARCH_RATIO`] or better, as after a gap or a jump in carrier: it takes that
    /// fit's levels, and its phase too when that is at least [`SEARCH_MIN_JUMP`] away (a
    /// timing the loop cannot pull back). A jump moves the symbols after it.
    fn search(&mut self, from: f64) {
        let fits: Vec<(i32, Fit, f64)> = (-SEARCH_PHASES / 2..SEARCH_PHASES / 2)
            .map(|i| {
                let d = f64::from(i) / f64::from(SEARCH_PHASES) * self.sps;
                let ys: Vec<f64> = (0..SEARCH_SYMBOLS)
                    .map(|j| self.y(from + d + j as f64 * self.sps))
                    .collect();
                let tracked = closure(&ys, self.centre, self.amplitude, self.sign);
                (i, self.fit(&ys), tracked)
            })
            .collect();
        let best = *fits
            .iter()
            .min_by(|a, b| a.1.closure.total_cmp(&b.1.closure))
            .expect("phases");
        let current = *fits.iter().find(|f| f.0 == 0).expect("phase 0");
        let take = if !self.acquired {
            Some(best)
        } else if current.2 > SEARCH_FLOOR && best.1.closure < SEARCH_RATIO * current.2 {
            Some(if best.0.abs() >= SEARCH_MIN_JUMP {
                best
            } else {
                current
            })
        } else {
            None
        };
        if let Some((phase, fit, _)) = take {
            self.next += f64::from(phase) / f64::from(SEARCH_PHASES) * self.sps;
            self.centre = fit.centre;
            self.amplitude = fit.amplitude;
            self.last = (0.0, 0.0);
            self.acquired = true;
        }
    }

    /// `ys` (raw samples) fitted to the levels: slice, then the least-squares centre and
    /// amplitude for those decisions, twice. The first block starts from the samples'
    /// mean, later ones from the tracked levels. One level alone keeps the amplitude.
    fn fit(&self, ys: &[f64]) -> Fit {
        let n = ys.len() as f64;
        let mean = ys.iter().sum::<f64>() / n;
        let mut centre = if self.acquired { self.centre } else { mean };
        let mut amplitude = self.amplitude;
        for _ in 0..2 {
            let ls = levels(ys, centre, amplitude, self.sign);
            let lm = ls.iter().sum::<f64>() / n;
            let var = ls.iter().map(|l| (l - lm).powi(2)).sum::<f64>() / n;
            if var > 1e-3 {
                let cov = ys
                    .iter()
                    .zip(&ls)
                    .map(|(y, l)| (y - mean) * (l - lm))
                    .sum::<f64>()
                    / n;
                amplitude = (cov / var).clamp(AMPLITUDE_RANGE.0, AMPLITUDE_RANGE.1);
            }
            centre = mean - amplitude * lm;
        }
        Fit {
            centre,
            amplitude,
            closure: closure(ys, centre, amplitude, self.sign),
        }
    }

    /// The matched filter's output at `pos`, linearly interpolated (every sample held).
    fn y(&self, pos: f64) -> f64 {
        let i = (pos.floor() as u64 - self.base) as usize;
        let f = pos - pos.floor();
        f64::from(self.buf[i]) * (1.0 - f) + f64::from(self.buf[i + 1]) * f
    }

    /// Slice the symbol at `self.next`, update the levels and the timing, and step on.
    fn symbol(&mut self) -> SoftDibit {
        let t = self.next;
        let y = self.y(t);
        let z = self.sign * (y - self.centre) / self.amplitude;
        let (dibit, soft) = slice(z);
        let level = level_of(z);

        let err = z - level;
        self.centre += CENTRE_GAIN * self.sign * self.amplitude * err;
        self.amplitude = (self.amplitude * (1.0 + AMPLITUDE_GAIN * err * level / LEVEL_POWER))
            .clamp(AMPLITUDE_RANGE.0, AMPLITUDE_RANGE.1);

        // Mueller–Müller: negative when sampling late, positive when early.
        let (last_z, last_level) = self.last;
        let timing = last_level * z - level * last_z;
        self.last = (z, level);
        let step = (TIMING_GAIN * timing).clamp(-MAX_TIMING_STEP, MAX_TIMING_STEP);
        self.next = t + self.sps * (1.0 + step);
        SoftDibit {
            dibit,
            soft,
            at: t - self.delay,
        }
    }
}

/// The level `z` (outer levels at ±1) slices to.
fn level_of(z: f64) -> f64 {
    f64::from(DIBIT_LEVELS[usize::from(slice(z).0)]) / 3.0
}

/// Each raw sample's level, outer at ±1 and in the raw polarity, sliced at `centre` and
/// `amplitude`.
fn levels(ys: &[f64], centre: f64, amplitude: f64, sign: f64) -> Vec<f64> {
    ys.iter()
        .map(|&y| sign * level_of(sign * (y - centre) / amplitude))
        .collect()
}

/// How far the eye has closed for raw samples `ys` sliced at `centre` and `amplitude`:
/// the mean square distance from their levels, outer levels at ±1.
fn closure(ys: &[f64], centre: f64, amplitude: f64, sign: f64) -> f64 {
    ys.iter()
        .zip(levels(ys, centre, amplitude, sign))
        .map(|(y, l)| ((y - centre) / amplitude - l).powi(2))
        .sum::<f64>()
        / ys.len() as f64
}

/// The C4FM receive filter's response at `f` symbol rates: the integrate-and-dump's
/// `sin(πf) / πf` through the raised cosine's band, then a raised-cosine taper from
/// [`C4FM_RECEIVE_PASS`] to [`C4FM_RECEIVE_STOP`], where the transmitter sends nothing and
/// the discriminator's noise, rising with frequency, is strongest. The whole chain stays
/// the raised cosine, free of intersymbol interference at the symbol centres.
fn c4fm_receive(f: f64) -> f64 {
    let dump = if f < 1e-12 {
        1.0
    } else {
        (PI * f).sin() / (PI * f)
    };
    let taper = if f <= C4FM_RECEIVE_PASS {
        1.0
    } else {
        let x = (f - C4FM_RECEIVE_PASS) / (C4FM_RECEIVE_STOP - C4FM_RECEIVE_PASS);
        0.5 * (1.0 + (PI * x.min(1.0)).cos())
    };
    dump * taper
}

/// The dibit and soft bits of `z`, the symbol sample with the outer levels at ±1: the
/// first bit from the sign, the second from the distance from the centre against the
/// inner-outer threshold (2/3), each soft value linear in the distance from its threshold
/// and certain at a level's distance (1/3).
fn slice(z: f64) -> (u8, [u8; 2]) {
    let confidence = |v: f64| (127.5 + 127.5 * (3.0 * v).clamp(-1.0, 1.0)).round() as u8;
    let soft = [confidence(-z), confidence(z.abs() - 2.0 / 3.0)];
    let dibit = (u8::from(soft[0] >= 128) << 1) | u8::from(soft[1] >= 128);
    (dibit, soft)
}

/// The first place `pattern` (dibits) matches with at most `max_errors` dibit errors,
/// in either polarity: (index, inverted). Inverted, every dibit's first bit is flipped
/// (each level negated). An empty pattern matches nowhere.
pub fn find_sync(dibits: &[SoftDibit], pattern: &[u8], max_errors: usize) -> Option<(usize, bool)> {
    if pattern.is_empty() || dibits.len() < pattern.len() {
        return None;
    }
    (0..=dibits.len() - pattern.len()).find_map(|i| {
        let window = &dibits[i..i + pattern.len()];
        let errors = |flip: u8| {
            window
                .iter()
                .zip(pattern)
                .filter(|(d, p)| d.dibit ^ flip != **p)
                .count()
        };
        if errors(0) <= max_errors {
            Some((i, false))
        } else if errors(0b10) <= max_errors {
            Some((i, true))
        } else {
            None
        }
    })
}

/// The low `bits` bits of `word` as dibits, first bit first. Panics unless `bits` is even
/// and at most 64.
pub fn sync_dibits(word: u64, bits: u32) -> Vec<u8> {
    assert!(
        bits.is_multiple_of(2) && bits <= 64,
        "sync_dibits: {bits} bits"
    );
    (0..bits / 2)
        .rev()
        .map(|i| ((word >> (2 * i)) & 3) as u8)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{awgn, c4fm_modulate, rrc_modulate_4fsk};

    /// The baud both P25 phase 1 and DMR send.
    const BAUD: u32 = 4800;
    /// P25's outer deviation, Hz.
    const P25_DEVIATION_HZ: f64 = 1800.0;
    /// DMR's outer deviation, Hz.
    const DMR_DEVIATION_HZ: f64 = 1944.0;
    /// DMR's root-raised-cosine roll-off.
    const DMR_ALPHA: f64 = 0.2;
    /// The channel the tests' SNRs are measured in, Hz.
    const VOICE_BANDWIDTH_HZ: f64 = 12_500.0;
    /// P25's 48-bit frame sync.
    const P25_SYNC: u64 = 0x5575_F5FF_77FF;
    /// Symbols the demodulator is given to settle before errors count.
    const SETTLE: usize = 100;

    fn c4fm() -> Fsk4Params {
        Fsk4Params {
            symbol_rate_baud: BAUD,
            deviation_hz: P25_DEVIATION_HZ,
            shape: Shape::C4fm,
        }
    }

    fn dmr() -> Fsk4Params {
        Fsk4Params {
            symbol_rate_baud: BAUD,
            deviation_hz: DMR_DEVIATION_HZ,
            shape: Shape::Rrc { alpha: DMR_ALPHA },
        }
    }

    /// `n` pseudo-random dibits from `seed`.
    fn dibits(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (s >> 62) as u8
            })
            .collect()
    }

    /// `tx` modulated for `params` at `rate`.
    fn modulate(tx: &[u8], params: Fsk4Params, rate: u32) -> Vec<Complex<f32>> {
        match params.shape {
            Shape::C4fm => c4fm_modulate(tx, params.symbol_rate_baud, params.deviation_hz, rate),
            Shape::Rrc { alpha } => {
                let symbols: Vec<u8> = tx.iter().map(|&d| dibit_symbol(d)).collect();
                rrc_modulate_4fsk(
                    &symbols,
                    params.symbol_rate_baud,
                    params.deviation_hz,
                    rate,
                    alpha,
                )
            }
        }
    }

    /// Noise at `snr_db` measured in a 12.5 kHz channel: `awgn` spreads it over the
    /// whole sample rate, so its SNR is lower by the ratio of the bandwidths.
    fn noise(iq: &mut [Complex<f32>], snr_db: f64, rate: u32, seed: u64) {
        let spread = 10.0 * (f64::from(rate) / VOICE_BANDWIDTH_HZ).log10();
        awgn(iq, (snr_db - spread) as f32, seed);
    }

    /// The transmitted symbol `d` stands for, from its position: the input was `tx`
    /// with its first `skip` samples dropped.
    fn symbol_index(d: &SoftDibit, skip: usize, sps: f64) -> i64 {
        ((d.at + skip as f64) / sps - 0.5).round() as i64
    }

    /// The received dibits paired with the transmitted ones from symbol `SETTLE` on,
    /// checking no symbol is skipped or repeated and none near the end is missing.
    fn pair(rx: &[SoftDibit], tx: &[u8], skip: usize, sps: f64) -> Vec<(SoftDibit, u8)> {
        let pairs: Vec<(i64, SoftDibit)> = rx
            .iter()
            .map(|d| (symbol_index(d, skip, sps), *d))
            .filter(|(k, _)| *k >= SETTLE as i64 && *k < tx.len() as i64)
            .collect();
        for w in pairs.windows(2) {
            assert_eq!(w[1].0, w[0].0 + 1, "symbols slipped near {}", w[0].0);
        }
        assert!(
            pairs.len() + SETTLE + 16 >= tx.len(),
            "{} of {} symbols received",
            pairs.len(),
            tx.len()
        );
        pairs
            .into_iter()
            .map(|(k, d)| (d, tx[k as usize]))
            .collect()
    }

    fn errors(pairs: &[(SoftDibit, u8)]) -> usize {
        pairs.iter().filter(|(d, t)| d.dibit != *t).count()
    }

    /// Feed `iq` in uneven pieces, as a stage's frames would arrive.
    fn feed_all(demod: &mut Fsk4Demod, iq: &[Complex<f32>]) -> Vec<SoftDibit> {
        iq.chunks(777).flat_map(|c| demod.feed(c)).collect()
    }

    fn round_trip(params: Fsk4Params, rate: u32, tx: &[u8], skip: usize) -> usize {
        let iq = modulate(tx, params, rate);
        let mut demod = Fsk4Demod::new(u64::from(rate), params).expect("rate");
        let rx = feed_all(&mut demod, &iq[skip..]);
        let sps = f64::from(rate) / f64::from(params.symbol_rate_baud);
        errors(&pair(&rx, tx, skip, sps))
    }

    #[test]
    fn clean_symbols_round_trip_at_three_rates() {
        let tx = dibits(2000, 7);
        for rate in [48_000, 50_000, 96_000] {
            for (name, params) in [("rrc", dmr()), ("c4fm", c4fm())] {
                let skip = rate as usize / 4800 / 3;
                assert_eq!(
                    round_trip(params, rate, &tx, skip),
                    0,
                    "{name} at {rate} Hz"
                );
            }
        }
    }

    #[test]
    fn soft_bits_follow_confidence() {
        let rate = 48_000;
        let sps = f64::from(rate) / f64::from(BAUD);
        let tx = dibits(2000, 11);
        for params in [dmr(), c4fm()] {
            let iq = modulate(&tx, params, rate);
            let mut demod = Fsk4Demod::new(u64::from(rate), params).expect("rate");
            for (d, _) in pair(&feed_all(&mut demod, &iq), &tx, 0, sps) {
                for s in d.soft {
                    assert!(!(32..=223).contains(&s), "clean soft bit {s}");
                }
            }

            let mut iq = modulate(&tx, params, rate);
            noise(&mut iq, 6.0, rate, 3);
            let mut demod = Fsk4Demod::new(u64::from(rate), params).expect("rate");
            let (mut right, mut wrong) = ((0.0, 0), (0.0, 0));
            for (d, t) in pair(&feed_all(&mut demod, &iq), &tx, 0, sps) {
                for (bit, s) in d.soft.iter().enumerate() {
                    let sent = (t >> (1 - bit)) & 1;
                    let got = u8::from(*s >= 128);
                    let distance = (f64::from(*s) - 127.5).abs();
                    let tally = if got == sent { &mut right } else { &mut wrong };
                    tally.0 += distance;
                    tally.1 += 1;
                }
            }
            assert!(wrong.1 > 0, "6 dB gave no wrong bits");
            let (right, wrong) = (right.0 / right.1 as f64, wrong.0 / wrong.1 as f64);
            assert!(
                wrong < right,
                "{:?}: wrong bits {wrong:.1} from 128, right ones {right:.1}",
                params.shape
            );
        }
    }

    #[test]
    fn inverted_and_offset_signals_still_sync() {
        let rate = 48_000u32;
        let sps = f64::from(rate) / f64::from(BAUD);
        let sync = sync_dibits(P25_SYNC, 48);
        let (before, after) = (300, 200);
        let mut tx = dibits(before, 21);
        tx.extend(&sync);
        tx.extend(dibits(after, 22));
        for (name, conjugate, offset_hz) in [("conjugated", true, 0.0), ("offset", false, 1000.0)] {
            let mut iq = modulate(&tx, c4fm(), rate);
            for (n, s) in iq.iter_mut().enumerate() {
                if conjugate {
                    *s = s.conj();
                }
                let phase = std::f64::consts::TAU * offset_hz * n as f64 / f64::from(rate);
                *s *= Complex::from_polar(1.0, phase as f32);
            }
            noise(&mut iq, 12.0, rate, 5);
            let mut demod = Fsk4Demod::new(u64::from(rate), c4fm()).expect("rate");
            // Up to the sync and a little past it, as a framer sees it.
            let split = ((before + sync.len() + 20) as f64 * sps) as usize;
            let rx = demod.feed(&iq[..split]);
            let (index, inverted) = find_sync(&rx, &sync, 2).expect(name);
            assert_eq!(symbol_index(&rx[index], 0, sps), before as i64, "{name}");
            assert_eq!(inverted, conjugate, "{name}");

            demod.invert(inverted);
            let rest = demod.feed(&iq[split..]);
            let pairs: Vec<(SoftDibit, u8)> = rest
                .iter()
                .map(|d| (symbol_index(d, 0, sps), *d))
                .filter(|(k, _)| (*k as usize) < tx.len())
                .map(|(k, d)| (d, tx[k as usize]))
                .collect();
            assert!(
                pairs.len() > after - 30,
                "{name}: {} after the split",
                pairs.len()
            );
            assert_eq!(errors(&pairs), 0, "{name}: dibits after invert({inverted})");
        }
    }

    #[test]
    fn noise_at_10_db_keeps_errors_low() {
        let rate = 48_000;
        let sps = f64::from(rate) / f64::from(BAUD);
        let tx = dibits(10_000, 31);
        for params in [dmr(), c4fm()] {
            let mut iq = modulate(&tx, params, rate);
            noise(&mut iq, 10.0, rate, 9);
            let skip = 4;
            let mut demod = Fsk4Demod::new(u64::from(rate), params).expect("rate");
            let pairs = pair(&feed_all(&mut demod, &iq[skip..]), &tx, skip, sps);
            let rate = errors(&pairs) as f64 / pairs.len() as f64;
            assert!(
                rate < 0.02,
                "{:?}: dibit error rate {:.2} %",
                params.shape,
                rate * 100.0
            );
        }
    }

    #[test]
    fn half_rate_works() {
        let params = Fsk4Params {
            symbol_rate_baud: 2400,
            deviation_hz: 1050.0,
            shape: Shape::Rrc { alpha: 0.2 },
        };
        let tx = dibits(1000, 41);
        assert_eq!(round_trip(params, 48_000, &tx, 7), 0);
    }

    #[test]
    fn a_low_rate_is_refused() {
        let err = Fsk4Demod::new(16_000, c4fm()).err().expect("refused");
        assert!(err.contains("16000") && err.contains("4800"), "{err}");
    }

    #[test]
    fn sample_positions_advance_by_a_symbol() {
        let rate = 50_000;
        let sps = f64::from(rate) / f64::from(BAUD);
        let tx = dibits(500, 51);
        for params in [dmr(), c4fm()] {
            let mut demod = Fsk4Demod::new(u64::from(rate), params).expect("rate");
            let rx = feed_all(&mut demod, &modulate(&tx, params, rate)[3..]);
            assert!(rx.len() > 480, "{} symbols", rx.len());
            for w in rx.windows(2) {
                let step = w[1].at - w[0].at;
                assert!((step - sps).abs() < 0.05 * sps, "step {step} of {sps}");
            }
        }
    }

    #[test]
    fn sync_words_split_into_dibits_first_bit_first() {
        assert_eq!(&sync_dibits(P25_SYNC, 48)[..6], &[1, 1, 1, 1, 1, 3]);
        assert_eq!(sync_dibits(0b10_00_11, 6), vec![2, 0, 3]);
    }
}
