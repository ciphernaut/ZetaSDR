//! IQ-power squelch for the voice demodulators (`nbfm_demod`, `am_demod`).
//!
//! Each block's mean power (dBFS) is compared with a threshold: a fixed one, or
//! (automatic) [`SQUELCH_OPEN_DB`] over the noise floor, the
//! [`SQUELCH_FLOOR_PERCENTILE`]th percentile of the last [`SQUELCH_FLOOR_BLOCKS`]
//! block powers (`measure`'s floor rule), seeded by the first block. It opens on
//! a block over the threshold and closes once the power has stayed
//! [`SQUELCH_HYSTERESIS_DB`] under it for [`SQUELCH_HANG_MS`].
//!
//! Two rules keep the automatic floor a noise floor when a signal outlasts the
//! window or is there from the first block: the floor is never taken above
//! [`SQUELCH_FLOOR_MAX_DB`] (so a strong carrier present from the start opens),
//! and while open it is at most the noise power under the signal, estimated
//! from the second and fourth moments of the last [`SQUELCH_MOMENT_BLOCKS`]
//! blocks (M2M4: for a constant-envelope signal plus Gaussian noise,
//! `S = sqrt(2·M2² − M4)` and `N = M2 − S`), so a transmission longer than the
//! window does not become the floor and close the squelch on itself.

use std::collections::VecDeque;

use num_complex::Complex;
use zeta_core::stage::StageError;
use zeta_core::types::{StreamId, StreamKind};

use crate::measure::percentile;

/// Automatic squelch: opens this far over the tracked noise floor, dB.
pub const SQUELCH_OPEN_DB: f32 = 6.0;
/// Closes this far under the opening threshold, dB.
pub const SQUELCH_HYSTERESIS_DB: f32 = 3.0;
/// ... once the power has stayed there this long, ms.
pub const SQUELCH_HANG_MS: u64 = 300;
/// The noise floor is this percentile of the recent block powers ...
pub const SQUELCH_FLOOR_PERCENTILE: usize = 15;
/// ... over this many blocks (about 1 s at the live 10 ms blocks).
pub const SQUELCH_FLOOR_BLOCKS: usize = 100;
/// The automatic floor is never taken above this, dBFS: a channel whose power
/// is over it by [`SQUELCH_OPEN_DB`] is open even with no quiet block seen.
pub const SQUELCH_FLOOR_MAX_DB: f32 = -40.0;
/// Blocks whose moments give the noise estimate under an open signal.
pub const SQUELCH_MOMENT_BLOCKS: usize = 10;
/// The power reported for an all-zero block, dBFS.
const POWER_MIN_DB: f32 = -200.0;

/// `p` (a mean `|x|²`) in dBFS, at least the floor reported for an all-zero block.
pub(crate) fn db(p: f64) -> f32 {
    if p > 0.0 {
        ((10.0 * p.log10()) as f32).max(POWER_MIN_DB)
    } else {
        POWER_MIN_DB
    }
}

/// Squelch on IQ block power (dBFS, mean `|x|²`).
#[derive(Clone, Debug)]
pub struct Squelch {
    threshold_dbfs: Option<f64>,
    /// Recent block powers, dBFS, oldest first.
    powers: VecDeque<f32>,
    /// Recent blocks' `(Σ|x|², Σ|x|⁴, n)`, oldest first.
    moments: VecDeque<(f64, f64, usize)>,
    open: bool,
    /// Time spent under the closing level while open, ns.
    under_ns: u64,
    power_db: f32,
    floor_db: f32,
    started: bool,
}

impl Squelch {
    /// `threshold_dbfs`: a fixed opening threshold, or `None` for automatic.
    pub fn new(threshold_dbfs: Option<f64>) -> Self {
        Self {
            threshold_dbfs,
            powers: VecDeque::with_capacity(SQUELCH_FLOOR_BLOCKS),
            moments: VecDeque::with_capacity(SQUELCH_MOMENT_BLOCKS),
            open: false,
            under_ns: 0,
            power_db: POWER_MIN_DB,
            floor_db: POWER_MIN_DB,
            started: false,
        }
    }

    /// The noise power under an open signal (M2M4 over the recent blocks), dBFS.
    fn noise_under_db(&self) -> f32 {
        let (m2, m4, n) = self
            .moments
            .iter()
            .fold((0.0, 0.0, 0), |(a, b, c), &(x, y, z)| (a + x, b + y, c + z));
        if n == 0 {
            return POWER_MIN_DB;
        }
        let (m2, m4) = (m2 / n as f64, m4 / n as f64);
        let s = (2.0 * m2 * m2 - m4).max(0.0).sqrt();
        db((m2 - s).max(0.0))
    }

    /// Feed one block at `rate_hz`; `Some(open)` on the first block and on each
    /// transition, `None` otherwise. An empty block changes nothing.
    pub fn feed(&mut self, iq: &[Complex<f32>], rate_hz: u64) -> Option<bool> {
        if iq.is_empty() {
            return None;
        }
        let (m2, m4) = iq.iter().fold((0.0f64, 0.0f64), |(a, b), x| {
            let p = x.norm_sqr() as f64;
            if p.is_finite() {
                (a + p, b + p * p)
            } else {
                (a, b)
            }
        });
        let p = db(m2 / iq.len() as f64);
        self.power_db = p;
        if self.powers.len() == SQUELCH_FLOOR_BLOCKS {
            self.powers.pop_front();
        }
        self.powers.push_back(p);
        if self.moments.len() == SQUELCH_MOMENT_BLOCKS {
            self.moments.pop_front();
        }
        self.moments.push_back((m2, m4, iq.len()));
        let tracked = percentile(self.powers.make_contiguous(), SQUELCH_FLOOR_PERCENTILE);
        let threshold = match self.threshold_dbfs {
            Some(t) => {
                self.floor_db = tracked;
                t as f32
            }
            None => {
                let mut floor = tracked.min(SQUELCH_FLOOR_MAX_DB);
                if self.open {
                    floor = floor.min(self.noise_under_db());
                }
                self.floor_db = floor;
                floor + SQUELCH_OPEN_DB
            }
        };
        let was = self.open;
        if p > threshold {
            self.open = true;
            self.under_ns = 0;
        } else if self.open && p < threshold - SQUELCH_HYSTERESIS_DB {
            let dur = if rate_hz == 0 {
                0
            } else {
                (iq.len() as u128 * 1_000_000_000 / rate_hz as u128) as u64
            };
            self.under_ns = self.under_ns.saturating_add(dur);
            if self.under_ns >= SQUELCH_HANG_MS * 1_000_000 {
                self.open = false;
                self.under_ns = 0;
            }
        } else {
            self.under_ns = 0;
        }
        let first = !std::mem::replace(&mut self.started, true);
        (first || was != self.open).then_some(self.open)
    }

    /// Whether the squelch is open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The last block's power, dBFS.
    pub fn power_db(&self) -> f32 {
        self.power_db
    }

    /// The noise floor the last decision used, dBFS (fixed threshold: the tracked
    /// floor, for reporting only).
    pub fn floor_db(&self) -> f32 {
        self.floor_db
    }
}

/// Lowest and highest accepted fixed `squelch_db`, dBFS.
pub const SQUELCH_DB_RANGE: (i64, i64) = (-120, 0);

/// `#[serde(default)]` for a `squelch` param: on unless turned off.
pub(crate) fn default_true() -> bool {
    true
}

/// The squelch a `kind` stage runs with its `squelch` and `squelch_db` params:
/// `None` when turned off; a `squelch_db` outside [`SQUELCH_DB_RANGE`] is refused
/// whether or not it is on.
pub(crate) fn stage_squelch(
    kind: &str,
    on: bool,
    squelch_db: Option<i64>,
) -> Result<Option<Squelch>, String> {
    let (lo, hi) = SQUELCH_DB_RANGE;
    if let Some(v) = squelch_db.filter(|v| !(lo..=hi).contains(v)) {
        return Err(format!(
            "{kind} params: squelch_db {v} out of range {lo}..={hi}"
        ));
    }
    Ok(on.then(|| Squelch::new(squelch_db.map(|v| v as f64))))
}

/// A squelched demodulator's outputs for `params`: audio, then the `squelch` events
/// unless `squelch` is `false`.
pub(crate) fn squelch_outputs(params: &serde_json::Value) -> Vec<StreamKind> {
    if params.get("squelch") == Some(&serde_json::Value::Bool(false)) {
        vec![StreamKind::Audio]
    } else {
        vec![StreamKind::Audio, StreamKind::Event]
    }
}

/// The audio and (when `squelched`) event streams among a demodulator's `outputs`.
pub(crate) fn squelch_streams(
    outputs: &[StreamId],
    squelched: bool,
) -> Result<(StreamId, Option<StreamId>), StageError> {
    match (outputs, squelched) {
        ([out], false) => Ok((out.clone(), None)),
        ([out, events], _) => Ok((out.clone(), Some(events.clone()))),
        _ => Err(StageError::Failed(format!(
            "expected {} outputs, got {}",
            if squelched { 2 } else { 1 },
            outputs.len()
        ))),
    }
}

/// The `squelch` event for a transition of the gate to `open` (the squelch `sq` open and
/// not `guarded`): `adjacent_db` the stronger neighbour's power under an adjacent-channel
/// guard (`None` without one); powers to 0.1 dB.
pub(crate) fn squelch_event(
    sq: &Squelch,
    open: bool,
    adjacent_db: Option<f32>,
    guarded: bool,
) -> serde_json::Value {
    let tenth = |v: f32| (v as f64 * 10.0).round() / 10.0;
    serde_json::json!({
        "protocol": "squelch",
        "open": open,
        "power_db": tenth(sq.power_db()),
        "floor_db": tenth(sq.floor_db()),
        "adjacent_db": adjacent_db.map(tenth),
        "guarded": guarded,
    })
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::fixtures::awgn;

    /// 20 ms blocks at 25 kHz.
    const RATE: u64 = 25_000;
    const BLOCK: usize = 500;

    /// One block of complex Gaussian noise at `db` dBFS.
    fn noise(db: f32, seed: u64) -> Vec<Complex<f32>> {
        let mut x = vec![Complex::new(0.0f32, 0.0); BLOCK];
        awgn(&mut x, -db, seed);
        x
    }

    /// One block of a complex tone at `db` dBFS (constant envelope).
    fn carrier(db: f32) -> Vec<Complex<f32>> {
        let a = 10f32.powf(db / 20.0);
        (0..BLOCK)
            .map(|n| {
                let ph = 2.0 * std::f32::consts::PI * 1_000.0 * n as f32 / RATE as f32;
                Complex::new(a * ph.cos(), a * ph.sin())
            })
            .collect()
    }

    #[test]
    fn opens_six_db_over_the_floor_and_hangs_300_ms() {
        let mut sq = Squelch::new(None);
        let mut seed = 1;
        let feed = |sq: &mut Squelch, x: Vec<Complex<f32>>| sq.feed(&x, RATE);
        assert_eq!(feed(&mut sq, noise(-60.0, seed)), Some(false));
        for _ in 1..100 {
            seed += 1;
            assert_eq!(feed(&mut sq, noise(-60.0, seed)), None);
        }
        assert!(
            (sq.floor_db() + 60.0).abs() < 1.0,
            "floor {}",
            sq.floor_db()
        );
        assert!(!sq.is_open());
        assert_eq!(feed(&mut sq, noise(-50.0, 999)), Some(true));
        assert!(sq.is_open());
        assert!(
            (sq.power_db() + 50.0).abs() < 1.0,
            "power {}",
            sq.power_db()
        );
        // 14 noise blocks (280 ms) under the threshold keep it open; the 15th (300 ms) closes.
        for _ in 0..14 {
            seed += 1;
            assert_eq!(feed(&mut sq, noise(-60.0, seed)), None);
            assert!(sq.is_open());
        }
        seed += 1;
        assert_eq!(feed(&mut sq, noise(-60.0, seed)), Some(false));
        assert!(!sq.is_open());
    }

    #[test]
    fn a_block_back_over_the_threshold_restarts_the_hang() {
        let mut sq = Squelch::new(None);
        for s in 0..100 {
            sq.feed(&noise(-60.0, s), RATE);
        }
        assert_eq!(sq.feed(&noise(-50.0, 500), RATE), Some(true));
        for s in 0..10 {
            assert_eq!(sq.feed(&noise(-60.0, 600 + s), RATE), None);
        }
        assert_eq!(sq.feed(&noise(-50.0, 700), RATE), None);
        for s in 0..14 {
            assert_eq!(sq.feed(&noise(-60.0, 800 + s), RATE), None);
        }
        assert_eq!(sq.feed(&noise(-60.0, 900), RATE), Some(false));
    }

    #[test]
    fn fixed_threshold_ignores_the_floor() {
        let mut sq = Squelch::new(Some(-40.0));
        assert_eq!(sq.feed(&noise(-50.0, 1), RATE), Some(false));
        for s in 0..200 {
            assert_eq!(sq.feed(&noise(-50.0, 10 + s), RATE), None);
        }
        assert_eq!(sq.feed(&noise(-30.0, 2), RATE), Some(true));
        // Fixed: closes 3 dB under -40 after the hang, whatever the floor.
        for s in 0..14 {
            assert_eq!(sq.feed(&noise(-45.0, 300 + s), RATE), None);
        }
        assert_eq!(sq.feed(&noise(-45.0, 400), RATE), Some(false));
        // Between -43 and -40 dBFS a closed squelch stays closed.
        assert_eq!(sq.feed(&noise(-41.5, 401), RATE), None);
        assert!(!sq.is_open());
    }

    #[test]
    fn first_block_reports_the_state() {
        let mut sq = Squelch::new(None);
        assert_eq!(sq.feed(&noise(-60.0, 1), RATE), Some(false));
        assert_eq!(sq.feed(&noise(-60.0, 2), RATE), None);
        let mut sq = Squelch::new(Some(-40.0));
        assert_eq!(sq.feed(&noise(-30.0, 1), RATE), Some(true));
        assert!(sq.is_open());
    }

    #[test]
    fn a_strong_carrier_from_the_first_block_opens() {
        // No quiet block to learn the floor from: the automatic floor is never taken
        // above SQUELCH_FLOOR_MAX_DB, so a carrier over it opens at once ...
        let mut sq = Squelch::new(None);
        assert_eq!(sq.feed(&carrier(-6.0), RATE), Some(true));
        // ... and a long one stays open: while open the floor is the noise under the
        // carrier (none here), not the carrier itself.
        for _ in 0..500 {
            assert_eq!(sq.feed(&carrier(-6.0), RATE), None);
        }
        assert!(sq.is_open());
    }

    #[test]
    fn a_long_transmission_over_noise_stays_open_and_closes_after() {
        let mut sq = Squelch::new(None);
        for s in 0..100 {
            sq.feed(&noise(-70.0, s), RATE);
        }
        // A carrier 20 dB over the noise for 5 s: well past the floor's 2 s window.
        let mut first = true;
        for s in 0..250 {
            let mut x = carrier(-50.0);
            awgn(&mut x, 20.0, 1_000 + s);
            let r = sq.feed(&x, RATE);
            assert_eq!(r, first.then_some(true), "block {s}");
            first = false;
        }
        assert!(sq.is_open());
        let mut closed_at = None;
        for s in 0..40 {
            if sq.feed(&noise(-70.0, 5_000 + s), RATE) == Some(false) {
                closed_at = Some(s);
                break;
            }
        }
        assert_eq!(closed_at, Some(14), "closes after the 300 ms hang");
    }

    #[test]
    fn steady_noise_never_opens() {
        let mut sq = Squelch::new(None);
        assert_eq!(sq.feed(&noise(-60.0, 0), RATE), Some(false));
        for s in 1..3_000 {
            assert_eq!(sq.feed(&noise(-60.0, s), RATE), None, "block {s}");
        }
    }
}
