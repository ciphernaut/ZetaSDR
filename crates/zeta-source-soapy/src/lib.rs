//! SoapySDR hardware source: device seam, capabilities and rate choice.

pub mod caps;
pub mod convert;
pub mod device;
#[cfg(feature = "test-mock")]
pub mod mock;
