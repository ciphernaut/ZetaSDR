//! [`WorkerPort`]: a [`Port`] whose device lives in a child process.
//!
//! The port spawns a worker that speaks the protocol of [`crate::wire`] (the
//! node spawns `zeta soapy-worker`, the tests the fake worker) and keeps a
//! snapshot of what the worker last reported, so the synchronous `Port`
//! methods (`fault`, `current`, `gain`, `stats`) answer from memory and never
//! wait on the child. `tune` and `set_gain` are round trips under a gate. A
//! supervisor task ([`crate::supervisor`]) owns the child: it reaps a worker
//! that died, turns the way it died into the port's fault, respawns it with a
//! bounded backoff and bumps [`Port::healed`] when a fresh worker says hello
//! after a fault.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{oneshot, watch};
use zeta_core::port::{GainInfo, IqReceiver, Port, PortError, PortStats, Tuning, WorkerInfo};
use zeta_core::types::{Capabilities, SourceId};

use crate::supervisor::{Link, Shared, Supervisor};
use crate::wire::{Cmd, Reply};

/// How long a worker has to say hello after it is spawned: UHD loads a B210's
/// FPGA image before the device opens, which takes seconds on a cold boot.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(30);
/// The wait before the first respawn after a worker ended.
pub const BACKOFF_FIRST: Duration = Duration::from_secs(1);
/// The cap the respawn wait doubles up to, and the poll interval while the
/// device is absent.
pub const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// How long a worker must have lived for the backoff to start again from
/// [`BACKOFF_FIRST`] when it ends: a worker that dies soon after each hello is
/// a crash loop and keeps backing off.
pub const ALIVE_RESET: Duration = Duration::from_secs(60);
/// How long a worker is given to exit after `shutdown` before it is killed.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
/// How long a `tune` or `set_gain` waits for the worker's reply: the worker's
/// own `SoapySource` bounds a retune by its `RETUNE_ACK` (5 s), plus 2 s for
/// the pipe and the worker. Copied rather than imported so this crate does not
/// depend on the driver crate.
pub const TUNE_TIMEOUT: Duration = Duration::from_secs(7);

/// How long a request that lost its worker waits for the reap to close the
/// link: the reap waits up to `READER_JOIN` for the reader threads, then
/// closes it at once.
const REAP_WAIT: Duration = crate::supervisor::READER_JOIN.saturating_add(Duration::from_secs(1));

/// The supervisor's clock. The defaults are the production values; tests
/// shorten them so a crash-and-respawn cycle takes milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timings {
    /// See [`HELLO_TIMEOUT`].
    pub hello: Duration,
    /// See [`BACKOFF_FIRST`].
    pub backoff_first: Duration,
    /// See [`BACKOFF_MAX`].
    pub backoff_max: Duration,
    /// See [`ALIVE_RESET`].
    pub alive_reset: Duration,
    /// See [`SHUTDOWN_GRACE`].
    pub shutdown_grace: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Timings {
            hello: HELLO_TIMEOUT,
            backoff_first: BACKOFF_FIRST,
            backoff_max: BACKOFF_MAX,
            alive_reset: ALIVE_RESET,
            shutdown_grace: SHUTDOWN_GRACE,
        }
    }
}

/// What to spawn: `program` with `args`, serving source `id`.
#[derive(Clone, Debug)]
pub struct WorkerConfig {
    pub id: SourceId,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub timings: Timings,
}

/// What the first spawn said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attach {
    /// The worker opened the device and said hello.
    Hello,
    /// The worker found no device (exit 3); `args` is what it looked for. The
    /// port is attached faulted with the cached capabilities and polls.
    NotFound { args: String },
    /// The worker could not be used; the text is the fault. [`WorkerPort::open`]
    /// returns it as its error, so no port is ever attached this way.
    Refused(String),
}

/// A [`Port`] backed by a supervised worker process; see the module docs.
pub struct WorkerPort {
    id: SourceId,
    caps: Capabilities,
    shared: Arc<Shared>,
    /// Serialises `tune` and `set_gain`, as a device takes one at a time.
    gate: tokio::sync::Mutex<()>,
    next_request: AtomicU64,
    healed: watch::Receiver<u64>,
    /// Dropped or fired to stop the supervisor, which shuts the worker down.
    cancel: Option<oneshot::Sender<()>>,
}

impl WorkerPort {
    /// Spawn the worker once and attach on what it says; see [`Attach`].
    ///
    /// A hello attaches with the hello's capability record. A device-not-found
    /// exit attaches only with `cached` (the record from an earlier open):
    /// the port is faulted `device not found: <args>` and the supervisor is
    /// already polling. Anything else (not found and nothing cached, an open
    /// that failed, a worker that died or never said hello) is the error,
    /// returned after the child was reaped, so a refused source leaves no
    /// process behind.
    pub async fn open(
        cfg: WorkerConfig,
        cached: Option<Capabilities>,
    ) -> Result<(Arc<WorkerPort>, Attach), String> {
        let id = cfg.id.clone();
        let shared = Arc::new(Shared::new(id.clone()));
        let (healed_tx, healed) = watch::channel(0);
        let (cancel, cancel_rx) = oneshot::channel();
        let (first_tx, first_rx) = oneshot::channel();
        let supervisor = Supervisor::new(cfg, shared.clone(), healed_tx, cached);
        tokio::spawn(supervisor.run(cancel_rx, first_tx));
        let first = first_rx
            .await
            .map_err(|_| format!("soapy worker for {}: supervisor ended", id.0))?;
        let (attach, caps) = match first {
            (Attach::Refused(e), _) => return Err(e),
            (attach, Some(caps)) => (attach, caps),
            (attach, None) => {
                return Err(format!(
                    "soapy worker for {}: attached ({attach:?}) without capabilities",
                    id.0
                ));
            }
        };
        let port = WorkerPort {
            id,
            caps,
            shared,
            gate: tokio::sync::Mutex::new(()),
            next_request: AtomicU64::new(1),
            healed,
            cancel: Some(cancel),
        };
        Ok((Arc::new(port), attach))
    }

    /// Refuse a band the device cannot take before anything is sent, with the
    /// texts `SoapySource` gives, so a bad request never waits on a worker.
    fn check(&self, t: Tuning) -> Result<(), PortError> {
        let (center, bw) = (t.center_hz.0, t.bandwidth_hz.0);
        let (min, max) = (
            self.caps.tune_min_hz.value().0,
            self.caps.tune_max_hz.value().0,
        );
        if !(min..=max).contains(&center) {
            return Err(out_of_range("center_hz", center, min, max));
        }
        let max_bw = self.caps.max_bandwidth_hz.value().0;
        if !(1..=max_bw).contains(&bw) {
            return Err(out_of_range("bandwidth_hz", bw, 1, max_bw));
        }
        Ok(())
    }

    /// The fault a request fails with when no worker can answer it.
    fn down(&self) -> PortError {
        PortError::Io(
            self.shared
                .snapshot()
                .fault()
                .unwrap_or_else(|| format!("soapy worker for {}: not running", self.id.0)),
        )
    }

    /// The fault for a request whose worker went away under it: the worker
    /// may have died moments ago and not been reaped yet, so wait for the
    /// supervisor to close the link (it sets the fault first) before reading
    /// the fault. The bound covers the reap's wait for the reader threads.
    async fn ended(&self, link: &Link) -> PortError {
        link.wait_closed(REAP_WAIT).await;
        self.down()
    }

    /// Send `cmd` to the live worker and wait for its reply. No live worker
    /// answers at once with the current fault; a worker that ends before it
    /// replies answers with the fault its end left.
    async fn request(&self, cmd: Cmd, what: &str) -> Result<Reply, PortError> {
        // Taken out first: `down` locks the snapshot again.
        let link = self.shared.snapshot().link.clone();
        let Some(link) = link else {
            return Err(self.down());
        };
        let id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let reply = match link.send(id, cmd).await {
            Ok(rx) => rx,
            Err(_) => return Err(self.ended(&link).await),
        };
        match tokio::time::timeout(TUNE_TIMEOUT, reply).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(self.ended(&link).await),
            Err(_) => {
                link.forget(id);
                Err(PortError::Io(format!(
                    "soapy worker for {}: no reply to {what} in {} s",
                    self.id.0,
                    TUNE_TIMEOUT.as_secs()
                )))
            }
        }
    }
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

fn unexpected(id: &SourceId, what: &str, r: &Reply) -> PortError {
    PortError::Io(format!(
        "soapy worker for {}: unexpected reply to {what}: {r:?}",
        id.0
    ))
}

impl Drop for WorkerPort {
    /// Stop the supervisor, which sends `shutdown`, waits the grace and then
    /// kills and reaps the worker. The child is also spawned kill-on-drop, so
    /// a runtime that ends before the supervisor runs still takes it down.
    fn drop(&mut self) {
        if let Some(c) = self.cancel.take() {
            let _ = c.send(());
        }
    }
}

#[async_trait]
impl Port for WorkerPort {
    fn id(&self) -> &SourceId {
        &self.id
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        self.check(t)?;
        let _gate = self.gate.lock().await;
        let cmd = Cmd::Tune {
            center_hz: t.center_hz.0,
            bandwidth_hz: t.bandwidth_hz.0,
        };
        match self.request(cmd, "tune").await? {
            Reply::Tuning(got) => {
                let mut s = self.shared.snapshot();
                s.current = Some(got);
                s.worker_fault = None;
                s.supervision_fault = None;
                Ok(got)
            }
            r => Err(unexpected(&self.id, "tune", &r)),
        }
    }

    fn current(&self) -> Option<Tuning> {
        self.shared.snapshot().current
    }

    fn subscribe(&self) -> IqReceiver {
        self.shared.tx.subscribe()
    }

    fn fault(&self) -> Option<String> {
        self.shared.snapshot().fault()
    }

    fn gain(&self) -> Option<GainInfo> {
        self.shared.snapshot().gain
    }

    fn stats(&self) -> Option<PortStats> {
        self.shared.snapshot().stats.clone()
    }

    async fn set_gain(&self, db: Option<f64>) -> Result<GainInfo, PortError> {
        let _gate = self.gate.lock().await;
        match self
            .request(Cmd::SetGain { gain_db: db }, "set_gain")
            .await?
        {
            Reply::Gain(g) => {
                self.shared.snapshot().gain = Some(g);
                Ok(g)
            }
            r => Err(unexpected(&self.id, "set_gain", &r)),
        }
    }

    fn healed(&self) -> Option<watch::Receiver<u64>> {
        Some(self.healed.clone())
    }

    fn worker(&self) -> Option<WorkerInfo> {
        let s = self.shared.snapshot();
        Some(WorkerInfo {
            pid: s.pid,
            restarts: s.restarts,
            since_ns: s.since_ns,
        })
    }
}
