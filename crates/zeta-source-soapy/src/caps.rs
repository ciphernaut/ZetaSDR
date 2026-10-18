//! Asserted capabilities and the sample-rate / format choice.

use zeta_core::types::{Capabilities, Claim, Hz, SampleFormat};

use crate::device::{RangeHz, SoapyDevice, WireFormat};

const RATE_QUANTUM_HZ: u64 = 100_000;

/// Asserted capabilities from the device's reported ranges.
///
/// Min-of-minimums / max-of-maximums overstates coverage on devices whose
/// ranges have gaps; `tune` still validates against the real device.
pub fn capabilities(dev: &dyn SoapyDevice) -> Result<Capabilities, String> {
    let freq = dev.frequency_ranges()?;
    let rates = dev.sample_rate_ranges()?;
    let (_, formats) = dev.formats()?;
    let min = freq.iter().map(|r| r.min_hz).fold(f64::INFINITY, f64::min);
    let max = freq
        .iter()
        .map(|r| r.max_hz)
        .fold(f64::NEG_INFINITY, f64::max);
    let bw = rates
        .iter()
        .map(|r| r.max_hz)
        .fold(f64::NEG_INFINITY, f64::max);
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
    })
}

/// Smallest supported rate that is at least `bandwidth_hz`.
pub fn pick_rate(ranges: &[RangeHz], bandwidth_hz: u64) -> Option<u64> {
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

    #[test]
    fn pick_rate_table() {
        let disc = [d(250e3), d(1.024e6), d(2.048e6), d(2.4e6)];
        assert_eq!(pick_rate(&disc, 1_500_000), Some(2_048_000));
        assert_eq!(pick_rate(&disc, 3_000_000), None);
        let c = [RangeHz {
            min_hz: 1e6,
            max_hz: 20e6,
            step_hz: 0.0,
        }];
        assert_eq!(pick_rate(&c, 12_500), Some(1_000_000));
        assert_eq!(pick_rate(&c, 2_345_678), Some(2_400_000));
        assert_eq!(pick_rate(&c, 19_950_000), Some(20_000_000));
        assert_eq!(pick_rate(&c, 21_000_000), None);
        let s = [RangeHz {
            min_hz: 1e6,
            max_hz: 1e7,
            step_hz: 5e5,
        }];
        assert_eq!(pick_rate(&s, 1_200_000), Some(1_500_000));
    }

    #[test]
    fn hackrf_like_capabilities() {
        let c = capabilities(&MockDevice::hackrf_like()).unwrap();
        assert_eq!(c.tune_min_hz, Claim::Asserted(Hz(1_000_000)));
        assert_eq!(c.tune_max_hz, Claim::Asserted(Hz(6_000_000_000)));
        assert_eq!(c.max_bandwidth_hz, Claim::Asserted(Hz(20_000_000)));
        assert_eq!(
            c.formats,
            [SampleFormat::Ci8, SampleFormat::Ci16, SampleFormat::Cf32]
        );
        assert_eq!(c.channels, 1);
        assert!(c.rx && !c.tx && !c.remote && !c.push);
    }

    #[test]
    fn remote_driver_is_remote() {
        let c = capabilities(&MockDevice::hackrf_like().with_driver("remote")).unwrap();
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
        assert_eq!(pick_rate(&unsorted, 300_000), Some(2_048_000));
        assert_eq!(pick_rate(&unsorted, 2_048_000), Some(2_048_000));
        let c = [RangeHz {
            min_hz: 1e6,
            max_hz: 20e6,
            step_hz: 0.0,
        }];
        assert_eq!(pick_rate(&c, 500_000), Some(1_000_000));
        assert_eq!(pick_rate(&c, 2_000_000), Some(2_000_000));
        let s = [RangeHz {
            min_hz: 1e6,
            max_hz: 1e7,
            step_hz: 5e5,
        }];
        assert_eq!(pick_rate(&s, 500_000), Some(1_000_000));
        assert_eq!(pick_rate(&s, 1_500_000), Some(1_500_000));
        assert_eq!(pick_rate(&s, 1_500_001), Some(2_000_000));
        assert_eq!(pick_rate(&s, 10_000_001), None);
    }
}
