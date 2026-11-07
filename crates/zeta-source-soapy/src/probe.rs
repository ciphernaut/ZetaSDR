//! The rate probe: the fastest rate a device streams cleanly, measured at open.
//!
//! A device asserts the top of its rate range (a B210 61.44 MHz), but the host
//! may not keep up with it: blocks overflow, or fewer samples arrive than the
//! rate says. The worker therefore streams each candidate rate for
//! [`PROBE_DWELL`], from the top down ([`probe_candidates`]), and reports the
//! first that delivers at least [`PROBE_MIN_DELIVERED`] of its rate with no
//! overflow as the source's `max_bandwidth_hz`, `Measured`.

use std::time::Duration;

use zeta_core::port::{Port, Tuning};
use zeta_core::types::Hz;

use crate::SoapySource;
use crate::caps::{RateModel, pick_rate};

/// How long each candidate rate streams.
pub const PROBE_DWELL: Duration = Duration::from_millis(1500);
/// The fraction of a candidate's rate that must arrive for it to pass.
pub const PROBE_MIN_DELIVERED: f64 = 0.98;
/// The most ladder rungs tried, from the top.
pub const PROBE_MAX_RUNGS: usize = 6;

/// The rates to probe, fastest first: a ladder's rungs from the top down, at
/// most [`PROBE_MAX_RUNGS`]; for reported ranges the top rate, its half and its
/// quarter (each the rate the ranges give for it).
pub fn probe_candidates(model: &RateModel) -> Vec<u64> {
    match model {
        RateModel::Ladder { rungs, .. } => {
            rungs.iter().rev().take(PROBE_MAX_RUNGS).copied().collect()
        }
        RateModel::Ranges(ranges) => {
            let max = ranges.iter().map(|r| r.max_hz).fold(0.0, f64::max).round() as u64;
            let mut out: Vec<u64> = [max, max / 2, max / 4]
                .into_iter()
                .filter(|&r| r > 0)
                .map(|r| pick_rate(model, r).unwrap_or(r))
                .collect();
            out.dedup();
            out
        }
    }
}

/// Stream each of [`probe_candidates`] at `center_hz` for [`PROBE_DWELL`] and
/// return the first rate that delivered at least [`PROBE_MIN_DELIVERED`] of
/// itself with no new overflow. None passing is the last rate that streamed
/// (the slowest); a candidate whose tune fails is skipped, and every tune
/// failing is an error. The stream is then closed and the source left as it
/// opened: nothing tuned, the probe's overflows not counted.
pub async fn probe_rate_cap(src: &SoapySource, center_hz: u64) -> Result<u64, String> {
    let cap = probe(src, center_hz).await;
    src.end_probe().await;
    cap
}

async fn probe(src: &SoapySource, center_hz: u64) -> Result<u64, String> {
    // The reader thread ends when nothing receives; hold a receiver throughout.
    let _rx = src.subscribe();
    let mut last = None;
    let mut errors = Vec::new();
    for rate in probe_candidates(src.rate_model()) {
        let band = Tuning {
            center_hz: Hz(center_hz),
            bandwidth_hz: Hz(rate),
        };
        if let Err(e) = src.tune(band).await {
            errors.push(format!("{rate} Hz: {e}"));
            continue;
        }
        last = Some(rate);
        let before = src.overflows();
        tokio::time::sleep(PROBE_DWELL).await;
        let overflows = src.overflows() - before;
        let measured = src.measured_rate_hz().unwrap_or(0.0);
        if overflows == 0 && measured >= PROBE_MIN_DELIVERED * rate as f64 {
            return Ok(rate);
        }
    }
    last.ok_or_else(|| {
        format!(
            "rate probe: no candidate rate tuned ({})",
            errors.join("; ")
        )
    })
}
