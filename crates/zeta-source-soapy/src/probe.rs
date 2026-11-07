//! The rate probe: the fastest rate a device streams cleanly, measured at open.
//!
//! A device asserts the top of its rate range (a B210 61.44 MHz), but the host
//! may not keep up with it: blocks overflow, or fewer samples arrive than the
//! rate says. The worker therefore streams each candidate rate, from the top
//! down ([`probe_candidates`]): after each retune it lets the stream settle for
//! [`PROBE_SETTLE`], then dwells [`PROBE_DWELL`], and the first rate that
//! delivers at least [`PROBE_MIN_DELIVERED`] of itself with at most
//! [`PROBE_MAX_OVERFLOWS`] overflow in the dwell is clean. Only a clean rate is
//! reported as the source's `max_bandwidth_hz`, `Measured`: the worker keeps
//! the driver's asserted cap when none is (see [`ProbeResult::clean`]).

use std::time::Duration;

use zeta_core::port::{Port, Tuning};
use zeta_core::types::Hz;

use crate::SoapySource;
use crate::caps::{RateModel, pick_rate};

/// How long the stream runs after each retune before the probe counts
/// anything: a retune restarts the stream, and a driver may drop a buffer or
/// two while it does, which says nothing about the rate the host sustains.
pub const PROBE_SETTLE: Duration = Duration::from_millis(200);
/// How long each candidate rate streams, after [`PROBE_SETTLE`], while its
/// overflows and delivered samples are counted.
pub const PROBE_DWELL: Duration = Duration::from_millis(1500);
/// The most overflows in a dwell a rate may have and still pass. Not zero: the
/// field host's B210 overflowed now and then at every rate in the plan's sweep
/// (11 a minute at 30.72 Msps, which it otherwise holds, and more at slower
/// rates), so a single sporadic overflow must not fail a rate the host keeps
/// up with, while a host that cannot keep up overflows many times a second.
pub const PROBE_MAX_OVERFLOWS: u64 = 1;
/// The fraction of a candidate's rate that must arrive for it to pass: a host
/// that falls behind delivers short without always overflowing.
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

/// What a rate probe found: the cap, how many candidate rates streamed (their
/// tune succeeded), and whether the cap streamed cleanly (`false`: none did,
/// and the cap is the slowest rate that streamed). A result that is not clean
/// is no measurement: the worker keeps the driver's asserted cap and says to
/// pin one with `max_rate_hz=`, so it is never cached and replayed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeResult {
    pub cap_hz: u64,
    pub tried: usize,
    pub clean: bool,
}

/// [`probe_rate`]'s cap.
pub async fn probe_rate_cap(src: &SoapySource, center_hz: u64) -> Result<u64, String> {
    probe_rate(src, center_hz).await.map(|p| p.cap_hz)
}

/// Stream each of [`probe_candidates`] at `center_hz`, [`PROBE_SETTLE`] then
/// [`PROBE_DWELL`], and take the first rate that delivered at least
/// [`PROBE_MIN_DELIVERED`] of itself with at most [`PROBE_MAX_OVERFLOWS`] new
/// overflow in the dwell. None passing is the slowest rate that
/// streamed, not clean; a candidate whose tune fails is skipped. Every tune
/// failing is an error, and so is a device the source gives up as lost during
/// the probe: no cap comes out of a rate that killed the device. The stream is
/// then closed and the source left as it opened: nothing tuned, the probe's
/// overflows not counted (a lost device's fault stays).
pub async fn probe_rate(src: &SoapySource, center_hz: u64) -> Result<ProbeResult, String> {
    let found = probe(src, center_hz).await;
    src.end_probe().await;
    found
}

async fn probe(src: &SoapySource, center_hz: u64) -> Result<ProbeResult, String> {
    // The reader thread ends when nothing receives; hold a receiver throughout.
    let _rx = src.subscribe();
    let mut lowest: Option<u64> = None;
    let mut tried = 0;
    let mut errors = Vec::new();
    for rate in probe_candidates(src.rate_model()) {
        let band = Tuning {
            center_hz: Hz(center_hz),
            bandwidth_hz: Hz(rate),
        };
        let tuned = src.tune(band).await;
        if let Some(e) = lost(src, rate) {
            return Err(e);
        }
        if let Err(e) = tuned {
            errors.push(format!("{rate} Hz: {e}"));
            continue;
        }
        tried += 1;
        lowest = Some(lowest.map_or(rate, |l| l.min(rate)));
        tokio::time::sleep(PROBE_SETTLE).await;
        if let Some(e) = lost(src, rate) {
            return Err(e);
        }
        let before = src.overflows();
        tokio::time::sleep(PROBE_DWELL).await;
        if let Some(e) = lost(src, rate) {
            return Err(e);
        }
        let overflows = src.overflows() - before;
        let measured = src.measured_rate_hz().unwrap_or(0.0);
        if overflows <= PROBE_MAX_OVERFLOWS && measured >= PROBE_MIN_DELIVERED * rate as f64 {
            return Ok(ProbeResult {
                cap_hz: rate,
                tried,
                clean: true,
            });
        }
    }
    let cap_hz = lowest.ok_or_else(|| {
        format!(
            "rate probe: no candidate rate tuned ({})",
            errors.join("; ")
        )
    })?;
    Ok(ProbeResult {
        cap_hz,
        tried,
        clean: false,
    })
}

/// The probe's error when the source gave its device up as lost at `rate`.
fn lost(src: &SoapySource, rate: u64) -> Option<String> {
    src.is_lost().then(|| {
        let fault = src.fault().unwrap_or_default();
        format!("rate probe: device lost at {rate} Hz: {fault}")
    })
}
