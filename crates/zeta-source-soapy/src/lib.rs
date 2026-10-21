//! SoapySDR hardware source: device seam, capabilities, rate choice and the
//! [`SoapySource`] port.
//!
//! This crate never links SoapySDR: the real driver adapter lives in the
//! separate `zeta-source-soapy-driver` crate (excluded from the workspace, it
//! needs `libsoapysdr-dev`). The receive gain is the driver's default unless the
//! device args carry `gain=DB` ([`split_zeta_args`], applied at open by
//! [`SoapySource::set_gain_now`]) or `source_set_gain` sets it
//! ([`zeta_core::port::Port::set_gain`]).

pub mod caps;
pub mod convert;
pub mod device;
#[cfg(feature = "test-mock")]
pub mod mock;
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
}

/// Take zeta's own `gain=DB` and `ppm=N` keys out of SoapySDR device `args`: the
/// args to hand the driver, and the values to apply after opening. A value that
/// is not a finite number is an error.
pub fn split_zeta_args(args: &str) -> Result<(String, ZetaArgs), String> {
    let mut z = ZetaArgs::default();
    let mut rest = Vec::new();
    for kv in args.split(',').filter(|kv| !kv.is_empty()) {
        match kv.split_once('=') {
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
    use super::{ZetaArgs, split_zeta_args};

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
                    ppm: Some(3.4)
                }
            )
        );
        assert_eq!(
            split_zeta_args("driver=uhd,ppm=fast").unwrap_err(),
            "ppm=fast: not a number"
        );
    }
}
