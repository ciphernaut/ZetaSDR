//! Spectrum rows (spectrum event v2): quantising a row of dB values to a byte a
//! bin above its floor, the inverse, and the mean of a scan step's rows across
//! sweeps ([`SweepAverage`]). The `fft` stage quantises its rows with these and
//! the node averages a scan's rows with them, so every row on the wire is
//! quantised the same way.

use std::collections::{HashMap, VecDeque};
use std::sync::OnceLock;

/// Quantisation step of a row's `q` values.
pub const STEP_DB: f32 = 0.5;
/// A row's `floor_db` is this percentile of its dB values ...
const FLOOR_PERCENTILE: f32 = 0.02;
/// ... minus this margin.
const FLOOR_MARGIN_DB: f32 = 5.0;

/// `(floor_db, q)` for a row of dB values: `floor_db` is the row's 2nd-percentile
/// value minus 5 dB and `q = clamp(round((db - floor_db) / STEP_DB), 0, 255)`, so
/// `floor_db + q * STEP_DB` is within half a step of `db` over 127.5 dB above the floor.
pub fn quantise_row(db: &[f32]) -> (f32, Vec<u8>) {
    quantise_row_masked(db, &[])
}

/// [`quantise_row`] with `floor_db` taken from the bins outside `masked` (sorted,
/// non-overlapping `[lo, hi)` ranges, as the `fft` stage's `masked` gives them), or
/// from every bin when `masked` covers them all; `q` is computed for every bin,
/// masked or not.
pub fn quantise_row_masked(db: &[f32], masked: &[[usize; 2]]) -> (f32, Vec<u8>) {
    if db.is_empty() {
        return (0.0, Vec::new());
    }
    let in_mask = |k: usize| masked.iter().any(|&[lo, hi]| (lo..hi).contains(&k));
    let mut sorted: Vec<f32> = db
        .iter()
        .enumerate()
        .filter(|&(k, _)| !in_mask(k))
        .map(|(_, &v)| v)
        .collect();
    if sorted.is_empty() {
        sorted = db.to_vec();
    }
    sorted.sort_by(f32::total_cmp);
    let idx = (FLOOR_PERCENTILE * (sorted.len() - 1) as f32) as usize;
    let floor = sorted[idx] - FLOOR_MARGIN_DB;
    let q = db
        .iter()
        .map(|&v| ((v - floor) / STEP_DB).round().clamp(0.0, 255.0) as u8)
        .collect();
    (floor, q)
}

/// A row's dB values from its `floor_db`, `step_db` and `q`: `floor_db + q × step_db`.
pub fn dequantise(floor_db: f32, step_db: f32, q: &[u8]) -> Vec<f32> {
    q.iter()
        .map(|&v| floor_db + f32::from(v) * step_db)
        .collect()
}

/// The linear power of `q × STEP_DB` dB, for each byte.
fn q_power() -> &'static [f64; 256] {
    static TABLE: OnceLock<[f64; 256]> = OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|q| 10f64.powf(q as f64 * f64::from(STEP_DB) / 10.0)))
}

/// The last rows pushed for one key, quantised: `(floor_db, q)`, all of one length.
#[derive(Default)]
struct History {
    rows: VecDeque<(f32, Vec<u8>)>,
}

/// The mean of each key's last `n` rows, in linear power: a scan step's row
/// averaged across sweeps. A key is `(center_hz, row within the step)`; the
/// caller keeps one `SweepAverage` per grid (an even step averages with the
/// even sweeps of its centre) and starts a new one when the grid changes. Rows
/// are kept quantised, a byte a bin, at [`STEP_DB`].
pub struct SweepAverage {
    n: usize,
    keys: HashMap<(u64, u32), History>,
}

impl SweepAverage {
    /// Average over the last `n` rows of each key (at least 1).
    pub fn new(n: u32) -> Self {
        Self {
            n: n.max(1) as usize,
            keys: HashMap::new(),
        }
    }

    /// Push a row of `floor_db` and `q` (at [`STEP_DB`]) for `key`; the mean
    /// linear power, in dB, of the last `n` rows pushed for it, and how many
    /// that was. A row of another length than the key's last restarts the key.
    pub fn push(&mut self, key: (u64, u32), floor_db: f32, q: &[u8]) -> (Vec<f32>, u32) {
        let h = self.keys.entry(key).or_default();
        if h.rows.front().is_some_and(|(_, r)| r.len() != q.len()) {
            h.rows.clear();
        }
        if h.rows.len() == self.n {
            h.rows.pop_front();
        }
        h.rows.push_back((floor_db, q.to_vec()));
        let table = q_power();
        let mut sum = vec![0f64; q.len()];
        for (floor, row) in &h.rows {
            let scale = 10f64.powf(f64::from(*floor) / 10.0);
            for (s, &v) in sum.iter_mut().zip(row) {
                *s += scale * table[v as usize];
            }
        }
        let count = h.rows.len();
        let db = sum
            .into_iter()
            .map(|s| (10.0 * (s / count as f64).log10()) as f32)
            .collect();
        (db, count as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed LCG's next value in `(0, 1]`.
    fn uniform(state: &mut u64) -> f64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((*state >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }

    /// A row of `n` bins of exponentially distributed power (noise), in dB.
    fn noise_row(state: &mut u64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|_| (10.0 * (-uniform(state).ln()).log10()) as f32)
            .collect()
    }

    fn std_dev(v: &[f32]) -> f64 {
        let n = v.len() as f64;
        let mean = v.iter().map(|&x| f64::from(x)).sum::<f64>() / n;
        (v.iter()
            .map(|&x| (f64::from(x) - mean).powi(2))
            .sum::<f64>()
            / n)
            .sqrt()
    }

    #[test]
    fn sixteen_sweeps_lower_the_ripple_and_sweeps_counts_up() {
        let mut state = 7;
        let mut avg = SweepAverage::new(16);
        let mut first = None;
        let mut last = Vec::new();
        for k in 1..=20u32 {
            let (floor, q) = quantise_row(&noise_row(&mut state, 4096));
            let (db, sweeps) = avg.push((100, 0), floor, &q);
            assert_eq!(sweeps, k.min(16), "row {k}");
            if k == 1 {
                first = Some(std_dev(&dequantise(floor, STEP_DB, &q)));
            }
            if k == 16 {
                last = db;
            }
        }
        let (one, mean) = (first.unwrap(), std_dev(&last));
        assert!(
            mean < one / 2.0,
            "ripple {mean:.2} dB averaged, {one:.2} dB one row"
        );
    }

    #[test]
    fn a_key_keeps_only_its_last_n_rows() {
        let mut avg = SweepAverage::new(2);
        let row = |db: f32| quantise_row(&[db; 8]);
        let mut got = (Vec::new(), 0);
        for db in [0.0, 0.0, -20.0, -20.0] {
            let (floor, q) = row(db);
            got = avg.push((1, 0), floor, &q);
        }
        assert_eq!(got.1, 2);
        for v in got.0 {
            assert!((v + 20.0).abs() < 0.3, "{v}");
        }
    }

    #[test]
    fn keys_do_not_mix() {
        let mut avg = SweepAverage::new(4);
        let (fa, qa) = quantise_row(&[0.0; 8]);
        let (fb, qb) = quantise_row(&[-30.0; 8]);
        for _ in 0..3 {
            avg.push((1, 0), fa, &qa);
        }
        // Another centre, and another row of the same centre, start their own.
        let (db, n) = avg.push((2, 0), fb, &qb);
        assert_eq!(n, 1);
        assert!(db.iter().all(|v| (v + 30.0).abs() < 0.3), "{db:?}");
        let (db, n) = avg.push((1, 1), fb, &qb);
        assert_eq!(n, 1);
        assert!(db.iter().all(|v| (v + 30.0).abs() < 0.3), "{db:?}");
        let (db, n) = avg.push((1, 0), fa, &qa);
        assert_eq!(n, 4);
        assert!(db.iter().all(|v| v.abs() < 0.3), "{db:?}");
    }

    #[test]
    fn a_row_of_another_length_restarts_its_key() {
        let mut avg = SweepAverage::new(8);
        let (f, q) = quantise_row(&[0.0; 8]);
        avg.push((1, 0), f, &q);
        avg.push((1, 0), f, &q);
        let (f, q) = quantise_row(&[-10.0; 16]);
        let (db, n) = avg.push((1, 0), f, &q);
        assert_eq!((db.len(), n), (16, 1));
        assert!(db.iter().all(|v| (v + 10.0).abs() < 0.3), "{db:?}");
    }

    #[test]
    fn dequantise_inverts_quantise_within_half_a_step() {
        let mut state = 3;
        // Keep the row within 127.5 dB of its floor, where `q` does not clamp.
        let db: Vec<f32> = noise_row(&mut state, 1024)
            .into_iter()
            .map(|v| v.max(-20.0))
            .collect();
        let (floor, q) = quantise_row(&db);
        let back = dequantise(floor, STEP_DB, &q);
        for (a, b) in db.iter().zip(&back) {
            assert!(
                (a - b).abs() <= STEP_DB / 2.0 + 1e-4,
                "{a} came back as {b}"
            );
        }
    }
}
