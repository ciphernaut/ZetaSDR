//! Ports for tests, behind the `test-fixtures` feature so the integration tests of this
//! crate and of the crates above it share them.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::port::{GainInfo, IqReceiver, Port, PortError, PortStats, Tuning, WorkerInfo};
use crate::types::{Capabilities, SourceId};

/// The gain range a [`FaultablePort`] reports, a HackRF-like 0..=76 dB.
const GAIN_RANGE_DB: (f64, f64) = (0.0, 76.0);

/// A port that is another port (a `FileSource`, typically) but for a fault the test sets
/// and clears, standing in for a device that is unplugged and found again by a worker.
///
/// While faulted, [`Port::fault`] reports it, `tune` fails with `Io(<fault>)` (a device
/// that cannot be opened) and `current` still answers the inner port's band, as a worker
/// port's cached capture does. [`FaultablePort::heal`] clears the fault and bumps the
/// counter [`Port::healed`] hands out, as a respawned worker does. It also reports a
/// plausible adjustable gain, recording every `set_gain`, and a worker, so the node's
/// recovery of a worker-like source can be tested over a recording. The wrapped port's
/// own `fault` is not consulted: the test owns the fault.
///
/// It wraps `Arc<dyn Port>` rather than `zeta-source-file`'s `FileSource` because that
/// crate depends on this one; pass the file source, it coerces.
pub struct FaultablePort {
    inner: Arc<dyn Port>,
    fault: Mutex<Option<String>>,
    healed: tokio::sync::watch::Sender<u64>,
    /// Set by [`FaultablePort::heal_held`]: [`Port::heal_settled`] ends the fault.
    held: Mutex<bool>,
    gain: Mutex<Option<f64>>,
    gains: Mutex<Vec<Option<f64>>>,
}

impl FaultablePort {
    pub fn new(inner: Arc<dyn Port>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            fault: Mutex::new(None),
            healed: tokio::sync::watch::channel(0).0,
            held: Mutex::new(false),
            gain: Mutex::new(Some(GAIN_RANGE_DB.0)),
            gains: Mutex::new(Vec::new()),
        })
    }

    /// Set (or with `None` clear) the fault the port reports. Clearing does not count as
    /// a heal; see [`FaultablePort::heal`].
    pub fn set_fault(&self, fault: Option<String>) {
        *self.fault.lock().unwrap() = fault;
    }

    /// Clear the fault and bump the healed counter, as a worker that found its device
    /// again does.
    pub fn heal(&self) {
        self.set_fault(None);
        self.healed.send_modify(|n| *n += 1);
    }

    /// Bump the healed counter and keep reporting the fault until [`Port::heal_settled`],
    /// as a worker back quicker than the node's polls does.
    pub fn heal_held(&self) {
        *self.held.lock().unwrap() = true;
        self.healed.send_modify(|n| *n += 1);
    }

    /// Every `set_gain` the port was asked for, in order.
    pub fn gain_calls(&self) -> Vec<Option<f64>> {
        self.gains.lock().unwrap().clone()
    }

    fn gain_info(&self) -> GainInfo {
        let db = *self.gain.lock().unwrap();
        GainInfo {
            db,
            min_db: GAIN_RANGE_DB.0,
            max_db: GAIN_RANGE_DB.1,
            automatic: db.is_none(),
            automatic_available: true,
        }
    }
}

#[async_trait]
impl Port for FaultablePort {
    fn id(&self) -> &SourceId {
        self.inner.id()
    }
    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        let fault = self.fault.lock().unwrap().clone();
        match fault {
            Some(f) => Err(PortError::Io(f)),
            None => self.inner.tune(t).await,
        }
    }
    fn current(&self) -> Option<Tuning> {
        self.inner.current()
    }
    fn subscribe(&self) -> IqReceiver {
        self.inner.subscribe()
    }
    fn fault(&self) -> Option<String> {
        self.fault.lock().unwrap().clone()
    }
    async fn idle(&self) {
        self.inner.idle().await
    }
    fn gain(&self) -> Option<GainInfo> {
        Some(self.gain_info())
    }
    fn stats(&self) -> Option<PortStats> {
        self.inner.stats()
    }
    async fn set_gain(&self, db: Option<f64>) -> Result<GainInfo, PortError> {
        self.gains.lock().unwrap().push(db);
        if let Some(v) = db
            && !(GAIN_RANGE_DB.0..=GAIN_RANGE_DB.1).contains(&v)
        {
            return Err(PortError::Unsupported(format!(
                "gain {v} dB out of range {}..={} dB",
                GAIN_RANGE_DB.0, GAIN_RANGE_DB.1
            )));
        }
        *self.gain.lock().unwrap() = db;
        Ok(self.gain_info())
    }
    fn healed(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        Some(self.healed.subscribe())
    }
    fn heal_settled(&self) {
        if std::mem::take(&mut *self.held.lock().unwrap()) {
            self.set_fault(None);
        }
    }
    fn worker(&self) -> Option<WorkerInfo> {
        Some(WorkerInfo {
            pid: Some(1),
            restarts: 0,
            since_ns: None,
        })
    }
}
