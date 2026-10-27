//! The SoapySDR [`Port`]: one receive channel read on a dedicated thread.
//!
//! SoapySDR reads block, so each active stream is owned by a `std::thread`
//! named `soapy-<id>` that reads blocks of `max(16 384, rate / 100)` samples
//! with a 100 ms timeout and sends them on the source's broadcast. Blocks are
//! stamped from the sample count: `t0 = anchor + count · 1e9 / rate` (integer
//! ns), `anchor` being the wall clock when the stream was activated. An
//! overflow (samples lost in the driver), a retune or a reopen anchors at
//! `max(now, end of the last block)`, so `t0` jumps forward and never back.
//!
//! A stream is never ahead of the wall clock (a buffered sample is stamped
//! when it was captured, before it is read), so a block ending more than
//! [`MAX_AHEAD_NS`] after now means the driver hands back buffers faster than
//! the rate (SoapyHackRF replaying a stale buffer after a rate change): the
//! stream is closed and reopened, and reads are discarded until the wall clock
//! has passed the end of the last block sent. More than [`MAX_RESTARTS`]
//! restarts within a minute fault the source. A rate change reopens the stream
//! for the same reason, instead of reactivating it in place.
//!
//! The overall receive gain is read when the source opens and set by
//! [`Port::set_gain`]: through the reader thread between reads while a stream
//! runs (so a gain change and a retune keep their order), on the device
//! directly otherwise.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use num_complex::Complex;
use tokio::sync::{broadcast, oneshot};
use zeta_core::port::{GainInfo, IqBlock, IqReceiver, Port, PortError, PortStats, Tuning};
use zeta_core::types::{Capabilities, Claim, Hz, SourceId, Timestamp};

use crate::caps::{
    RateModel, capabilities, is_uhd, next_rate_above, pick_format, pick_rate, probe_rate_ranges,
    uhd_ladder,
};
use crate::device::{RangeHz, ReadError, SoapyDevice, SoapyRx, WireFormat};

/// Blocks buffered per receiver.
const BROADCAST_CAPACITY: usize = 64;
/// Smallest read, in samples.
const MIN_BLOCK: usize = 16_384;
/// Timeout of one blocking read.
const READ_TIMEOUT_US: i64 = 100_000;
/// Consecutive read timeouts (1 s without samples) that fault the port.
const QUIET_READS: u32 = 10;
/// Default wait of `tune` for the reader thread to apply a retune.
const RETUNE_ACK: Duration = Duration::from_secs(2);
/// Window of [`SoapySource::measured_rate_hz`].
const RATE_WINDOW: Duration = Duration::from_secs(10);
/// How far (ns) a block's end may run ahead of the wall clock before the
/// stream is restarted.
pub const MAX_AHEAD_NS: u64 = 2_000_000_000;
/// Stream restarts within [`RESTART_WINDOW`] the source survives; one more
/// faults it.
pub const MAX_RESTARTS: u32 = 5;
/// Window of [`MAX_RESTARTS`].
const RESTART_WINDOW: Duration = Duration::from_secs(60);

#[derive(Default)]
struct Status {
    fault: Option<String>,
    current: Option<Tuning>,
    /// Requested centre and picked rate of the stream as applied, for the
    /// unchanged-band check; updated by the reader when it applies a retune.
    band: Option<(u64, u64)>,
    /// End of the last block sent (ns); a new stream never anchors before it.
    last_end: u64,
    overflows: u64,
    timeouts: u64,
    /// Streams restarted because their stamps ran ahead of the wall clock.
    restarts: u64,
    /// When each restart within the last [`RESTART_WINDOW`] happened.
    restart_times: VecDeque<Instant>,
    /// `(when, samples)` per block over the last [`RATE_WINDOW`].
    window: VecDeque<(Instant, u64)>,
    /// The overall receive gain as last applied (read at open); `None` for a
    /// device that reports no gain range.
    gain: Option<GainInfo>,
}

/// A command handed to the reader thread, applied between reads.
enum Cmd {
    Retune(Retune),
    /// A checked gain (see [`apply_gain`]), acknowledged with the gain after it
    /// or the failed device call.
    Gain {
        db: Option<f64>,
        ack: oneshot::Sender<Result<GainInfo, String>>,
    },
}

/// The device's gain as read now: `None` when it reports no usable range.
fn read_gain(dev: &dyn SoapyDevice) -> Option<GainInfo> {
    let (min_db, max_db) = dev.gain_range().ok()?;
    if !(min_db.is_finite() && max_db.is_finite() && min_db <= max_db) {
        return None;
    }
    let automatic_available = dev.has_gain_mode().unwrap_or(false);
    let automatic = automatic_available && dev.gain_mode().unwrap_or(false);
    let db = match automatic {
        true => None,
        false => dev.gain().ok(),
    };
    Some(GainInfo {
        db,
        min_db,
        max_db,
        automatic,
        automatic_available,
    })
}

/// Apply a gain [`SoapySource::check_gain`] accepted: `None` turns automatic
/// gain on; a value turns it off (where the device has it) and sets the overall
/// gain, read back. The result is recorded in `status`; after a failed call the
/// device is read again so `status` says what it runs at.
fn apply_gain(
    dev: &dyn SoapyDevice,
    status: &Mutex<Status>,
    db: Option<f64>,
) -> Result<GainInfo, String> {
    let Some(mut info) = status.lock().unwrap().gain else {
        return Err("source has no adjustable gain".into());
    };
    let applied = match db {
        None => dev
            .set_gain_mode(true)
            .map_err(|e| format!("soapy set_gain_mode(true): {e}"))
            .map(|()| {
                info.automatic = true;
                info.db = None;
            }),
        Some(d) => (|| {
            if info.automatic_available {
                dev.set_gain_mode(false)
                    .map_err(|e| format!("soapy set_gain_mode(false): {e}"))?;
            }
            dev.set_gain(d)
                .map_err(|e| format!("soapy set_gain({d}): {e}"))?;
            info.automatic = false;
            info.db = Some(dev.gain().unwrap_or(d));
            Ok(())
        })(),
    };
    let mut st = status.lock().unwrap();
    match applied {
        Ok(()) => {
            st.gain = Some(info);
            Ok(info)
        }
        Err(e) => {
            st.gain = read_gain(dev).or(st.gain);
            Err(e)
        }
    }
}

/// A retune: acknowledged once applied and recorded in [`Status`], or with
/// the reason it was not.
struct Retune {
    center_hz: u64,
    /// The rate picked for `bandwidth_hz`.
    rate_hz: u64,
    /// The band the capture must cover: an actual rate below it is refused.
    bandwidth_hz: u64,
    ack: oneshot::Sender<Result<(), RateError>>,
}

/// Why a rate could not be applied.
#[derive(Debug)]
enum RateError {
    /// A device call failed: the port faults.
    Io(String),
    /// The device gives less than the band: refused, the capture unchanged.
    Narrow(String),
}

/// A rate the device runs at: `asked` is what was set (the picked rate, or
/// the one retried above it), `actual` what the device reads back.
#[derive(Clone, Copy)]
struct Rate {
    asked: u64,
    actual: u64,
}

/// Set `rate_hz` and read the actual rate back. An actual rate below
/// `bandwidth_hz` (a device that rounded down) is asked once more at the next
/// rate above both it and the request (see [`next_rate_above`]): asking
/// below the request cannot help. Still short, it is refused.
fn set_rate(
    dev: &dyn SoapyDevice,
    rates: &RateModel,
    rate_hz: u64,
    bandwidth_hz: u64,
) -> Result<Rate, RateError> {
    let set = |hz: u64| -> Result<Rate, RateError> {
        dev.set_sample_rate(hz as f64)
            .map_err(|e| RateError::Io(format!("soapy set_sample_rate({hz}): {e}")))?;
        let actual = dev
            .sample_rate()
            .map_err(|e| RateError::Io(format!("soapy sample_rate: {e}")))?
            .round() as u64;
        if actual == 0 {
            return Err(RateError::Io(
                "soapy sample_rate: device reports 0 Hz".into(),
            ));
        }
        Ok(Rate { asked: hz, actual })
    };
    let mut got = set(rate_hz)?;
    if got.actual < bandwidth_hz
        && let Some(next) = next_rate_above(rates, got.actual.max(got.asked))
    {
        got = set(next)?;
    }
    if got.actual < bandwidth_hz {
        return Err(RateError::Narrow(format!(
            "soapy: device gives {} Hz for {} Hz, below the {bandwidth_hz} Hz band",
            got.actual, got.asked
        )));
    }
    Ok(got)
}

/// A running reader thread; dropping it asks the thread to stop (it exits
/// after the read in progress, at most 100 ms, and deactivates the stream).
struct Reader {
    cmds: std_mpsc::Sender<Cmd>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Reader {
    fn alive(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| !h.is_finished())
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A SoapySDR device's first receive channel as a push [`Port`]. Nothing
/// touches the device until the first `tune`; dropping the source stops its
/// reader thread. The broadcast lives as long as the source, so receivers
/// taken before a fault (the node's ring feeder) resume after a retune; a
/// reader's end is reported through [`Port::fault`].
pub struct SoapySource {
    id: SourceId,
    caps: Capabilities,
    dev: Arc<dyn SoapyDevice>,
    freq: Vec<RangeHz>,
    rates: Arc<RateModel>,
    format: WireFormat,
    tx: broadcast::Sender<IqBlock>,
    status: Arc<Mutex<Status>>,
    reader: Mutex<Option<Reader>>,
    retune_timeout: Duration,
    ppm: f64,
    /// Serialises `tune`, which may wait on the reader thread.
    gate: tokio::sync::Mutex<()>,
}

impl SoapySource {
    /// Query the device's ranges and formats once; no streaming yet. As
    /// [`SoapySource::with_device_args`] with no device arguments.
    pub fn with_device(id: SourceId, dev: Arc<dyn SoapyDevice>) -> Result<Self, PortError> {
        Self::with_device_args(id, dev, "")
    }

    /// Query the device's ranges and formats once; no streaming yet. `args`
    /// (the SoapySDR device arguments it was opened with) and its driver key
    /// decide whether it is a UHD device, whose rate range is probed at its
    /// maximum clock (see [`probe_rate_ranges`]) and whose rates are exact
    /// divisors of that clock (see [`uhd_ladder`]).
    pub fn with_device_args(
        id: SourceId,
        dev: Arc<dyn SoapyDevice>,
        args: &str,
    ) -> Result<Self, PortError> {
        let io = |e: String| PortError::Io(format!("soapy {}: {e}", dev.driver_key()));
        let probe = is_uhd(args, &dev.driver_key());
        let rates = probe_rate_ranges(dev.as_ref(), probe).map_err(io)?;
        if probe {
            let max = rates.iter().map(|r| r.max_hz).fold(0.0, f64::max);
            eprintln!(
                "zeta-source-soapy: {}: sample-rate range max {} Hz after probe",
                id.0,
                max.round() as u64
            );
        }
        let caps = capabilities(dev.as_ref(), &rates).map_err(io)?;
        let rates = Arc::new(if probe {
            uhd_ladder(&rates)
        } else {
            RateModel::Ranges(rates)
        });
        let freq = dev.frequency_ranges().map_err(io)?;
        let (native, supported) = dev.formats().map_err(io)?;
        let format = pick_format(native, &supported).map_err(PortError::Unsupported)?;
        let status = Status {
            gain: read_gain(dev.as_ref()),
            ..Status::default()
        };
        Ok(Self {
            id,
            caps,
            dev,
            freq,
            rates,
            format,
            tx: broadcast::channel(BROADCAST_CAPACITY).0,
            status: Arc::new(Mutex::new(status)),
            reader: Mutex::new(None),
            retune_timeout: RETUNE_ACK,
            ppm: 0.0,
            gate: tokio::sync::Mutex::new(()),
        })
    }

    /// Correct a device that reads `ppm` parts per million high: the hardware
    /// is tuned that much higher than every frequency this source reports
    /// (`current`, blocks, the band keep the requested centre).
    pub fn with_ppm(mut self, ppm: f64) -> Self {
        self.ppm = ppm;
        self
    }

    /// How long `tune` waits for the reader thread to apply a retune
    /// (default 2 s); past it the port faults with `soapy retune timed out`.
    pub fn with_retune_timeout(mut self, d: Duration) -> Self {
        self.retune_timeout = d;
        self
    }

    /// Read overflows (samples dropped by the driver) since the source opened.
    pub fn overflows(&self) -> u64 {
        self.status.lock().unwrap().overflows
    }

    /// Reads that timed out (100 ms without samples) since the source opened.
    pub fn read_timeouts(&self) -> u64 {
        self.status.lock().unwrap().timeouts
    }

    /// Samples per second actually delivered over the last 10 s, for reports;
    /// the capabilities stay `Asserted`.
    pub fn measured_rate_hz(&self) -> Option<f64> {
        let st = self.status.lock().unwrap();
        let (first, last) = (st.window.front()?, st.window.back()?);
        let secs = last.0.duration_since(first.0).as_secs_f64();
        let n: u64 = st.window.iter().skip(1).map(|(_, n)| n).sum();
        (secs > 0.0).then(|| n as f64 / secs)
    }

    /// Refuse a gain the device cannot take: `None` without automatic gain, a
    /// value outside the range, any gain on a device without a gain range.
    fn check_gain(&self, db: Option<f64>) -> Result<(), PortError> {
        let Some(g) = self.status.lock().unwrap().gain else {
            return Err(PortError::Unsupported(
                "source has no adjustable gain".into(),
            ));
        };
        match db {
            None if !g.automatic_available => Err(PortError::Unsupported(format!(
                "source {} has no automatic gain",
                self.id.0
            ))),
            Some(d) if !(g.min_db..=g.max_db).contains(&d) => Err(PortError::Unsupported(format!(
                "gain_db {d} out of range {}..={} dB for source {}",
                g.min_db, g.max_db, self.id.0
            ))),
            _ => Ok(()),
        }
    }

    /// [`Port::set_gain`] on the device directly, for a source nothing streams
    /// from yet (the driver's `gain=` at open): the same checks, texts and
    /// device calls. Once the source is shared, use [`Port::set_gain`], which
    /// orders the change with retunes.
    pub fn set_gain_now(&self, db: Option<f64>) -> Result<GainInfo, PortError> {
        self.check_gain(db)?;
        apply_gain(self.dev.as_ref(), &self.status, db).map_err(PortError::Io)
    }

    fn fail(&self, msg: String) -> PortError {
        self.status.lock().unwrap().fault = Some(msg.clone());
        PortError::Io(msg)
    }

    /// The answer on the live, unfaulted reader, retuning it when the band
    /// changed; `Ok(None)` when a new stream is needed.
    async fn retune_live(
        &self,
        center_hz: u64,
        rate_hz: u64,
        bandwidth_hz: u64,
    ) -> Result<Option<Tuning>, PortError> {
        let ack = {
            let reader = self.reader.lock().unwrap();
            let Some(r) = reader.as_ref().filter(|r| r.alive()) else {
                return Ok(None);
            };
            let st = self.status.lock().unwrap();
            if st.fault.is_some() {
                return Ok(None);
            }
            let covers = st.current.is_some_and(|c| c.bandwidth_hz.0 >= bandwidth_hz);
            if st.band == Some((center_hz, rate_hz)) && covers {
                return Ok(st.current);
            }
            let (ack, done) = oneshot::channel();
            let cmd = Cmd::Retune(Retune {
                center_hz,
                rate_hz,
                bandwidth_hz,
                ack,
            });
            if r.cmds.send(cmd).is_err() {
                return Ok(None);
            }
            done
        };
        match tokio::time::timeout(self.retune_timeout, ack).await {
            // The reader recorded the applied band in `status` before acking.
            Ok(Ok(Ok(()))) => Ok(self.status.lock().unwrap().current),
            Ok(Ok(Err(RateError::Io(m)))) => Err(PortError::Io(m)),
            // Refused: the reader kept the capture it had.
            Ok(Ok(Err(RateError::Narrow(m)))) => Err(PortError::Unsupported(m)),
            Ok(Err(_)) => Ok(None),
            Err(_) => {
                // The device is mid-retune and the reader may still apply it
                // (and then record it). Fault and stop the reader so the next
                // tune reopens, whatever band it asks for.
                if let Some(r) = self.reader.lock().unwrap().as_ref() {
                    r.stop.store(true, Ordering::Relaxed);
                }
                Err(self.fail("soapy retune timed out".into()))
            }
        }
    }

    /// Configure and activate a new stream, then start its reader thread.
    /// A rate the device gives below `bandwidth_hz` is refused
    /// (`Unsupported`) before the stream opens; `current` stays `None`.
    fn open_stream(
        &self,
        center_hz: u64,
        rate_hz: u64,
        bandwidth_hz: u64,
    ) -> Result<Tuning, PortError> {
        let dev = &self.dev;
        let rate =
            set_rate(dev.as_ref(), &self.rates, rate_hz, bandwidth_hz).map_err(|e| match e {
                RateError::Io(m) => self.fail(m),
                RateError::Narrow(m) => PortError::Unsupported(m),
            })?;
        let actual = rate.actual;
        let hw = hw_hz(center_hz, self.ppm);
        dev.set_frequency(hw as f64)
            .map_err(|e| self.fail(format!("soapy set_frequency({hw}): {e}")))?;
        let mut rx = dev
            .open_rx(self.format)
            .map_err(|e| self.fail(format!("soapy open_rx({:?}): {e}", self.format)))?;
        rx.activate()
            .map_err(|e| self.fail(format!("soapy activate: {e}")))?;
        let cur = Tuning {
            center_hz: Hz(center_hz),
            bandwidth_hz: Hz(actual),
        };
        {
            // Cleared before the thread starts, so a fault it raises sticks.
            let mut st = self.status.lock().unwrap();
            st.fault = None;
            st.current = Some(cur);
            st.band = Some((center_hz, rate_hz));
            st.window.clear();
        }
        let (cmds, cmds_rx) = std_mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let ctx = Ctx {
            id: self.id.0.clone(),
            dev: self.dev.clone(),
            format: self.format,
            rates: self.rates.clone(),
            tx: self.tx.clone(),
            status: self.status.clone(),
            stop: stop.clone(),
            cmds: cmds_rx,
            ppm: self.ppm,
        };
        let handle = std::thread::Builder::new()
            .name(format!("soapy-{}", self.id.0))
            .spawn(move || run(ctx, rx, center_hz, rate_hz, rate))
            .map_err(|e| self.fail(format!("soapy reader thread: {e}")))?;
        *self.reader.lock().unwrap() = Some(Reader {
            cmds,
            stop,
            handle: Some(handle),
        });
        Ok(cur)
    }
}

/// The frequency to ask the hardware for: `center_hz` raised by `ppm` parts per
/// million.
fn hw_hz(center_hz: u64, ppm: f64) -> u64 {
    (center_hz as f64 * (1.0 + ppm / 1e6)).round() as u64
}

fn out_of_range(field: &'static str, value: u64, min: u64, max: u64) -> PortError {
    PortError::OutOfRange {
        field,
        value,
        min,
        max,
        suggested: None,
    }
}

fn claim(c: &Claim<Hz>) -> u64 {
    c.value().0
}

#[async_trait]
impl Port for SoapySource {
    fn id(&self) -> &SourceId {
        &self.id
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    /// Validates the centre against the device's ranges and picks the smallest
    /// supported rate at least `bandwidth_hz` wide (on a UHD device the
    /// smallest exact divisor of its master clock). A device that gives less
    /// than the band is asked once for the next rate above the request, then
    /// refused with `Unsupported`, the capture as it was. An unchanged band on a
    /// live, unfaulted reader is a no-op; a changed one is applied by the
    /// reader between reads; otherwise (first tune, fault, reader gone) the
    /// stream is (re)opened, which clears the fault. Returns the capture band:
    /// the requested centre, the device's actual sample rate wide.
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        let (center, bw) = (t.center_hz.0, t.bandwidth_hz.0);
        let (fmin, fmax) = (claim(&self.caps.tune_min_hz), claim(&self.caps.tune_max_hz));
        let c = center as f64;
        if !self.freq.iter().any(|r| r.min_hz <= c && c <= r.max_hz) {
            return Err(out_of_range("center_hz", center, fmin, fmax));
        }
        let max_rate = claim(&self.caps.max_bandwidth_hz);
        let rate = (bw > 0)
            .then(|| pick_rate(&self.rates, bw))
            .flatten()
            .ok_or_else(|| out_of_range("bandwidth_hz", bw, 1, max_rate))?;

        let _gate = self.gate.lock().await;
        if let Some(cur) = self.retune_live(center, rate, bw).await? {
            return Ok(cur);
        }
        // A new stream: stop the old reader (finished or faulted) and wait for
        // it to deactivate its stream before the device is reconfigured.
        let old = self.reader.lock().unwrap().take();
        if let Some(mut old) = old {
            let handle = old.handle.take();
            drop(old);
            if let Some(h) = handle {
                let _ = tokio::task::spawn_blocking(move || h.join()).await;
            }
        }
        {
            let mut st = self.status.lock().unwrap();
            st.current = None;
            st.band = None;
        }
        self.open_stream(center, rate, bw)
    }

    fn current(&self) -> Option<Tuning> {
        self.status.lock().unwrap().current
    }

    fn subscribe(&self) -> IqReceiver {
        self.tx.subscribe()
    }

    fn fault(&self) -> Option<String> {
        self.status.lock().unwrap().fault.clone()
    }

    fn gain(&self) -> Option<GainInfo> {
        self.status.lock().unwrap().gain
    }

    fn stats(&self) -> Option<PortStats> {
        let measured_rate_hz = self.measured_rate_hz().map(|r| r.round() as u64);
        let st = self.status.lock().unwrap();
        Some(PortStats {
            measured_rate_hz,
            overflows: st.overflows,
            timeouts: st.timeouts,
            restarts: st.restarts,
        })
    }

    /// Checked against the range read at open (refusals are `Unsupported` with
    /// the op's texts), then applied by the live, unfaulted reader between
    /// reads, after any retune queued before it. Otherwise on the device
    /// directly, once a reader that is still running (faulted, or stopped after
    /// a retune timeout, and maybe inside a device call) has ended: past
    /// `retune_timeout` that is `Io` and nothing is applied. A reader that does
    /// not ack within `retune_timeout` is `Io` too, and drops the change if it
    /// has not started it (the ack is gone). A failed device call is `Io` and
    /// does not fault the port.
    async fn set_gain(&self, db: Option<f64>) -> Result<GainInfo, PortError> {
        self.check_gain(db)?;
        let _gate = self.gate.lock().await;
        let ack = {
            let reader = self.reader.lock().unwrap();
            let live = reader.as_ref().filter(|r| r.alive());
            match live {
                Some(r) if self.status.lock().unwrap().fault.is_none() => {
                    let (ack, done) = oneshot::channel();
                    r.cmds.send(Cmd::Gain { db, ack }).ok().map(|()| done)
                }
                _ => None,
            }
        };
        if let Some(done) = ack {
            match tokio::time::timeout(self.retune_timeout, done).await {
                Ok(Ok(r)) => return r.map_err(PortError::Io),
                // The reader ended before applying it: the device is free.
                Ok(Err(_)) => {}
                // Dropping `done` closes the ack: the reader skips the command.
                Err(_) => return Err(PortError::Io("soapy set_gain timed out".into())),
            }
        }
        self.reader_ended().await?;
        apply_gain(self.dev.as_ref(), &self.status, db).map_err(PortError::Io)
    }
}

impl SoapySource {
    /// Wait up to `retune_timeout` for a reader thread that is still running to
    /// end, so a direct device call never overlaps one of its own. The reader
    /// is left in place (a later `tune` joins and replaces it).
    async fn reader_ended(&self) -> Result<(), PortError> {
        let deadline = Instant::now() + self.retune_timeout;
        loop {
            let running = self
                .reader
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|r| r.handle.as_ref())
                .is_some_and(|h| !h.is_finished());
            if !running {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(PortError::Io(
                    "soapy set_gain: the reader thread has not stopped".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

/// What the reader thread shares with the source.
struct Ctx {
    id: String,
    dev: Arc<dyn SoapyDevice>,
    /// The wire format the stream was opened with, for [`reopen`].
    format: WireFormat,
    rates: Arc<RateModel>,
    tx: broadcast::Sender<IqBlock>,
    status: Arc<Mutex<Status>>,
    stop: Arc<AtomicBool>,
    cmds: std_mpsc::Receiver<Cmd>,
    ppm: f64,
}

impl Ctx {
    fn fault(&self, m: String) {
        self.status.lock().unwrap().fault = Some(m);
    }
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
}

fn block_len(rate_hz: u64) -> usize {
    MIN_BLOCK.max(usize::try_from(rate_hz / 100).unwrap_or(usize::MAX))
}

/// Sample-count clock: `t0 = anchor + count · 1e9 / rate`.
struct Clock {
    anchor: u64,
    count: u64,
    rate: u64,
}

impl Clock {
    fn at(&self, count: u64) -> u64 {
        self.anchor + (u128::from(count) * 1_000_000_000 / u128::from(self.rate)) as u64
    }

    /// Start counting again from `max(now, end of the last block)`.
    fn reanchor(&mut self, rate: u64) {
        self.anchor = now_ns().max(self.at(self.count));
        self.count = 0;
        self.rate = rate;
    }
}

/// A stream that is gone: what `rx` holds between closing the old stream and
/// opening the new one in [`reopen`] (and after a failed open, until the
/// reader exits).
struct Closed;

impl SoapyRx for Closed {
    fn activate(&mut self) -> Result<(), String> {
        Err("stream closed".into())
    }
    fn deactivate(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn read(&mut self, _: &mut [Complex<f32>], _: i64) -> Result<usize, ReadError> {
        Err(ReadError::Fatal("stream closed".into()))
    }
}

/// Close the stream and open and activate a new one in the same format. The
/// old one is closed first, so the driver drops any buffer it still held
/// (SoapyHackRF keeps a half-read one across `activateStream`).
fn reopen(ctx: &Ctx, rx: &mut Box<dyn SoapyRx>) -> Result<(), String> {
    *rx = Box::new(Closed);
    *rx = ctx
        .dev
        .open_rx(ctx.format)
        .map_err(|e| format!("soapy open_rx({:?}): {e}", ctx.format))?;
    rx.activate().map_err(|e| format!("soapy activate: {e}"))
}

/// Apply a retune between reads: a rate change (or a rate short of the band)
/// reopens the stream. `picked` is the rate picked for the stream running at
/// `rate`. A refused rate restores `rate.asked` and reopens the stream on the
/// old centre (the frequency is set only once the rate holds); a device error
/// leaves the stream to fault.
fn apply(
    ctx: &Ctx,
    rx: &mut Box<dyn SoapyRx>,
    cmd: &Retune,
    picked: u64,
    rate: Rate,
) -> Result<Rate, RateError> {
    let io = RateError::Io;
    let hw = hw_hz(cmd.center_hz, ctx.ppm);
    if cmd.rate_hz == picked && rate.actual >= cmd.bandwidth_hz {
        ctx.dev
            .set_frequency(hw as f64)
            .map_err(|e| io(format!("soapy set_frequency({hw}): {e}")))?;
        return Ok(rate);
    }
    let got = match set_rate(ctx.dev.as_ref(), &ctx.rates, cmd.rate_hz, cmd.bandwidth_hz) {
        Err(RateError::Narrow(m)) => {
            let back = set_rate(ctx.dev.as_ref(), &ctx.rates, rate.asked, 0)?;
            if back.actual != rate.actual {
                return Err(io(format!(
                    "soapy: restoring {} Hz gave {} Hz, not {} Hz",
                    rate.asked, back.actual, rate.actual
                )));
            }
            reopen(ctx, rx).map_err(io)?;
            return Err(RateError::Narrow(m));
        }
        r => r?,
    };
    ctx.dev
        .set_frequency(hw as f64)
        .map_err(|e| io(format!("soapy set_frequency({hw}): {e}")))?;
    reopen(ctx, rx).map_err(io)?;
    Ok(got)
}

/// The reader thread. Exits on stop, on no receivers, or with a fault (set
/// before the stream is deactivated); always deactivates on exit. A block
/// that would end more than [`MAX_AHEAD_NS`] after now is not sent: the
/// stream is reopened and reads are discarded until the wall clock passes the
/// last block sent (`catch_up`), then the clock anchors there.
fn run(ctx: Ctx, mut rx: Box<dyn SoapyRx>, mut center: u64, mut picked: u64, mut rate: Rate) {
    // A reopen continues after the previous stream's last block.
    let last_end = ctx.status.lock().unwrap().last_end;
    let mut clock = Clock {
        anchor: now_ns().max(last_end),
        count: 0,
        rate: rate.actual,
    };
    let mut buf = vec![Complex::new(0.0f32, 0.0); block_len(rate.actual)];
    let mut quiet = 0u32;
    // After a restart: the end of the last block sent, which the wall clock
    // must pass before a block is sent again.
    let mut catch_up: Option<u64> = None;
    loop {
        if ctx.stop.load(Ordering::Relaxed) || ctx.tx.receiver_count() == 0 {
            break;
        }
        if let Ok(cmd) = ctx.cmds.try_recv() {
            let cmd = match cmd {
                Cmd::Retune(cmd) => cmd,
                // A closed ack: the caller timed out and reported the change
                // as failed, so it is not applied.
                Cmd::Gain { ack, .. } if ack.is_closed() => continue,
                Cmd::Gain { db, ack } => {
                    let _ = ack.send(apply_gain(ctx.dev.as_ref(), &ctx.status, db));
                    continue;
                }
            };
            match apply(&ctx, &mut rx, &cmd, picked, rate) {
                Ok(got) => {
                    let actual = got.actual;
                    center = cmd.center_hz;
                    picked = cmd.rate_hz;
                    rate = got;
                    clock.reanchor(actual);
                    buf.resize(block_len(actual), Complex::new(0.0, 0.0));
                    {
                        let mut st = ctx.status.lock().unwrap();
                        st.window.clear();
                        st.band = Some((cmd.center_hz, cmd.rate_hz));
                        st.current = Some(Tuning {
                            center_hz: Hz(cmd.center_hz),
                            bandwidth_hz: Hz(actual),
                        });
                    }
                    let _ = cmd.ack.send(Ok(()));
                }
                Err(RateError::Narrow(m)) => {
                    // The old capture runs on; it restarts after the gap.
                    clock.reanchor(clock.rate);
                    let _ = cmd.ack.send(Err(RateError::Narrow(m)));
                }
                Err(RateError::Io(m)) => {
                    ctx.fault(m.clone());
                    let _ = cmd.ack.send(Err(RateError::Io(m)));
                    break;
                }
            }
            continue;
        }
        match rx.read(&mut buf, READ_TIMEOUT_US) {
            Ok(n) if n > 0 => {
                quiet = 0;
                if let Some(t) = catch_up {
                    let now = now_ns();
                    if now < t {
                        continue;
                    }
                    clock.anchor = now.max(t);
                    clock.count = 0;
                    catch_up = None;
                }
                let end = clock.at(clock.count + n as u64);
                let now = now_ns();
                if end > now + MAX_AHEAD_NS {
                    let lead_ms = (end - now) / 1_000_000;
                    let (restarts, last_end) = {
                        let mut st = ctx.status.lock().unwrap();
                        st.restarts += 1;
                        let at = Instant::now();
                        st.restart_times.push_back(at);
                        while st
                            .restart_times
                            .front()
                            .is_some_and(|t| at.duration_since(*t) > RESTART_WINDOW)
                        {
                            st.restart_times.pop_front();
                        }
                        (st.restart_times.len(), st.last_end)
                    };
                    if restarts > MAX_RESTARTS as usize {
                        ctx.fault(format!(
                            "soapy: samples arrive faster than {} Hz ({restarts} stream restarts in {RESTART_WINDOW:?})",
                            clock.rate
                        ));
                        break;
                    }
                    eprintln!(
                        "zeta: soapy {}: samples ran {lead_ms} ms ahead of the clock; stream restarted",
                        ctx.id
                    );
                    if let Err(m) = reopen(&ctx, &mut rx) {
                        ctx.fault(m);
                        break;
                    }
                    catch_up = Some(last_end);
                    continue;
                }
                let t0 = clock.at(clock.count);
                clock.count += n as u64;
                {
                    let mut st = ctx.status.lock().unwrap();
                    let now = Instant::now();
                    st.last_end = end;
                    st.window.push_back((now, n as u64));
                    while st
                        .window
                        .front()
                        .is_some_and(|(t, _)| now.duration_since(*t) > RATE_WINDOW)
                    {
                        st.window.pop_front();
                    }
                }
                let block = IqBlock {
                    t0: Timestamp(t0),
                    sample_rate_hz: Hz(clock.rate),
                    center_hz: Hz(center),
                    samples: buf[..n].into(),
                };
                if ctx.tx.send(block).is_err() {
                    break;
                }
            }
            // A zero-length read counts as a timeout: no samples arrived.
            Ok(_) | Err(ReadError::Timeout) => {
                quiet += 1;
                ctx.status.lock().unwrap().timeouts += 1;
                if quiet >= QUIET_READS {
                    ctx.fault("no samples for 1000 ms (device unplugged?)".into());
                    break;
                }
            }
            Err(ReadError::Overflow) => {
                ctx.status.lock().unwrap().overflows += 1;
                clock.reanchor(clock.rate);
            }
            Err(ReadError::Fatal(m)) => {
                ctx.fault(format!("read failed: {m}"));
                break;
            }
        }
    }
    let _ = rx.deactivate();
}
