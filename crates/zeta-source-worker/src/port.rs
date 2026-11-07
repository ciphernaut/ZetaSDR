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
use zeta_core::types::{Capabilities, Claim, Hz, SourceId};

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

/// What [`WorkerPort::close`] allows beyond the shutdown grace: the kill and
/// reap of a worker that outlived it, and the reader threads' join.
const CLOSE_SLACK: Duration = Duration::from_secs(1);

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

/// What to spawn: `program` with `args`, serving source `id`. With
/// `ring_bytes > 0` (and memfds available) each spawn gets a shared-memory
/// ring of that size for its blocks, and `--ring-fd 3` after `args`; with 0
/// the blocks come on the pipe.
#[derive(Clone, Debug)]
pub struct WorkerConfig {
    pub id: SourceId,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub timings: Timings,
    pub ring_bytes: usize,
}

/// The arguments `zeta` is spawned with as the worker of `--source-soapy
/// id=args`: `soapy-worker --id ID --args ARGS`, and `--rate-cap-hz N` when the
/// capabilities cached from an earlier open carry a measured cap `N`, so the
/// worker reports it instead of probing the device's rates again. An asserted
/// cap is not passed: the worker probes, or reads it from `max_rate_hz=`.
pub fn worker_args(id: &str, args: &str, cached: Option<&Capabilities>) -> Vec<String> {
    let mut out = ["soapy-worker", "--id", id, "--args", args]
        .map(String::from)
        .to_vec();
    if let Some(Claim::Measured(Hz(cap))) = cached.map(|c| &c.max_bandwidth_hz) {
        out.extend(["--rate-cap-hz".into(), cap.to_string()]);
    }
    out
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
    /// Taken by the first [`WorkerPort::close`] or by `Drop`.
    cancel: std::sync::Mutex<Option<oneshot::Sender<()>>>,
    /// Turns true once the supervisor has returned, its worker reaped.
    done: watch::Receiver<bool>,
    /// How long [`WorkerPort::close`] waits for `done`.
    close_bound: Duration,
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
        let (done_tx, done) = watch::channel(false);
        let close_bound = cfg.timings.shutdown_grace.saturating_add(CLOSE_SLACK);
        let supervisor = Supervisor::new(cfg, shared.clone(), healed_tx, cached);
        tokio::spawn(async move {
            supervisor.run(cancel_rx, first_tx).await;
            done_tx.send_replace(true);
        });
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
            cancel: std::sync::Mutex::new(Some(cancel)),
            done,
            close_bound,
        };
        Ok((Arc::new(port), attach))
    }

    /// Shut the worker down and wait until it is reaped: `shutdown` on its
    /// stdin, the grace to exit (the device closes on the way out), then the
    /// kill. Call it before the process exits: the shutdown `Drop` starts runs
    /// on the supervisor task, which a runtime tearing down aborts midway,
    /// leaving the child to the kill-on-drop SIGKILL and the device mid-stream.
    /// It works while other references to the port remain (the node holds
    /// one), is bounded by the shutdown grace plus a second, and a second call
    /// only waits for the first.
    pub async fn close(&self) {
        let cancel = self.cancel.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(c) = cancel {
            let _ = c.send(());
        }
        let mut done = self.done.clone();
        if tokio::time::timeout(self.close_bound, done.wait_for(|d| *d))
            .await
            .is_err()
        {
            eprintln!(
                "zeta: soapy worker for {}: not shut down within {} s",
                self.id.0,
                self.close_bound.as_secs_f64()
            );
        }
    }

    /// The inode of the live worker's ring memfd, so a test can tell one
    /// spawn's ring from the next; `None` without a live ring.
    #[cfg(feature = "test-bin")]
    pub fn ring_ino(&self) -> Option<u64> {
        use std::os::fd::AsRawFd;
        let ring = self.shared.snapshot().ring.clone()?;
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `fstat` writes a `stat` into the buffer and nothing else; the
        // descriptor is the ring's, alive while `ring` is held, and the buffer
        // is read only when the call succeeded.
        let st = unsafe {
            if libc::fstat(ring.fd().as_raw_fd(), st.as_mut_ptr()) < 0 {
                return None;
            }
            st.assume_init()
        };
        Some(st.st_ino)
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
    async fn request(&self, cmd: Cmd, what: &str) -> Result<(Reply, Arc<Link>), PortError> {
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
            Ok(Ok(r)) => r.map(|r| (r, link)),
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

/// Record a tune the worker on `link` confirmed: the band, and the faults
/// cleared, as a successful tune heals a source. Only while that worker is
/// still the live one: a reply read just before its worker died can reach
/// here after the reap set the fault the death left (or after a fresh worker
/// reported a fault of its own), and must not wipe it.
fn settle_tune(shared: &Shared, link: &Arc<Link>, got: Tuning) {
    let mut s = shared.snapshot();
    if s.link.as_ref().is_some_and(|l| Arc::ptr_eq(l, link)) {
        s.current = Some(got);
        s.worker_fault = None;
        s.supervision_fault = None;
    }
}

/// Record a gain the worker on `link` confirmed, while it is the live one
/// (see [`settle_tune`]); the next worker reports its own in its hello.
fn settle_gain(shared: &Shared, link: &Arc<Link>, g: GainInfo) {
    let mut s = shared.snapshot();
    if s.link.as_ref().is_some_and(|l| Arc::ptr_eq(l, link)) {
        s.gain = Some(g);
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
        let cancel = self.cancel.get_mut().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = cancel.take() {
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
            (Reply::Tuning(got), link) => {
                settle_tune(&self.shared, &link, got);
                Ok(got)
            }
            (r, _) => Err(unexpected(&self.id, "tune", &r)),
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
            (Reply::Gain(g), link) => {
                settle_gain(&self.shared, &link, g);
                Ok(g)
            }
            (r, _) => Err(unexpected(&self.id, "set_gain", &r)),
        }
    }

    fn healed(&self) -> Option<watch::Receiver<u64>> {
        Some(self.healed.clone())
    }

    fn heal_settled(&self) {
        self.shared.snapshot().healing_fault = None;
    }

    fn worker(&self) -> Option<WorkerInfo> {
        let s = self.shared.snapshot();
        Some(WorkerInfo {
            pid: s.pid,
            restarts: s.restarts,
            since_ns: s.since_ns,
            transport: s.transport.clone(),
            transport_dropped: s.transport_dropped,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeta_core::types::Hz;

    /// A link over the stdin of a `cat` child: the settle functions only
    /// compare links, nothing is written.
    fn link() -> (tokio::process::Child, Arc<Link>) {
        let mut child = tokio::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        (child, Arc::new(Link::new(stdin)))
    }

    fn tuning() -> Tuning {
        Tuning {
            center_hz: Hz(100_000_000),
            bandwidth_hz: Hz(2_000_000),
        }
    }

    #[tokio::test]
    async fn a_late_reply_from_a_reaped_worker_leaves_its_fault() {
        let shared = Shared::new(SourceId("f".into()));
        let (_old_child, old) = link();
        let (_new_child, new) = link();
        // The worker the tune went through died and was reaped: no link, the
        // fault its end left.
        shared.snapshot().supervision_fault = Some("died".into());
        settle_tune(&shared, &old, tuning());
        let g = GainInfo {
            db: Some(10.0),
            min_db: 0.0,
            max_db: 60.0,
            automatic: false,
            automatic_available: true,
        };
        settle_gain(&shared, &old, g);
        {
            let s = shared.snapshot();
            assert_eq!(s.fault().as_deref(), Some("died"));
            assert_eq!(s.current, None);
            assert_eq!(s.gain, None);
        }
        // A fresh worker is live now and reports a fault of its own: the old
        // worker's reply must not clear it either.
        {
            let mut s = shared.snapshot();
            s.link = Some(new.clone());
            s.worker_fault = Some("overflow".into());
        }
        settle_tune(&shared, &old, tuning());
        assert_eq!(shared.snapshot().fault().as_deref(), Some("overflow"));
        // The live worker's own reply heals.
        settle_tune(&shared, &new, tuning());
        settle_gain(&shared, &new, g);
        let s = shared.snapshot();
        assert_eq!(s.fault(), None);
        assert_eq!(s.supervision_fault, None);
        assert_eq!(s.current, Some(tuning()));
        assert_eq!(s.gain, Some(g));
    }
}
