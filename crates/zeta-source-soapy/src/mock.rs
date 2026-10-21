//! Scriptable in-memory device for tests (feature `test-mock`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use num_complex::Complex;

use crate::device::{RangeHz, ReadError, SoapyDevice, SoapyRx, WireFormat};

#[derive(Clone, Debug, PartialEq)]
pub enum MockRead {
    Samples(usize),
    Timeout,
    Overflow,
    Fatal(String),
}

pub struct MockDevice {
    driver: String,
    freq: Vec<RangeHz>,
    rates: Vec<RangeHz>,
    native: Option<WireFormat>,
    formats: Vec<WireFormat>,
    channels: usize,
    reads: Arc<Mutex<VecDeque<MockRead>>>,
    calls: Arc<Mutex<Vec<String>>>,
    rate: Mutex<f64>,
    fails: Arc<Mutex<Vec<(String, String)>>>,
    delays: Mutex<Vec<(String, u64)>>,
    /// `(floor, ceiling)`: the reported rate range's maximum follows the last
    /// set rate within these bounds, as a UHD device's follows its master clock.
    follow: Option<(f64, f64)>,
    /// With `follow`: refuse a rate above the ceiling instead of clamping it.
    refuse_above: bool,
    /// A UHD master clock: rates are the clock over an integer.
    decimation: Option<f64>,
    /// `(fraction, reads)`: the next `reads` answers of `sample_rate` come
    /// back `fraction` short of the rate set.
    shortfall: Mutex<(f64, usize)>,
    range_reads: Mutex<usize>,
}

impl MockDevice {
    pub fn hackrf_like() -> Self {
        Self {
            driver: "hackrf".into(),
            freq: vec![RangeHz {
                min_hz: 1e6,
                max_hz: 6e9,
                step_hz: 0.0,
            }],
            rates: vec![RangeHz {
                min_hz: 1e6,
                max_hz: 20e6,
                step_hz: 0.0,
            }],
            native: Some(WireFormat::Cs8),
            formats: vec![WireFormat::Cs8, WireFormat::Cs16, WireFormat::Cf32],
            channels: 1,
            reads: Arc::default(),
            calls: Arc::default(),
            rate: Mutex::new(0.0),
            fails: Arc::default(),
            delays: Mutex::default(),
            follow: None,
            refuse_above: false,
            decimation: None,
            shortfall: Mutex::new((0.0, 0)),
            range_reads: Mutex::new(0),
        }
    }

    /// Report a single continuous rate range whose maximum is the last set
    /// rate clamped to `floor..=ceiling` (`floor` before any rate is set).
    pub fn with_rate_follow(mut self, floor: f64, ceiling: f64) -> Self {
        self.follow = Some((floor, ceiling));
        self
    }

    /// With [`MockDevice::with_rate_follow`]: `set_sample_rate` above the
    /// ceiling fails and leaves the rate unchanged (as UHD refuses a rate its
    /// clock cannot reach) instead of being clamped.
    pub fn refusing_above_ceiling(mut self) -> Self {
        self.refuse_above = true;
        self
    }

    /// A UHD device at master clock `mcr_hz`: `set_sample_rate(r)` sets
    /// `mcr / round(mcr / r)` (at least one), as UHD divides its clock by an
    /// integer, and the range reads `[mcr / 512, mcr]`, continuous.
    pub fn with_uhd_decimation(mut self, mcr_hz: f64) -> Self {
        self.decimation = Some(mcr_hz);
        self
    }

    /// Every read of `sample_rate` answers `fraction` short of the rate set,
    /// as a device that rounds down.
    pub fn with_rate_shortfall(self, fraction: f64) -> Self {
        *self.shortfall.lock().unwrap() = (fraction, usize::MAX);
        self
    }

    /// The next `reads` answers of `sample_rate` come back `fraction` short,
    /// on a device already shared.
    pub fn shortfall_next(&self, fraction: f64, reads: usize) {
        *self.shortfall.lock().unwrap() = (fraction, reads);
    }

    /// Calls of `sample_rate_ranges` so far.
    pub fn range_reads(&self) -> usize {
        *self.range_reads.lock().unwrap()
    }

    pub fn with_driver(mut self, d: &str) -> Self {
        self.driver = d.into();
        self
    }

    pub fn with_rates(mut self, r: Vec<RangeHz>) -> Self {
        self.rates = r;
        self
    }

    pub fn with_reads(self, r: Vec<MockRead>) -> Self {
        *self.reads.lock().unwrap() = r.into();
        self
    }

    /// Append steps to the read script (shared with any open stream).
    pub fn push_reads(&self, r: Vec<MockRead>) {
        self.reads.lock().unwrap().extend(r);
    }

    /// Make the named call (`set_frequency`, `set_sample_rate`, `open_rx`,
    /// `activate`) fail once with `msg`; calls are still recorded.
    pub fn fail_on(self, op: &str, msg: &str) -> Self {
        self.fails.lock().unwrap().push((op.into(), msg.into()));
        self
    }

    /// As [`MockDevice::fail_on`], on a device already shared.
    pub fn fail_next(&self, op: &str, msg: &str) {
        self.fails.lock().unwrap().push((op.into(), msg.into()));
    }

    /// Make the next named control call (`set_frequency`, `set_sample_rate`)
    /// block for `ms` before it completes.
    pub fn delay_next(&self, op: &str, ms: u64) {
        self.delays.lock().unwrap().push((op.into(), ms));
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn rec(&self, s: String) {
        self.calls.lock().unwrap().push(s);
    }

    fn check(&self, op: &str) -> Result<(), String> {
        let ms = {
            let mut d = self.delays.lock().unwrap();
            d.iter().position(|(o, _)| o == op).map(|i| d.remove(i).1)
        };
        if let Some(ms) = ms {
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
        take_fail(&self.fails, op)
    }
}

impl SoapyDevice for MockDevice {
    fn driver_key(&self) -> String {
        self.driver.clone()
    }
    fn frequency_ranges(&self) -> Result<Vec<RangeHz>, String> {
        Ok(self.freq.clone())
    }
    fn sample_rate_ranges(&self) -> Result<Vec<RangeHz>, String> {
        *self.range_reads.lock().unwrap() += 1;
        if let Some(mcr) = self.decimation {
            return Ok(vec![RangeHz {
                min_hz: mcr / 512.0,
                max_hz: mcr,
                step_hz: 0.0,
            }]);
        }
        if let Some((floor, ceiling)) = self.follow {
            let max = self.rate.lock().unwrap().clamp(floor, ceiling);
            return Ok(vec![RangeHz {
                min_hz: self.rates.first().map_or(1e6, |r| r.min_hz),
                max_hz: max,
                step_hz: 0.0,
            }]);
        }
        Ok(self.rates.clone())
    }
    fn formats(&self) -> Result<(Option<WireFormat>, Vec<WireFormat>), String> {
        Ok((self.native, self.formats.clone()))
    }
    fn rx_channels(&self) -> Result<usize, String> {
        Ok(self.channels)
    }
    fn set_frequency(&self, hz: f64) -> Result<(), String> {
        self.rec(format!("set_frequency({hz})"));
        self.check("set_frequency")?;
        Ok(())
    }
    fn set_sample_rate(&self, hz: f64) -> Result<(), String> {
        self.rec(format!("set_sample_rate({hz})"));
        self.check("set_sample_rate")?;
        if let Some((_, ceiling)) = self.follow
            && self.refuse_above
            && hz > ceiling
        {
            return Err(format!("rate {hz} above {ceiling}"));
        }
        *self.rate.lock().unwrap() = match self.decimation {
            Some(mcr) => mcr / (mcr / hz).round().max(1.0),
            None => hz,
        };
        Ok(())
    }
    fn sample_rate(&self) -> Result<f64, String> {
        let rate = *self.rate.lock().unwrap();
        let mut short = self.shortfall.lock().unwrap();
        if short.1 == 0 {
            return Ok(rate);
        }
        if short.1 != usize::MAX {
            short.1 -= 1;
        }
        Ok(rate * (1.0 - short.0))
    }
    fn open_rx(&self, format: WireFormat) -> Result<Box<dyn SoapyRx>, String> {
        self.rec(format!("open_rx({format:?})"));
        self.check("open_rx")?;
        Ok(Box::new(MockRx {
            reads: self.reads.clone(),
            calls: self.calls.clone(),
            fails: self.fails.clone(),
            next: 0,
        }))
    }
}

fn take_fail(f: &Mutex<Vec<(String, String)>>, op: &str) -> Result<(), String> {
    let mut f = f.lock().unwrap();
    match f.iter().position(|(o, _)| o == op) {
        Some(i) => Err(f.remove(i).1),
        None => Ok(()),
    }
}

struct MockRx {
    reads: Arc<Mutex<VecDeque<MockRead>>>,
    calls: Arc<Mutex<Vec<String>>>,
    fails: Arc<Mutex<Vec<(String, String)>>>,
    next: u64,
}

impl SoapyRx for MockRx {
    fn activate(&mut self) -> Result<(), String> {
        self.calls.lock().unwrap().push("activate".into());
        take_fail(&self.fails, "activate")?;
        Ok(())
    }
    fn deactivate(&mut self) -> Result<(), String> {
        self.calls.lock().unwrap().push("deactivate".into());
        Ok(())
    }
    /// An empty script reads as `Timeout` forever. A `Timeout` step blocks for
    /// `timeout_us`, as a real device does.
    fn read(&mut self, out: &mut [Complex<f32>], timeout_us: i64) -> Result<usize, ReadError> {
        let step = self
            .reads
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(MockRead::Timeout);
        match step {
            MockRead::Samples(n) => {
                let n = n.min(out.len());
                for o in &mut out[..n] {
                    let v = self.next as f32;
                    *o = Complex::new(v, -v);
                    self.next += 1;
                }
                Ok(n)
            }
            MockRead::Timeout => {
                let us = u64::try_from(timeout_us).unwrap_or(0);
                std::thread::sleep(std::time::Duration::from_micros(us));
                Err(ReadError::Timeout)
            }
            MockRead::Overflow => Err(ReadError::Overflow),
            MockRead::Fatal(m) => Err(ReadError::Fatal(m)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripted_reads_and_calls() {
        let d = MockDevice::hackrf_like().with_reads(vec![
            MockRead::Samples(3),
            MockRead::Overflow,
            MockRead::Fatal("x".into()),
        ]);
        d.set_frequency(1e8).unwrap();
        d.set_sample_rate(2e6).unwrap();
        let mut rx = d.open_rx(WireFormat::Cs8).unwrap();
        rx.activate().unwrap();
        let mut b = [Complex::new(0.0, 0.0); 8];
        assert_eq!(rx.read(&mut b, 1000), Ok(3));
        assert_eq!(b[2], Complex::new(2.0, -2.0));
        assert_eq!(rx.read(&mut b, 1000), Err(ReadError::Overflow));
        assert_eq!(rx.read(&mut b, 1000), Err(ReadError::Fatal("x".into())));
        assert_eq!(rx.read(&mut b, 1000), Err(ReadError::Timeout));
        rx.deactivate().unwrap();
        assert_eq!(
            d.calls(),
            [
                "set_frequency(100000000)",
                "set_sample_rate(2000000)",
                "open_rx(Cs8)",
                "activate",
                "deactivate"
            ]
        );
    }

    #[test]
    fn control_calls_can_fail() {
        let d = MockDevice::hackrf_like()
            .fail_on("set_frequency", "f")
            .fail_on("set_sample_rate", "r")
            .fail_on("open_rx", "o");
        assert_eq!(d.set_frequency(1e8), Err("f".into()));
        assert!(d.set_frequency(1e8).is_ok());
        assert_eq!(d.set_sample_rate(2e6), Err("r".into()));
        assert!(d.open_rx(WireFormat::Cs8).is_err());
        let d = MockDevice::hackrf_like().fail_on("activate", "a");
        let mut rx = d.open_rx(WireFormat::Cs8).unwrap();
        assert_eq!(rx.activate(), Err("a".into()));
        assert!(rx.activate().is_ok());
    }
}
