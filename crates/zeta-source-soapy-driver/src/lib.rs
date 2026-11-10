//! The real SoapySDR device behind `zeta-source-soapy`'s device seam.
//!
//! This crate links libSoapySDR and is excluded from the workspace; the port
//! logic (reader thread, timestamps, faults) lives in `zeta-source-soapy` and
//! is tested there against its mock. Only channel 0 of the receive direction
//! is used. The receive gain is the driver's default unless the args carry
//! `gain=DB` (see [`zeta_source_soapy::split_zeta_args`]) or `source_set_gain` sets it.

use std::collections::BTreeMap;

use num_complex::Complex;
use soapysdr::{Args, Device, Direction, ErrorCode, Format, RxStream, StreamSample};
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
}

/// Open the device matching `args` as a [`SoapySource`]. A `gain=DB` key is
/// zeta's own: it is removed before the driver sees the args, and once the source
/// is open it is applied as `source_set_gain` applies a gain
/// ([`SoapySource::set_gain_now`]: automatic gain off, the overall receive gain
/// set to it, a value outside the device's range refused). A `ppm=N` key is
/// zeta's too: [`SoapySource::with_ppm`], and so is `settle_ms=N`
/// ([`SoapySource::with_settle`], in place of the radio's default), and
/// `usable=F` ([`SoapySource::with_usable`], the part of a capture a scan
/// advances by). So are
/// `max_rate_hz=N` and `bits=N`, removed here and read by the worker (the rate
/// cap it reports, the link depth it runs at).
pub fn open(id: SourceId, args: &str) -> Result<SoapySource, PortError> {
    let (args, zeta) = zeta_source_soapy::split_zeta_args(args).map_err(PortError::Io)?;
    let d = Driver::new(&args).map_err(PortError::Io)?;
    let mut src = SoapySource::with_device_args(id.clone(), std::sync::Arc::new(d), &args)?;
    if let Some(ppm) = zeta.ppm {
        src = src.with_ppm(ppm);
    }
    if let Some(ms) = zeta.settle_ms {
        src = src.with_settle(std::time::Duration::from_secs_f64(ms / 1000.0));
    }
    if let Some(usable) = zeta.usable {
        src = src.with_usable(usable);
    }
    if let Some(db) = zeta.gain {
        let g = src.set_gain_now(Some(db)).map_err(|e| {
            let e = match e {
                PortError::Unsupported(m) | PortError::Io(m) => m,
                e => e.to_string(),
            };
            PortError::Io(format!("soapy {}: gain={db}: {e}", id.0))
        })?;
        eprintln!(
            "zeta: gain for {}: {} dB (range {} to {} dB)",
            id.0,
            g.db.unwrap_or(db),
            g.min_db,
            g.max_db
        );
    }
    Ok(src)
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
    fn hardware_key(&self) -> String {
        self.dev.hardware_key().unwrap_or_default()
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
    fn gain_range(&self) -> Result<(f64, f64), String> {
        self.dev
            .gain_range(Direction::Rx, CH)
            .map(|r| (r.minimum, r.maximum))
            .map_err(s)
    }
    fn gain(&self) -> Result<f64, String> {
        self.dev.gain(Direction::Rx, CH).map_err(s)
    }
    fn set_gain(&self, db: f64) -> Result<(), String> {
        self.dev.set_gain(Direction::Rx, CH, db).map_err(s)
    }
    fn has_gain_mode(&self) -> Result<bool, String> {
        self.dev.has_gain_mode(Direction::Rx, CH).map_err(s)
    }
    fn gain_mode(&self) -> Result<bool, String> {
        self.dev.gain_mode(Direction::Rx, CH).map_err(s)
    }
    fn set_gain_mode(&self, automatic: bool) -> Result<(), String> {
        self.dev
            .set_gain_mode(Direction::Rx, CH, automatic)
            .map_err(s)
    }
    /// The options of the receive stream's argument `key`
    /// (`getStreamArgsInfo`; SoapyUHD names the link formats under `WIRE`).
    fn stream_arg_options(&self, key: &str) -> Result<Vec<String>, String> {
        Ok(self
            .dev
            .stream_args_info(Direction::Rx, CH)
            .map_err(s)?
            .into_iter()
            .find(|a| a.key == key)
            .map(|a| a.options.into_iter().map(|(v, _name)| v).collect())
            .unwrap_or_default())
    }
    fn open_rx(
        &self,
        format: WireFormat,
        args: &[(String, String)],
    ) -> Result<Box<dyn SoapyRx>, String> {
        let mut a = Args::new();
        for (k, v) in args {
            a.set(k.as_str(), v.as_str());
        }
        Ok(match format {
            WireFormat::Cs8 => Box::new(Rx::<Complex<i8>>::new(&self.dev, a, cs8_to_cf32)?),
            WireFormat::Cs16 => Box::new(Rx::<Complex<i16>>::new(&self.dev, a, cs16_to_cf32)?),
            WireFormat::Cf32 => Box::new(Rx::<Complex<f32>>::new(&self.dev, a, |x, o| {
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
    fn new(
        dev: &Device,
        args: Args,
        convert: fn(&[E], &mut [Complex<f32>]),
    ) -> Result<Self, String> {
        Ok(Self {
            stream: dev.rx_stream_args::<E, _>(&[CH], args).map_err(s)?,
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
    /// The device is gone: dropping the `RxStream` would call
    /// `deactivateStream` and `closeStream` on a dead handle, which segfaults
    /// in libusb. Leak it instead; one stream and device clone per unplug is
    /// the accepted cost.
    fn abandon(self: Box<Self>) {
        std::mem::forget(self.stream);
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
