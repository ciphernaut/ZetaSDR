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
//!   decisions) at each of 16 phases across a symbol, each phase from one start and the
//!   closest two from every start.
//! - It takes a fit for a signal when the eye is open, no level holds most of the stretch
//!   (a carrier sits on one), and the eye depends on the timing: its contrast, the
//!   closure half a symbol off over the closure on time, is high. Noise's eye barely
//!   depends on the timing. Where noise also fits (an amplitude over [`NOISE_AMPLITUDE`]
//!   of the declared deviation; noise fits at 1.15 to 2.1, whatever its power) the eye
//!   must be far more open or the contrast far higher. So a transmitter well off its
//!   declared deviation is still taken: 0.6 to 2.0 of it at 15 dB.
//! - **One level's share.** At most 19 of a stretch's 32 symbols may sit on one level;
//!   20 or more are refused ([`SIGNAL_SHARE`]), for acquiring and for holding alike. A
//!   carrier, or a stretch mostly carrier, sits on one level, and a fit can then put that
//!   level anywhere: the limit is what keeps a carrier from being taken for a signal.
//!   Random data puts about 8 of 32 on each level and rarely more than 15 on one; the
//!   48-bit syncs of P25 and DMR put 12 or 13 of their 24 on one level. A stretch mostly
//!   one symbol is never acquired on. But signals do send long runs of one dibit. Every
//!   clear P25 call's header carries one: its zero message indicator, manufacturer and key
//!   IDs (and a low talkgroup's high bits) code to zeros, which keep 20 to 24 searches'
//!   stretches in a row on one level (talkgroup 65000, talkgroup 1, a unit-to-unit call;
//!   [`crate::p25::P25_COAST_SEARCHES`]). A DMR voice burst's centre can carry 24 dibits of
//!   00. So a lock that meets such a stretch coasts through it
//!   ([`Fsk4Params::coast_searches`], set by each mode from its longest run), bounded
//!   because within a stretch a run and a carrier on that level cannot be told apart.
//! - A fit with an amplitude outside [`SIGNAL_AMPLITUDE`] is not taken but counted
//!   ([`Fsk4Counters::off_deviation`]), so a stage can say why a signal is not decoded.
//! - A full search's fit must also explain the next search's stretch, since a stretch
//!   part noise or carrier can fit with a false centre.
//! - While no signal is held, symbols wait [`PENDING_SYMBOLS`] behind. When one is found,
//!   every symbol not yet given out is sliced again on its timing and levels, back to the
//!   signal's first symbol.
//! - A fresh lock is on probation, every symbol held ([`HELD_MAX`] says what that
//!   guarantees): [`PROBATION`] searches if taken on strong evidence, [`WEAK_PROBATION`]
//!   on weak (what noise can rarely pass for). A stretch that does not hold ends it at
//!   once; otherwise it is judged on its mean evidence over the probation, as a weak
//!   signal's scatters from stretch to stretch. A strong lock whose mean is weak serves a
//!   weak lock's probation instead of being called false. It may be refitted on strong
//!   evidence only: at any phase on probation, at the tracked phase after.
//! - **Coasting.** A stretch that fails to hold only on one level's share is coasted
//!   through, for at most the mode's [`Fsk4Params::coast_searches`]: the lock is neither
//!   proven further nor refitted, and on probation every symbol stays held. A proven lock
//!   counts the bound in a row, and stops coasting once its tracked levels move
//!   [`COAST_DRIFT`] from those it was fitted to (a carrier between two levels is dragged
//!   onto one, a run sliced right moves nothing): such a carrier is let go as soon as a
//!   receiver that never coasts lets it go, one on a level after the bound. A lock on
//!   probation counts the bound over its whole probation, and coasts only while its
//!   evidence so far is a signal's, since a lock on the wrong levels can show one of them
//!   taking most of a stretch.
//! - Once proven, the lock's levels are fitted again over all its own symbols and every
//!   held symbol is sliced again on them: levels from one 32-symbol stretch, at 10 dB,
//!   sit a tenth of a decision distance off. (If the proving search's own best fit
//!   corrects the levels instead, that fit slices them, and this refit is not made.)
//! - A taken phase is refined between the 16 searched ones, from the closures either side.
//! - [`Fsk4Demod::flush`] gives out whatever is still held when the input ends; it is
//!   final.
//!
//! **Weak signals.** At 0.8 of the declared deviation and 10 dB (about 8 dB on the
//! mode's own) acquisition from 32-symbol stretches is marginal: measured over 200 lone
//! bursts each, 5 to 8 % of 72-symbol bursts and up to 1 % of 132-symbol ones are lost,
//! and 3 in 500 bursts of a handheld's train. A held signal there is let go now and then
//! when one stretch's eye closes past [`HOLD_CLOSURE`], about once in 4000 searches, and
//! taken again: 7 of 50 clear P25 calls of two superframes and 7 of 50 DMR repeater
//! calls of six counted such a loss, and 3 of the 50 P25 calls a false lock at their
//! start. At 15 dB, or at the declared deviation at 10 dB, none of these happened.
//!
//! [`SoftDibit::at`] is never negative and never goes backwards. It steps by a symbol
//! within 4 % while a lock holds, and by more where a signal is found anew.

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
/// Searches a lock taken on strong evidence must hold before its symbols are given out:
/// until then a false lock (noise that fitted, just before a signal) can still be undone.
pub const PROBATION: usize = 2;
/// Searches a lock taken on weak evidence ([`Verdict::Weak`]), or a strong one whose
/// mean evidence was weak, must hold: noise has been seen to pass for a weak signal four
/// searches in a row, never this many.
pub const WEAK_PROBATION: usize = 8;
/// The eye closure under which held levels are not refitted: a quarter of the way from a
/// level to its threshold.
pub const REFIT_FLOOR: f64 = 1.0 / 144.0;
/// How much better a refit must close the eye for its levels to replace the held ones.
pub const REFIT_RATIO: f64 = 0.5;
/// How much better the fit at a lock's proving must close the eye to slice the held
/// symbols again, and how far (amplitude, and centre, as shares of the amplitude) its
/// levels may lie from the tracked ones: a correction of them, as when the lock was taken
/// from a stretch part carrier. A weak signal's stretch can fit a little more closely to
/// other levels that merge two of its own; those must not be taken.
const PROVEN_REFIT_RATIO: f64 = 0.75;
const PROVEN_REFIT_DRIFT: f64 = 0.1;
/// How far from the levels a proven lock was last fitted to the tracked ones may lie while
/// it coasts (amplitude, and centre, as shares of the amplitude). A carrier between two
/// levels drags the centre a third of the amplitude onto one; at this bound it is let go
/// in the same search as by a receiver that never coasts. A weak signal's levels wander
/// too: over 50 clear P25 calls at 0.8 of the deviation and 10 dB, the bound cost one
/// loss more (7, against 6 with no bound).
pub const COAST_DRIFT: f64 = 0.1;
/// Least-squares rounds in a fit.
const FIT_ROUNDS: usize = 4;
/// Least-squares rounds in a search's first fit of each phase.
const COARSE_ROUNDS: usize = 2;
/// Phases across one symbol a search tries.
const SEARCH_PHASES: i32 = 16;
/// The fewest phases away from the tracked one a search moves the timing; nearer is the
/// loop's to correct.
const SEARCH_MIN_JUMP: i32 = 2;
/// Phases a search fits from every start, the closest from one start first.
const REFINED_PHASES: usize = 2;
/// The most eye closure (mean square distance of the samples from their levels, outer
/// levels at ±1) a held signal may show: three fifths of the way from a level to its
/// threshold.
pub const HOLD_CLOSURE: f64 = 1.0 / 25.0;
/// How far a held signal's amplitude may drift from the one its lock was taken at. Noise
/// after the signal drags the tracked amplitude off (up to noise's 1.2 to 2.1).
pub const HOLD_DRIFT: f64 = 0.2;
/// The most eye closure a stretch may show to be taken for a new signal: half the way
/// from a level to its threshold. Noise alone, fitted, shows 0.017 to 0.04, so this alone
/// does not tell them apart.
pub const ACQUIRE_CLOSURE: f64 = 1.0 / 36.0;
/// The largest share of a stretch's symbols one level may take in a signal. At most 19 of
/// a stretch's 32 symbols may sit on one level; 20 or more are refused.
/// A carrier sits on one level, and a stretch mostly carrier nearly so; random data puts
/// at most about half its 32 symbols on one level, a 48-bit sync of outer symbols 54 %.
pub const SIGNAL_SHARE: f64 = 0.6;
/// The outer amplitude a new signal may show, in declared outer deviations: a stretch
/// with a signal's eye outside it is counted ([`Fsk4Counters::off_deviation`]), not taken.
pub const SIGNAL_AMPLITUDE: (f64, f64) = (0.5, 2.0);
/// The fitted amplitude up to which noise hardly ever fits: noise alone, fitted, shows
/// 1.15 to 2.1 (1st to 99th percentile, all three modes), whatever its power, since a
/// discriminator's output on noise does not depend on it.
pub const NOISE_AMPLITUDE: f64 = 1.1;
/// The contrast (closure half a symbol off over the closure on time) every signal must
/// show. Noise shows 1.65 (median); a signal at 10 dB, 0.8 of its deviation, 3.5 to 3.9
/// (median).
pub const LOW_CONTRAST: f64 = 2.0;
/// The contrast that makes a stretch strong evidence at any amplitude. Noise showed at
/// most 4.7 in 7500 stretches; a signal at 10 dB, 1.2 of its deviation, 6.6 to 7.7
/// (median).
pub const HIGH_CONTRAST: f64 = 5.0;
/// The eye closure noise alone seldom fits to (its 1st percentile is 0.017). Below it,
/// with [`LOW_CONTRAST`], a stretch is strong evidence up to [`NOISE_AMPLITUDE`] and
/// weak evidence above, where runs of noise have been seen to reach it. A signal at
/// 10 dB, 1.2 of its deviation, fits 0.007 (median).
pub const NOISE_CLOSURE: f64 = 1.0 / 64.0;
/// Symbols held back while no signal is held, so that a signal found later can have its
/// first ones sliced again: its stretch's 32, the search that confirms it, and the
/// searches its acquisition may take beyond those.
pub const PENDING_SYMBOLS: usize = 96;
/// The most symbols held while a lock is on probation, or for [`FULL_SEARCH_AFTER`]
/// searches after one failed it.
///
/// What this guarantees: a lock taken with [`PENDING_SYMBOLS`] held holds everything
/// until it is proven, or until the searches after it was found false are over. So none
/// of the symbols it sliced, from its own stretch or before it, goes out on its levels
/// before that decision, and a lock taken in those searches slices them all again.
///
/// What it does not: if no lock follows a false one, the symbols it sliced go out as it
/// sliced them, older ones than its stretch included. And when a second lock is taken
/// before the first's held symbols are out (two on probation in a row), more than this
/// may be held, and the oldest then go out as they stand.
///
/// The way this loses a signal: a burst whose lock never proves (weak, or too short for
/// its probation: a lone 72-symbol P25 terminator cannot serve [`WEAK_PROBATION`]) is
/// held whole, and a noise lock taken before it is out, with no real lock after, gives
/// it out on noise's levels. Measured over 200 lone bursts each, after and before 200 ms
/// of noise: none lost at 15 dB at the declared deviation or 0.8 of it; at 10 dB none at
/// the declared deviation, and at 0.8 of it 0 to 1 % of 132-symbol bursts and 5 to 8 %
/// of 72-symbol ones.
///
/// It is 200 symbols: 42 ms at 4800 baud, 83 ms at 2400. A receiver holds one search's
/// symbols more for each search its mode lets a lock on probation coast
/// ([`Fsk4Params::coast_searches`]), so that the guarantee covers a coasted probation:
/// 456 symbols (95 ms) for P25's 32, 296 (62 ms) for DMR's 12.
pub const HELD_MAX: usize = 2 * PENDING_SYMBOLS + SEARCH_EVERY;
// A lock re-slices from the last symbol given out to where the receiver had got, which can
// be one search past `PENDING_SYMBOLS`: hence the margin.
const _: () = assert!(
    PENDING_SYMBOLS + SEARCH_EVERY + (WEAK_PROBATION + FULL_SEARCH_AFTER) * SEARCH_EVERY
        <= HELD_MAX
);

/// The receive filter: matched to the transmitter's pulse.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    /// A root raised cosine of roll-off `alpha` at each end (DMR: 0.2).
    Rrc { alpha: f64 },
    /// P25's C4FM: an integrate-and-dump over one symbol after the transmitter's raised
    /// cosine and inverse-sinc shaping.
    C4fm,
}

/// What a 4FSK signal is: its symbol rate, the deviation of its outer symbols, its pulse,
/// and how long its lock may coast on one level.
#[derive(Clone, Copy, Debug)]
pub struct Fsk4Params {
    pub symbol_rate_baud: u32,
    pub deviation_hz: f64,
    pub shape: Shape,
    /// The most searches a lock coasts through stretches that fail only on
    /// [`SIGNAL_SHARE`] (one level taking 20 or more of 32; the eye still closed on the
    /// held levels to within [`HOLD_CLOSURE`] and the amplitude within [`HOLD_DRIFT`]): in
    /// a row once it is proven, in all over its probation. Coasting, the lock is neither
    /// proven further nor refitted; one more such stretch and it is let go, as a lock
    /// that does not hold is. The receiver holds this many searches' symbols more on
    /// probation ([`HELD_MAX`]).
    ///
    /// Within one stretch a run of one dibit and a carrier sitting on that level cannot be
    /// told apart, so this is a bound, not a test: set it from the longest run of one
    /// dibit the mode can send, as the searches in a row whose stretch it fills (a search
    /// every [`SEARCH_EVERY`] symbols), with a margin. Each search coasted is that much
    /// longer a carrier on a held level is taken for the signal before the lock is lost.
    /// 0 coasts not at all.
    pub coast_searches: u32,
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

/// What a receiver has seen, for a stage to put in its job's context. Every count is
/// since the receiver was made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fsk4Counters {
    /// Signals locked on (refits of a held one not counted).
    pub locks: u64,
    /// Locks undone on probation: a stretch that fitted like a signal and then did not
    /// hold (noise, or a fit to a stretch part noise or carrier).
    pub false_locks: u64,
    /// Proven locks lost: the signal ended or faded.
    pub losses: u64,
    /// Stretches whose eye was open as a signal's but whose amplitude lay outside
    /// [`SIGNAL_AMPLITUDE`], so they were not taken: a transmitter that far off the
    /// declared deviation shows here and nowhere else.
    pub off_deviation: u64,
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
    /// Input samples fed.
    fed: u64,
    /// Matched-filter output from sample `base`, outer deviation at ±1.
    buf: Vec<f32>,
    base: u64,
    /// Where the next symbol's centre is expected, in matched-filter samples.
    next: f64,
    /// Whether the last search found the signal (and the levels and timing on it).
    locked: bool,
    /// Searches in a row that found no signal.
    unheld: usize,
    /// Searches left, after a lock failed its probation, for which every symbol is held.
    grace: usize,
    /// Searches the lock has held since it was taken, and the searches it must hold to be
    /// proven ([`PROBATION`] or [`WEAK_PROBATION`]).
    proven: usize,
    probation: usize,
    /// The evidence the lock was taken on, which it must show on probation.
    lock_class: Verdict,
    /// Whether [`Fsk4Demod::flush`] has run: nothing more is given out.
    flushed: bool,
    /// Searches the lock has coasted through (in a row once proven, in all on probation),
    /// and the most it may ([`Fsk4Params::coast_searches`]).
    coasted: usize,
    coast_max: usize,
    /// The levels (centre, amplitude) the lock was last fitted to.
    steady: (f64, f64),
    /// The most symbols held while a lock is on probation or just failed it: [`HELD_MAX`],
    /// and a search's symbols more for each search a lock on probation may coast.
    held_max: usize,
    /// Where the stretch the lock was last fitted on starts.
    lock_start: f64,
    /// The held stretches' closures and contrasts summed over the probation so far.
    trial: (f64, f64),
    /// The amplitude the lock was taken at.
    lock_amplitude: f64,
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
    counters: Fsk4Counters,
    /// Which of the acquisition's transitions were taken, for the tests.
    #[cfg(test)]
    paths: std::cell::Cell<Paths>,
}

/// How often each of the acquisition's rarer transitions was taken.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
struct Paths {
    /// A strong lock whose mean evidence was weak, demoted to a weak lock's probation.
    demoted: u32,
    /// Locks taken in the search that found the last one false.
    relocked: u32,
    /// Locks taken in the searches after a false lock while every symbol is held.
    in_grace: u32,
    /// Refits on probation that closed the eye far better but only weakly: refused.
    weak_refit_refused: u32,
    /// Fits at proving that closed the eye better but moved the levels too far: refused.
    proven_refit_refused: u32,
    /// Fits in `settle` that moved the levels too far: refused.
    settle_refused: u32,
    /// Symbols given out while every symbol is to be held (on probation, or in the searches
    /// after a false lock): only past [`HELD_MAX`].
    out_while_held: u32,
    /// Proving refits taken, and `settle`s run.
    proven_refits: u32,
    settles: u32,
    /// Searches a lock coasted through, and coasts ended at the bound.
    coasts: u32,
    coasts_ended: u32,
    /// Locks found false on probation by a stretch that failed only on one level's share.
    false_on_share: u32,
    /// Searches made.
    searches: u32,
    /// The most searches a proven lock coasted in a row, and a lock on probation in all.
    most_coasted_proven: u32,
    most_coasted_on_probation: u32,
    /// Refits taken on probation.
    probation_refits: u32,
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
    /// The closure half a symbol away on the same levels, over this one: how much the eye
    /// depends on the timing. Noise's barely does. Zero until measured.
    contrast: f64,
}

/// What a stretch looks like.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Verdict {
    /// A signal noise hardly ever passes for.
    Strong,
    /// A signal as weak as noise can look (noise below [`NOISE_AMPLITUDE`] is rare, not
    /// unknown): a lock on it serves [`WEAK_PROBATION`].
    Weak,
    /// A signal's eye, but an amplitude outside [`SIGNAL_AMPLITUDE`].
    OffDeviation,
    Not,
}

impl Verdict {
    fn is_signal(self) -> bool {
        matches!(self, Self::Strong | Self::Weak)
    }
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
            contrast: 0.0,
        }
    }

    /// Whether the stretch is a new signal on these levels. It must have the eye open to
    /// within [`ACQUIRE_CLOSURE`], no one level taking most of it, and a contrast of
    /// [`LOW_CONTRAST`]. Then it is:
    /// - strong if its contrast reaches [`HIGH_CONTRAST`], or its eye is open past
    ///   [`NOISE_CLOSURE`] at an amplitude noise rarely fits at (up to
    ///   [`NOISE_AMPLITUDE`]): noise hardly ever shows either;
    /// - weak if its eye is open past [`NOISE_CLOSURE`] at a higher amplitude, or its
    ///   amplitude is that low but its eye no more than open: noise can, rarely, and runs
    ///   of it a few searches long have been seen;
    /// - [`Verdict::OffDeviation`] instead of either if its amplitude lies outside
    ///   [`SIGNAL_AMPLITUDE`].
    fn verdict(&self) -> Verdict {
        if self.closure >= ACQUIRE_CLOSURE
            || self.share > SIGNAL_SHARE
            || self.contrast < LOW_CONTRAST
        {
            return Verdict::Not;
        }
        let low = self.amplitude <= NOISE_AMPLITUDE;
        let open = self.closure < NOISE_CLOSURE;
        let strong = self.contrast >= HIGH_CONTRAST || (open && low);
        if !strong && !open && !low {
            Verdict::Not
        } else if !(SIGNAL_AMPLITUDE.0..=SIGNAL_AMPLITUDE.1).contains(&self.amplitude) {
            Verdict::OffDeviation
        } else if strong {
            Verdict::Strong
        } else {
            Verdict::Weak
        }
    }

    /// Whether the stretch still shows the signal a lock taken at amplitude `taken` holds:
    /// the amplitude within [`HOLD_DRIFT`] of `taken`, the eye open to within
    /// [`HOLD_CLOSURE`], no one level taking most of it.
    fn holds(&self, taken: f64) -> bool {
        (self.amplitude / taken - 1.0).abs() <= HOLD_DRIFT
            && self.closure < HOLD_CLOSURE
            && self.share <= SIGNAL_SHARE
    }

    /// Whether the stretch fails [`Self::holds`] on one level's share alone: a lock may
    /// coast through it ([`Fsk4Params::coast_searches`]).
    fn coasts(&self, taken: f64) -> bool {
        self.share > SIGNAL_SHARE
            && Fit {
                share: 0.0,
                ..*self
            }
            .holds(taken)
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
            fed: 0,
            buf: Vec::new(),
            base: 0,
            // Half a symbol in, so a search's earliest phase is the first sample.
            next: delay + sps / 2.0,
            locked: false,
            unheld: FULL_SEARCH_AFTER,
            grace: 0,
            proven: 0,
            probation: PROBATION,
            lock_class: Verdict::Strong,
            trial: (0.0, 0.0),
            lock_start: 0.0,
            flushed: false,
            coasted: 0,
            steady: (0.0, 1.0),
            coast_max: params.coast_searches as usize,
            held_max: HELD_MAX + params.coast_searches as usize * SEARCH_EVERY,
            lock_amplitude: 1.0,
            candidate: None,
            until_search: SEARCH_SYMBOLS,
            pending: VecDeque::new(),
            given: f64::NEG_INFINITY,
            centre: 0.0,
            amplitude: 1.0,
            sign: 1.0,
            last: (0.0, 0.0),
            counters: Fsk4Counters::default(),
            #[cfg(test)]
            paths: std::cell::Cell::new(Paths::default()),
        })
    }

    /// The symbols `iq` completes, in position order. A symbol can come out later than
    /// the samples that complete it: while no signal is held, by up to
    /// [`PENDING_SYMBOLS`] (20 ms at 4800 baud, 40 ms at 2400); while a lock is on
    /// probation or just failed it, by up to [`HELD_MAX`] (42 ms at 4800 baud, 83 ms at
    /// 2400) and a search's symbols more for each search the mode lets a lock coast; add
    /// the filters' delay, about 7 symbols. A proven lock gives each symbol out as it is
    /// sliced.
    pub fn feed(&mut self, iq: &[Complex<f32>]) -> Vec<SoftDibit> {
        if self.flushed {
            return Vec::new();
        }
        self.fed += iq.len() as u64;
        self.filter(iq);
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
            // Locked past probation, every symbol goes out. On probation, and a few
            // searches after a lock that failed it, every symbol is held, so the next lock
            // can still slice again what a false one sliced. Otherwise they wait their turn.
            let hold = if self.locked && self.proven >= self.probation {
                0
            } else if self.locked || self.grace > 0 {
                self.held_max
            } else {
                PENDING_SYMBOLS
            };
            self.give(hold, &mut out);
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

    /// Every symbol still held back, and the last ones the filters still owe, as if the
    /// input were followed by silence: for a stage to call when its input ends. Symbols
    /// whose centres lie past the last sample fed are not made up. A flush is final: the
    /// receiver gives out nothing after it, from [`feed`](Self::feed) or another flush,
    /// and a stage whose input starts again makes a new one.
    pub fn flush(&mut self) -> Vec<SoftDibit> {
        if self.flushed {
            return Vec::new();
        }
        self.flushed = true;
        let silence = (self.delay + 2.0 * self.sps).ceil() as usize;
        self.filter(&vec![Complex::new(0.0, 0.0); silence]);
        let end = self.fed as f64;
        while self.holds(self.next) && self.next - self.delay < end {
            let d = self.symbol();
            self.pending.push_back(d);
        }
        let mut out = Vec::new();
        self.give(0, &mut out);
        out
    }

    /// What the receiver has seen so far.
    pub fn counters(&self) -> Fsk4Counters {
        self.counters
    }

    /// Set by the framer once sync tells the polarity: `true` slices the discriminator
    /// upside down, from the next symbol given out (held ones are turned too). The same
    /// polarity again changes nothing.
    pub fn invert(&mut self, inverted: bool) {
        let sign = if inverted { -1.0 } else { 1.0 };
        if sign == self.sign {
            return;
        }
        for d in &mut self.pending {
            d.dibit ^= 0b10;
            d.soft[0] = 255 - d.soft[0];
        }
        self.sign = sign;
        self.last = (0.0, 0.0);
    }

    /// `iq` through the channel low-pass, the discriminator and the matched filter, onto
    /// the end of the buffer.
    fn filter(&mut self, iq: &[Complex<f32>]) {
        let headroom = DISCRIMINATOR_HEADROOM as f32;
        let mut freq = nbfm_demod(
            &self.channel.process(iq),
            self.rate_hz,
            (self.deviation_hz * DISCRIMINATOR_HEADROOM as f64).round() as u64,
            &mut self.prev,
        );
        freq.iter_mut().for_each(|x| *x *= headroom);
        self.buf.extend(self.matched.process(&freq));
    }

    /// Give out the held symbols beyond the last `hold`.
    fn give(&mut self, hold: usize, out: &mut Vec<SoftDibit>) {
        #[cfg(test)]
        {
            let out = self.pending.len().saturating_sub(hold) as u32;
            if hold == self.held_max {
                self.note(|p| p.out_while_held += out);
            }
        }
        while self.pending.len() > hold {
            let d = self.pending.pop_front().expect("pending");
            self.given = d.at + self.delay;
            out.push(d);
        }
    }

    /// Note a transition taken, for the tests.
    #[cfg(test)]
    fn note(&self, f: impl FnOnce(&mut Paths)) {
        let mut paths = self.paths.get();
        f(&mut paths);
        self.paths.set(paths);
    }

    /// Whether the samples to interpolate at `pos` are held.
    fn holds(&self, pos: f64) -> bool {
        pos.floor() as u64 + 1 < self.base + self.buf.len() as u64
    }

    /// The search over the last [`SEARCH_SYMBOLS`] symbols' stretch, every
    /// [`SEARCH_EVERY`] symbols.
    ///
    /// Locked, in this order:
    /// - a fit afresh on strong evidence that closes the eye at least [`REFIT_RATIO`]
    ///   better than the held levels shows them wrong (taken from a stretch part noise or
    ///   carrier), and is taken instead: at any phase on probation, at the tracked phase
    ///   after it;
    /// - before either, a stretch that fails only on one level's share is coasted through
    ///   ([`Fsk4Params::coast_searches`]): the lock neither proven further nor refitted;
    /// - otherwise the lock holds while its stretch does ([`Fit::holds`]: the eye within
    ///   [`HOLD_CLOSURE`], no level taking most of it, the amplitude within
    ///   [`HOLD_DRIFT`]). On probation, at its last search, the mean evidence over the
    ///   probation must also be what the lock was taken on: a strong lock whose mean is
    ///   weak is demoted to [`WEAK_PROBATION`] instead. At proving, the best fit of the
    ///   stretch slices the held symbols again if it is a correction of the levels
    ///   ([`proving_refit`]); if not, [`Self::settle`] refits them over the lock's own
    ///   symbols.
    ///
    /// Not holding (and a lock on probation that did not hold is false), the stretch is
    /// fitted: at the tracked phase alone while a signal was held within
    /// [`FULL_SEARCH_AFTER`] searches (the same one, faded or hit), at each of
    /// [`SEARCH_PHASES`] phases otherwise or after a false lock. A full search's fit must be
    /// a signal ([`Fit::verdict`]) and also explain the next search's stretch before it is
    /// taken, as a stretch half carrier or noise can fit falsely; so no lock is taken in
    /// the search that finds one false. A fit taken locks the receiver ([`Self::adopt`]);
    /// none, and symbols are held.
    fn search(&mut self) {
        #[cfg(test)]
        self.note(|p| p.searches += 1);
        let from = self.next - SEARCH_SYMBOLS as f64 * self.sps;
        if from - self.sps / 2.0 < self.base as f64 {
            return;
        }
        let ys: Vec<f64> = (0..SEARCH_SYMBOLS)
            .map(|j| self.y(from + j as f64 * self.sps))
            .collect();
        let held = Fit::of(&ys, self.centre, self.amplitude, self.sign);
        let was_locked = self.locked;
        let probation = self.proven < self.probation;
        // A lock coasts through a stretch that fails only on one level's share (a run of
        // one dibit, as every clear P25 header carries), for at most `coast_max` searches:
        // no credit toward its probation, no refit, every symbol still held while it is
        // on probation. Proven, the bound counts the searches in a row; on probation,
        // every one of the probation's, so `held_max` covers them. On probation it also
        // needs the evidence so far to be a signal's (the eye open and timed over the
        // stretches held; none yet, and the lock was just taken on one): a lock on the
        // wrong levels, as on a transmitter far off its deviation, can show one of them
        // taking most of a stretch. And the levels must still be near those the lock was
        // fitted to ([`COAST_DRIFT`]): a run sliced right moves neither, while the
        // decision-directed levels follow a carrier between two levels onto one. Such a
        // carrier, and one off every level, is let go at once; one on a level, after the
        // bound.
        if held.share <= SIGNAL_SHARE && !probation {
            self.coasted = 0;
        }
        let steady = probation || {
            let (centre, amplitude) = self.steady;
            (self.amplitude / amplitude - 1.0).abs() <= COAST_DRIFT
                && (self.centre - centre).abs() < COAST_DRIFT * amplitude
        };
        let evident = !probation || self.proven == 0 || {
            let n = self.proven as f64;
            self.trial.0 / n < ACQUIRE_CLOSURE && self.trial.1 / n >= LOW_CONTRAST
        };
        let coasts = self.locked && held.coasts(self.lock_amplitude) && evident && steady;
        let coasting = coasts && self.coasted < self.coast_max;
        #[cfg(test)]
        if coasts && !coasting {
            self.note(|p| p.coasts_ended += 1);
        }
        if coasting {
            self.coasted += 1;
            self.unheld = 0;
            #[cfg(test)]
            {
                let coasted = self.coasted as u32;
                self.note(|p| {
                    p.coasts += 1;
                    let most = if probation {
                        &mut p.most_coasted_on_probation
                    } else {
                        &mut p.most_coasted_proven
                    };
                    *most = (*most).max(coasted);
                });
            }
            return;
        }
        let stretch = |d: f64| -> Vec<f64> {
            (0..SEARCH_SYMBOLS)
                .map(|j| self.y(from + d + j as f64 * self.sps))
                .collect()
        };
        // `fit` with its contrast: the closure half a symbol on, on the same levels.
        let timed = |d: f64, mut fit: Fit| -> Fit {
            let off = Fit::of(
                &stretch(d + self.sps / 2.0),
                fit.centre,
                fit.amplitude,
                self.sign,
            );
            fit.contrast = off.closure / fit.closure.max(f64::MIN_POSITIVE);
            fit
        };
        let offset = |i: i32| f64::from(i) / f64::from(SEARCH_PHASES) * self.sps;
        // The phase at `d` refined between the searched ones: the closures a phase step
        // either side, on the same levels, fitted to a parabola.
        let fine = |d: f64, fit: &Fit| -> f64 {
            let step = self.sps / f64::from(SEARCH_PHASES);
            if from + d - step - 1.0 < self.base as f64 {
                return d;
            }
            let at = |d: f64| Fit::of(&stretch(d), fit.centre, fit.amplitude, self.sign).closure;
            let (before, after) = (at(d - step), at(d + step));
            let curve = before - 2.0 * fit.closure + after;
            if curve <= 0.0 {
                return d;
            }
            d + (0.5 * (before - after) / curve).clamp(-0.5, 0.5) * step
        };
        let every = -SEARCH_PHASES / 2..SEARCH_PHASES / 2;
        // Each phase's fit, the closest first: each phase from one start, then the
        // closest few from every start.
        let fits = |phases: std::ops::Range<i32>| -> Vec<(i32, Fit)> {
            let mut fits: Vec<(i32, Fit)> = phases
                .map(|i| (i, self.fit(&stretch(offset(i)), false)))
                .collect();
            fits.sort_by(|a, b| a.1.closure.total_cmp(&b.1.closure));
            for f in fits.iter_mut().take(REFINED_PHASES) {
                f.1 = self.fit(&stretch(offset(f.0)), true);
            }
            fits.sort_by(|a, b| a.1.closure.total_cmp(&b.1.closure));
            fits
        };
        let held = timed(0.0, held);
        if self.locked && held.closure > REFIT_FLOOR {
            let (phase, refit) = fits(if probation { every.clone() } else { 0..1 })[0];
            let refit = timed(offset(phase), refit);
            // Only on strong evidence, on probation and after it: a stretch part signal, part
            // the noise after it, can fit far better than the held levels and weakly, and
            // would slice the signal again.
            let better = refit.closure < REFIT_RATIO * held.closure;
            #[cfg(test)]
            if better && refit.verdict() == Verdict::Weak && probation {
                self.note(|p| p.weak_refit_refused += 1);
            }
            if better && refit.verdict() == Verdict::Strong {
                let at = from + fine(offset(phase), &refit);
                #[cfg(test)]
                if probation {
                    self.note(|p| p.probation_refits += 1);
                }
                // The same lock refitted: its probation's searches, evidence and coasts
                // stand.
                let (proven, trial, coasted) = (self.proven, self.trial, self.coasted);
                self.adopt(at, refit);
                (self.proven, self.trial, self.coasted) = (proven, trial, coasted);
                self.unheld = 0;
                return;
            }
        }
        // On probation a stretch that does not hold ends the lock at once. Otherwise the
        // lock is judged on its mean evidence over the whole probation, as a weak signal's
        // contrast scatters from stretch to stretch: at its last search the mean must show
        // the evidence the lock was taken on. A strong lock whose mean shows only weak
        // evidence is not false but weak: it serves a weak lock's probation instead.
        let last = probation && self.proven + 1 == self.probation;
        let mean = {
            let n = (self.proven + 1) as f64;
            Fit {
                closure: (self.trial.0 + held.closure) / n,
                contrast: (self.trial.1 + held.contrast) / n,
                ..held
            }
            .verdict()
        };
        let demote = last && self.lock_class == Verdict::Strong && mean == Verdict::Weak;
        let holding = held.holds(self.lock_amplitude)
            && (!last
                || demote
                || match self.lock_class {
                    Verdict::Strong => mean == Verdict::Strong,
                    _ => mean.is_signal(),
                });
        if self.locked && holding {
            // Proven now: this stretch is all signal, which the one the lock was taken from
            // may not have been. Its best fit, if any better, slices again every symbol not
            // yet given out.
            let refit = (last && !demote).then(|| {
                let (phase, refit) = fits(every)[0];
                let refit = timed(offset(phase), refit);
                (from + fine(offset(phase), &refit), refit)
            });
            let decided =
                refit.map(|(at, r)| (at, r, proving_refit(&held, &r, self.centre, self.amplitude)));
            #[cfg(test)]
            let refused = decided.is_some_and(|(_, _, (_, refused))| refused);
            let better = decided
                .filter(|(_, _, (take, _))| *take)
                .map(|(at, r, _)| (at, r));
            #[cfg(test)]
            {
                self.note(|p| p.proven_refit_refused += u32::from(refused));
                self.note(|p| p.demoted += u32::from(demote));
            }
            if probation {
                self.trial.0 += held.closure;
                self.trial.1 += held.contrast;
            }
            self.unheld = 0;
            self.proven += 1;
            if self.proven == self.probation {
                self.coasted = 0;
            }
            if demote {
                self.lock_class = Verdict::Weak;
                self.probation = WEAK_PROBATION;
            }
            // Proven: the levels fitted again over the lock's own symbols, unless the proving
            // fit has just sliced them again on a stretch of its own.
            if let Some((at, refit)) = better {
                self.adopt(at, refit);
                self.proven = self.probation;
                #[cfg(test)]
                {
                    self.note(|p| p.proven_refits += 1);
                }
            } else if last && !demote {
                self.settle();
            }
            return;
        }
        let false_lock = self.locked && probation;
        #[cfg(test)]
        if false_lock && held.coasts(self.lock_amplitude) {
            self.note(|p| p.false_on_share += 1);
        }
        let full = false_lock || self.unheld >= FULL_SEARCH_AFTER;
        let fits = fits(if full { every } else { 0..1 });
        let best = fits[0];
        // The tracked phase, if the best is too near it to be worth a jump.
        let (phase, fit) = match fits.iter().find(|f| f.0 == 0) {
            Some(&(_, tracked))
                if best.0.abs() < SEARCH_MIN_JUMP && timed(0.0, tracked).verdict().is_signal() =>
            {
                (0, tracked)
            }
            _ => best,
        };
        let start = from + offset(phase);
        let fit = timed(offset(phase), fit);
        let verdict = fit.verdict();
        let signal = verdict.is_signal();
        // The last full search's fit, if it explains this stretch on its own grid; of it and
        // this search's fit, the one explaining this stretch better is taken.
        let confirmed = self.candidate.and_then(|(at, c)| {
            let d = at + ((from - at) / self.sps).round() * self.sps - from;
            let again = timed(d, Fit::of(&stretch(d), c.centre, c.amplitude, self.sign));
            again.verdict().is_signal().then_some((from + d, again))
        });
        let taken = match confirmed {
            Some(again) if !signal || again.1.closure < fit.closure => Some(again),
            Some(_) => Some((start, fit)),
            None if !full && signal => Some((start, fit)),
            None => None,
        };
        let taken = taken.map(|(at, fit)| (from + fine(at - from, &fit), fit));
        self.counters.false_locks += u64::from(false_lock);
        self.counters.off_deviation += u64::from(verdict == Verdict::OffDeviation);
        self.candidate = (full && signal).then_some((start, fit));
        self.locked = taken.is_some();
        if let Some((start, fit)) = taken {
            #[cfg(test)]
            {
                self.note(|p| p.relocked += u32::from(false_lock));
                self.note(|p| p.in_grace += u32::from(!false_lock && self.grace > 0));
            }
            if !was_locked || false_lock {
                self.counters.locks += 1;
            }
            self.unheld = 0;
            self.grace = 0;
            self.candidate = None;
            self.adopt(start, fit);
            self.lock_class = fit.verdict();
            self.probation = if self.lock_class == Verdict::Strong {
                PROBATION
            } else {
                WEAK_PROBATION
            };
        } else {
            if was_locked && !probation {
                self.counters.losses += 1;
            }
            (self.unheld, self.grace) = if false_lock {
                (FULL_SEARCH_AFTER, FULL_SEARCH_AFTER)
            } else {
                (self.unheld + 1, self.grace.saturating_sub(1))
            };
        }
    }

    /// A lock just proven: its levels fitted again over every held symbol from its own
    /// stretch on, all of them signal (48 to 96 symbols, where a search's stretch has
    /// 32), and every held symbol sliced again on them where it is. Levels from one
    /// stretch sit, at 10 dB, a tenth of a decision distance off; these sit nearer. A fit
    /// that moves them further than [`PROVEN_REFIT_DRIFT`] is not used.
    fn settle(&mut self) {
        let from = self.lock_start;
        let ys: Vec<f64> = self
            .pending
            .iter()
            .map(|d| d.at + self.delay)
            .filter(|&t| t >= from && t - 1.0 >= self.base as f64 && self.holds(t))
            .map(|t| self.y(t))
            .collect();
        if ys.len() < SEARCH_SYMBOLS {
            return;
        }
        let fit = self.fit(&ys, true);
        if !levels_near(&fit, self.centre, self.amplitude) {
            #[cfg(test)]
            {
                self.note(|p| p.settle_refused += 1);
            }
            return;
        }
        #[cfg(test)]
        {
            self.note(|p| p.settles += 1);
        }
        self.steady = (fit.centre, fit.amplitude);
        let (base, end) = (self.base as f64, self.base + self.buf.len() as u64);
        for i in 0..self.pending.len() {
            let t = self.pending[i].at + self.delay;
            if t - 1.0 >= base && (t.floor() as u64) + 1 < end {
                let z = self.sign * (self.y(t) - fit.centre) / fit.amplitude;
                let d = &mut self.pending[i];
                (d.dibit, d.soft) = slice(z);
            }
        }
    }

    /// Lock on `fit`, whose stretch starts at `start`: the levels from it and the timing on
    /// its grid. Every symbol after the last one given out is sliced again: those before
    /// the stretch as the fit says (they may be the signal's first, or nothing), those
    /// from it on tracked as usual, up to where the receiver had got.
    fn adopt(&mut self, start: f64, fit: Fit) {
        let reached = self.next;
        self.lock_start = start;
        self.centre = fit.centre;
        self.amplitude = fit.amplitude;
        self.lock_amplitude = fit.amplitude;
        self.steady = (fit.centre, fit.amplitude);
        self.last = (0.0, 0.0);
        self.coasted = 0;
        self.proven = 0;
        self.trial = (0.0, 0.0);
        self.pending.clear();
        let after = self.given + self.sps / 2.0;
        // As far back as the samples go, before anything is given out, and never before
        // the input's first sample.
        let floor = after.max(self.base as f64 + 1.0).max(self.delay);
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
            if d.at + self.delay > after && d.at >= 0.0 {
                self.pending.push_back(d);
            }
        }
    }

    /// `ys` (raw samples) fitted to the levels: slice, then the least-squares centre and
    /// amplitude for those decisions, round after round. With `every_start`, for
    /// [`FIT_ROUNDS`] from the samples' mean or the tracked centre and from the declared or
    /// the tracked amplitude (which noise drags off), the closest fit kept; otherwise for
    /// [`COARSE_ROUNDS`] from the mean and the declared amplitude alone. One level alone
    /// keeps the amplitude.
    fn fit(&self, ys: &[f64], every_start: bool) -> Fit {
        let n = ys.len() as f64;
        let mean = ys.iter().sum::<f64>() / n;
        let rounds = if every_start {
            FIT_ROUNDS
        } else {
            COARSE_ROUNDS
        };
        let from = |mut centre: f64, mut amplitude: f64| {
            for _ in 0..rounds {
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
        let starts: &[(f64, f64)] = if every_start {
            &[
                (mean, 1.0),
                (mean, self.amplitude),
                (self.centre, 1.0),
                (self.centre, self.amplitude),
            ]
        } else {
            &[(mean, 1.0)]
        };
        starts
            .iter()
            .map(|&(c, a)| from(c, a))
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

/// Whether `fit`'s levels lie within [`PROVEN_REFIT_DRIFT`] of `centre` and `amplitude`:
/// the amplitude within that share of it, the centre within that share of the amplitude.
fn levels_near(fit: &Fit, centre: f64, amplitude: f64) -> bool {
    (fit.amplitude / amplitude - 1.0).abs() <= PROVEN_REFIT_DRIFT
        && (fit.centre - centre).abs() < PROVEN_REFIT_DRIFT * amplitude
}

/// Whether the best fit at a lock's proving, `refit`, slices the held symbols again, and
/// whether it was refused for its levels: (taken, refused). It must be a signal and close
/// the eye by [`PROVEN_REFIT_RATIO`] on the held levels' `held`, and be a correction of
/// the tracked levels (`centre`, `amplitude`), not other ones ([`levels_near`]): a weak
/// signal's stretch can fit more closely to levels that merge two of its own.
fn proving_refit(held: &Fit, refit: &Fit, centre: f64, amplitude: f64) -> (bool, bool) {
    let closer = refit.verdict().is_signal() && refit.closure < PROVEN_REFIT_RATIO * held.closure;
    let near = levels_near(refit, centre, amplitude);
    (closer && near, closer && !near)
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
    use crate::dmr::DMR_COAST_SEARCHES;
    use crate::p25::P25_COAST_SEARCHES;
    /// Dibits in a P25 header data unit, status symbols included.
    const P25_HEADER_DIBITS: usize = 396;
    /// The scenes of `held_symbols_stay_held_until_the_lock_is_decided` measured with no
    /// lock in the grace.
    const MEASURED_CHECKED: usize = 42;
    /// The calls of `held_symbols_stay_held_through_a_clear_p25_header` measured with no
    /// lock in the grace.
    const MEASURED_P25_CHECKED: usize = 79;
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
            coast_searches: P25_COAST_SEARCHES,
        }
    }

    fn dmr() -> Fsk4Params {
        Fsk4Params {
            symbol_rate_baud: BAUD,
            deviation_hz: DMR_DEVIATION_HZ,
            shape: Shape::Rrc { alpha: DMR_ALPHA },
            coast_searches: DMR_COAST_SEARCHES,
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
            coast_searches: DMR_COAST_SEARCHES,
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

    /// The rate the acquisition tests run at.
    const RATE: u32 = 48_000;
    /// The SNR the acquisition tests' signals sit at, in 12.5 kHz.
    const SIGNAL_SNR_DB: f64 = 15.0;
    /// A DMR handheld's burst and the gap after it, s.
    const BURST_S: f64 = 0.0275;
    const BURST_GAP_S: f64 = 0.0325;

    /// A 2400-baud mode in a 6.25 kHz channel (NXDN and dPMR's rate).
    fn half() -> Fsk4Params {
        Fsk4Params {
            symbol_rate_baud: 2400,
            deviation_hz: 1050.0,
            shape: Shape::Rrc { alpha: 0.2 },
            coast_searches: DMR_COAST_SEARCHES,
        }
    }

    /// How a scene's signal goes out: a mode, its transmitter's deviation as a share of
    /// the mode's, the SNR in 12.5 kHz, and the noise's seed.
    #[derive(Clone, Copy, Debug)]
    struct Tx {
        params: Fsk4Params,
        deviation: f64,
        snr_db: f64,
        seed: u64,
    }

    impl Tx {
        fn new(params: Fsk4Params, seed: u64) -> Self {
            Self {
                params,
                deviation: 1.0,
                snr_db: SIGNAL_SNR_DB,
                seed,
            }
        }

        fn off(self, deviation: f64, snr_db: f64) -> Self {
            Self {
                deviation,
                snr_db,
                ..self
            }
        }

        /// `dibits` as this transmitter sends them.
        fn modulate(&self, dibits: &[u8]) -> Vec<Complex<f32>> {
            let sent = Fsk4Params {
                deviation_hz: self.params.deviation_hz * self.deviation,
                ..self.params
            };
            modulate(dibits, sent, RATE)
        }

        fn sps(&self) -> f64 {
            f64::from(RATE) / f64::from(self.params.symbol_rate_baud)
        }

        /// Symbols in `s` seconds.
        fn symbols(&self, s: f64) -> usize {
            (f64::from(self.params.symbol_rate_baud) * s).round() as usize
        }
    }

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

    /// A capture and what was sent in it, for the receiver of `tx.params`.
    struct Scene {
        name: String,
        tx: Tx,
        iq: Vec<Complex<f32>>,
        sent: Vec<Sent>,
    }

    /// `n` samples of unmodulated carrier at the signal's level, at `iq`'s last phase.
    fn carrier_after(iq: &mut Vec<Complex<f32>>, n: usize) {
        let last = iq.last().copied().unwrap_or(Complex::new(1.0, 0.0));
        iq.extend(std::iter::repeat_n(last, n));
    }

    /// Lead-in `iq`, then 600 dibits, then 20 ms of carrier. The carrier lets the receive
    /// filters give out the signal's last symbols undisturbed: a capture that ends at its
    /// last symbol cannot give its last few through a 12-symbol filter, and noise right
    /// after them reaches them through its tail.
    fn after_lead(name: &str, mut iq: Vec<Complex<f32>>, tx: Tx) -> Scene {
        let start = iq.len();
        let dibits = dibits(600, tx.seed);
        iq.extend(tx.modulate(&dibits));
        carrier_after(&mut iq, RATE as usize / 50);
        noise_under_unit(&mut iq, tx.snr_db, tx.seed + 100);
        Scene {
            name: name.into(),
            tx,
            iq,
            sent: vec![Sent {
                start,
                dibits,
                check: 2..600,
            }],
        }
    }

    /// `s` seconds of noise alone, then a signal ([`after_lead`]: 20 ms of carrier after).
    fn after_noise_for(tx: Tx, s: f64) -> Scene {
        let lead = vec![Complex::new(0.0, 0.0); (f64::from(RATE) * s) as usize];
        after_lead(&format!("{s} s of noise"), lead, tx)
    }

    fn after_noise(tx: Tx) -> Scene {
        after_noise_for(tx, 1.0)
    }

    /// `ms` of carrier, then a signal ([`after_lead`]: 20 ms of carrier after).
    fn after_carrier(tx: Tx, ms: usize) -> Scene {
        let lead = vec![Complex::new(1.0, 0.0); RATE as usize * ms / 1000];
        after_lead(&format!("{ms} ms of carrier"), lead, tx)
    }

    /// Ten bursts, each [`BURST_S`] of symbols then [`BURST_GAP_S`] of nothing (132 and
    /// 156 symbol periods at 4800 baud), the noise throughout.
    fn bursts(tx: Tx) -> Scene {
        bursts_of(tx, 70)
    }

    /// [`bursts`], burst `b`'s dibits from seed `payload + b`.
    fn bursts_of(tx: Tx, payload: u64) -> Scene {
        let (n, gap) = (tx.symbols(BURST_S), tx.symbols(BURST_GAP_S));
        let mut iq = Vec::new();
        let mut sent = Vec::new();
        for b in 0..10 {
            let start = iq.len();
            let dibits = dibits(n, payload + b);
            iq.extend(tx.modulate(&dibits));
            iq.extend(std::iter::repeat_n(
                Complex::new(0.0, 0.0),
                (gap as f64 * tx.sps()) as usize,
            ));
            sent.push(Sent {
                start,
                dibits,
                check: 2..n - 2,
            });
        }
        noise_under_unit(&mut iq, tx.snr_db, tx.seed);
        Scene {
            name: "bursts".into(),
            tx,
            iq,
            sent,
        }
    }

    /// 600 dibits, 500 ms of noise alone, 600 more 300 Hz away, then 20 ms of carrier (as in
    /// [`after_lead`]).
    fn second_after_gap(tx: Tx) -> Scene {
        let mut iq = tx.modulate(&dibits(600, 64));
        iq.extend(std::iter::repeat_n(
            Complex::new(0.0, 0.0),
            RATE as usize / 2,
        ));
        let start = iq.len();
        let second = dibits(600, 65);
        let mut tail = tx.modulate(&second);
        carrier_after(&mut tail, RATE as usize / 50);
        for (n, s) in tail.iter_mut().enumerate() {
            let phase = std::f64::consts::TAU * 300.0 * n as f64 / f64::from(RATE);
            *s *= Complex::from_polar(1.0, phase as f32);
        }
        iq.extend(tail);
        noise_under_unit(&mut iq, tx.snr_db, tx.seed);
        Scene {
            name: "a second signal after 500 ms".into(),
            tx,
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
        let sps = scene.tx.sps();
        scene
            .sent
            .iter()
            .enumerate()
            .filter_map(|(n, s)| {
                let mut got = vec![None; s.dibits.len()];
                for d in rx {
                    let k = ((d.at - s.start as f64) / sps - 0.5).round();
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
                        "{} ({:?}): transmission {n}: {} wrong {:?}; right from {first_right}",
                        scene.name,
                        scene.tx,
                        wrong.len(),
                        &wrong[..wrong.len().min(12)]
                    )
                })
            })
            .collect()
    }

    /// `scene` through a receiver for its mode, flushed at its end as a stage does when
    /// its input ends.
    fn decode(scene: &Scene) -> (Fsk4Demod, Vec<SoftDibit>) {
        let mut demod = Fsk4Demod::new(u64::from(RATE), scene.tx.params).expect("rate");
        let mut rx = feed_all(&mut demod, &scene.iq);
        rx.extend(demod.flush());
        (demod, rx)
    }

    fn assert_decoded(scenes: impl IntoIterator<Item = Scene>) {
        for scene in scenes {
            let misses = misses(&scene, &decode(&scene).1);
            assert!(misses.is_empty(), "{misses:#?}");
        }
    }

    #[test]
    fn a_signal_after_noise_is_decoded_from_its_start() {
        assert_decoded([dmr(), c4fm()].map(|p| after_noise(Tx::new(p, 61))));
    }

    #[test]
    fn a_signal_after_carrier_is_decoded_from_its_start() {
        assert_decoded(
            [dmr(), c4fm()]
                .into_iter()
                .flat_map(|p| [200, 1000].map(|ms| after_carrier(Tx::new(p, 62), ms))),
        );
    }

    #[test]
    fn bursts_with_silence_between_are_each_decoded() {
        assert_decoded([dmr(), c4fm()].map(|p| bursts(Tx::new(p, 63))));
    }

    #[test]
    fn a_second_signal_after_a_gap_is_decoded_from_its_start() {
        assert_decoded([dmr(), c4fm()].map(|p| second_after_gap(Tx::new(p, 66))));
    }

    #[test]
    fn at_2400_baud_a_signal_after_noise_and_bursts_are_decoded() {
        assert_decoded([
            after_noise(Tx::new(half(), 61)),
            bursts(Tx::new(half(), 63)),
        ]);
    }

    /// The transmitters off their mode's deviation the tests try: 0.8 and 1.2 of it, at 15
    /// and 10 dB.
    fn off_deviation(p: Fsk4Params, seed: u64) -> impl Iterator<Item = Tx> {
        [0.8, 1.2]
            .into_iter()
            .flat_map(move |dev| [15.0, 10.0].map(|snr| Tx::new(p, seed).off(dev, snr)))
    }

    #[test]
    fn a_transmitter_off_its_deviation_is_decoded_from_its_start() {
        // At 15 dB every dibit after the first 2. At 10 dB the symbols err at their own
        // rate wherever they are (0.8 of the deviation sits near 8 dB on the mode's own:
        // 0.86 % for DMR, 1.33 % for C4FM), so a run without an error is a matter of the
        // seed; `at_10_db_a_start_errs_as_the_steady_state_does` holds those starts to
        // their own rate instead.
        assert_decoded(
            [dmr(), c4fm()]
                .into_iter()
                .flat_map(|p| off_deviation(p, 61))
                .filter(|tx| tx.snr_db >= SIGNAL_SNR_DB)
                .flat_map(|tx| [after_noise(tx), bursts(tx)]),
        );
    }

    /// The dibits a signal's start is judged on after its first [`START_TAIL`]: through
    /// the 50th.
    const START_CHECKED: std::ops::Range<usize> = START_TAIL..50;
    /// The first dibits of a signal after noise that the noise before it reaches through
    /// the receive filter's tail. At 10 dB and 0.8 of the deviation, in 660 starts, dibits
    /// 2 and 3 erred 17 and 13 times for DMR against 5.7 expected per index at the steady
    /// state, while dibits 4 to 49 together erred at the steady state's rate (292 against
    /// 262, +1.8 standard deviations); dibits 0 and 1 the scenes leave out already.
    const START_TAIL: usize = 4;
    /// The most wrong dibits one start may have in [`START_CHECKED`]: more than any of
    /// 4320 windows of that length in the steady state showed (at most 7, at 0.8 of the
    /// deviation and 10 dB), and far fewer than a start lost to a late or wrong lock.
    const START_ERRORS_MAX: usize = 8;

    /// The wrong (or missing) dibits among each of `scene`'s transmissions'
    /// [`START_CHECKED`].
    fn start_errors(scene: &Scene) -> Vec<usize> {
        let (_, rx) = decode(scene);
        let sps = scene.tx.sps();
        scene
            .sent
            .iter()
            .map(|s| {
                let mut got = [None; 50];
                for d in &rx {
                    let k = ((d.at - s.start as f64) / sps - 0.5).round();
                    if (0.0..50.0).contains(&k) {
                        got[k as usize] = Some(d.dibit);
                    }
                }
                START_CHECKED
                    .filter(|&k| got[k] != Some(s.dibits[k]))
                    .count()
            })
            .collect()
    }

    /// The least count a Poisson variable of mean `mean` stays at or under with
    /// probability `p`.
    fn poisson_quantile(mean: f64, p: f64) -> usize {
        let (mut k, mut term) = (0, (-mean).exp());
        let mut sum = term;
        while sum < p {
            k += 1;
            term *= mean / k as f64;
            sum += term;
        }
        k
    }

    #[test]
    fn at_10_db_a_start_errs_as_the_steady_state_does() {
        for tx in [dmr(), c4fm()]
            .into_iter()
            .flat_map(|p| off_deviation(p, 5))
            .filter(|tx| tx.snr_db < SIGNAL_SNR_DB)
        {
            // The steady state: the rate over 200 000 dibits of long signals.
            let (mut wrong, mut sent) = (0, 0);
            for s in 0..10 {
                let dibits = dibits(20_000, 7 + s);
                let mut iq = tx.modulate(&dibits);
                noise_under_unit(&mut iq, tx.snr_db, 8 + 100 * s);
                let mut demod = Fsk4Demod::new(u64::from(RATE), tx.params).expect("rate");
                let pairs = pair(&feed_all(&mut demod, &iq), &dibits, 0, tx.sps());
                wrong += errors(&pairs);
                sent += pairs.len();
            }
            let rate = wrong as f64 / sent as f64;
            // The starts of signals after 1 s of noise and of bursts, 20 seeds each.
            let starts: Vec<usize> = (0..20)
                .flat_map(|seed| {
                    let tx = Tx {
                        seed: 2000 + seed,
                        ..tx
                    };
                    [after_noise(tx), bursts(tx)]
                })
                .flat_map(|scene| start_errors(&scene))
                .collect();
            // No start lost.
            let worst = starts.iter().copied().max().unwrap_or(0);
            assert!(
                worst <= START_ERRORS_MAX,
                "{tx:?}: a start with {worst} wrong"
            );
            // All of them together err at the steady state's rate, within 3 standard
            // deviations: the count the steady state's rate exceeds with the chance of a
            // normal variable passing 3 of them, 0.135 %.
            let checked = (starts.len() * START_CHECKED.len()) as f64;
            let expected = rate * checked;
            let bound = poisson_quantile(expected, 0.99865) as f64;
            let wrong: usize = starts.iter().sum();
            assert!(
                wrong as f64 <= bound,
                "{tx:?}: {wrong} wrong in {} starts, {expected:.1} at the steady state's {:.3} %",
                starts.len(),
                rate * 100.0
            );
        }
    }

    #[test]
    fn a_transmitter_off_its_deviation_holds_its_lock() {
        for tx in [dmr(), c4fm()]
            .into_iter()
            .flat_map(|p| off_deviation(p, 67))
        {
            let mut iq = vec![Complex::new(0.0f32, 0.0); RATE as usize];
            let start = iq.len();
            iq.extend(tx.modulate(&dibits(5000, tx.seed)));
            noise_under_unit(&mut iq, tx.snr_db, tx.seed);
            let mut demod = Fsk4Demod::new(u64::from(RATE), tx.params).expect("rate");
            feed_all(&mut demod, &iq[..start]);
            let before = demod.counters();
            feed_all(&mut demod, &iq[start..]);
            let after = demod.counters();
            // One lock that stands, none lost, and held at the end.
            let kept = |c: Fsk4Counters| c.locks - c.false_locks;
            assert_eq!(
                kept(after) - kept(before),
                1,
                "{tx:?}: {before:?} then {after:?}"
            );
            assert_eq!(
                after.losses, before.losses,
                "{tx:?}: {before:?} then {after:?}"
            );
            assert!(demod.locked && demod.proven >= demod.probation, "{tx:?}");
        }
    }

    #[test]
    fn a_signal_after_5_s_of_noise_is_decoded_from_its_start() {
        assert_decoded(
            [dmr(), c4fm(), half()].into_iter().flat_map(|p| {
                (0..20).map(move |seed| after_noise_for(Tx::new(p, 1000 + seed), 5.0))
            }),
        );
    }

    #[test]
    fn flush_gives_out_a_signal_that_ends_with_the_input() {
        for p in [dmr(), c4fm(), half()] {
            let tx = Tx::new(p, 68);
            // Noise alone: held back until the flush.
            let mut iq = vec![Complex::new(0.0f32, 0.0); RATE as usize / 10];
            noise_under_unit(&mut iq, tx.snr_db, tx.seed);
            let mut demod = Fsk4Demod::new(u64::from(RATE), p).expect("rate");
            let fed = feed_all(&mut demod, &iq).len();
            let flushed = demod.flush();
            assert!(!flushed.is_empty(), "{p:?}: nothing held");
            assert!(
                (fed + flushed.len()) as f64 >= iq.len() as f64 / tx.sps() - 2.0,
                "{p:?}: {fed} fed and {} flushed of {} symbols",
                flushed.len(),
                iq.len() as f64 / tx.sps()
            );
            // A signal ending with the input: all of it but its last 2.
            let dibits = dibits(600, tx.seed);
            let mut iq = tx.modulate(&dibits);
            noise_under_unit(&mut iq, tx.snr_db, tx.seed);
            let mut demod = Fsk4Demod::new(u64::from(RATE), p).expect("rate");
            let mut rx = feed_all(&mut demod, &iq);
            rx.extend(demod.flush());
            let scene = Scene {
                name: "a signal to the end of the input".into(),
                tx,
                iq,
                sent: vec![Sent {
                    start: 0,
                    dibits,
                    check: 0..598,
                }],
            };
            let misses = misses(&scene, &rx);
            assert!(misses.is_empty(), "{misses:#?}");
            assert!(rx.windows(2).all(|w| w[1].at > w[0].at), "{p:?}");
        }
    }

    #[test]
    fn a_flush_is_final() {
        let mut demod = Fsk4Demod::new(u64::from(RATE), dmr()).expect("rate");
        let iq = modulate(&dibits(300, 93), dmr(), RATE);
        assert!(!feed_all(&mut demod, &iq).is_empty());
        assert!(!demod.flush().is_empty());
        assert!(demod.feed(&iq).is_empty());
        assert!(demod.flush().is_empty());
    }

    #[test]
    fn a_transmitter_far_off_its_deviation_is_counted_and_not_acquired() {
        for p in [dmr(), c4fm()] {
            for deviation in [2.5, 0.4] {
                let scene = after_noise(Tx::new(p, 69).off(deviation, SIGNAL_SNR_DB));
                let (demod, _) = decode(&scene);
                // Not acquired: no lock taken stands. (The unlocked tracker may still slice
                // much of it right; that is not acquisition.)
                let c = demod.counters();
                assert_eq!(
                    c.locks, c.false_locks,
                    "{:?} at {deviation}: {c:?}",
                    p.shape
                );
                assert!(
                    demod.counters().off_deviation > 0,
                    "{:?} at {deviation}: {:?}",
                    p.shape,
                    demod.counters()
                );
            }
            // At the declared deviation nothing is off it.
            let scene = after_noise(Tx::new(p, 69));
            let (demod, rx) = decode(&scene);
            assert!(misses(&scene, &rx).is_empty(), "{:?}", p.shape);
            assert_eq!(demod.counters().off_deviation, 0, "{:?}", p.shape);
        }
    }

    #[test]
    fn positions_never_go_backwards() {
        let scenes = [dmr(), c4fm()]
            .into_iter()
            .flat_map(|p| {
                [
                    after_noise(Tx::new(p, 61)),
                    after_carrier(Tx::new(p, 62), 200),
                    after_carrier(Tx::new(p, 62), 1000),
                    bursts(Tx::new(p, 63)),
                    second_after_gap(Tx::new(p, 66)),
                    bursts(Tx::new(p, 63).off(1.2, 10.0)),
                ]
            })
            .chain([
                after_noise(Tx::new(half(), 61)),
                bursts(Tx::new(half(), 63)),
            ]);
        for scene in scenes {
            let (_, rx) = decode(&scene);
            assert!(rx[0].at >= 0.0, "{}: {}", scene.name, rx[0].at);
            for w in rx.windows(2) {
                assert!(
                    w[1].at > w[0].at,
                    "{} ({:?}): {} after {}",
                    scene.name,
                    scene.tx,
                    w[1].at,
                    w[0].at
                );
            }
        }
    }

    #[test]
    fn a_run_of_one_symbol_leaves_the_dibits_after_it() {
        let mut tx = dibits(500, 81);
        tx.extend(std::iter::repeat_n(1u8, 300));
        tx.extend(dibits(500, 82));
        assert_decoded([dmr(), c4fm()].map(|params| {
            let mut iq = modulate(&tx, params, RATE);
            carrier_after(&mut iq, RATE as usize / 50);
            noise_under_unit(&mut iq, SIGNAL_SNR_DB, 83);
            Scene {
                name: "a run of 300".into(),
                tx: Tx::new(params, 83),
                iq,
                sent: vec![Sent {
                    start: 0,
                    dibits: tx.clone(),
                    check: 800..1300,
                }],
            }
        }));
    }

    #[test]
    fn invert_resets_the_timing_detector() {
        let mut demod = Fsk4Demod::new(u64::from(RATE), dmr()).expect("rate");
        demod.feed(&modulate(&dibits(300, 91), dmr(), RATE));
        assert_ne!(demod.last, (0.0, 0.0));
        demod.invert(true);
        assert_eq!(demod.last, (0.0, 0.0));
        // The same polarity again changes nothing.
        demod.feed(&modulate(&dibits(100, 92), dmr(), RATE));
        let last = demod.last;
        assert_ne!(last, (0.0, 0.0));
        demod.invert(true);
        assert_eq!(demod.last, last);
    }

    /// A lone burst of `n` dibits, 200 ms of noise alone before and after.
    fn lone_burst(tx: Tx, n: usize) -> Scene {
        let gap = vec![Complex::new(0.0f32, 0.0); RATE as usize / 5];
        let mut iq = gap.clone();
        let start = iq.len();
        let dibits = dibits(n, tx.seed + 7);
        iq.extend(tx.modulate(&dibits));
        iq.extend(gap);
        noise_under_unit(&mut iq, tx.snr_db, tx.seed);
        Scene {
            name: format!("a lone {n}-dibit burst"),
            tx,
            iq,
            sent: vec![Sent {
                start,
                dibits,
                check: 2..n - 2,
            }],
        }
    }

    /// Each of `scene`'s transmissions' wrong (or missing) dibits over its checked range.
    fn errors_per_sent(scene: &Scene, rx: &[SoftDibit]) -> Vec<usize> {
        let sps = scene.tx.sps();
        scene
            .sent
            .iter()
            .map(|s| {
                let mut got = vec![None; s.dibits.len()];
                for d in rx {
                    let k = ((d.at - s.start as f64) / sps - 0.5).round();
                    if k >= 0.0 && (k as usize) < got.len() {
                        got[k as usize] = Some(d.dibit);
                    }
                }
                s.check
                    .clone()
                    .filter(|&k| got[k] != Some(s.dibits[k]))
                    .count()
            })
            .collect()
    }

    #[test]
    fn noise_alone_proves_no_lock() {
        let mut taken = 0;
        for p in [dmr(), c4fm(), half()] {
            let mut iq = vec![Complex::new(0.0f32, 0.0); RATE as usize * 20];
            noise_under_unit(&mut iq, SIGNAL_SNR_DB, 1201);
            let mut demod = Fsk4Demod::new(u64::from(RATE), p).expect("rate");
            feed_all(&mut demod, &iq);
            let c = demod.counters();
            assert_eq!(c.locks, c.false_locks, "{:?}: {c:?}", p.shape);
            assert_eq!(c.losses, 0, "{:?}: {c:?}", p.shape);
            taken += c.locks;
        }
        // Noise did take locks, so probation was what undid them.
        assert!(taken > 0);
    }

    #[test]
    fn every_burst_of_a_train_is_decoded() {
        let over = |dev: f64, snr: f64| -> Vec<String> {
            (0..50)
                .flat_map(|seed| {
                    let tx = Tx::new(dmr(), 6000 + seed).off(dev, snr);
                    let scene = bursts_of(tx, 10_000 + 16 * seed);
                    let (_, rx) = decode(&scene);
                    errors_per_sent(&scene, &rx)
                        .into_iter()
                        .enumerate()
                        .filter(|(_, e)| *e > START_ERRORS_MAX)
                        .map(move |(b, e)| format!("seed {} burst {b}: {e} wrong", 6000 + seed))
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        for (dev, snr) in [(1.0, 15.0), (1.0, 10.0), (0.8, 15.0)] {
            let over = over(dev, snr);
            assert!(over.is_empty(), "{dev} at {snr} dB: {over:?}");
        }
        // 0.8 of the deviation at 10 dB (about 8 dB on the mode's own): 3 of these 500
        // bursts were measured not acquired well enough, a known limit. Held to that, with
        // one burst of margin for arithmetic that differs between machines.
        let over = over(0.8, 10.0);
        assert!(over.len() <= 3 + 1, "0.8 at 10 dB: {over:?}");
    }

    /// Of 200 lone bursts of `n` dibits from `p` at `snr`, those decoded within
    /// [`START_ERRORS_MAX`] errors over their checked range.
    fn lone_bursts_decoded(p: Fsk4Params, n: usize, deviation: f64, snr: f64) -> usize {
        (0..200)
            .filter(|seed| {
                let scene = lone_burst(Tx::new(p, 5000 + seed).off(deviation, snr), n);
                let (_, rx) = decode(&scene);
                errors_per_sent(&scene, &rx)[0] <= START_ERRORS_MAX
            })
            .count()
    }

    #[test]
    fn a_lone_132_dibit_burst_is_decoded() {
        // A DMR handheld's burst on its own, noise before and after.
        assert_eq!(lone_bursts_decoded(dmr(), 132, 1.0, 15.0), 200);
        // At 10 dB none of the 200 was measured lost; one allowed.
        assert!(lone_bursts_decoded(dmr(), 132, 1.0, 10.0) >= 199);
    }

    #[test]
    fn a_lone_72_dibit_burst_is_decoded() {
        // A P25 terminator's length on its own, noise before and after.
        assert_eq!(lone_bursts_decoded(c4fm(), 72, 1.0, 15.0), 200);
        // At 10 dB none of the 200 was measured lost; one allowed.
        assert!(lone_bursts_decoded(c4fm(), 72, 1.0, 10.0) >= 199);
    }

    /// A lone 132-dibit burst with `share` of its dibits on one level (00, at +1), spread
    /// evenly so every 32-dibit stretch has its share within one, the rest on the other
    /// three levels.
    fn skewed_burst(tx: Tx, share: f64) -> Scene {
        let mut scene = lone_burst(tx, 132);
        let mut other = dibits(132, tx.seed + 9).into_iter();
        let skewed: Vec<u8> = (0..132)
            .map(|i| {
                let on = ((i + 1) as f64 * share).floor() > (i as f64 * share).floor();
                if on {
                    0
                } else {
                    // One of 01, 10 and 11.
                    1 + other.next().expect("dibits") % 3
                }
            })
            .collect();
        let start = scene.sent[0].start;
        let mut iq = vec![Complex::new(0.0f32, 0.0); start];
        iq.extend(tx.modulate(&skewed));
        iq.extend(std::iter::repeat_n(
            Complex::new(0.0, 0.0),
            RATE as usize / 5,
        ));
        noise_under_unit(&mut iq, tx.snr_db, tx.seed);
        scene.iq = iq;
        scene.sent[0].dibits = skewed;
        scene
    }

    #[test]
    fn the_share_one_level_may_take_is_pinned() {
        for p in [dmr(), c4fm()] {
            // 18 of a stretch's 32 on one level, give or take one: under 60 %, locked.
            let scene = skewed_burst(Tx::new(p, 71), 0.5625);
            let (demod, rx) = decode(&scene);
            let c = demod.counters();
            assert!(c.locks > c.false_locks, "{:?} at 56 %: {c:?}", p.shape);
            let misses = misses(&scene, &rx);
            assert!(misses.is_empty(), "{:?} at 56 %: {misses:#?}", p.shape);
            // 80 %: refused. (A fit can split the one level between two of its own and be
            // taken for a moment; no lock stands.)
            let scene = skewed_burst(Tx::new(p, 71), 0.8);
            let (demod, _) = decode(&scene);
            let c = demod.counters();
            assert_eq!(c.locks, c.false_locks, "{:?} at 80 %: {c:?}", p.shape);
        }
    }

    /// A train of DMR bursts at 0.8 of the deviation and 10 dB, decoded: its transitions
    /// and each burst's errors.
    fn weak_train(seed: u64) -> (Paths, Vec<usize>) {
        let scene = bursts(Tx::new(dmr(), seed).off(0.8, 10.0));
        let (demod, rx) = decode(&scene);
        (demod.paths.get(), errors_per_sent(&scene, &rx))
    }

    #[test]
    fn a_strong_lock_with_weak_mean_evidence_is_demoted_not_dropped() {
        // A scene found to take the transition (the seed is the scene's, not tuned).
        let (paths, errors) = weak_train(7000);
        assert!(paths.demoted > 0, "{paths:?}");
        let worst = errors.iter().copied().max().unwrap_or(0);
        assert!(worst <= START_ERRORS_MAX, "{errors:?}");
    }

    #[test]
    fn a_lock_taken_after_a_false_one_slices_its_symbols_again() {
        // A scene found to take a lock in the searches after a false one, and to refuse a
        // weak refit on probation. No lock is taken in the very search that finds one
        // false: a full search needs the last one's candidate to confirm it, and searches on
        // probation make none.
        let (paths, errors) = weak_train(7003);
        assert!(paths.in_grace > 0, "{paths:?}");
        assert!(paths.weak_refit_refused > 0, "{paths:?}");
        assert_eq!(paths.relocked, 0, "{paths:?}");
        let worst = errors.iter().copied().max().unwrap_or(0);
        assert!(worst <= START_ERRORS_MAX, "{errors:?}");
    }

    #[test]
    fn a_proving_fit_is_taken_only_as_a_correction_of_the_levels() {
        let fit = |closure: f64, centre: f64, amplitude: f64| Fit {
            centre,
            amplitude,
            closure,
            share: 0.3,
            contrast: 10.0,
        };
        let held = fit(0.02, 0.0, 1.0);
        // Closer and near the tracked levels: taken.
        assert_eq!(
            proving_refit(&held, &fit(0.01, 0.05, 1.05), 0.0, 1.0),
            (true, false)
        );
        // Closer but on other levels: refused.
        assert_eq!(
            proving_refit(&held, &fit(0.01, 0.15, 1.0), 0.0, 1.0),
            (false, true)
        );
        assert_eq!(
            proving_refit(&held, &fit(0.01, 0.0, 1.15), 0.0, 1.0),
            (false, true)
        );
        // Not closer by enough: neither.
        assert_eq!(
            proving_refit(&held, &fit(0.016, 0.0, 1.0), 0.0, 1.0),
            (false, false)
        );
    }

    /// A receiver fed a lone burst at 15 dB until a lock is on probation, a search past
    /// its taking.
    fn on_probation() -> Fsk4Demod {
        let scene = lone_burst(Tx::new(dmr(), 73), 132);
        let mut demod = Fsk4Demod::new(u64::from(RATE), dmr()).expect("rate");
        for chunk in scene.iq.chunks(24) {
            demod.feed(chunk);
            if demod.locked && demod.proven == 1 {
                return demod;
            }
        }
        panic!("no lock on probation");
    }

    #[test]
    fn settle_refuses_levels_far_from_the_tracked_ones() {
        // Tracked levels 25 % off what the held symbols fit: the fit moves them too far.
        let mut demod = on_probation();
        demod.amplitude *= 1.25;
        let before: Vec<u8> = demod.pending.iter().map(|d| d.dibit).collect();
        demod.settle();
        let after: Vec<u8> = demod.pending.iter().map(|d| d.dibit).collect();
        assert_eq!(demod.paths.get().settle_refused, 1);
        assert_eq!(demod.paths.get().settles, 0);
        assert_eq!(before, after);
        // 5 % off: the fit corrects them, and the held symbols are sliced again on it.
        let mut demod = on_probation();
        demod.amplitude *= 1.05;
        demod.settle();
        assert_eq!(demod.paths.get().settle_refused, 0);
        assert_eq!(demod.paths.get().settles, 1);
    }

    #[test]
    fn lone_bursts_at_0_8_of_the_deviation_and_10_db_keep_their_measured_rate() {
        // The known limit of a weak signal (about 8 dB on the mode's own), measured over
        // these 200 seeds; each held to its figure with one burst of margin for arithmetic
        // that differs between machines.
        for (p, n, lost) in [
            (dmr(), 132, 0),
            (dmr(), 72, 10),
            (c4fm(), 132, 2),
            (c4fm(), 72, 16),
        ] {
            let decoded = lone_bursts_decoded(p, n, 0.8, 10.0);
            assert!(
                200 - decoded <= lost + 1,
                "{:?}, {n} dibits: {} of 200 lost, {lost} measured",
                p.shape,
                200 - decoded
            );
        }
    }

    #[test]
    fn a_proving_fit_that_corrects_the_levels_replaces_settle() {
        // A lock on probation whose tracked amplitude is 5 % off, fed on through the search
        // that proves it: that search's fit corrects the levels and slices the held symbols
        // again, and `settle` is not run on top of it.
        let scene = lone_burst(Tx::new(dmr(), 73), 132);
        let mut demod = on_probation();
        demod.amplitude *= 1.05;
        let mut fed = demod.fed as usize;
        let mut rx = Vec::new();
        for chunk in scene.iq[fed..].chunks(24) {
            rx.extend(demod.feed(chunk));
            fed += chunk.len();
            if demod.proven >= demod.probation {
                break;
            }
        }
        let paths = demod.paths.get();
        assert!(demod.proven >= demod.probation, "{paths:?}");
        assert_eq!(paths.proven_refits, 1, "{paths:?}");
        assert_eq!(paths.settles, 0, "{paths:?}");
        // And the burst comes out right, its held symbols sliced again on the fit.
        rx.extend(feed_all(&mut demod, &scene.iq[fed..]));
        rx.extend(demod.flush());
        let errors = errors_per_sent(&scene, &rx)[0];
        assert!(errors <= START_ERRORS_MAX, "{errors} wrong");
    }

    #[test]
    fn a_refit_on_probation_keeps_the_coast_budget_spent() {
        // A lock on probation that has coasted 5 searches, its tracked amplitude 30 % off,
        // fed to its next search: that search refits it (any phase, strong evidence), and
        // the 5 searches stay spent, as its probation's searches and evidence do.
        let scene = lone_burst(Tx::new(dmr(), 73), 132);
        let mut demod = on_probation();
        demod.amplitude *= 1.3;
        demod.coasted = 5;
        let (proven, searches) = (demod.proven, demod.paths.get().searches);
        let fed = demod.fed as usize;
        for chunk in scene.iq[fed..].chunks(24) {
            demod.feed(chunk);
            if demod.paths.get().searches > searches {
                break;
            }
        }
        let paths = demod.paths.get();
        assert_eq!(paths.probation_refits, 1, "{paths:?}");
        assert_eq!(demod.proven, proven, "{paths:?}");
        assert_eq!(demod.coasted, 5, "{paths:?}");
    }

    #[test]
    fn held_symbols_stay_held_until_the_lock_is_decided() {
        // While a lock is on probation, or in the searches after it was found false, every
        // symbol is held (up to HELD_MAX). Over 50 weak trains, none goes out early except
        // where a second lock was taken in those searches (two on probation in a row, which
        // HELD_MAX's doc allows). With the margin of one search gone (192), 7 did.
        let mut checked = 0;
        for seed in 0..50 {
            let tx = Tx::new(dmr(), 6000 + seed).off(0.8, 10.0);
            let scene = bursts_of(tx, 10_000 + 16 * seed);
            let (demod, _) = decode(&scene);
            let paths = demod.paths.get();
            if paths.in_grace == 0 {
                assert_eq!(paths.out_while_held, 0, "seed {}: {paths:?}", 6000 + seed);
                checked += 1;
            }
        }
        // MEASURED_CHECKED of the 50 have no lock in the grace; one fewer allowed.
        assert!(checked + 1 >= MEASURED_CHECKED, "{checked} scenes checked");
    }

    /// `dibits` from `tx` after 200 ms of noise alone, then 20 ms of carrier (as in
    /// [`after_lead`]); and where the transmission ends.
    fn after_200_ms_of_noise(name: &str, tx: Tx, dibits: Vec<u8>) -> (Scene, usize) {
        let mut iq = vec![Complex::new(0.0f32, 0.0); RATE as usize / 5];
        let start = iq.len();
        iq.extend(tx.modulate(&dibits));
        let end = iq.len();
        carrier_after(&mut iq, RATE as usize / 50);
        noise_under_unit(&mut iq, tx.snr_db, tx.seed);
        let n = dibits.len();
        let scene = Scene {
            name: name.into(),
            tx,
            iq,
            sent: vec![Sent {
                start,
                dibits,
                check: 2..n - 2,
            }],
        };
        (scene, end)
    }

    /// `scene` decoded: the counters when its transmission starts and when it ends, the
    /// transitions taken by its end, and every dibit given out.
    fn counted(scene: &Scene, end: usize) -> (Fsk4Counters, Fsk4Counters, Paths, Vec<SoftDibit>) {
        let start = scene.sent[0].start;
        let mut demod = Fsk4Demod::new(u64::from(RATE), scene.tx.params).expect("rate");
        let mut rx = feed_all(&mut demod, &scene.iq[..start]);
        let before = demod.counters();
        rx.extend(feed_all(&mut demod, &scene.iq[start..end]));
        let (after, paths) = (demod.counters(), demod.paths.get());
        rx.extend(feed_all(&mut demod, &scene.iq[end..]));
        rx.extend(demod.flush());
        (before, after, paths, rx)
    }

    /// One lock that stands from `before` to `after`, none false and none lost. (Signed:
    /// a lock held at `before` that is found false after leaves fewer standing.)
    fn one_lock(before: Fsk4Counters, after: Fsk4Counters) -> bool {
        let kept = |c: Fsk4Counters| c.locks as i64 - c.false_locks as i64;
        kept(after) - kept(before) == 1
            && after.false_locks == before.false_locks
            && after.losses == before.losses
    }

    /// A clear P25 call of 2 superframes to `dst`, a talkgroup or (`private`) a radio.
    fn clear_p25_call(dst: u32, private: bool) -> Vec<u8> {
        use crate::p25::{Call, EncSync, LinkControl, P25_ALGID_CLEAR, encode_call};
        let lc = LinkControl {
            call: if private { Call::Private } else { Call::Group },
            dst,
            src: 9_990_001,
            mfid: 0,
            emergency: false,
        };
        let clear = EncSync {
            algid: P25_ALGID_CLEAR,
            kid: 0,
        };
        encode_call(0x293, lc, clear, 2)
    }

    /// The clear headers the tests run: talkgroup 65000, talkgroup 1 (the usual
    /// conventional default, whose header carries a longer run) and a unit-to-unit call
    /// (talkgroup 0 in the header, the longest).
    fn clear_p25_calls() -> [(&'static str, Vec<u8>); 3] {
        [
            ("talkgroup 65000", clear_p25_call(65_000, false)),
            ("talkgroup 1", clear_p25_call(1, false)),
            ("a unit-to-unit call", clear_p25_call(9_990_002, true)),
        ]
    }

    #[test]
    fn held_symbols_stay_held_through_a_clear_p25_header() {
        // A weak lock on probation coasts through the header's run with every symbol held,
        // its coasts bounded over the whole probation so the hold covers them. Over 30
        // calls per header at 0.8 of the deviation and 10 dB, none gives a symbol out while
        // held, except where a second lock was taken in the grace (as HELD_MAX allows).
        let (mut checked, mut refits) = (0, 0);
        for (name, call) in clear_p25_calls() {
            for seed in 0..30 {
                let tx = Tx::new(c4fm(), 8000 + seed).off(0.8, 10.0);
                let (scene, _) = after_200_ms_of_noise(name, tx, call.clone());
                let (demod, _) = decode(&scene);
                let paths = demod.paths.get();
                refits += paths.probation_refits;
                if paths.in_grace == 0 {
                    assert_eq!(
                        paths.out_while_held,
                        0,
                        "{name}, seed {}: {paths:?}",
                        8000 + seed
                    );
                    checked += 1;
                }
            }
        }
        assert!(
            checked + 1 >= MEASURED_P25_CHECKED,
            "{checked} calls checked"
        );
        assert!(refits > 0, "no refit on probation");
    }

    #[test]
    fn a_clear_p25_call_keeps_one_lock_through_its_header() {
        // Each header's run keeps 20 (talkgroup 65000), 23 (talkgroup 1) or 24 (a
        // unit-to-unit call) searches' stretches on one level in a row.
        let header = 2..P25_HEADER_DIBITS;
        for (name, call) in clear_p25_calls() {
            for (tx, header_errors) in [
                (Tx::new(c4fm(), 8000), 0),
                // At the steady state's rate (1.33 %, 5.2 expected), within 3 standard
                // deviations.
                (
                    Tx::new(c4fm(), 8000).off(0.8, 10.0),
                    poisson_quantile(0.0133 * 392.0, 0.99865),
                ),
            ] {
                let (scene, end) = after_200_ms_of_noise(name, tx, call.clone());
                let (before, after, paths, rx) = counted(&scene, end);
                assert!(
                    one_lock(before, after),
                    "{name}, {tx:?}: {before:?} then {after:?}"
                );
                // The header's run made it coast.
                assert!(paths.coasts > 0, "{name}, {tx:?}");
                let sent = &scene.sent[0];
                let header_scene = Scene {
                    name: "the header".into(),
                    tx,
                    iq: Vec::new(),
                    sent: vec![Sent {
                        start: sent.start,
                        dibits: sent.dibits.clone(),
                        check: header.clone(),
                    }],
                };
                let wrong = errors_per_sent(&header_scene, &rx)[0];
                assert!(
                    wrong <= header_errors,
                    "{name}, {tx:?}: {wrong} of the header's dibits wrong"
                );
            }
        }
        // At 0.8 of the deviation and 10 dB, over 30 calls per header (seeds 8000 on),
        // each held to its measured figure with one call of margin. Failed by the header's
        // run (a coast that ran out, or a lock found false on one level's share, inside the
        // call): 1, 2 and 1, all false on share after weak evidence, none a coast run out
        // (with a coast of 28, one unit-to-unit call ran out). Not keeping exactly one lock
        // in all: 7, 7 and 8, the rest a stretch's eye closing past HOLD_CLOSURE mid-call
        // or a false lock at the start.
        for ((name, call), (by_run, not_one)) in
            clear_p25_calls().into_iter().zip([(1, 7), (2, 7), (1, 8)])
        {
            let (mut run_failed, mut not_kept) = (0, 0);
            for seed in 0..30 {
                let tx = Tx::new(c4fm(), 8000 + seed).off(0.8, 10.0);
                let (scene, end) = after_200_ms_of_noise(name, tx, call.clone());
                let (before, after, paths, _) = counted(&scene, end);
                run_failed += u32::from(paths.coasts_ended > 0 || paths.false_on_share > 0);
                not_kept += u32::from(!one_lock(before, after));
            }
            assert!(
                run_failed <= by_run + 1,
                "{name}: {run_failed} of 30 failed by the run, {by_run} measured"
            );
            assert!(
                not_kept <= not_one + 1,
                "{name}: {not_kept} of 30 not one lock, {not_one} measured"
            );
        }
    }

    #[test]
    fn a_dmr_repeater_call_keeps_its_lock() {
        use crate::dmr::{Call, FullLc, SlotPlan, encode_repeater};
        let lc = FullLc {
            call: Call::Group,
            dst: 9990,
            src: 9_990_001,
            fid: 0,
            encrypted: false,
            emergency: false,
        };
        // At colour code 0, where a voice burst's centre can be 24 dibits of 00.
        let downlink = encode_repeater(
            0,
            [
                SlotPlan {
                    lc: Some(lc),
                    superframes: 6,
                    control: false,
                },
                SlotPlan {
                    lc: None,
                    superframes: 6,
                    control: false,
                },
            ],
        );
        // Over 30 seeds each, none took a false lock or lost one.
        for tx in [Tx::new(dmr(), 8100), Tx::new(dmr(), 8100).off(1.0, 10.0)] {
            let (scene, end) = after_200_ms_of_noise("a repeater's call", tx, downlink.clone());
            let (before, after, paths, _) = counted(&scene, end);
            assert!(one_lock(before, after), "{tx:?}: {before:?} then {after:?}");
            // Its call bursts made it coast.
            assert!(paths.coasts > 0, "{tx:?}");
        }
    }

    /// A proven lock on 600 dibits of `p` at 15 dB, then a carrier at `level` (of the
    /// held levels, outer at ±1) to the end, fed a search at a time: the searches from
    /// the carrier's start to the lock's loss, and from its first coast to the loss.
    fn carrier_after_a_lock(p: Fsk4Params, level: f64) -> (u32, Option<u32>, Paths) {
        let tx = Tx::new(p, 74);
        let mut iq = tx.modulate(&dibits(600, 75));
        noise_under_unit(&mut iq, tx.snr_db, tx.seed);
        let mut demod = Fsk4Demod::new(u64::from(RATE), p).expect("rate");
        feed_all(&mut demod, &iq);
        assert!(demod.locked && demod.proven >= demod.probation, "{p:?}");
        // The held level's frequency: the levels' centre and amplitude are in outer
        // deviations.
        let hz = p.deviation_hz * (demod.centre + demod.sign * level * demod.amplitude);
        let n = RATE as usize;
        let mut carrier: Vec<Complex<f32>> = (0..n)
            .map(|k| {
                let phase = std::f64::consts::TAU * hz * k as f64 / f64::from(RATE);
                Complex::from_polar(1.0, phase as f32)
            })
            .collect();
        noise_under_unit(&mut carrier, tx.snr_db, tx.seed + 1);
        let (losses, searches) = (demod.counters().losses, demod.paths.get().searches);
        let mut first_coast = None;
        let piece = (SEARCH_EVERY as f64 * tx.sps()) as usize;
        for chunk in carrier.chunks(piece) {
            demod.feed(chunk);
            let paths = demod.paths.get();
            if first_coast.is_none() && paths.coasts > 0 {
                first_coast = Some(paths.searches);
            }
            if demod.counters().losses > losses {
                let lost = paths.searches;
                return (lost - searches, first_coast.map(|f| lost - f), paths);
            }
        }
        panic!("{p:?}: the lock on a carrier at {level} was never let go");
    }

    #[test]
    fn a_carrier_on_a_held_level_is_let_go_within_the_coast() {
        for p in [dmr(), c4fm()] {
            // On a held level (00, +1/3): it coasts the whole bound, then is let go.
            let (_, coasted, paths) = carrier_after_a_lock(p, 1.0 / 3.0);
            let coasted = coasted.expect("coasted");
            assert!(
                coasted <= p.coast_searches + 1,
                "{:?}: {coasted} searches, {paths:?}",
                p.shape
            );
            assert_eq!(paths.coasts, p.coast_searches, "{:?}: {paths:?}", p.shape);
            // Between two (+2/3): let go at once, never coasting, in the same search as a
            // receiver that does not coast at all (5, the carrier filling a stretch and
            // one more).
            let (searches, coasted, paths) = carrier_after_a_lock(p, 2.0 / 3.0);
            assert_eq!(coasted, None, "{:?}: {paths:?}", p.shape);
            let (today, ..) = carrier_after_a_lock(
                Fsk4Params {
                    coast_searches: 0,
                    ..p
                },
                2.0 / 3.0,
            );
            assert_eq!(searches, today, "{:?}: {paths:?}", p.shape);
            let fill = (SEARCH_SYMBOLS / SEARCH_EVERY) as u32;
            assert!(searches <= fill + 1, "{:?}: {searches} searches", p.shape);
        }
    }

    #[test]
    fn sync_words_split_into_dibits_first_bit_first() {
        assert_eq!(&sync_dibits(P25_SYNC, 48)[..6], &[1, 1, 1, 1, 1, 3]);
        assert_eq!(sync_dibits(0b10_00_11, 6), vec![2, 0, 3]);
    }
}
