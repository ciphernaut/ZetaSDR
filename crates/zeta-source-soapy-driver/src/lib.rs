//! The real SoapySDR device behind `zeta-source-soapy`'s device seam.
//!
//! This crate links libSoapySDR and is excluded from the workspace; the port
//! logic (reader thread, timestamps, faults) lives in `zeta-source-soapy` and
//! is tested there against its mock. Only channel 0 of the receive direction
//! is used. The receive gain is the driver's default unless the args carry
//! `gain=DB` (see [`zeta_source_soapy::split_gain`]).

use std::collections::BTreeMap;

use num_complex::Complex;
use soapysdr::{Device, Direction, ErrorCode, Format, RxStream, StreamSample};
use zeta_core::port::PortError;
use zeta_core::types::SourceId;
use zeta_source_soapy::SoapySource;
use zeta_source_soapy::convert::{cs8_to_cf32, cs16_to_cf32};
use zeta_source_soapy::device::{RangeHz, ReadError, SoapyDevice, SoapyRx, WireFormat};

const CH: usize = 0;

/// An opened SoapySDR device.
pub struct Driver {
    dev: Device,
}

impl Driver {
    /// `soapysdr::Device::new(args)`, e.g. `"driver=hackrf"`.
    pub fn new(args: &str) -> Result<Self, String> {
        Device::new(args)
            .map(|dev| Self { dev })
            .map_err(|e| format!("soapy open {args:?}: {e}"))
    }

    /// Manual gain: automatic gain off (where the driver has it), then the
    /// overall receive gain of channel 0 set to `db`.
    fn set_rx_gain(&self, db: f64) -> Result<(), String> {
        if self.dev.has_gain_mode(Direction::Rx, CH).unwrap_or(false) {
            self.dev
                .set_gain_mode(Direction::Rx, CH, false)
                .map_err(s)?;
        }
        self.dev.set_gain(Direction::Rx, CH, db).map_err(s)
    }
}

/// Open the device matching `args` as a [`SoapySource`]. A `gain=DB` key is
/// zeta's own: it is removed before the driver sees the args, and once the device
/// is open automatic gain is turned off and the overall receive gain set to it.
pub fn open(id: SourceId, args: &str) -> Result<SoapySource, PortError> {
    let (args, gain) = zeta_source_soapy::split_gain(args).map_err(PortError::Io)?;
    let d = Driver::new(&args).map_err(PortError::Io)?;
    if let Some(db) = gain {
        d.set_rx_gain(db)
            .map_err(|e| PortError::Io(format!("soapy {}: gain={db}: {e}", id.0)))?;
        let range = d
            .dev
            .gain_range(Direction::Rx, CH)
            .map(|r| format!(" (range {} to {} dB)", r.minimum, r.maximum))
            .unwrap_or_default();
        eprintln!("zeta: gain for {}: {db} dB{range}", id.0);
    }
    SoapySource::with_device_args(id, std::sync::Arc::new(d), &args)
}

/// Devices matching `filter` (`""` for all): the `SoapySDRUtil --find`
/// equivalent.
pub fn find(filter: &str) -> Result<Vec<BTreeMap<String, String>>, String> {
    let found = soapysdr::enumerate(filter).map_err(|e| e.to_string())?;
    Ok(found
        .iter()
        .map(|a| {
            a.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .collect())
}

fn s(e: soapysdr::Error) -> String {
    e.to_string()
}

fn ranges(r: Vec<soapysdr::Range>) -> Vec<RangeHz> {
    r.into_iter()
        .map(|r| RangeHz {
            min_hz: r.minimum,
            max_hz: r.maximum,
            step_hz: r.step,
        })
        .collect()
}

fn wire(f: Format) -> Option<WireFormat> {
    match f {
        Format::CS8 => Some(WireFormat::Cs8),
        Format::CS16 => Some(WireFormat::Cs16),
        Format::CF32 => Some(WireFormat::Cf32),
        _ => None,
    }
}

impl SoapyDevice for Driver {
    fn driver_key(&self) -> String {
        self.dev.driver_key().unwrap_or_default()
    }
    fn frequency_ranges(&self) -> Result<Vec<RangeHz>, String> {
        self.dev
            .frequency_range(Direction::Rx, CH)
            .map(ranges)
            .map_err(s)
    }
    fn sample_rate_ranges(&self) -> Result<Vec<RangeHz>, String> {
        self.dev
            .get_sample_rate_range(Direction::Rx, CH)
            .map(ranges)
            .map_err(s)
    }
    fn formats(&self) -> Result<(Option<WireFormat>, Vec<WireFormat>), String> {
        let native = self
            .dev
            .native_stream_format(Direction::Rx, CH)
            .ok()
            .and_then(|(f, _fullscale)| wire(f));
        let supported = self
            .dev
            .stream_formats(Direction::Rx, CH)
            .map_err(s)?
            .into_iter()
            .filter_map(wire)
            .collect();
        Ok((native, supported))
    }
    fn rx_channels(&self) -> Result<usize, String> {
        self.dev.num_channels(Direction::Rx).map_err(s)
    }
    fn set_frequency(&self, hz: f64) -> Result<(), String> {
        self.dev.set_frequency(Direction::Rx, CH, hz, ()).map_err(s)
    }
    fn set_sample_rate(&self, hz: f64) -> Result<(), String> {
        self.dev.set_sample_rate(Direction::Rx, CH, hz).map_err(s)
    }
    fn sample_rate(&self) -> Result<f64, String> {
        self.dev.sample_rate(Direction::Rx, CH).map_err(s)
    }
    fn open_rx(&self, format: WireFormat) -> Result<Box<dyn SoapyRx>, String> {
        Ok(match format {
            WireFormat::Cs8 => Box::new(Rx::<Complex<i8>>::new(&self.dev, cs8_to_cf32)?),
            WireFormat::Cs16 => Box::new(Rx::<Complex<i16>>::new(&self.dev, cs16_to_cf32)?),
            WireFormat::Cf32 => Box::new(Rx::<Complex<f32>>::new(&self.dev, |x, o| {
                o.copy_from_slice(x)
            })?),
        })
    }
}

/// A typed receive stream with a scratch buffer in its wire format.
struct Rx<E: StreamSample> {
    stream: RxStream<E>,
    scratch: Vec<E>,
    convert: fn(&[E], &mut [Complex<f32>]),
}

impl<E: StreamSample + Default + Copy + Send> Rx<E> {
    fn new(dev: &Device, convert: fn(&[E], &mut [Complex<f32>])) -> Result<Self, String> {
        Ok(Self {
            stream: dev.rx_stream::<E>(&[CH]).map_err(s)?,
            scratch: Vec::new(),
            convert,
        })
    }
}

impl<E: StreamSample + Default + Copy + Send> SoapyRx for Rx<E> {
    fn activate(&mut self) -> Result<(), String> {
        if self.stream.active() {
            return Ok(());
        }
        self.stream.activate(None).map_err(s)
    }
    fn deactivate(&mut self) -> Result<(), String> {
        if !self.stream.active() {
            return Ok(());
        }
        self.stream.deactivate(None).map_err(s)
    }
    fn read(&mut self, out: &mut [Complex<f32>], timeout_us: i64) -> Result<usize, ReadError> {
        if self.scratch.len() < out.len() {
            self.scratch.resize(out.len(), E::default());
        }
        let buf = &mut self.scratch[..out.len()];
        match self.stream.read(&mut [buf], timeout_us) {
            Ok(n) => {
                (self.convert)(&self.scratch[..n], &mut out[..n]);
                Ok(n)
            }
            Err(e) => Err(match e.code {
                ErrorCode::Timeout => ReadError::Timeout,
                ErrorCode::Overflow => ReadError::Overflow,
                _ => ReadError::Fatal(e.to_string()),
            }),
        }
    }
}
