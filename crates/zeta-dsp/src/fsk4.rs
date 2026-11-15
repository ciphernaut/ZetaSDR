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
//! hand. The P25 constants above are taken from published descriptions of its C4FM clause
//! and are not yet checked against the document.
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
//! 5. every [`SEARCH_EVERY`] symbols, a search of the last [`SEARCH_SYMBOLS`] for a signal;
//! 6. the symbols given out: at once while a signal is held, otherwise
//!    [`PENDING_SYMBOLS`] behind, so they can be sliced again when one is found.
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
//! **Acquisition.** On air every transmission starts after noise, carrier or nothing, and
//! a P25 one has no preamble: its first frame sync is its first 24 symbols. So:
//! - A search fits a stretch's samples to the levels (least squares on their own
//!   decisions) at each of 16 phases across a symbol. It takes the fit for a signal when
//!   the eye is open, no level holds most of the stretch (a carrier sits on one), and the
//!   amplitude is near the declared deviation ([`SIGNAL_AMPLITUDE`]). That last test is
//!   what tells noise, which fits at 1.2 to 2.1 whatever its power, from a weak signal.
//! - A full search's fit must also explain the next search's stretch, since a stretch
//!   part noise or carrier can fit with a false centre.
//! - While no signal is held, symbols wait [`PENDING_SYMBOLS`] behind. When one is found,
//!   every symbol not yet given out is sliced again on its timing and levels, back to the
//!   signal's first symbol.
//! - A fresh lock is on probation for [`PROBATION`] searches, its symbols still held:
//!   held to the acquisition standard, and refitted at every phase, so a false one is
//!   undone before anything it sliced goes out.
//!
//! [`SoftDibit::at`] never goes backwards. It steps by a symbol within 4 % while a lock
//! holds, and by more where a signal is found anew.

use std::collections::VecDeque;
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
/// Symbols one search looks back over.
pub const SEARCH_SYMBOLS: usize = 32;
/// Symbols between searches.
pub const SEARCH_EVERY: usize = 8;
/// Searches in a row without a signal after which a search tries every phase; before,
/// the signal last held is taken to be the same one, its timing standing.
pub const FULL_SEARCH_AFTER: usize = 4;
/// Searches a fresh lock must hold before its symbols are given out: until then a false
/// lock (noise that fitted, just before a signal) can still be undone.
pub const PROBATION: usize = 2;
/// The eye closure under which held levels are not refitted: a quarter of the way from a
/// level to its threshold.
pub const REFIT_FLOOR: f64 = 1.0 / 144.0;
/// How much better a refit must close the eye for its levels to replace the held ones.
pub const REFIT_RATIO: f64 = 0.5;
/// Least-squares rounds in a fit.
const FIT_ROUNDS: usize = 4;
/// Phases across one symbol a search tries.
const SEARCH_PHASES: i32 = 16;
/// The fewest phases away from the tracked one a search moves the timing; nearer is the
/// loop's to correct.
const SEARCH_MIN_JUMP: i32 = 2;
/// The most eye closure (mean square distance of the samples from their levels, outer
/// levels at ±1) a held signal may show: three fifths of the way from a level to its
/// threshold.
pub const HOLD_CLOSURE: f64 = 1.0 / 25.0;
/// The most eye closure a stretch may show to be taken for a new signal: half the way
/// from a level to its threshold. Noise alone, fitted, shows 0.017 to 0.04, so this alone
/// does not tell them apart ([`SIGNAL_AMPLITUDE`] does).
pub const ACQUIRE_CLOSURE: f64 = 1.0 / 36.0;
/// The largest share of a stretch's symbols one level may take in a signal. A carrier
/// sits on one level, and a stretch mostly carrier nearly so; random data puts at most
/// about half its 32 symbols on one level, a 48-bit sync of outer symbols 54 %.
pub const SIGNAL_SHARE: f64 = 0.6;
/// The outer amplitude a new signal shows, in declared outer deviations. Noise alone,
/// fitted, shows 1.2 to 2.1 (1st to 99th percentile) whatever its level, since a
/// discriminator's output on noise does not depend on its power; that is what tells the
/// two apart. A transmitter further off its declared deviation is not acquired.
pub const SIGNAL_AMPLITUDE: (f64, f64) = (0.75, 1.15);
/// Symbols held back while no signal is held or a lock is on probation: as many as a
/// signal's start can be found behind (a search's stretch, the confirming search, a false
/// lock's probation, with room), so they can be sliced again once it is.
pub const PENDING_SYMBOLS: usize = 96;

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
    /// Whether the last search found the signal (and the levels and timing on it).
    locked: bool,
    /// Searches in a row that found no signal.
    unheld: usize,
    /// Searches the lock has held since it was taken.
    proven: usize,
    /// A full search's signal-like fit (with where its stretch started) awaiting the
    /// next search's confirmation.
    candidate: Option<(f64, Fit)>,
    /// Symbols until the next search.
    until_search: usize,
    /// Symbols sliced but not given out: held while not locked or on probation, so a
    /// signal found behind them can have them sliced again.
    pending: VecDeque<SoftDibit>,
    /// The matched-filter position of the last symbol given out.
    given: f64,
    /// The levels: their centre (the carrier offset) and the outer amplitude, raw units.
    centre: f64,
    amplitude: f64,
    /// -1 when the discriminator is inverted.
    sign: f64,
    /// The last symbol's sample (outer levels at ±1) and decided level, for the timing.
    last: (f64, f64),
}

/// A stretch of symbol samples against a set of levels.
#[derive(Clone, Copy, Debug)]
struct Fit {
    centre: f64,
    amplitude: f64,
    /// Mean square distance from the levels, outer levels at ±1.
    closure: f64,
    /// The largest share of the samples any one level takes.
    share: f64,
}

impl Fit {
    /// `ys` (raw samples) sliced at `centre` and `amplitude`.
    fn of(ys: &[f64], centre: f64, amplitude: f64, sign: f64) -> Self {
        let n = ys.len() as f64;
        let ls = levels(ys, centre, amplitude, sign);
        let mut counts = [0usize; 4];
        for l in &ls {
            // Levels ±1/3, ±1 to 0..=3.
            counts[((l * 3.0 + 3.0) / 2.0).round() as usize] += 1;
        }
        Self {
            centre,
            amplitude,
            closure: ys
                .iter()
                .zip(&ls)
                .map(|(y, l)| ((y - centre) / amplitude - l).powi(2))
                .sum::<f64>()
                / n,
            share: counts.iter().copied().max().unwrap_or(0) as f64 / n,
        }
    }

    /// Whether the stretch looks like 4FSK on these levels: the eye open to within
    /// `closure`, no one level taking most of it, and an amplitude near the declared
    /// deviation.
    fn is_signal(&self, closure: f64) -> bool {
        self.closure < closure
            && self.share <= SIGNAL_SHARE
            && (SIGNAL_AMPLITUDE.0..=SIGNAL_AMPLITUDE.1).contains(&self.amplitude)
    }
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
            // Half a symbol in, so a search's earliest phase is the first sample.
            next: delay + sps / 2.0,
            locked: false,
            unheld: FULL_SEARCH_AFTER,
            proven: 0,
            candidate: None,
            until_search: SEARCH_SYMBOLS,
            pending: VecDeque::new(),
            given: f64::NEG_INFINITY,
            centre: 0.0,
            amplitude: 1.0,
            sign: 1.0,
            last: (0.0, 0.0),
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
        while self.holds(self.next) {
            // The stretch ends half a symbol before the next symbol, so it is all held.
            if self.until_search == 0 {
                self.until_search = SEARCH_EVERY;
                self.search();
            }
            if self.holds(self.next) {
                let d = self.symbol();
                self.pending.push_back(d);
                self.until_search -= 1;
            }
            // Locked past probation, every symbol goes out; otherwise they wait their turn.
            let hold = if self.locked && self.proven >= PROBATION {
                0
            } else {
                PENDING_SYMBOLS
            };
            while self.pending.len() > hold {
                let d = self.pending.pop_front().expect("pending");
                self.given = d.at + self.delay;
                out.push(d);
            }
        }
        // Keep what a search or a slice again reaches: a search's stretch, at its earliest
        // phase, and every symbol not given out.
        let window = self.next - (SEARCH_SYMBOLS as f64 + 1.5) * self.sps;
        let keep = (window.min(self.given) - 1.0).floor().max(0.0) as u64;
        let n = (keep.saturating_sub(self.base) as usize).min(self.buf.len());
        self.buf.drain(..n);
        self.base += n as u64;
        out
    }

    /// Set by the framer once sync tells the polarity: `true` slices the discriminator
    /// upside down, from the next symbol given out (held ones are turned too).
    pub fn invert(&mut self, inverted: bool) {
        let sign = if inverted { -1.0 } else { 1.0 };
        if sign != self.sign {
            for d in &mut self.pending {
                d.dibit ^= 0b10;
                d.soft[0] = 255 - d.soft[0];
            }
        }
        self.sign = sign;
        self.last = (0.0, 0.0);
    }

    /// Whether the samples to interpolate at `pos` are held.
    fn holds(&self, pos: f64) -> bool {
        pos.floor() as u64 + 1 < self.base + self.buf.len() as u64
    }

    /// The search over the last [`SEARCH_SYMBOLS`] symbols' stretch, every
    /// [`SEARCH_EVERY`] symbols.
    ///
    /// Locked:
    /// - a fit afresh that closes the eye at least [`REFIT_RATIO`] better than the held
    ///   levels shows them wrong (taken from a stretch part noise or carrier), and is taken
    ///   instead: at any phase while the lock is on probation, at the tracked phase after;
    /// - otherwise the lock holds while the held levels still see a signal
    ///   ([`Fit::is_signal`], to [`ACQUIRE_CLOSURE`] on probation and [`HOLD_CLOSURE`]
    ///   after).
    ///
    /// Not holding, the stretch is fitted: at the tracked phase alone while a signal was
    /// held within [`FULL_SEARCH_AFTER`] searches (the same one, faded or hit), at each of
    /// [`SEARCH_PHASES`] phases otherwise or after a false lock. A full search's fit must
    /// also explain the next search's stretch before it is taken, as a stretch half
    /// carrier or noise can fit falsely. A fit taken locks the receiver ([`Self::adopt`]);
    /// none, and symbols are held.
    fn search(&mut self) {
        let from = self.next - SEARCH_SYMBOLS as f64 * self.sps;
        if from - self.sps / 2.0 < self.base as f64 {
            return;
        }
        let stretch = |d: f64| -> Vec<f64> {
            (0..SEARCH_SYMBOLS)
                .map(|j| self.y(from + d + j as f64 * self.sps))
                .collect()
        };
        let offset = |i: i32| f64::from(i) / f64::from(SEARCH_PHASES) * self.sps;
        let every = -SEARCH_PHASES / 2..SEARCH_PHASES / 2;
        // Each phase's fit, the closest first.
        let fits = |phases: std::ops::Range<i32>| -> Vec<(i32, Fit)> {
            let mut fits: Vec<(i32, Fit)> =
                phases.map(|i| (i, self.fit(&stretch(offset(i))))).collect();
            fits.sort_by(|a, b| a.1.closure.total_cmp(&b.1.closure));
            fits
        };
        let held = Fit::of(&stretch(0.0), self.centre, self.amplitude, self.sign);
        let probation = self.proven < PROBATION;
        if self.locked && held.closure > REFIT_FLOOR {
            let (phase, refit) = fits(if probation { every.clone() } else { 0..1 })[0];
            if refit.is_signal(ACQUIRE_CLOSURE) && refit.closure < REFIT_RATIO * held.closure {
                let proven = self.proven;
                self.adopt(from + offset(phase), refit);
                self.proven = proven;
                self.unheld = 0;
                return;
            }
        }
        let limit = if probation {
            ACQUIRE_CLOSURE
        } else {
            HOLD_CLOSURE
        };
        if self.locked && held.is_signal(limit) {
            self.unheld = 0;
            self.proven += 1;
            return;
        }
        let false_lock = self.locked && probation;
        let full = false_lock || self.unheld >= FULL_SEARCH_AFTER;
        let fits = fits(if full { every } else { 0..1 });
        let best = fits[0];
        // The tracked phase, if the best is too near it to be worth a jump.
        let (phase, fit) = match fits.iter().find(|f| f.0 == 0) {
            Some(&tracked)
                if best.0.abs() < SEARCH_MIN_JUMP && tracked.1.is_signal(ACQUIRE_CLOSURE) =>
            {
                tracked
            }
            _ => best,
        };
        let start = from + offset(phase);
        let signal = fit.is_signal(ACQUIRE_CLOSURE);
        // The last full search's fit, if it explains this stretch on its own grid; of it and
        // this search's fit, the one explaining this stretch better is taken.
        let confirmed = self.candidate.and_then(|(at, c)| {
            let d = at + ((from - at) / self.sps).round() * self.sps - from;
            let again = Fit::of(&stretch(d), c.centre, c.amplitude, self.sign);
            again
                .is_signal(ACQUIRE_CLOSURE)
                .then_some((from + d, again))
        });
        let taken = match confirmed {
            Some(again) if !signal || again.1.closure < fit.closure => Some(again),
            Some(_) => Some((start, fit)),
            None if !full && signal => Some((start, fit)),
            None => None,
        };
        self.candidate = (full && signal).then_some((start, fit));
        self.locked = taken.is_some();
        if let Some((start, fit)) = taken {
            self.unheld = 0;
            self.candidate = None;
            self.adopt(start, fit);
        } else if false_lock {
            self.unheld = FULL_SEARCH_AFTER;
        } else {
            self.unheld += 1;
        }
    }

    /// Lock on `fit`, whose stretch starts at `start`: the levels from it and the timing on
    /// its grid. Every symbol after the last one given out is sliced again: those before
    /// the stretch as the fit says (they may be the signal's first, or nothing), those
    /// from it on tracked as usual, up to where the receiver had got.
    fn adopt(&mut self, start: f64, fit: Fit) {
        let reached = self.next;
        self.centre = fit.centre;
        self.amplitude = fit.amplitude;
        self.last = (0.0, 0.0);
        self.proven = 0;
        self.pending.clear();
        let after = self.given + self.sps / 2.0;
        // As far back as the samples go, before anything is given out.
        let floor = after.max(self.base as f64 + 1.0);
        let behind = ((start - floor) / self.sps).ceil().max(0.0) as usize;
        for k in (1..behind).rev() {
            let t = start - k as f64 * self.sps;
            if t - 1.0 >= self.base as f64 {
                let z = self.sign * (self.y(t) - self.centre) / self.amplitude;
                let (dibit, soft) = slice(z);
                self.pending.push_back(SoftDibit {
                    dibit,
                    soft,
                    at: t - self.delay,
                });
            }
        }
        self.next = start;
        while self.next < reached - self.sps / 2.0 && self.holds(self.next) {
            let d = self.symbol();
            if d.at + self.delay > after {
                self.pending.push_back(d);
            }
        }
    }

    /// `ys` (raw samples) fitted to the levels: slice, then the least-squares centre and
    /// amplitude for those decisions, [`FIT_ROUNDS`] times. It starts from the samples'
    /// mean or the tracked centre, and from the declared or the tracked amplitude (which
    /// noise drags off), and the closest fit is kept. One level alone keeps the amplitude.
    fn fit(&self, ys: &[f64]) -> Fit {
        let n = ys.len() as f64;
        let mean = ys.iter().sum::<f64>() / n;
        let from = |mut centre: f64, mut amplitude: f64| {
            for _ in 0..FIT_ROUNDS {
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
            Fit::of(ys, centre, amplitude, self.sign)
        };
        [
            (mean, 1.0),
            (mean, self.amplitude),
            (self.centre, 1.0),
            (self.centre, self.amplitude),
        ]
        .into_iter()
        .map(|(c, a)| from(c, a))
        .min_by(|a, b| a.closure.total_cmp(&b.closure))
        .expect("starts")
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

    /// The rate and samples per symbol the acquisition tests run at.
    const RATE: u32 = 48_000;
    const SPS: f64 = 10.0;
    /// The SNR the acquisition tests' signals sit at, in 12.5 kHz.
    const SIGNAL_SNR_DB: f64 = 15.0;

    /// Noise at `snr_db` in 12.5 kHz below a unit carrier, whatever `iq` holds there.
    fn noise_under_unit(iq: &mut [Complex<f32>], snr_db: f64, seed: u64) {
        // `awgn` takes an all-zero buffer's power as 1.
        let mut n = vec![Complex::new(0.0f32, 0.0); iq.len()];
        noise(&mut n, snr_db, RATE, seed);
        iq.iter_mut().zip(n).for_each(|(s, v)| *s += v);
    }

    /// One transmission in a scene: where its first symbol starts, its dibits, and the
    /// indices that must come out right.
    struct Sent {
        start: usize,
        dibits: Vec<u8>,
        check: std::ops::Range<usize>,
    }

    /// A capture and what was sent in it.
    struct Scene {
        name: String,
        iq: Vec<Complex<f32>>,
        sent: Vec<Sent>,
    }

    /// `n` samples of unmodulated carrier at the signal's level, at `iq`'s last phase.
    fn carrier_after(iq: &mut Vec<Complex<f32>>, n: usize) {
        let last = iq.last().copied().unwrap_or(Complex::new(1.0, 0.0));
        iq.extend(std::iter::repeat_n(last, n));
    }

    /// Lead-in `lead` (already in `iq`), then 600 dibits from `seed`, then 20 ms of carrier
    /// so the receive filters give out the last symbols undisturbed.
    fn after_lead(name: &str, mut iq: Vec<Complex<f32>>, params: Fsk4Params, seed: u64) -> Scene {
        let start = iq.len();
        let dibits = dibits(600, seed);
        iq.extend(modulate(&dibits, params, RATE));
        carrier_after(&mut iq, RATE as usize / 50);
        noise_under_unit(&mut iq, SIGNAL_SNR_DB, seed + 100);
        Scene {
            name: name.into(),
            iq,
            sent: vec![Sent {
                start,
                dibits,
                check: 2..600,
            }],
        }
    }

    fn after_noise(params: Fsk4Params, seed: u64) -> Scene {
        let lead = vec![Complex::new(0.0, 0.0); RATE as usize];
        after_lead("1 s of noise", lead, params, seed)
    }

    fn after_carrier(params: Fsk4Params, ms: usize, seed: u64) -> Scene {
        let lead = vec![Complex::new(1.0, 0.0); RATE as usize * ms / 1000];
        after_lead(&format!("{ms} ms of carrier"), lead, params, seed)
    }

    /// Ten bursts of 132 dibits, each followed by 156 symbol periods of nothing.
    fn bursts(params: Fsk4Params, seed: u64) -> Scene {
        let mut iq = Vec::new();
        let mut sent = Vec::new();
        for b in 0..10 {
            let start = iq.len();
            let dibits = dibits(132, 70 + b);
            iq.extend(modulate(&dibits, params, RATE));
            iq.extend(std::iter::repeat_n(
                Complex::new(0.0, 0.0),
                156 * SPS as usize,
            ));
            sent.push(Sent {
                start,
                dibits,
                check: 2..130,
            });
        }
        noise_under_unit(&mut iq, SIGNAL_SNR_DB, seed);
        Scene {
            name: "bursts".into(),
            iq,
            sent,
        }
    }

    /// 600 dibits, 500 ms of noise alone, 600 more 300 Hz away.
    fn second_after_gap(params: Fsk4Params, seed: u64) -> Scene {
        let mut iq = modulate(&dibits(600, 64), params, RATE);
        iq.extend(std::iter::repeat_n(
            Complex::new(0.0, 0.0),
            RATE as usize / 2,
        ));
        let start = iq.len();
        let second = dibits(600, 65);
        let mut tail = modulate(&second, params, RATE);
        carrier_after(&mut tail, RATE as usize / 50);
        for (n, s) in tail.iter_mut().enumerate() {
            let phase = std::f64::consts::TAU * 300.0 * n as f64 / f64::from(RATE);
            *s *= Complex::from_polar(1.0, phase as f32);
        }
        iq.extend(tail);
        noise_under_unit(&mut iq, SIGNAL_SNR_DB, seed);
        Scene {
            name: "a second signal after 500 ms".into(),
            iq,
            sent: vec![Sent {
                start,
                dibits: second,
                check: 2..600,
            }],
        }
    }

    /// What `rx` got wrong of `scene`'s checked dibits: per transmission, the wrong (or
    /// missing) indices.
    fn misses(scene: &Scene, rx: &[SoftDibit]) -> Vec<String> {
        scene
            .sent
            .iter()
            .enumerate()
            .filter_map(|(n, s)| {
                let mut got = vec![None; s.dibits.len()];
                for d in rx {
                    let k = ((d.at - s.start as f64) / SPS - 0.5).round();
                    if k >= 0.0 && (k as usize) < got.len() {
                        got[k as usize] = Some(d.dibit);
                    }
                }
                let wrong: Vec<usize> = s
                    .check
                    .clone()
                    .filter(|&k| got[k] != Some(s.dibits[k]))
                    .collect();
                (!wrong.is_empty()).then(|| {
                    let first_right = wrong.last().map_or(0, |w| w + 1);
                    format!(
                        "{}: transmission {n}: {} wrong {:?}; right from {first_right}",
                        scene.name,
                        wrong.len(),
                        &wrong[..wrong.len().min(12)]
                    )
                })
            })
            .collect()
    }

    fn assert_decoded(scenes: impl Fn(Fsk4Params) -> Vec<Scene>) {
        for params in [dmr(), c4fm()] {
            for scene in scenes(params) {
                let mut demod = Fsk4Demod::new(u64::from(RATE), params).expect("rate");
                let rx = feed_all(&mut demod, &scene.iq);
                let misses = misses(&scene, &rx);
                assert!(misses.is_empty(), "{:?}: {misses:#?}", params.shape);
            }
        }
    }

    #[test]
    fn a_signal_after_noise_is_decoded_from_its_start() {
        assert_decoded(|p| vec![after_noise(p, 61)]);
    }

    #[test]
    fn a_signal_after_carrier_is_decoded_from_its_start() {
        assert_decoded(|p| vec![after_carrier(p, 200, 62), after_carrier(p, 1000, 62)]);
    }

    #[test]
    fn bursts_with_silence_between_are_each_decoded() {
        assert_decoded(|p| vec![bursts(p, 63)]);
    }

    #[test]
    fn a_second_signal_after_a_gap_is_decoded_from_its_start() {
        assert_decoded(|p| vec![second_after_gap(p, 66)]);
    }

    #[test]
    fn positions_never_go_backwards() {
        for params in [dmr(), c4fm()] {
            for scene in [
                after_noise(params, 61),
                after_carrier(params, 200, 62),
                after_carrier(params, 1000, 62),
                bursts(params, 63),
                second_after_gap(params, 66),
            ] {
                let mut demod = Fsk4Demod::new(u64::from(RATE), params).expect("rate");
                let rx = feed_all(&mut demod, &scene.iq);
                for w in rx.windows(2) {
                    assert!(
                        w[1].at > w[0].at,
                        "{:?} {}: {} after {}",
                        params.shape,
                        scene.name,
                        w[1].at,
                        w[0].at
                    );
                }
            }
        }
    }

    #[test]
    fn a_run_of_one_symbol_leaves_the_dibits_after_it() {
        let mut tx = dibits(500, 81);
        tx.extend(std::iter::repeat_n(1u8, 300));
        tx.extend(dibits(500, 82));
        assert_decoded(|params| {
            let mut iq = modulate(&tx, params, RATE);
            carrier_after(&mut iq, RATE as usize / 50);
            noise_under_unit(&mut iq, SIGNAL_SNR_DB, 83);
            vec![Scene {
                name: "a run of 300".into(),
                iq,
                sent: vec![Sent {
                    start: 0,
                    dibits: tx.clone(),
                    check: 800..1300,
                }],
            }]
        });
    }

    #[test]
    fn invert_resets_the_timing_detector() {
        let mut demod = Fsk4Demod::new(u64::from(RATE), dmr()).expect("rate");
        demod.feed(&modulate(&dibits(300, 91), dmr(), RATE));
        assert_ne!(demod.last, (0.0, 0.0));
        demod.invert(true);
        assert_eq!(demod.last, (0.0, 0.0));
    }

    #[test]
    fn sync_words_split_into_dibits_first_bit_first() {
        assert_eq!(&sync_dibits(P25_SYNC, 48)[..6], &[1, 1, 1, 1, 1, 3]);
        assert_eq!(sync_dibits(0b10_00_11, 6), vec![2, 0, 3]);
    }
}
