//! Scriptable in-memory device for tests (feature `test-mock`).
//!
//! Reads follow a script ([`MockDevice::with_reads`], [`MockDevice::push_reads`]);
//! an empty script reads as timeouts, or with [`MockDevice::with_paced_reads`]
//! as a real-time stream at the rate set, which [`MockDevice::pace_at`] can make
//! short or overflowing at a given rate (for the rate probe).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use num_complex::Complex;

use crate::device::{RangeHz, ReadError, SoapyDevice, SoapyRx, WireFormat};

#[derive(Clone, Debug, PartialEq)]
pub enum MockRead {
    Samples(usize),
    /// `n` samples after blocking for `ms` (a read that holds the reader).
    Slow(usize, u64),
    Timeout,
    Overflow,
    Fatal(String),
    /// The device vanishes (an unplug): from this read on every read is
    /// `Fatal(msg)` (the rest of the script is not consumed), and every later
    /// device or stream call is recorded by [`MockDevice::calls_after_gone`]
    /// and fails with `device gone`.
    Gone(String),
}

/// The unplug state shared by a device and its streams: the message once
/// gone, and the calls made after.
#[derive(Default)]
struct Gone {
    msg: Mutex<Option<String>>,
    after: Mutex<Vec<String>>,
}

impl Gone {
    /// Record `op` and fail when the device is gone.
    fn touch(&self, op: &str) -> Result<(), String> {
        if self.msg.lock().unwrap().is_some() {
            self.after.lock().unwrap().push(op.into());
            return Err("device gone".into());
        }
        Ok(())
    }
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
    /// `(call, thread name)` of each control call, in order.
    threads: Mutex<Vec<(String, String)>>,
    /// Control calls in progress, and how often one started while another ran.
    in_flight: AtomicUsize,
    overlaps: AtomicUsize,
    rate: Arc<Mutex<f64>>,
    /// With paced reads: `(rate, delivered, overflow_every)` per rate set by
    /// [`MockDevice::pace_at`].
    paced: Option<Arc<Vec<Pace>>>,
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
    /// Whether the device has automatic gain (default true).
    gain_mode_available: bool,
    /// `(db, automatic)`: the gain set and whether automatic gain is on.
    gain: Mutex<(f64, bool)>,
    gone: Arc<Gone>,
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
            threads: Mutex::default(),
            in_flight: AtomicUsize::new(0),
            overlaps: AtomicUsize::new(0),
            rate: Arc::new(Mutex::new(0.0)),
            paced: None,
            fails: Arc::default(),
            delays: Mutex::default(),
            follow: None,
            refuse_above: false,
            decimation: None,
            shortfall: Mutex::new((0.0, 0)),
            range_reads: Mutex::new(0),
            gain_mode_available: true,
            gain: Mutex::new((0.0, false)),
            gone: Arc::default(),
        }
    }

    /// Whether the device has automatic gain (`has_gain_mode`); the default is
    /// true. The gain range is always 0 to 76 dB.
    pub fn with_gain_mode(mut self, available: bool) -> Self {
        self.gain_mode_available = available;
        self
    }

    /// Once the read script is empty, a read is not a timeout but blocks until
    /// its buffer's samples are due at the rate set, then returns them (the
    /// buffer left as it was), so the stream delivers in real time. The pace
    /// restarts with each stream and each rate change. See
    /// [`MockDevice::pace_at`].
    pub fn with_paced_reads(mut self) -> Self {
        self.paced.get_or_insert_with(Arc::default);
        self
    }

    /// With paced reads, while the rate set is `rate_hz`: a read returns
    /// `delivered` of its samples (a device that falls short of its rate), and
    /// every `overflow_every`-th read overflows instead (0: none).
    pub fn pace_at(mut self, rate_hz: f64, delivered: f64, overflow_every: u32) -> Self {
        let paces = self.paced.get_or_insert_with(Arc::default);
        Arc::make_mut(paces).push(Pace {
            rate_hz,
            delivered,
            overflow_every,
            overflow_reads: Vec::new(),
        });
        self
    }

    /// With paced reads, while the rate set is `rate_hz`: the reads numbered
    /// in `reads` (from 1, counted from when the pace last restarted) overflow
    /// at once and the rest deliver in full, so a test places single overflows
    /// in time (a read is a hundredth of a second of samples for rates above
    /// 1.6 MHz). Such an overflow is an event between full reads: it costs no
    /// samples, so the delivered rate stays whole and only the overflow count
    /// says anything.
    pub fn overflow_reads_at(mut self, rate_hz: f64, reads: &[u64]) -> Self {
        let paces = self.paced.get_or_insert_with(Arc::default);
        Arc::make_mut(paces).push(Pace {
            rate_hz,
            delivered: 1.0,
            overflow_every: 0,
            overflow_reads: reads.to_vec(),
        });
        self
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
    /// `activate`, `set_gain`, `set_gain_mode`) fail once with `msg`; calls are
    /// still recorded.
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

    /// The device and stream calls made after a [`MockRead::Gone`] read; empty
    /// when nothing touched the vanished device (`abandon` and drops excluded).
    pub fn calls_after_gone(&self) -> Vec<String> {
        self.gone.after.lock().unwrap().clone()
    }

    /// `(call, thread name)` of each control call (`set_frequency`,
    /// `set_sample_rate`, `set_gain`, `set_gain_mode`), in order.
    pub fn call_threads(&self) -> Vec<(String, String)> {
        self.threads.lock().unwrap().clone()
    }

    /// How many control calls started while another was still running.
    pub fn overlaps(&self) -> usize {
        self.overlaps.load(Ordering::SeqCst)
    }

    fn rec(&self, s: String) {
        self.calls.lock().unwrap().push(s);
    }

    /// Record a control call and run `f` as its body, counting overlaps.
    fn control<T>(&self, s: String, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        self.gone.touch(&s)?;
        let name = std::thread::current().name().unwrap_or("?").to_string();
        self.threads.lock().unwrap().push((s.clone(), name));
        self.rec(s);
        if self.in_flight.fetch_add(1, Ordering::SeqCst) > 0 {
            self.overlaps.fetch_add(1, Ordering::SeqCst);
        }
        let r = f();
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        r
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
        self.gone.touch("frequency_ranges")?;
        Ok(self.freq.clone())
    }
    fn sample_rate_ranges(&self) -> Result<Vec<RangeHz>, String> {
        self.gone.touch("sample_rate_ranges")?;
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
        self.gone.touch("formats")?;
        Ok((self.native, self.formats.clone()))
    }
    fn rx_channels(&self) -> Result<usize, String> {
        self.gone.touch("rx_channels")?;
        Ok(self.channels)
    }
    fn set_frequency(&self, hz: f64) -> Result<(), String> {
        self.control(format!("set_frequency({hz})"), || {
            self.check("set_frequency")
        })
    }
    fn set_sample_rate(&self, hz: f64) -> Result<(), String> {
        self.control(format!("set_sample_rate({hz})"), || {
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
        })
    }
    fn sample_rate(&self) -> Result<f64, String> {
        self.gone.touch("sample_rate")?;
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
        self.gone.touch("open_rx")?;
        self.rec(format!("open_rx({format:?})"));
        self.check("open_rx")?;
        Ok(Box::new(MockRx {
            reads: self.reads.clone(),
            calls: self.calls.clone(),
            fails: self.fails.clone(),
            gone: self.gone.clone(),
            next: 0,
            paced: self.paced.clone().map(|paces| Paced {
                rate: self.rate.clone(),
                paces,
                clock: None,
            }),
        }))
    }
    fn gain_range(&self) -> Result<(f64, f64), String> {
        self.gone.touch("gain_range")?;
        Ok((0.0, 76.0))
    }
    fn gain(&self) -> Result<f64, String> {
        self.gone.touch("gain")?;
        Ok(self.gain.lock().unwrap().0)
    }
    fn set_gain(&self, db: f64) -> Result<(), String> {
        self.control(format!("set_gain({db})"), || {
            self.check("set_gain")?;
            self.gain.lock().unwrap().0 = db;
            Ok(())
        })
    }
    fn has_gain_mode(&self) -> Result<bool, String> {
        self.gone.touch("has_gain_mode")?;
        Ok(self.gain_mode_available)
    }
    fn gain_mode(&self) -> Result<bool, String> {
        self.gone.touch("gain_mode")?;
        Ok(self.gain.lock().unwrap().1)
    }
    fn set_gain_mode(&self, automatic: bool) -> Result<(), String> {
        self.control(format!("set_gain_mode({automatic})"), || {
            self.check("set_gain_mode")?;
            if !self.gain_mode_available {
                return Err("no automatic gain".into());
            }
            self.gain.lock().unwrap().1 = automatic;
            Ok(())
        })
    }
}

fn take_fail(f: &Mutex<Vec<(String, String)>>, op: &str) -> Result<(), String> {
    let mut f = f.lock().unwrap();
    match f.iter().position(|(o, _)| o == op) {
        Some(i) => Err(f.remove(i).1),
        None => Ok(()),
    }
}

/// How a paced stream behaves at one rate ([`MockDevice::pace_at`]).
#[derive(Clone, Debug)]
struct Pace {
    rate_hz: f64,
    delivered: f64,
    overflow_every: u32,
    overflow_reads: Vec<u64>,
}

/// A stream's pacing: the device's rate, the per-rate behaviour, and
/// `(rate, start, samples due, reads)` since the pace last restarted.
struct Paced {
    rate: Arc<Mutex<f64>>,
    paces: Arc<Vec<Pace>>,
    clock: Option<(f64, Instant, u64, u64)>,
}

impl Paced {
    /// A read of `len` samples at the rate set: block until they are due, then
    /// the samples it returns, or an overflow. `None` before any rate is set.
    fn read(&mut self, len: usize) -> Option<Result<usize, ReadError>> {
        let rate = *self.rate.lock().unwrap();
        if rate <= 0.0 {
            return None;
        }
        let pace = self.paces.iter().find(|p| (p.rate_hz - rate).abs() < 0.5);
        let (delivered, every) = pace.map_or((1.0, 0), |p| (p.delivered, p.overflow_every));
        let clock = match &mut self.clock {
            Some(c) if c.0 == rate => c,
            c => c.insert((rate, Instant::now(), 0, 0)),
        };
        clock.3 += 1;
        if pace.is_some_and(|p| p.overflow_reads.contains(&clock.3)) {
            return Some(Err(ReadError::Overflow));
        }
        clock.2 += len as u64;
        let due = clock.1 + Duration::from_secs_f64(clock.2 as f64 / rate);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        if every > 0 && clock.3.is_multiple_of(u64::from(every)) {
            return Some(Err(ReadError::Overflow));
        }
        Some(Ok((len as f64 * delivered).round() as usize))
    }
}

struct MockRx {
    reads: Arc<Mutex<VecDeque<MockRead>>>,
    calls: Arc<Mutex<Vec<String>>>,
    fails: Arc<Mutex<Vec<(String, String)>>>,
    gone: Arc<Gone>,
    next: u64,
    paced: Option<Paced>,
}

impl Drop for MockRx {
    fn drop(&mut self) {
        self.calls.lock().unwrap().push("drop".into());
        // Dropping a stream of a vanished device is a native call on it.
        let _ = self.gone.touch("drop");
    }
}

impl SoapyRx for MockRx {
    fn activate(&mut self) -> Result<(), String> {
        self.calls.lock().unwrap().push("activate".into());
        self.gone.touch("activate")?;
        take_fail(&self.fails, "activate")?;
        Ok(())
    }
    fn deactivate(&mut self) -> Result<(), String> {
        self.calls.lock().unwrap().push("deactivate".into());
        self.gone.touch("deactivate")?;
        Ok(())
    }
    fn abandon(self: Box<Self>) {
        self.calls.lock().unwrap().push("abandon".into());
        // As the driver: leak the stream, so its drop never runs.
        std::mem::forget(self);
    }
    /// An empty script reads as `Timeout` forever (paced samples with
    /// [`MockDevice::with_paced_reads`]). A `Timeout` step blocks for
    /// `timeout_us`, as a real device does.
    fn read(&mut self, out: &mut [Complex<f32>], timeout_us: i64) -> Result<usize, ReadError> {
        if let Some(m) = self.gone.msg.lock().unwrap().clone() {
            self.gone.after.lock().unwrap().push("read".into());
            return Err(ReadError::Fatal(m));
        }
        let step = self.reads.lock().unwrap().pop_front();
        let step = match (step, &mut self.paced) {
            (Some(step), _) => step,
            (None, Some(p)) => match p.read(out.len()) {
                Some(r) => return r,
                None => MockRead::Timeout,
            },
            (None, None) => MockRead::Timeout,
        };
        match step {
            MockRead::Slow(n, ms) => {
                std::thread::sleep(std::time::Duration::from_millis(ms));
                self.reads.lock().unwrap().push_front(MockRead::Samples(n));
                self.read(out, timeout_us)
            }
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
            MockRead::Gone(m) => {
                *self.gone.msg.lock().unwrap() = Some(m.clone());
                Err(ReadError::Fatal(m))
            }
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
