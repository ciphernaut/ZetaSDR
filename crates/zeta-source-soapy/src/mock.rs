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
        }
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

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn rec(&self, s: String) {
        self.calls.lock().unwrap().push(s);
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
        Ok(())
    }
    fn set_sample_rate(&self, hz: f64) -> Result<(), String> {
        self.rec(format!("set_sample_rate({hz})"));
        *self.rate.lock().unwrap() = hz;
        Ok(())
    }
    fn sample_rate(&self) -> Result<f64, String> {
        Ok(*self.rate.lock().unwrap())
    }
    fn open_rx(&self, format: WireFormat) -> Result<Box<dyn SoapyRx>, String> {
        self.rec(format!("open_rx({format:?})"));
        Ok(Box::new(MockRx {
            reads: self.reads.clone(),
            calls: self.calls.clone(),
            next: 0,
        }))
    }
}

struct MockRx {
    reads: Arc<Mutex<VecDeque<MockRead>>>,
    calls: Arc<Mutex<Vec<String>>>,
    next: u64,
}

impl SoapyRx for MockRx {
    fn activate(&mut self) -> Result<(), String> {
        self.calls.lock().unwrap().push("activate".into());
        Ok(())
    }
    fn deactivate(&mut self) -> Result<(), String> {
        self.calls.lock().unwrap().push("deactivate".into());
        Ok(())
    }
    fn read(&mut self, out: &mut [Complex<f32>], _timeout_us: i64) -> Result<usize, ReadError> {
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
            MockRead::Timeout => Err(ReadError::Timeout),
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
}
