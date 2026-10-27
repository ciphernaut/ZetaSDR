//! The seam between the port and a SoapySDR device (real or mock).

use num_complex::Complex;

/// A frequency or rate range in Hz as reported by a driver.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RangeHz {
    pub min_hz: f64,
    pub max_hz: f64,
    pub step_hz: f64,
}

/// A wire sample format the runtime can convert to cf32.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireFormat {
    Cs8,
    Cs16,
    Cf32,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    #[error("read timed out")]
    Timeout,
    #[error("overflow")]
    Overflow,
    #[error("fatal: {0}")]
    Fatal(String),
}

pub trait SoapyDevice: Send + Sync {
    fn driver_key(&self) -> String;
    fn frequency_ranges(&self) -> Result<Vec<RangeHz>, String>;
    fn sample_rate_ranges(&self) -> Result<Vec<RangeHz>, String>;
    /// Native format, and supported formats intersected with {CS8, CS16, CF32}.
    fn formats(&self) -> Result<(Option<WireFormat>, Vec<WireFormat>), String>;
    fn rx_channels(&self) -> Result<usize, String>;
    fn set_frequency(&self, hz: f64) -> Result<(), String>;
    fn set_sample_rate(&self, hz: f64) -> Result<(), String>;
    fn sample_rate(&self) -> Result<f64, String>;
    fn open_rx(&self, format: WireFormat) -> Result<Box<dyn SoapyRx>, String>;
    /// The overall receive gain's range in dB, `(min, max)`.
    fn gain_range(&self) -> Result<(f64, f64), String>;
    /// The overall receive gain in dB.
    fn gain(&self) -> Result<f64, String>;
    /// Set the overall receive gain in dB.
    fn set_gain(&self, db: f64) -> Result<(), String>;
    /// Whether the driver has automatic gain.
    fn has_gain_mode(&self) -> Result<bool, String>;
    /// Whether automatic gain is on.
    fn gain_mode(&self) -> Result<bool, String>;
    /// Turn automatic gain on or off.
    fn set_gain_mode(&self, automatic: bool) -> Result<(), String>;
}

pub trait SoapyRx: Send {
    fn activate(&mut self) -> Result<(), String>;
    fn deactivate(&mut self) -> Result<(), String>;
    /// End a stream of a device that is gone (unplugged): make no further
    /// native call on it, neither deactivate nor close. The handle may be
    /// leaked; one per unplug is the accepted cost.
    fn abandon(self: Box<Self>);
    /// Implementations convert to cf32 inside `read`.
    fn read(&mut self, out: &mut [Complex<f32>], timeout_us: i64) -> Result<usize, ReadError>;
}
