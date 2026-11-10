//! The rate probe against the paced mock device: candidates from the top of
//! the ladder, the first clean one wins (a sporadic overflow allowed, the
//! settle after a retune not counted), and zeta's own `max_rate_hz=` item;
//! then the probe per sample depth on the device's link and the source's own
//! depth.

use std::sync::Arc;
use std::time::{Duration, Instant};

use zeta_core::port::Port;
use zeta_core::types::{Claim, Depth, Hz, SourceId};
use zeta_source_soapy::caps::{LinkDepth, RateModel, front_end, link_depths};
use zeta_source_soapy::device::RangeHz;
use zeta_source_soapy::mock::MockDevice;
use zeta_source_soapy::probe::{
    PROBE_DWELL, PROBE_MAX_OVERFLOWS, PROBE_MAX_RUNGS, PROBE_RETRIES, PROBE_SETTLE, ProbeResult,
    ProbeTiming, asserted_depths, own_depth, probe_candidates, probe_depths_with, probe_rate_with,
};
use zeta_source_soapy::{SoapySource, ZetaArgs, split_zeta_args};

const CENTER: u64 = 100_000_000;
/// The probe's clock in these tests: the settle and dwell the read numbers
/// below are placed for, shorter than the production 1.5 s settle so a probe
/// of the small mock ladder stays cheap.
const TIMING: ProbeTiming = ProbeTiming {
    settle: Duration::from_millis(200),
    dwell: Duration::from_millis(1500),
};

async fn probe_rate(src: &SoapySource, center: u64) -> Result<ProbeResult, String> {
    probe_rate_with(src, center, TIMING).await
}

async fn probe_rate_cap(src: &SoapySource, center: u64) -> Result<u64, String> {
    probe_rate(src, center).await.map(|p| p.cap_hz)
}
/// Every tenth read overflows: a rung the device cannot keep up with.
const OVERFLOWING: u32 = 10;
/// Reads 60 and 120 after a retune: about 0.6 s and 1.2 s in, inside the dwell
/// that follows the 0.2 s settle (reads are 10 ms at the top rung), as the
/// tests' [`TIMING`] has it.
const IN_DWELL: [u64; 2] = [60, 120];
/// Read 1: as the stream starts after a retune, inside the settle.
const IN_SETTLE: u64 = 1;

/// A UHD-like device at a 6.144 MHz master clock (a tenth of a B210's, so
/// the paced reads stay cheap in a debug build), streaming in real time once
/// the script is empty. Its top rungs: 6.144, 3.072, 2.048, 1.536, 1.2288 and
/// 1.024 MHz.
fn ladder() -> MockDevice {
    MockDevice::hackrf_like()
        .with_driver("b200")
        .with_uhd_decimation(6.144e6)
        .with_paced_reads()
}

fn source(dev: &Arc<MockDevice>) -> SoapySource {
    SoapySource::with_device(SourceId("sdr0".into()), dev.clone()).unwrap()
}

/// The `set_sample_rate` calls from call `from` on.
fn rate_calls(dev: &MockDevice, from: usize) -> Vec<String> {
    dev.calls()[from..]
        .iter()
        .filter(|c| c.starts_with("set_sample_rate"))
        .cloned()
        .collect()
}

#[tokio::test]
async fn a_ladder_is_probed_from_the_top_and_the_first_clean_rung_wins() {
    let dev = Arc::new(ladder().pace_at(6.144e6, 1.0, OVERFLOWING).pace_at(
        3.072e6,
        1.0,
        OVERFLOWING,
    ));
    let src = source(&dev);
    let opened = dev.calls().len();
    assert_eq!(
        probe_rate(&src, CENTER).await,
        Ok(ProbeResult {
            cap_hz: 2_048_000,
            tried: 3,
            clean: true,
        })
    );
    assert_eq!(
        rate_calls(&dev, opened),
        [
            "set_sample_rate(6144000)",
            "set_sample_rate(3072000)",
            "set_sample_rate(2048000)"
        ]
    );
    assert_eq!(src.fault(), None);
    // Left as it opened: no stream, nothing tuned, the probe's overflows not
    // counted against the device.
    assert_eq!(src.current(), None);
    assert_eq!(src.overflows(), 0);
    assert!(dev.calls().ends_with(&["deactivate".into(), "drop".into()]));
}

#[tokio::test]
async fn a_rung_with_one_overflow_in_its_dwell_is_clean() {
    assert_eq!(PROBE_MAX_OVERFLOWS, 1);
    let dev = Arc::new(ladder().overflow_reads_at(6.144e6, &IN_DWELL[..1]));
    let src = source(&dev);
    assert_eq!(
        probe_rate(&src, CENTER).await,
        Ok(ProbeResult {
            cap_hz: 6_144_000,
            tried: 1,
            clean: true,
        })
    );
}

#[tokio::test]
async fn a_rung_that_overflows_on_both_attempts_falls_to_the_next_rung() {
    assert_eq!(PROBE_RETRIES, 1);
    // An overflow every 0.3 s in the retry's dwell as in the first, the
    // samples all arriving (an overflow every tenth read would deliver short,
    // which is not retried): the rung is rejected, and `tried` counts it once.
    let every_30: Vec<u64> = (1..=100).map(|i| i * 30).collect();
    let dev = Arc::new(ladder().overflow_reads_at(6.144e6, &every_30));
    let src = source(&dev);
    let opened = dev.calls().len();
    let start = Instant::now();
    assert_eq!(
        probe_rate(&src, CENTER).await,
        Ok(ProbeResult {
            cap_hz: 3_072_000,
            tried: 2,
            clean: true,
        })
    );
    // Two attempts at the top rung, a settle and a dwell each, then the next.
    let took = start.elapsed();
    let attempt = TIMING.settle + TIMING.dwell;
    assert!(took >= 3 * attempt, "{took:?}");
    // Not a third attempt at the top rung.
    assert!(took < 4 * attempt, "{took:?}");
    assert_eq!(
        rate_calls(&dev, opened),
        ["set_sample_rate(6144000)", "set_sample_rate(3072000)"]
    );
}

#[tokio::test]
async fn a_rung_that_overflows_twice_then_streams_clean_on_the_retry_is_accepted() {
    // The stream overflows twice in the first dwell (reads 60 and 120 after
    // it starts) and never again: the retry's dwell is clean. The retry tunes
    // the same band, which a live stream already holds, so the device is not
    // restarted: the second attempt is a longer settle and a fresh dwell.
    let dev = Arc::new(ladder().overflow_reads_at(6.144e6, &IN_DWELL));
    let src = source(&dev);
    let opened = dev.calls().len();
    let start = Instant::now();
    assert_eq!(
        probe_rate(&src, CENTER).await,
        Ok(ProbeResult {
            cap_hz: 6_144_000,
            tried: 1,
            clean: true,
        })
    );
    assert!(start.elapsed() >= 2 * (TIMING.settle + TIMING.dwell));
    assert_eq!(rate_calls(&dev, opened), ["set_sample_rate(6144000)"]);
}

#[tokio::test]
async fn a_first_attempt_that_lost_samples_does_not_fail_a_clean_retry() {
    // Reads 60 to 100 (inside the first dwell) deliver half their samples,
    // and two overflow: a stumble that costs about 13 % of that dwell. The
    // retry's own dwell is whole, and is what the rung is judged on (the
    // source's windowed rate would still carry the stumble).
    let lost: Vec<u64> = (60..=100).collect();
    let dev = Arc::new(ladder().short_reads_at(6.144e6, &lost, 0.5, &IN_DWELL));
    let src = source(&dev);
    assert_eq!(
        probe_rate(&src, CENTER).await,
        Ok(ProbeResult {
            cap_hz: 6_144_000,
            tried: 1,
            clean: true,
        })
    );
}

#[tokio::test]
async fn an_overflow_while_the_retune_settles_is_not_counted() {
    // Two overflows in all, but the first falls in the settle window.
    let dev = Arc::new(ladder().overflow_reads_at(6.144e6, &[IN_SETTLE, IN_DWELL[0]]));
    let src = source(&dev);
    assert_eq!(probe_rate_cap(&src, CENTER).await, Ok(6_144_000));
}

#[tokio::test]
async fn a_rung_short_of_the_delivered_floor_is_rejected() {
    let dev = Arc::new(ladder().pace_at(6.144e6, 0.9, 0));
    let src = source(&dev);
    let opened = dev.calls().len();
    assert_eq!(probe_rate_cap(&src, CENTER).await, Ok(3_072_000));
    assert_eq!(
        rate_calls(&dev, opened),
        ["set_sample_rate(6144000)", "set_sample_rate(3072000)"]
    );
}

#[tokio::test]
async fn a_probe_that_never_streams_clean_caps_at_the_lowest_rung() {
    let mut dev = ladder();
    let src0 = source(&Arc::new(ladder()));
    let candidates = probe_candidates(src0.rate_model());
    assert_eq!(candidates.len(), PROBE_MAX_RUNGS);
    for &r in &candidates {
        dev = dev.pace_at(r as f64, 1.0, OVERFLOWING);
    }
    let dev = Arc::new(dev);
    let src = source(&dev);
    let opened = dev.calls().len();
    let start = Instant::now();
    assert_eq!(
        probe_rate(&src, CENTER).await,
        Ok(ProbeResult {
            cap_hz: *candidates.last().unwrap(),
            tried: PROBE_MAX_RUNGS,
            clean: false,
        })
    );
    let took = start.elapsed();
    // Each rung is tried twice: a settle and a dwell per attempt.
    let attempts = PROBE_MAX_RUNGS as u32 * (1 + PROBE_RETRIES);
    let bound = (TIMING.settle + TIMING.dwell + Duration::from_millis(500)) * attempts;
    assert!(took <= bound, "{took:?} > {bound:?}");
    assert_eq!(rate_calls(&dev, opened).len(), PROBE_MAX_RUNGS);
}

#[tokio::test]
async fn a_rung_that_loses_the_device_ends_the_probe_with_an_error() {
    // Every read at the top rung overflows: the reader gives the device up as
    // lost after its overflow storm, inside the dwell. No cap may come out.
    let dev = Arc::new(ladder().pace_at(6.144e6, 1.0, 1));
    let src = source(&dev);
    let opened = dev.calls().len();
    let err = probe_rate(&src, CENTER).await.unwrap_err();
    assert!(err.contains("overflow storm"), "{err}");
    assert!(src.fault().is_some_and(|f| f.contains("overflow storm")));
    assert_eq!(rate_calls(&dev, opened), ["set_sample_rate(6144000)"]);
}

#[tokio::test]
async fn the_fallback_is_the_lowest_rung_that_tuned_and_counts_only_those() {
    // The top rung's tune fails; the rest all overflow: the cap is the lowest
    // rung, five rungs tried, none clean.
    let mut dev = ladder();
    let candidates = probe_candidates(source(&Arc::new(ladder())).rate_model());
    for &r in &candidates {
        dev = dev.pace_at(r as f64, 1.0, OVERFLOWING);
    }
    let dev = Arc::new(dev);
    let src = source(&dev);
    // After the open, whose rate-range probe calls `set_sample_rate` too.
    dev.fail_next("set_sample_rate", "busy");
    assert_eq!(
        probe_rate(&src, CENTER).await,
        Ok(ProbeResult {
            cap_hz: *candidates.last().unwrap(),
            tried: PROBE_MAX_RUNGS - 1,
            clean: false,
        })
    );
}

#[tokio::test]
async fn a_probe_whose_every_tune_fails_is_an_error() {
    // Centre outside the device's range: every candidate's tune is refused.
    let dev = Arc::new(ladder());
    let src = source(&dev);
    let opened = dev.calls().len();
    assert!(probe_rate_cap(&src, 10_000_000_000).await.is_err());
    assert!(rate_calls(&dev, opened).is_empty());
}

#[test]
fn the_production_settle_outlasts_a_master_clock_switch() {
    assert_eq!(ProbeTiming::default().settle, PROBE_SETTLE);
    assert_eq!(ProbeTiming::default().dwell, PROBE_DWELL);
    assert_eq!(PROBE_SETTLE, Duration::from_millis(1500));
}

#[test]
fn a_ladder_is_probed_from_the_top_at_most_six_rungs() {
    let src = source(&Arc::new(ladder()));
    assert_eq!(
        probe_candidates(src.rate_model()),
        [
            6_144_000, 3_072_000, 2_048_000, 1_536_000, 1_228_800, 1_024_000
        ]
    );
    let b210 = MockDevice::hackrf_like()
        .with_driver("b200")
        .with_uhd_decimation(61.44e6);
    assert_eq!(
        probe_candidates(source(&Arc::new(b210)).rate_model()),
        [
            61_440_000, 30_720_000, 20_480_000, 15_360_000, 12_288_000, 10_240_000
        ]
    );
}

#[test]
fn a_range_device_probes_max_half_quarter() {
    let model = RateModel::Ranges(vec![RangeHz {
        min_hz: 1e6,
        max_hz: 20e6,
        step_hz: 0.0,
    }]);
    assert_eq!(
        probe_candidates(&model),
        [20_000_000, 10_000_000, 5_000_000]
    );
}

#[test]
fn set_max_bandwidth_replaces_the_claim() {
    let mut src = source(&Arc::new(ladder()));
    src.set_max_bandwidth(Claim::Measured(Hz(20_480_000)));
    assert_eq!(
        src.capabilities().max_bandwidth_hz,
        Claim::Measured(Hz(20_480_000))
    );
}

#[test]
fn a_max_rate_item_asserts_the_cap_and_is_stripped_from_the_driver_args() {
    assert_eq!(
        split_zeta_args("driver=hackrf,max_rate_hz=8000000,gain=32").unwrap(),
        (
            "driver=hackrf".into(),
            ZetaArgs {
                gain: Some(32.0),
                ppm: None,
                max_rate_hz: Some(8_000_000),
                settle_ms: None,
                usable: None,
                bits: None,
            }
        )
    );
    assert_eq!(
        split_zeta_args("driver=uhd,max_rate_hz=fast").unwrap_err(),
        "max_rate_hz=fast: not a whole number of Hz"
    );
    assert!(split_zeta_args("driver=uhd,max_rate_hz=0").is_err());
}

// The sample depths on the device's link (`WIRE` on SoapyUHD).

/// The link formats a B210 names under SoapyUHD's `WIRE` stream argument.
const WIRES: [&str; 3] = ["sc16", "sc12", "sc8"];

fn depth(bits: u8, lossless: bool, cap: Claim<Hz>) -> Depth {
    Depth {
        bits,
        lossless,
        max_bandwidth_hz: cap,
    }
}

fn measured(hz: u64) -> Claim<Hz> {
    Claim::Measured(Hz(hz))
}

/// The mock ladder as a B210 (a 12-bit converter) naming `sc16`, `sc12` and
/// `sc8`.
fn b210_link() -> MockDevice {
    ladder().with_hardware("B210").with_wire_formats(&WIRES)
}

/// How many `open_rx` calls carried the stream argument `WIRE=<wire>`.
fn opened_with(dev: &MockDevice, wire: &str) -> usize {
    let arg = format!("WIRE={wire})");
    dev.calls()
        .iter()
        .filter(|c| c.starts_with("open_rx(") && c.ends_with(&arg))
        .count()
}

#[tokio::test]
async fn each_depth_is_probed_and_the_fastest_lossless_one_is_the_sources_own() {
    // `sc16` cannot keep up at the top rung; `sc12`, three quarters of the
    // bytes, can.
    let dev = Arc::new(b210_link().overflow_at_wire("sc16", 6.144e6));
    let mut src = source(&dev);
    let depths = probe_depths_with(&mut src, CENTER, &[], TIMING)
        .await
        .unwrap();
    assert_eq!(
        depths,
        [
            depth(16, true, measured(3_072_000)),
            depth(12, true, measured(6_144_000)),
            depth(8, false, measured(6_144_000)),
        ]
    );
    for wire in WIRES {
        assert!(opened_with(&dev, wire) >= 1, "{wire}: {:?}", dev.calls());
    }
    let own = own_depth(&depths, None).unwrap();
    assert_eq!(own, 12);
    src.set_depths(depths.clone(), own);
    assert_eq!(src.capabilities().depths, depths);
    assert_eq!(src.capabilities().max_bandwidth_hz, measured(6_144_000));
    assert_eq!(src.depth(), Some(depths[1].clone()));
    // Left as it opened: nothing tuned, nothing counted.
    assert_eq!(src.current(), None);
    assert_eq!(src.overflows(), 0);
}

#[test]
fn a_tie_goes_to_the_deepest() {
    let depths = [
        depth(16, true, measured(30_720_000)),
        depth(12, true, measured(30_720_000)),
        depth(8, false, measured(61_440_000)),
    ];
    assert_eq!(own_depth(&depths, None), Ok(16));
}

#[test]
fn a_lossy_depth_is_never_the_default() {
    let depths = [
        depth(16, true, measured(15_360_000)),
        depth(12, true, measured(20_480_000)),
        depth(8, false, measured(61_440_000)),
    ];
    assert_eq!(own_depth(&depths, None), Ok(12));
    // Only as an explicit choice.
    assert_eq!(own_depth(&depths, Some(8)), Ok(8));
}

#[test]
fn an_unmeasured_depth_does_not_outrank_a_measured_one() {
    // A depth whose probe found no rung clean keeps the device's asserted
    // top rate: no evidence that it is faster.
    let depths = [
        depth(16, true, Claim::Asserted(Hz(61_440_000))),
        depth(12, true, measured(30_720_000)),
    ];
    assert_eq!(own_depth(&depths, None), Ok(12));
}

#[tokio::test]
async fn a_known_depth_cap_skips_its_probe() {
    let dev = Arc::new(b210_link());
    let mut src = source(&dev);
    let depths = probe_depths_with(&mut src, CENTER, &[(16, 3_072_000)], TIMING)
        .await
        .unwrap();
    assert_eq!(
        depths,
        [
            depth(16, true, measured(3_072_000)),
            depth(12, true, measured(6_144_000)),
            depth(8, false, measured(6_144_000)),
        ]
    );
    assert_eq!(opened_with(&dev, "sc16"), 0, "{:?}", dev.calls());
    assert_eq!(opened_with(&dev, "sc12"), 1, "{:?}", dev.calls());
}

#[test]
fn an_unlisted_bits_argument_names_the_depths() {
    let depths = [
        depth(16, true, measured(30_720_000)),
        depth(12, true, measured(30_720_000)),
        depth(8, false, measured(61_440_000)),
    ];
    let e = own_depth(&depths, Some(10)).unwrap_err();
    assert!(e.contains("bits=10") && e.contains("16, 12, 8"), "{e}");
    let e = own_depth(&[], Some(12)).unwrap_err();
    assert!(e.contains("bits=12"), "{e}");
    // `bits=` is zeta's own key, taken out of the driver's args.
    let (rest, z) = split_zeta_args("driver=uhd,bits=12").unwrap();
    assert_eq!((rest.as_str(), z.bits), ("driver=uhd", Some(12)));
    assert_eq!(
        split_zeta_args("bits=fine").unwrap_err(),
        "bits=fine: not a number of bits from 1 to 32"
    );
    assert!(split_zeta_args("bits=0").is_err());
    assert!(split_zeta_args("bits=33").is_err());
}

#[test]
fn max_rate_hz_asserts_every_depth_without_a_probe() {
    let dev = Arc::new(b210_link());
    let src = source(&dev);
    let opened = dev.calls().len();
    let cap = Claim::Asserted(Hz(8_000_000));
    assert_eq!(
        asserted_depths(&src, 8_000_000),
        [
            depth(16, true, cap.clone()),
            depth(12, true, cap.clone()),
            depth(8, false, cap),
        ]
    );
    assert!(dev.calls()[opened..].is_empty(), "{:?}", dev.calls());
}

#[tokio::test]
async fn a_device_without_link_formats_has_no_depths() {
    // None named, or only one: the source has one depth.
    for dev in [ladder(), ladder().with_wire_formats(&["sc16"])] {
        let dev = Arc::new(dev);
        let mut src = source(&dev);
        assert!(src.link_depths().is_empty());
        let opened = dev.calls().len();
        assert_eq!(
            probe_depths_with(&mut src, CENTER, &[], TIMING).await,
            Ok(vec![])
        );
        assert!(dev.calls()[opened..].is_empty(), "{:?}", dev.calls());
        assert!(asserted_depths(&src, 8_000_000).is_empty());
        assert_eq!(src.depth(), None);
        assert!(src.capabilities().depths.is_empty());
    }
}

#[test]
fn an_unknown_converter_makes_only_the_deepest_lossless() {
    let wire = |w: &str| ("WIRE".to_string(), w.to_string());
    // A converter of unknown depth: only the deepest format is sure to hold
    // every bit. The order the driver names them in does not matter, and a
    // format the runtime has no depth for (`fc32`) is left out.
    let dev = MockDevice::hackrf_like().with_wire_formats(&["sc8", "sc16", "sc12", "fc32"]);
    assert_eq!(
        link_depths(&dev, &front_end("hackrf", "")),
        [
            LinkDepth {
                bits: 16,
                lossless: true,
                arg: wire("sc16"),
            },
            LinkDepth {
                bits: 12,
                lossless: false,
                arg: wire("sc12"),
            },
            LinkDepth {
                bits: 8,
                lossless: false,
                arg: wire("sc8"),
            },
        ]
    );
    // A 12-bit converter: 12 bits hold all of it.
    let lossless: Vec<bool> = link_depths(&b210_link(), &front_end("b200", "B210"))
        .iter()
        .map(|d| d.lossless)
        .collect();
    assert_eq!(lossless, [true, true, false]);
}
