//! The rate probe: the fastest rate a device streams cleanly, measured at open.
//!
//! A device asserts the top of its rate range (a B210 61.44 MHz), but the host
//! may not keep up with it: blocks overflow, or fewer samples arrive than the
//! rate says. The worker therefore streams each candidate rate, from the top
//! down ([`probe_candidates`]): after each retune it lets the stream settle for
//! [`PROBE_SETTLE`], then dwells [`PROBE_DWELL`], and the first rate that
//! delivers at least [`PROBE_MIN_DELIVERED`] of itself with at most
//! [`PROBE_MAX_OVERFLOWS`] overflow in the dwell is clean; a rung rejected for
//! overflows is tried [`PROBE_RETRIES`] more time: the retry re-issues the tune, which is a no-op on the running
//! stream, so it is a second settle and a fresh dwell, and every attempt's
//! delivered rate is judged on the samples of its own dwell. Only a clean rate is
//! reported as the source's `max_bandwidth_hz`, `Measured`: the worker keeps
//! the driver's asserted cap when none is (see [`ProbeResult::clean`]).
//!
//! A device whose link carries more than one sample depth has the ladder run
//! at each ([`probe_depths`]), and the source runs the narrowest lossless one
//! that measures as wide as the widest ([`own_depth`]); the record's `max_bandwidth_hz` is that depth's figure.
//! That probe says what it is doing before each rung (a [`Starting`]): the
//! host is busy while the radio streams flat out, for up to a minute or more.

use std::time::Duration;

use zeta_core::port::{Port, Starting, Tuning};
use zeta_core::types::{Claim, Depth, Hz};

use crate::SoapySource;
use crate::caps::{RateModel, pick_rate};

/// How long the stream runs after each retune before the probe counts
/// anything: a retune restarts the stream, and a device may drop and overflow
/// for a second or two after a rate change (a B210 does after its master clock
/// switches, though the same rung then streams almost cleanly), which says
/// nothing about the rate the host sustains.
pub const PROBE_SETTLE: Duration = Duration::from_millis(1500);
/// How long each candidate rate streams, after [`PROBE_SETTLE`], while its
/// overflows and delivered samples are counted.
pub const PROBE_DWELL: Duration = Duration::from_millis(1500);
/// The most overflows in a dwell a rate may have and still pass. Not zero: the
/// field host's B210 overflowed now and then at every rate in the plan's sweep
/// (11 a minute at 30.72 Msps, which it otherwise holds, and more at slower
/// rates), so a single sporadic overflow must not fail a rate the host keeps
/// up with, while a host that cannot keep up overflows many times a second.
pub const PROBE_MAX_OVERFLOWS: u64 = 1;
/// How many times a rung rejected for overflows is tried again before the
/// probe moves down: the retry tunes the same band again (a live stream
/// already holds it, so the device is not restarted), waits [`PROBE_SETTLE`]
/// again and dwells again, and is judged as the first attempt was; a device
/// that stumbles for a second or two after a rate change has then had that
/// long to settle. A rung that is only short of [`PROBE_MIN_DELIVERED`] (no
/// overflow beyond the allowance), fails to tune or loses the device is not
/// retried.
pub const PROBE_RETRIES: u32 = 1;
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

/// The probe's clock: [`PROBE_SETTLE`] and [`PROBE_DWELL`] by default, shorter
/// in tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeTiming {
    pub settle: Duration,
    pub dwell: Duration,
}

impl Default for ProbeTiming {
    fn default() -> Self {
        ProbeTiming {
            settle: PROBE_SETTLE,
            dwell: PROBE_DWELL,
        }
    }
}

/// [`probe_rate`]'s cap.
pub async fn probe_rate_cap(src: &SoapySource, center_hz: u64) -> Result<u64, String> {
    probe_rate(src, center_hz).await.map(|p| p.cap_hz)
}

/// Stream each of [`probe_candidates`] at `center_hz`, [`PROBE_SETTLE`] then
/// [`PROBE_DWELL`], and take the first rate that delivered at least
/// [`PROBE_MIN_DELIVERED`] of itself with at most [`PROBE_MAX_OVERFLOWS`] new
/// overflow in the dwell (a rung that overflowed gets [`PROBE_RETRIES`] more
/// attempts: the tune is re-issued, a no-op on the running stream, then a
/// settle and a dwell). None passing is the slowest rate that
/// streamed, not clean; a candidate whose tune fails is skipped. Every tune
/// failing is an error, and so is a device the source gives up as lost during
/// the probe: no cap comes out of a rate that killed the device. The stream is
/// then closed and the source left as it opened: nothing tuned, the probe's
/// overflows not counted (a lost device's fault stays).
pub async fn probe_rate(src: &SoapySource, center_hz: u64) -> Result<ProbeResult, String> {
    probe_rate_with(src, center_hz, ProbeTiming::default()).await
}

/// [`probe_rate`] with its own settle and dwell.
pub async fn probe_rate_with(
    src: &SoapySource,
    center_hz: u64,
    timing: ProbeTiming,
) -> Result<ProbeResult, String> {
    probe_rate_reporting(src, center_hz, timing, &mut |_, _| {}).await
}

/// [`probe_rate_with`], calling `on_rung` with each candidate's index and rate
/// before its first attempt.
async fn probe_rate_reporting(
    src: &SoapySource,
    center_hz: u64,
    timing: ProbeTiming,
    on_rung: &mut dyn FnMut(usize, u64),
) -> Result<ProbeResult, String> {
    let found = probe(src, center_hz, timing, on_rung).await;
    src.end_probe().await;
    found
}

async fn probe(
    src: &SoapySource,
    center_hz: u64,
    timing: ProbeTiming,
    on_rung: &mut dyn FnMut(usize, u64),
) -> Result<ProbeResult, String> {
    // The reader thread ends when nothing receives; hold a receiver throughout.
    let _rx = src.subscribe();
    let mut lowest: Option<u64> = None;
    let mut tried = 0;
    let mut errors = Vec::new();
    for (i, rate) in probe_candidates(src.rate_model()).into_iter().enumerate() {
        on_rung(i, rate);
        let band = Tuning {
            center_hz: Hz(center_hz),
            bandwidth_hz: Hz(rate),
        };
        let mut counted = false;
        for _attempt in 0..=PROBE_RETRIES {
            let tuned = src.tune(band).await;
            if let Some(e) = lost(src, rate) {
                return Err(e);
            }
            if let Err(e) = tuned {
                errors.push(format!("{rate} Hz: {e}"));
                break;
            }
            if !counted {
                counted = true;
                tried += 1;
                lowest = Some(lowest.map_or(rate, |l| l.min(rate)));
            }
            tokio::time::sleep(timing.settle).await;
            if let Some(e) = lost(src, rate) {
                return Err(e);
            }
            let before = src.overflows();
            let samples_before = src.samples_delivered();
            tokio::time::sleep(timing.dwell).await;
            if let Some(e) = lost(src, rate) {
                return Err(e);
            }
            let overflows = src.overflows() - before;
            // The samples of this dwell alone: the source's windowed rate
            // would carry the settle, and a first attempt's stumble.
            let received = src.samples_delivered() - samples_before;
            let due = rate as f64 * timing.dwell.as_secs_f64();
            let short = (received as f64) < PROBE_MIN_DELIVERED * due;
            let overflowed = overflows > PROBE_MAX_OVERFLOWS;
            if !short && !overflowed {
                return Ok(ProbeResult {
                    cap_hz: rate,
                    tried,
                    clean: true,
                });
            }
            // Short with no overflow beyond the allowance is a host that
            // cannot keep up, not a stumble: no retry.
            if !overflowed {
                break;
            }
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

/// The rate cap of each sample depth on the device's link
/// ([`SoapySource::link_depths`]), in its order: a depth `known` lists (`(bits,
/// hz)`, a cap an earlier open measured) is `Measured` at it without a probe;
/// every other is streamed through [`probe_rate`] at that depth, lossy ones
/// included (the capability record is set once, at open), and is `Measured`
/// at the clean rate found, or keeps the device's asserted cap when no rung
/// was clean. A depth whose probe errs (the stream will not open at it, no
/// rate tuned) is left out, with a stderr line, and the others stand; only a
/// device lost during the probe is the error. The source is left as it
/// opened: nothing tuned, the driver's default depth. Empty for a device with
/// one depth.
///
/// `on_progress` is called before each rung with a [`Starting`] (see
/// [`rung_progress`]); never when every depth is `known`.
pub async fn probe_depths(
    src: &mut SoapySource,
    center_hz: u64,
    known: &[(u8, u64)],
    on_progress: &mut dyn FnMut(Starting),
) -> Result<Vec<Depth>, String> {
    probe_depths_with(src, center_hz, known, ProbeTiming::default(), on_progress).await
}

/// What [`probe_depths`] and [`rate_cap`] report before a rung: `measuring`,
/// the depth and the rate the rung streams (`12-bit at 30720000 Hz`; the rate
/// alone, `30720000 Hz`, on a link of one depth, whose bits the source does not
/// know), the rung's `step` counted from 1 over every depth, `of` the steps
/// taken before it plus the most rungs the probe may still try (this one, the
/// rest of its depth's ladder and every depth after it: never more than the
/// last report's), and `eta_ms` the time those take at most, each rung retried
/// ([`PROBE_RETRIES`]) and each attempt a [`PROBE_SETTLE`] and a
/// [`PROBE_DWELL`] (the production figures, whatever the probe's own
/// [`ProbeTiming`]).
pub fn rung_progress(bits: Option<u8>, rate_hz: u64, step: u32, of: u32) -> Starting {
    let attempt = (PROBE_SETTLE + PROBE_DWELL).as_millis() as u64;
    Starting {
        what: "measuring".into(),
        detail: match bits {
            Some(bits) => format!("{bits}-bit at {rate_hz} Hz"),
            None => format!("{rate_hz} Hz"),
        },
        step,
        of,
        eta_ms: u64::from(of - step + 1) * u64::from(1 + PROBE_RETRIES) * attempt,
    }
}

/// [`probe_depths`] with its own settle and dwell.
pub async fn probe_depths_with(
    src: &mut SoapySource,
    center_hz: u64,
    known: &[(u8, u64)],
    timing: ProbeTiming,
    on_progress: &mut dyn FnMut(Starting),
) -> Result<Vec<Depth>, String> {
    let asserted = src.capabilities().max_bandwidth_hz.clone();
    let links = src.link_depths().to_vec();
    let is_known = |bits: u8| known.iter().any(|(b, _)| *b == bits);
    let rungs = probe_candidates(src.rate_model()).len() as u32;
    // The rungs of the depths after the one being probed, and the steps reported.
    let mut later = rungs * links.iter().filter(|l| !is_known(l.bits)).count() as u32;
    let mut step = 0;
    let mut depths = Vec::with_capacity(links.len());
    let mut found = Ok(());
    for link in &links {
        let cap = match known.iter().find(|(bits, _)| *bits == link.bits) {
            Some(&(_, hz)) => Claim::Measured(Hz(hz)),
            None => {
                later -= rungs;
                let bits = link.bits;
                let mut report = |i: usize, rate: u64| {
                    step += 1;
                    let of = step - 1 + (rungs - i as u32) + later;
                    on_progress(rung_progress(Some(bits), rate, step, of));
                };
                src.use_depth(Some(link.bits)).await;
                match probe_rate_reporting(src, center_hz, timing, &mut report).await {
                    Ok(p) if p.clean => Claim::Measured(Hz(p.cap_hz)),
                    Ok(_) => asserted.clone(),
                    Err(e) if src.is_lost() => {
                        found = Err(format!("{}-bit depth: {e}", link.bits));
                        break;
                    }
                    Err(e) => {
                        eprintln!(
                            "zeta-source-soapy: {}: {}-bit depth left out: {e}",
                            src.id().0,
                            link.bits
                        );
                        continue;
                    }
                }
            }
        };
        depths.push(Depth {
            bits: link.bits,
            lossless: link.lossless,
            max_bandwidth_hz: cap,
        });
    }
    if !links.is_empty() {
        src.use_depth(None).await;
    }
    found.map(|()| depths)
}

/// How the rate cap of a device whose link has one depth is set at open
/// ([`rate_cap`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RateCap {
    /// `max_rate_hz=N`: asserted, nothing probed.
    Asserted(u64),
    /// `--rate-cap-hz N`, a cap an earlier open measured: measured, nothing
    /// probed.
    Passed(u64),
    /// Neither: what [`probe_rate`] found.
    Probed(Result<ProbeResult, String>),
}

/// The rate cap of a device whose link has one depth: `asserted`
/// (`max_rate_hz=`), else `cached` (`--rate-cap-hz`), else [`probe_rate`] at
/// `center_hz`, reporting each rung to `on_progress` before it streams (see
/// [`rung_progress`]; `of` is the ladder's rungs). Nothing is reported when
/// the cap is given.
pub async fn rate_cap(
    src: &SoapySource,
    center_hz: u64,
    asserted: Option<u64>,
    cached: Option<u64>,
    on_progress: &mut dyn FnMut(Starting),
) -> RateCap {
    let timing = ProbeTiming::default();
    rate_cap_with(src, center_hz, asserted, cached, timing, on_progress).await
}

/// [`rate_cap`] with its own settle and dwell.
pub async fn rate_cap_with(
    src: &SoapySource,
    center_hz: u64,
    asserted: Option<u64>,
    cached: Option<u64>,
    timing: ProbeTiming,
    on_progress: &mut dyn FnMut(Starting),
) -> RateCap {
    match (asserted, cached) {
        (Some(hz), _) => RateCap::Asserted(hz),
        (None, Some(hz)) => RateCap::Passed(hz),
        (None, None) => {
            let rungs = probe_candidates(src.rate_model()).len() as u32;
            let mut report = |i: usize, rate: u64| {
                on_progress(rung_progress(None, rate, i as u32 + 1, rungs));
            };
            RateCap::Probed(probe_rate_reporting(src, center_hz, timing, &mut report).await)
        }
    }
}

/// Every sample depth on the device's link at the operator's `hz`
/// (`max_rate_hz=`), asserted: nothing is probed.
pub fn asserted_depths(src: &SoapySource, hz: u64) -> Vec<Depth> {
    src.link_depths()
        .iter()
        .map(|l| Depth {
            bits: l.bits,
            lossless: l.lossless,
            max_bandwidth_hz: Claim::Asserted(Hz(hz)),
        })
        .collect()
}

/// The source's own depth among `depths`: `asked` (`bits=`) when listed,
/// else an error naming the listed ones. Unasked, the narrowest lossless
/// depth that measures as wide as the widest: among the lossless depths, the
/// narrowest whose cap is measured and not below the highest measured
/// lossless cap. A lossless depth carries all the converter gives, so a
/// narrower one that streams as wide is the same fidelity in fewer bytes on
/// the link; one that measured narrower, or was never measured clean, would
/// shrink the source's bandwidth (a resumed job's capture could no longer
/// fit), so it does not win. With no lossless depth measured, the lossless
/// depth with the highest asserted cap, the deepest on a tie. A lossy depth
/// is only ever asked for; with none lossless, the deepest.
pub fn own_depth(depths: &[Depth], asked: Option<u8>) -> Result<u8, String> {
    if let Some(bits) = asked {
        if depths.iter().any(|d| d.bits == bits) {
            return Ok(bits);
        }
        let listed: Vec<String> = depths.iter().map(|d| d.bits.to_string()).collect();
        return Err(match listed.is_empty() {
            true => format!("bits={bits}: the device's link has one sample depth"),
            false => format!(
                "bits={bits}: the device's link carries {} bits",
                listed.join(", ")
            ),
        });
    }
    let measured = |d: &Depth| match d.max_bandwidth_hz {
        Claim::Measured(hz) => Some(hz.0),
        _ => None,
    };
    let lossless = || depths.iter().filter(|d| d.lossless);
    let widest = lossless().filter_map(measured).max();
    let chosen = match widest {
        Some(widest) => lossless()
            .filter(|d| measured(d).is_some_and(|hz| hz >= widest))
            .min_by_key(|d| d.bits),
        None => lossless().max_by_key(|d| (d.max_bandwidth_hz.value().0, d.bits)),
    };
    chosen
        .or_else(|| depths.iter().max_by_key(|d| d.bits))
        .map(|d| d.bits)
        .ok_or_else(|| "the device's link has one sample depth".to_string())
}

/// The probe's error when the source gave its device up as lost at `rate`.
fn lost(src: &SoapySource, rate: u64) -> Option<String> {
    src.is_lost().then(|| {
        let fault = src.fault().unwrap_or_default();
        format!("rate probe: device lost at {rate} Hz: {fault}")
    })
}
