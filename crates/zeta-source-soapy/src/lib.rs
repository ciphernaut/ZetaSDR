//! SoapySDR hardware source: device seam, capabilities, rate choice and the
//! [`SoapySource`] port.
//!
//! This crate never links SoapySDR: the real driver adapter lives in the
//! separate `zeta-source-soapy-driver` crate (excluded from the workspace, it
//! needs `libsoapysdr-dev`). The receive gain is the driver's default unless the
//! device args carry `gain=DB` ([`split_gain`], applied at open by
//! [`SoapySource::set_gain_now`]) or `source_set_gain` sets it
//! ([`zeta_core::port::Port::set_gain`]).

pub mod caps;
pub mod convert;
pub mod device;
#[cfg(feature = "test-mock")]
pub mod mock;
pub mod source;

pub use source::SoapySource;

/// Take zeta's own `gain=DB` key out of SoapySDR device `args`: the args to hand
/// the driver, and the overall receive gain in dB to set after opening (`None`:
/// leave the driver's default). A `gain` that is not a finite number is an error.
pub fn split_gain(args: &str) -> Result<(String, Option<f64>), String> {
    let mut gain = None;
    let mut rest = Vec::new();
    for kv in args.split(',').filter(|kv| !kv.is_empty()) {
        match kv.split_once('=') {
            Some((k, v)) if k.trim() == "gain" => {
                let db: f64 = v
                    .trim()
                    .parse()
                    .ok()
                    .filter(|d: &f64| d.is_finite())
                    .ok_or_else(|| format!("gain={v}: not a number of dB"))?;
                gain = Some(db);
            }
            _ => rest.push(kv),
        }
    }
    Ok((rest.join(","), gain))
}

#[cfg(test)]
mod tests {
    use super::split_gain;

    #[test]
    fn gain_is_taken_out_of_the_device_args() {
        assert_eq!(
            split_gain("driver=uhd,serial=31B92DD,gain=40").unwrap(),
            ("driver=uhd,serial=31B92DD".into(), Some(40.0))
        );
        assert_eq!(
            split_gain("gain=12.5,driver=hackrf").unwrap(),
            ("driver=hackrf".into(), Some(12.5))
        );
        assert_eq!(
            split_gain("driver=uhd").unwrap(),
            ("driver=uhd".into(), None)
        );
        assert!(split_gain("driver=uhd,gain=loud").is_err());
        assert!(split_gain("driver=uhd,gain=inf").is_err());
    }
}
