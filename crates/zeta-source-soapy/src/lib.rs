//! SoapySDR hardware source: device seam, capabilities, rate choice and the
//! [`SoapySource`] port.
//!
//! This crate never links SoapySDR: the real driver adapter lives in the
//! separate `zeta-source-soapy-driver` crate (excluded from the workspace, it
//! needs `libsoapysdr-dev`). Device gains are left at the driver's defaults;
//! a `source.set_gain` operation is future work.

pub mod caps;
pub mod convert;
pub mod device;
#[cfg(feature = "test-mock")]
pub mod mock;
pub mod source;

pub use source::SoapySource;
