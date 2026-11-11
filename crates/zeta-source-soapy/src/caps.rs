//! Asserted capabilities, the per-radio front-end table and the sample-rate /
//! format choice.

use std::time::Duration;

use zeta_core::types::{Capabilities, Claim, Hz, SampleFormat};

use crate::device::{RangeHz, SoapyDevice, WireFormat};

const RATE_QUANTUM_HZ: u64 = 100_000;

/// Doubling rounds of [`probe_rate_ranges`] on a UHD device.
const UHD_PROBE_ROUNDS: usize = 3;
/// UHD master-clock ceilings, descending (N3xx, X3xx, B2xx, B2xx alternate,
/// B1xx): tried once when doubling the rate stops widening the range.
const UHD_CLOCK_CEILINGS_HZ: [f64; 5] = [250e6, 200e6, 61.44e6, 56e6, 32e6];

fn max_rate(ranges: &[RangeHz]) -> f64 {
    ranges
        .iter()
        .map(|r| r.max_hz)
        .fold(f64::NEG_INFINITY, f64::max)
}

/// Driver keys a UHD device reports (SoapyUHD gives the device type, `b200`
/// for a B210, rather than `uhd`); matched as case-insensitive prefixes.
const UHD_DRIVER_KEYS: [&str; 7] = ["uhd", "b200", "b100", "x300", "n3xx", "e3xx", "usrp2"];

/// Whether a device opened with `args` reporting `driver_key` is a UHD device,
/// whose rate range depends on its master clock: `driver=uhd` in the args, or
/// a UHD driver key.
pub fn is_uhd(args: &str, driver_key: &str) -> bool {
    let by_args = args.split(',').any(|kv| {
        kv.split_once('=').is_some_and(|(k, v)| {
            k.trim().eq_ignore_ascii_case("driver") && v.trim().eq_ignore_ascii_case("uhd")
        })
    });
    let key = driver_key.to_ascii_lowercase();
    by_args || UHD_DRIVER_KEYS.iter().any(|k| key.starts_with(k))
}

/// What the source knows of a radio's front end beyond what its driver
/// reports: how long a tune takes to settle after `set_frequency` returns
/// (the reader discards the samples until then, see
/// [`crate::SoapySource::with_settle`]), the bits its ADC resolves
/// (`None`: not known), and the link formats ([`LINK_FORMAT_ARG`] options) its
/// driver accepts without naming them in its stream-argument information.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrontEnd {
    pub settle: Duration,
    pub adc_bits: Option<u8>,
    pub link_formats: &'static [&'static str],
}

/// The settle of a radio [`FRONT_ENDS`] has no row for: conservative, ten
/// times the B2xx family's measured one.
pub const SETTLE_DEFAULT: Duration = Duration::from_millis(10);

/// The part of a capture, centred, that a SoapySDR device's front-end filter
/// passes flat, asserted for every device (`usable=` overrides it, see
/// [`crate::SoapySource::with_usable`]): a scan advances by this much of each
/// capture and masks the rest. Measured 2026-11-10 on the field host's B210:
/// 28 MHz of a 30.72 Msps capture clean, the full width rolled off; 0.9 gives
/// 27.648 MHz.
pub const USABLE_DEFAULT: f64 = 0.9;

/// The radios whose front end is measured or known: a UHD device (see
/// [`is_uhd`]) whose hardware key starts with the prefix (matched
/// case-insensitively; SoapyUHD gives the motherboard, `B210` for a B210). The
/// B2xx family (an AD9361 behind a 12-bit ADC): a fast tune's power is within
/// 0.05 dB of steady 1 ms after `set_frequency` returns, measured on the
/// field host's B210. Its link also carries `sc12`, which UHD's B200 code
/// accepts as `otw_format`, while SoapyUHD (0.4.1) names only `sc8` and `sc16`
/// under `WIRE` (it hands the argument on as `otw_format`, as its upstream
/// source reads; not checked against a live radio). The probe finds whether
/// it streams.
const FRONT_ENDS: [(&str, FrontEnd); 1] = [(
    "b2",
    FrontEnd {
        settle: Duration::from_millis(1),
        adc_bits: Some(12),
        link_formats: &["sc12"],
    },
)];

/// The front end of a device reporting `driver_key` and `hardware_key`: its
/// row in [`FRONT_ENDS`], or [`SETTLE_DEFAULT`] and no ADC depth.
pub fn front_end(driver_key: &str, hardware_key: &str) -> FrontEnd {
    let hw = hardware_key.to_ascii_lowercase();
    FRONT_ENDS
        .iter()
        .find(|(prefix, _)| is_uhd("", driver_key) && hw.starts_with(prefix))
        .map_or(
            FrontEnd {
                settle: SETTLE_DEFAULT,
                adc_bits: None,
                link_formats: &[],
            },
            |(_, f)| *f,
        )
}

/// The receive-stream argument a driver names its link formats under: the
/// sample depth between the device and the host (SoapyUHD's `WIRE`, UHD's
/// `otw_format`), distinct from the format the host is handed (UHD unpacks on
/// the host, so the reader still gets CS16 or CF32).
pub const LINK_FORMAT_ARG: &str = "WIRE";

/// The link formats a source can run, as `(option, bits per I and Q sample)`,
/// deepest first: `sc16` (4 bytes a sample, UHD's default), `sc12` (3 bytes)
/// and `sc8` (2 bytes, the low bits dropped). A driver's other options
/// (`fc32`, 8 bytes, no use here) are left out.
const LINK_FORMATS: [(&str, u8); 3] = [("sc16", 16), ("sc12", 12), ("sc8", 8)];

/// One sample depth a device's link can carry: `bits` per I and Q sample,
/// whether it holds every bit the converter resolves (`lossless`), and the
/// stream argument that selects it (`("WIRE", "sc12")`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkDepth {
    pub bits: u8,
    pub lossless: bool,
    pub arg: (String, String),
}

/// The sample depths `dev`'s link can carry, deepest first: the options of
/// its [`LINK_FORMAT_ARG`] stream argument, with those its front end's row
/// adds (`fe.link_formats`, accepted but not named), that are
/// [`LINK_FORMATS`]; an option both name keeps the driver's spelling. A depth
/// is lossless when it holds at least the converter's bits (`fe.adc_bits`);
/// with the converter's depth unknown, only the deepest is. A link of fewer
/// than two (a driver that names none, or cannot be asked) has one depth, and
/// no list. Whether a depth streams at all is the probe's to find.
pub fn link_depths(dev: &dyn SoapyDevice, fe: &FrontEnd) -> Vec<LinkDepth> {
    let mut options = dev.stream_arg_options(LINK_FORMAT_ARG).unwrap_or_default();
    for extra in fe.link_formats {
        if !options.iter().any(|o| o.eq_ignore_ascii_case(extra)) {
            options.push(extra.to_string());
        }
    }
    // Each with the driver's own spelling of it.
    let named: Vec<(&String, u8)> = LINK_FORMATS
        .into_iter()
        .filter_map(|(o, bits)| {
            let n = options.iter().find(|n| n.eq_ignore_ascii_case(o))?;
            Some((n, bits))
        })
        .collect();
    if named.len() < 2 {
        return Vec::new();
    }
    let deepest = named[0].1;
    named
        .into_iter()
        .map(|(option, bits)| LinkDepth {
            bits,
            lossless: fe.adc_bits.map_or(bits == deepest, |adc| bits >= adc),
            arg: (LINK_FORMAT_ARG.to_string(), option.clone()),
        })
        .collect()
}

/// The device's sample-rate ranges. A UHD device reports them for its
/// current master clock (a B210 opens at 16 MHz), so with `probe` (see
/// [`is_uhd`]) the range is re-read after asking for twice the reported
/// maximum (errors ignored; the driver raises its clock as far as it can),
/// while the maximum grows, at most three rounds. When a doubling does not
/// widen it (UHD refuses a rate above its clock's reach), the known clock
/// ceilings above the current maximum are tried once, highest first, and
/// doubling continues from the first that widens it. The rate in force
/// before is then restored. Without `probe` the ranges are read once. A
/// `master_clock_rate=` device argument still fixes the clock, and with it
/// the range.
pub fn probe_rate_ranges(dev: &dyn SoapyDevice, probe: bool) -> Result<Vec<RangeHz>, String> {
    let mut rates = dev.sample_rate_ranges()?;
    if !probe || rates.is_empty() {
        return Ok(rates);
    }
    let saved = dev.sample_rate().ok().filter(|r| *r > 0.0);
    // The range after asking for `hz`, when it reaches higher than `than`.
    let grown = |hz: f64, than: f64| -> Option<Vec<RangeHz>> {
        let _ = dev.set_sample_rate(hz);
        dev.sample_rate_ranges()
            .ok()
            .filter(|r| !r.is_empty() && max_rate(r) > than)
    };
    let mut ceilings_tried = false;
    for _ in 0..UHD_PROBE_ROUNDS {
        let before = max_rate(&rates);
        if let Some(r) = grown(2.0 * before, before) {
            rates = r;
            continue;
        }
        // UHD refuses a rate its clock cannot reach rather than clamping it:
        // try the known clock ceilings above the current maximum, once.
        if ceilings_tried {
            break;
        }
        ceilings_tried = true;
        match UHD_CLOCK_CEILINGS_HZ
            .iter()
            .filter(|&&c| c > before)
            .find_map(|&c| grown(c, before))
        {
            Some(r) => rates = r,
            None => break,
        }
    }
    if let Some(r) = saved {
        let _ = dev.set_sample_rate(r);
    }
    Ok(rates)
}

/// Asserted capabilities from the device's reported ranges, with `rates`
/// from [`probe_rate_ranges`].
///
/// Min-of-minimums / max-of-maximums overstates coverage on devices whose
/// ranges have gaps; `tune` still validates against the real device.
pub fn capabilities(dev: &dyn SoapyDevice, rates: &[RangeHz]) -> Result<Capabilities, String> {
    let freq = dev.frequency_ranges()?;
    let (_, formats) = dev.formats()?;
    let min = freq.iter().map(|r| r.min_hz).fold(f64::INFINITY, f64::min);
    let max = freq
        .iter()
        .map(|r| r.max_hz)
        .fold(f64::NEG_INFINITY, f64::max);
    let bw = max_rate(rates);
    if freq.is_empty() || rates.is_empty() {
        return Err("device reports no frequency or sample-rate range".into());
    }
    let channels = u16::try_from(dev.rx_channels()?).map_err(|e| e.to_string())?;
    Ok(Capabilities {
        tune_min_hz: Claim::Asserted(Hz(min.round() as u64)),
        tune_max_hz: Claim::Asserted(Hz(max.round() as u64)),
        max_bandwidth_hz: Claim::Asserted(Hz(bw.round() as u64)),
        formats: formats
            .iter()
            .map(|f| match f {
                WireFormat::Cs8 => SampleFormat::Ci8,
                WireFormat::Cs16 => SampleFormat::Ci16,
                WireFormat::Cf32 => SampleFormat::Cf32,
            })
            .collect(),
        channels,
        rx: true,
        tx: false,
        remote: dev.driver_key() == "remote",
        push: false,
        usable_fraction: Claim::Asserted(USABLE_DEFAULT),
        depths: vec![],
        sweep: None,
    })
}

/// Deepest decimation of a UHD ladder.
const UHD_MAX_DECIMATION: u64 = 512;

/// The rates a source asks its device for.
#[derive(Clone, Debug, PartialEq)]
pub enum RateModel {
    /// The device's reported ranges: discrete rates, stepped ranges, or a
    /// continuous range on the 100 kHz quantum.
    Ranges(Vec<RangeHz>),
    /// A UHD device divides its master clock `mcr_hz` by an integer: the
    /// exact divisors `rungs` (ascending), each an integer number of hertz.
    Ladder { mcr_hz: u64, rungs: Vec<u64> },
}

/// The UHD ladder from the probed `rates` (see [`probe_rate_ranges`]): with
/// `mcr` the range's maximum (the master clock) and `min` its smallest
/// minimum, every `mcr / N` for `N` in `1..=min(512, mcr / min)` that divides
/// `mcr` exactly. Odd `N` is allowed. 61.44 MHz = 2^15 * 3 * 5^4, so every
/// B210 rung reduces against 48 kHz to a factor of at most 1 280.
pub fn uhd_ladder(rates: &[RangeHz]) -> RateModel {
    let mcr = max_rate(rates).round().max(0.0) as u64;
    let min = rates
        .iter()
        .map(|r| r.min_hz)
        .fold(f64::INFINITY, f64::min)
        .round()
        .max(1.0) as u64;
    let deepest = UHD_MAX_DECIMATION.min(mcr / min);
    let mut rungs: Vec<u64> = (1..=deepest)
        .filter(|n| mcr.is_multiple_of(*n))
        .map(|n| mcr / n)
        .collect();
    rungs.reverse();
    RateModel::Ladder { mcr_hz: mcr, rungs }
}

/// Smallest supported rate that is at least `bandwidth_hz`: on a ladder the
/// smallest rung (`None` above the clock); otherwise as the ranges allow.
pub fn pick_rate(model: &RateModel, bandwidth_hz: u64) -> Option<u64> {
    match model {
        RateModel::Ranges(ranges) => pick_range_rate(ranges, bandwidth_hz),
        RateModel::Ladder { rungs, .. } => rungs.iter().copied().find(|&r| r >= bandwidth_hz),
    }
}

/// The smallest rate strictly above `actual_hz`: the next rung, or the next
/// supported rate of the ranges (on a continuous range the next 100 kHz
/// quantum). Asked once when a device gives less than the band.
pub fn next_rate_above(model: &RateModel, actual_hz: u64) -> Option<u64> {
    pick_rate(model, actual_hz.checked_add(1)?)
}

fn pick_range_rate(ranges: &[RangeHz], bandwidth_hz: u64) -> Option<u64> {
    let bw = bandwidth_hz as f64;
    ranges
        .iter()
        .filter_map(|r| {
            if r.max_hz < bw {
                return None;
            }
            if r.step_hz > 0.0 {
                let (min, step) = (r.min_hz.round() as u64, r.step_hz.round() as u64);
                if step == 0 {
                    return None;
                }
                let k = bandwidth_hz.saturating_sub(min).div_ceil(step);
                let v = min + k * step;
                (v as f64 <= r.max_hz).then_some(v)
            } else if r.min_hz == r.max_hz {
                Some(r.min_hz.round() as u64)
            } else {
                let want = bandwidth_hz.max(r.min_hz.ceil() as u64);
                let q = want.div_ceil(RATE_QUANTUM_HZ) * RATE_QUANTUM_HZ;
                Some(if q as f64 <= r.max_hz {
                    q
                } else {
                    r.max_hz.round() as u64
                })
            }
        })
        .min()
}

/// Native format when it is one the runtime converts and the device supports;
/// otherwise the first of CF32 > CS16 > CS8 in `supported`. Never returns a
/// format outside `supported`; errors when none of the three is supported.
pub fn pick_format(
    native: Option<WireFormat>,
    supported: &[WireFormat],
) -> Result<WireFormat, String> {
    if let Some(n) = native.filter(|n| supported.contains(n)) {
        return Ok(n);
    }
    [WireFormat::Cf32, WireFormat::Cs16, WireFormat::Cs8]
        .into_iter()
        .find(|f| supported.contains(f))
        .ok_or_else(|| "device supports none of CF32, CS16, CS8".to_string())
}

#[cfg(all(test, feature = "test-mock"))]
mod tests {
    use super::*;
    use crate::mock::MockDevice;

    fn d(v: f64) -> RangeHz {
        RangeHz {
            min_hz: v,
            max_hz: v,
            step_hz: 0.0,
        }
    }

    fn ladder() -> RateModel {
        uhd_ladder(&[RangeHz {
            min_hz: 120_000.0,
            max_hz: 61_440_000.0,
            step_hz: 0.0,
        }])
    }

    fn gcd(a: u64, b: u64) -> u64 {
        if b == 0 { a } else { gcd(b, a % b) }
    }

    #[test]
    fn uhd_ladder_picks_exact_divisors_of_the_master_clock() {
        let m = ladder();
        for (bw, want) in [
            (12_500, Some(120_000)),
            (200_000, Some(204_800)),
            (1_000_000, Some(1_024_000)),
            (2_400_000, Some(2_457_600)),
            (4_000_000, Some(4_096_000)),
            (10_240_000, Some(10_240_000)),
            (16_000_000, Some(20_480_000)),
            (61_440_000, Some(61_440_000)),
            (61_440_001, None),
        ] {
            assert_eq!(pick_rate(&m, bw), want, "{bw}");
        }
        let RateModel::Ladder { mcr_hz, rungs } = &m else {
            panic!("expected a ladder")
        };
        assert_eq!(*mcr_hz, 61_440_000);
        assert_eq!(rungs.first(), Some(&120_000));
        assert_eq!(rungs.last(), Some(&61_440_000));
        assert!(rungs.windows(2).all(|w| w[0] < w[1]), "ascending");
        for &r in rungs {
            assert!(mcr_hz.is_multiple_of(r), "{r} divides the clock");
            let g = gcd(r, 48_000);
            assert!((r / g).max(48_000 / g) <= 1_280, "{r}");
        }
    }

    #[test]
    fn the_mock_reproduces_the_live_rates() {
        let dev = MockDevice::hackrf_like()
            .with_driver("b200")
            .with_uhd_decimation(61.44e6);
        let rate = |hz: f64| {
            dev.set_sample_rate(hz).unwrap();
            dev.sample_rate().unwrap().round() as u64
        };
        assert_eq!(rate(400_000.0), 398_961);
        assert_eq!(rate(1_040_000.0), 1_041_356);
        // The live 260 339 Hz was a channel rate (`channel_rate_hz`), not the device's.
        assert_eq!(1_041_356 / 4, 260_339);
        assert_eq!(rate(4_000_000.0), 4_096_000);
        let rates = dev.sample_rate_ranges().unwrap();
        assert_eq!(
            rates,
            [RangeHz {
                min_hz: 120_000.0,
                max_hz: 61.44e6,
                step_hz: 0.0
            }]
        );
    }

    #[test]
    fn next_rate_above_steps_one_rung_or_quantum() {
        assert_eq!(next_rate_above(&ladder(), 398_961), Some(409_600));
        // N 150 -> 128: no N in 129..150 divides 2^15 * 3 * 5^4.
        assert_eq!(next_rate_above(&ladder(), 409_600), Some(480_000));
        assert_eq!(next_rate_above(&ladder(), 61_440_000), None);
        let ranges = RateModel::Ranges(vec![RangeHz {
            min_hz: 1e6,
            max_hz: 20e6,
            step_hz: 0.0,
        }]);
        assert_eq!(
            next_rate_above(
                &RateModel::Ranges(vec![RangeHz {
                    min_hz: 100e3,
                    max_hz: 20e6,
                    step_hz: 0.0,
                }]),
                398_961
            ),
            Some(400_000)
        );
        assert_eq!(next_rate_above(&ranges, 398_961), Some(1_000_000));
        assert_eq!(next_rate_above(&ranges, 1_000_000), Some(1_100_000));
        assert_eq!(next_rate_above(&ranges, 20_000_000), None);
    }

    #[test]
    fn pick_rate_table() {
        let disc = [d(250e3), d(1.024e6), d(2.048e6), d(2.4e6)];
        assert_eq!(
            pick_rate(&RateModel::Ranges(disc.to_vec()), 1_500_000),
            Some(2_048_000)
        );
        assert_eq!(
            pick_rate(&RateModel::Ranges(disc.to_vec()), 3_000_000),
            None
        );
        let c = [RangeHz {
            min_hz: 1e6,
            max_hz: 20e6,
            step_hz: 0.0,
        }];
        assert_eq!(
            pick_rate(&RateModel::Ranges(c.to_vec()), 12_500),
            Some(1_000_000)
        );
        assert_eq!(
            pick_rate(&RateModel::Ranges(c.to_vec()), 2_345_678),
            Some(2_400_000)
        );
        assert_eq!(
            pick_rate(&RateModel::Ranges(c.to_vec()), 19_950_000),
            Some(20_000_000)
        );
        assert_eq!(pick_rate(&RateModel::Ranges(c.to_vec()), 21_000_000), None);
        let s = [RangeHz {
            min_hz: 1e6,
            max_hz: 1e7,
            step_hz: 5e5,
        }];
        assert_eq!(
            pick_rate(&RateModel::Ranges(s.to_vec()), 1_200_000),
            Some(1_500_000)
        );
    }

    #[test]
    fn hackrf_like_capabilities() {
        let c = {
            let d = MockDevice::hackrf_like();
            capabilities(&d, &d.sample_rate_ranges().unwrap()).unwrap()
        };
        assert_eq!(c.tune_min_hz, Claim::Asserted(Hz(1_000_000)));
        assert_eq!(c.tune_max_hz, Claim::Asserted(Hz(6_000_000_000)));
        assert_eq!(c.max_bandwidth_hz, Claim::Asserted(Hz(20_000_000)));
        assert_eq!(
            c.formats,
            [SampleFormat::Ci8, SampleFormat::Ci16, SampleFormat::Cf32]
        );
        assert_eq!(c.channels, 1);
        assert!(c.rx && !c.tx && !c.remote && !c.push);
        assert_eq!(c.usable_fraction, Claim::Asserted(0.9));
        assert!(c.depths.is_empty());
    }

    #[test]
    fn uhd_rate_range_is_probed_at_the_maximum_clock() {
        // SoapyUHD reports the device type as the driver key.
        let dev = MockDevice::hackrf_like()
            .with_driver("b200")
            .with_rate_follow(16e6, 61.44e6);
        dev.set_sample_rate(8e6).unwrap();
        let rates = probe_rate_ranges(&dev, is_uhd("", &dev.driver_key())).unwrap();
        let c = capabilities(&dev, &rates).unwrap();
        assert_eq!(c.max_bandwidth_hz, Claim::Asserted(Hz(61_440_000)));
        assert_eq!(
            pick_rate(&RateModel::Ranges(rates.clone()), 30_000_000),
            Some(30_000_000)
        );
        // Probed 32 M, then 64 M (clipped to 61.44 M), then 122.88 M: no
        // growth, nor from the ceilings above 61.44 M.
        assert_eq!(dev.range_reads(), 6);
        assert_eq!(
            dev.calls(),
            [
                "set_sample_rate(8000000)",
                "set_sample_rate(32000000)",
                "set_sample_rate(64000000)",
                "set_sample_rate(122880000)",
                "set_sample_rate(250000000)",
                "set_sample_rate(200000000)",
                "set_sample_rate(8000000)"
            ]
        );
        assert_eq!(dev.sample_rate(), Ok(8e6), "rate restored");
    }

    #[test]
    fn uhd_probe_tries_clock_ceilings_when_a_doubling_is_refused() {
        // A B210 refuses 64 M outright: 32 M, then the ceilings reach 61.44 M.
        let dev = MockDevice::hackrf_like()
            .with_driver("b200")
            .with_rate_follow(16e6, 61.44e6)
            .refusing_above_ceiling();
        dev.set_sample_rate(8e6).unwrap();
        let rates = probe_rate_ranges(&dev, is_uhd("", &dev.driver_key())).unwrap();
        let c = capabilities(&dev, &rates).unwrap();
        assert_eq!(c.max_bandwidth_hz, Claim::Asserted(Hz(61_440_000)));
        assert_eq!(
            dev.calls(),
            [
                "set_sample_rate(8000000)",
                "set_sample_rate(32000000)",
                "set_sample_rate(64000000)",
                "set_sample_rate(250000000)",
                "set_sample_rate(200000000)",
                "set_sample_rate(61440000)",
                "set_sample_rate(122880000)",
                "set_sample_rate(8000000)"
            ]
        );
        assert_eq!(dev.sample_rate(), Ok(8e6), "rate restored");
    }

    #[test]
    fn non_uhd_rate_range_is_read_once() {
        let dev = MockDevice::hackrf_like().with_rate_follow(16e6, 61.44e6);
        let rates = probe_rate_ranges(&dev, is_uhd("driver=hackrf", &dev.driver_key())).unwrap();
        let c = capabilities(&dev, &rates).unwrap();
        assert_eq!(c.max_bandwidth_hz, Claim::Asserted(Hz(16_000_000)));
        assert_eq!(dev.range_reads(), 1);
        assert!(dev.calls().is_empty(), "{:?}", dev.calls());
    }

    #[test]
    fn uhd_gate_matches_args_and_real_driver_keys() {
        for key in [
            "uhd", "b200", "B200", "b100", "x300", "n3xx", "e3xx", "usrp2",
        ] {
            assert!(is_uhd("", key), "{key}");
        }
        assert!(is_uhd("driver=uhd,type=b200", "something"));
        assert!(is_uhd("serial=1, driver = UHD", ""));
        assert!(!is_uhd("driver=hackrf", "hackrf"));
        assert!(!is_uhd("driver=rtlsdr", "RTLSDR"));
        assert!(!is_uhd("driver=remote,remote:driver=uhd", "remote"));
    }

    #[test]
    fn the_b2xx_family_settles_in_1_ms_and_the_rest_in_the_default() {
        let b2xx = FrontEnd {
            settle: Duration::from_millis(1),
            adc_bits: Some(12),
            link_formats: &["sc12"],
        };
        // SoapyUHD names the device type as its driver key and the
        // motherboard as its hardware key.
        assert_eq!(front_end("b200", "B210"), b2xx);
        assert_eq!(front_end("uhd", "B200mini"), b2xx);
        assert_eq!(front_end("b200", "b205mini"), b2xx);
        let default = FrontEnd {
            settle: SETTLE_DEFAULT,
            adc_bits: None,
            link_formats: &[],
        };
        assert_eq!(SETTLE_DEFAULT, Duration::from_millis(10));
        assert_eq!(front_end("x300", "X310"), default);
        assert_eq!(front_end("uhd", ""), default);
        assert_eq!(front_end("hackrf", "B2"), default, "a B2 not on UHD");
        assert_eq!(front_end("hackrf", "HackRF One"), default);
        assert_eq!(front_end("", ""), default);
    }

    #[test]
    fn remote_driver_is_remote() {
        let c = {
            let d = MockDevice::hackrf_like().with_driver("remote");
            capabilities(&d, &d.sample_rate_ranges().unwrap()).unwrap()
        };
        assert!(c.remote);
    }

    #[test]
    fn pick_format_native_when_supported() {
        let all = [WireFormat::Cs8, WireFormat::Cs16, WireFormat::Cf32];
        assert_eq!(
            pick_format(Some(WireFormat::Cs8), &all),
            Ok(WireFormat::Cs8)
        );
        assert_eq!(pick_format(None, &all), Ok(WireFormat::Cf32));
        assert_eq!(
            pick_format(Some(WireFormat::Cf32), &[WireFormat::Cs16]),
            Ok(WireFormat::Cs16)
        );
    }

    #[test]
    fn pick_format_falls_back_within_supported() {
        assert_eq!(pick_format(None, &[WireFormat::Cs8]), Ok(WireFormat::Cs8));
        assert_eq!(
            pick_format(None, &[WireFormat::Cs8, WireFormat::Cs16]),
            Ok(WireFormat::Cs16)
        );
    }

    #[test]
    fn pick_format_unsupported_is_error() {
        assert!(pick_format(None, &[]).is_err());
        assert!(pick_format(Some(WireFormat::Cs8), &[]).is_err());
    }

    #[test]
    fn pick_rate_edges() {
        let unsorted = [d(2.4e6), d(250e3), d(2.048e6)];
        assert_eq!(
            pick_rate(&RateModel::Ranges(unsorted.to_vec()), 300_000),
            Some(2_048_000)
        );
        assert_eq!(
            pick_rate(&RateModel::Ranges(unsorted.to_vec()), 2_048_000),
            Some(2_048_000)
        );
        let c = [RangeHz {
            min_hz: 1e6,
            max_hz: 20e6,
            step_hz: 0.0,
        }];
        assert_eq!(
            pick_rate(&RateModel::Ranges(c.to_vec()), 500_000),
            Some(1_000_000)
        );
        assert_eq!(
            pick_rate(&RateModel::Ranges(c.to_vec()), 2_000_000),
            Some(2_000_000)
        );
        let s = [RangeHz {
            min_hz: 1e6,
            max_hz: 1e7,
            step_hz: 5e5,
        }];
        assert_eq!(
            pick_rate(&RateModel::Ranges(s.to_vec()), 500_000),
            Some(1_000_000)
        );
        assert_eq!(
            pick_rate(&RateModel::Ranges(s.to_vec()), 1_500_000),
            Some(1_500_000)
        );
        assert_eq!(
            pick_rate(&RateModel::Ranges(s.to_vec()), 1_500_001),
            Some(2_000_000)
        );
        assert_eq!(pick_rate(&RateModel::Ranges(s.to_vec()), 10_000_001), None);
    }
}
