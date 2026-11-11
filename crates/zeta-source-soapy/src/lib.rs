//! SoapySDR hardware source: device seam, capabilities, rate choice and the
//! [`SoapySource`] port.
//!
//! This crate never links SoapySDR: the real driver adapter lives in the
//! separate `zeta-source-soapy-driver` crate (excluded from the workspace, it
//! needs `libsoapysdr-dev`). The receive gain is the driver's default unless the
//! device args carry `gain=DB` ([`split_zeta_args`], applied at open by
//! [`SoapySource::set_gain_now`]) or `source_set_gain` sets it
//! ([`zeta_core::port::Port::set_gain`]). At open the worker probes the rate
//! ladder for the fastest rate the device streams cleanly ([`probe`]) unless
//! `max_rate_hz=N` asserts one. A tune delivers only settled samples: the
//! reader drops the tune's backlog and settle, the radio's
//! ([`caps::front_end`]) unless `settle_ms=N` gives one. A scan advances by the
//! capture's usable part, [`caps::USABLE_DEFAULT`] unless `usable=F` gives one.
//! A device whose link carries more than one sample depth ([`caps::link_depths`])
//! has each probed at open ([`probe::probe_depths`]) and runs the narrowest
//! lossless one ([`probe::own_depth`]) unless `bits=N` asks for another.

pub mod attach;
pub mod caps;
pub mod convert;
pub mod device;
#[cfg(feature = "test-mock")]
pub mod mock;
pub mod probe;
pub mod source;

pub use source::SoapySource;

/// zeta's own keys in SoapySDR device args, taken out by [`split_zeta_args`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ZetaArgs {
    /// `gain=DB`: the overall receive gain in dB to set after opening (`None`:
    /// leave the driver's default).
    pub gain: Option<f64>,
    /// `ppm=N`: the device reads `N` parts per million high, so the hardware is
    /// tuned `N` ppm higher than every frequency zeta reports (see
    /// [`SoapySource::with_ppm`]).
    pub ppm: Option<f64>,
    /// `max_rate_hz=N`: the fastest rate the device streams cleanly, asserted
    /// by the operator; the worker reports it as the cap instead of probing
    /// (see [`probe::probe_rate_cap`]).
    pub max_rate_hz: Option<u64>,
    /// `settle_ms=N`: how long a tune takes to settle after the device's
    /// `set_frequency` returns, in ms, in place of the radio's default (see
    /// [`caps::front_end`] and [`SoapySource::with_settle`]).
    pub settle_ms: Option<f64>,
    /// `usable=F`: the part of a capture the front end passes flat, from 0.5
    /// to 1, in place of [`caps::USABLE_DEFAULT`] (see
    /// [`SoapySource::with_usable`]).
    pub usable: Option<f64>,
    /// `bits=N`: the sample depth on the device's link the source runs at, in
    /// place of the narrowest lossless one (see [`probe::own_depth`]); a lossy
    /// depth only this way.
    pub bits: Option<u8>,
}

/// The longest `settle_ms=` accepted (10 s): far past any radio's settle, and
/// a bound that keeps the value convertible to a `Duration`.
pub const SETTLE_MS_MAX: f64 = 10_000.0;

/// The deepest `bits=` accepted.
pub const BITS_MAX: u8 = 32;

/// Take zeta's own `gain=DB`, `ppm=N`, `max_rate_hz=N`, `settle_ms=N`,
/// `usable=F` and `bits=N` keys out of SoapySDR device `args`: the args to
/// hand the driver, and the values to apply after opening. A gain or ppm that
/// is not a finite number, a rate that is not a positive whole number of Hz,
/// a settle that is not a number of ms from 0 to [`SETTLE_MS_MAX`], a usable
/// fraction that is not a number from 0.5 to 1, or a depth that is not a
/// whole number of bits from 1 to [`BITS_MAX`], is an error.
pub fn split_zeta_args(args: &str) -> Result<(String, ZetaArgs), String> {
    let mut z = ZetaArgs::default();
    let mut rest = Vec::new();
    for kv in args.split(',').filter(|kv| !kv.is_empty()) {
        match kv.split_once('=') {
            Some((k, v)) if k.trim() == "max_rate_hz" => {
                let hz = v.trim().parse().ok().filter(|hz: &u64| *hz > 0);
                z.max_rate_hz =
                    Some(hz.ok_or_else(|| format!("max_rate_hz={v}: not a whole number of Hz"))?);
            }
            Some((k, v)) if k.trim() == "settle_ms" => {
                let ms = v.trim().parse().ok();
                let ms = ms.filter(|ms: &f64| (0.0..=SETTLE_MS_MAX).contains(ms));
                z.settle_ms = Some(ms.ok_or_else(|| {
                    format!("settle_ms={v}: not a number of ms from 0 to {SETTLE_MS_MAX}")
                })?);
            }
            Some((k, v)) if k.trim() == "bits" => {
                let b = v.trim().parse().ok();
                let b = b.filter(|b: &u8| (1..=BITS_MAX).contains(b));
                z.bits = Some(b.ok_or_else(|| {
                    format!("bits={v}: not a number of bits from 1 to {BITS_MAX}")
                })?);
            }
            Some((k, v)) if k.trim() == "usable" => {
                let u = v.trim().parse().ok();
                let u = u.filter(|u: &f64| (0.5..=1.0).contains(u));
                z.usable =
                    Some(u.ok_or_else(|| format!("usable={v}: not a fraction from 0.5 to 1"))?);
            }
            Some((k, v)) if matches!(k.trim(), "gain" | "ppm") => {
                let n: Option<f64> = v.trim().parse().ok().filter(|d: &f64| d.is_finite());
                if k.trim() == "gain" {
                    z.gain = Some(n.ok_or_else(|| format!("gain={v}: not a number of dB"))?);
                } else {
                    z.ppm = Some(n.ok_or_else(|| format!("ppm={v}: not a number"))?);
                }
            }
            _ => rest.push(kv),
        }
    }
    Ok((rest.join(","), z))
}

#[cfg(test)]
mod tests {
    use super::{SETTLE_MS_MAX, ZetaArgs, split_zeta_args};

    fn split(args: &str) -> (String, Option<f64>) {
        let (rest, z) = split_zeta_args(args).unwrap();
        (rest, z.gain)
    }

    #[test]
    fn gain_is_taken_out_of_the_device_args() {
        assert_eq!(
            split("driver=uhd,serial=31B92DD,gain=40"),
            ("driver=uhd,serial=31B92DD".into(), Some(40.0))
        );
        assert_eq!(
            split("gain=12.5,driver=hackrf"),
            ("driver=hackrf".into(), Some(12.5))
        );
        assert_eq!(split("driver=uhd"), ("driver=uhd".into(), None));
        assert!(split_zeta_args("driver=uhd,gain=loud").is_err());
        assert!(split_zeta_args("driver=uhd,gain=inf").is_err());
    }

    #[test]
    fn ppm_is_taken_out_of_the_device_args() {
        assert_eq!(
            split_zeta_args("driver=uhd,ppm=3.4,gain=40").unwrap(),
            (
                "driver=uhd".into(),
                ZetaArgs {
                    gain: Some(40.0),
                    ppm: Some(3.4),
                    max_rate_hz: None,
                    settle_ms: None,
                    usable: None,
                    bits: None,
                }
            )
        );
        assert_eq!(
            split_zeta_args("driver=uhd,ppm=fast").unwrap_err(),
            "ppm=fast: not a number"
        );
    }

    #[test]
    fn settle_ms_is_taken_out_of_the_device_args() {
        let (rest, z) = split_zeta_args("driver=uhd,settle_ms=2.5").unwrap();
        assert_eq!((rest.as_str(), z.settle_ms), ("driver=uhd", Some(2.5)));
        assert_eq!(
            split_zeta_args("settle_ms=0").unwrap().1.settle_ms,
            Some(0.0)
        );
        assert_eq!(split_zeta_args("driver=uhd").unwrap().1.settle_ms, None);
        assert_eq!(
            split_zeta_args("driver=uhd,settle_ms=fast").unwrap_err(),
            "settle_ms=fast: not a number of ms from 0 to 10000"
        );
        assert!(split_zeta_args("settle_ms=-1").is_err());
        assert!(split_zeta_args("settle_ms=inf").is_err());
        assert!(split_zeta_args("settle_ms=NaN").is_err());
        // Bounded, so nothing past the parser sees a settle a `Duration`
        // cannot hold.
        assert_eq!(
            split_zeta_args("settle_ms=10000").unwrap().1.settle_ms,
            Some(SETTLE_MS_MAX)
        );
        assert_eq!(
            split_zeta_args("settle_ms=10001").unwrap_err(),
            "settle_ms=10001: not a number of ms from 0 to 10000"
        );
        assert!(split_zeta_args("settle_ms=1e300").is_err());
    }

    #[test]
    fn usable_is_taken_out_of_the_device_args() {
        let (rest, z) = split_zeta_args("driver=uhd,usable=0.85").unwrap();
        assert_eq!((rest.as_str(), z.usable), ("driver=uhd", Some(0.85)));
        assert_eq!(split_zeta_args("usable=0.5").unwrap().1.usable, Some(0.5));
        assert_eq!(split_zeta_args("usable=1").unwrap().1.usable, Some(1.0));
        assert_eq!(split_zeta_args("driver=uhd").unwrap().1.usable, None);
        assert_eq!(
            split_zeta_args("driver=uhd,usable=0.4").unwrap_err(),
            "usable=0.4: not a fraction from 0.5 to 1"
        );
        assert!(split_zeta_args("usable=1.01").is_err());
        assert!(split_zeta_args("usable=NaN").is_err());
        assert!(split_zeta_args("usable=most").is_err());
    }
}
