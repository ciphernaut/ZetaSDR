//! Asserted capabilities and the sample-rate / format choice.

use zeta_core::types::{Capabilities, Claim, Hz, SampleFormat};

use crate::device::{RangeHz, SoapyDevice, WireFormat};

const RATE_QUANTUM_HZ: u64 = 100_000;

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
                let k = ((bw - r.min_hz) / r.step_hz).ceil().max(0.0);
                let v = r.min_hz + k * r.step_hz;
                (v <= r.max_hz).then(|| v.round() as u64)
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

/// Native format when it is one the runtime converts, else cf32.
pub fn pick_format(native: Option<WireFormat>, supported: &[WireFormat]) -> WireFormat {
    match native {
        Some(n) if supported.contains(&n) => n,
        _ => WireFormat::Cf32,
    }
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
    fn pick_format_prefers_native() {
        let all = [WireFormat::Cs8, WireFormat::Cs16, WireFormat::Cf32];
        assert_eq!(pick_format(Some(WireFormat::Cs8), &all), WireFormat::Cs8);
        assert_eq!(pick_format(None, &all), WireFormat::Cf32);
    }
}
