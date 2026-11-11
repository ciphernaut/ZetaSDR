//! The KiwiSDR [`Port`]: one remote receiver channel in IQ mode.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use zeta_core::port::{
    IqBlock, IqReceiver, Port, PortError, SWEEP_ROWS_CAPACITY, SweepControl, SweepRequest,
    SweepRow, SweepRows, SweepState, Tuning,
};
use zeta_core::types::{
    Capabilities, Claim, Hz, SampleFormat, SourceId, SourcePosition, SweepCaps,
};

use crate::session::{self, Ctx, Status};
use crate::status::KIWI_STATUS_PATH;
use crate::wf::{self, WF_END_WAIT_MS, WF_FPS, WF_SETTLE_FRAMES, WF_ZOOM_MAX};

/// Highest centre frequency a KiwiSDR tunes, in Hz.
pub(crate) const TUNE_MAX_HZ: u64 = 30_000_000;
/// Widest IQ passband: `±6000` Hz around the centre.
pub(crate) const MAX_BANDWIDTH_HZ: u64 = 12_000;
/// Default delay after a new centre is sent before the session delivers
/// frames again, in ms.
pub(crate) const TUNE_SETTLE_MS: u64 = 250;
/// Blocks buffered per receiver.
const BROADCAST_CAPACITY: usize = 64;

/// Where and how to reach one KiwiSDR. `Debug` and `Display` never show the
/// password.
#[derive(Clone)]
pub struct KiwiConfig {
    pub host: String,
    pub port: u16,
    pub password: Option<String>,
    /// Waits before each reconnect after a transport failure, in order.
    pub reconnect_delays_ms: Vec<u64>,
    /// A connection silent for this long is treated as lost.
    pub read_timeout_ms: u64,
    /// Bound on connecting plus the `sample_rate`/`audio_rate` handshake.
    pub handshake_timeout_ms: u64,
    /// How long after a new centre is sent the session drops `SND` frames:
    /// their header carries no frequency, so frames in flight (and the
    /// filter's ring-down) cannot be told from the new band's. Default
    /// [`TUNE_SETTLE_MS`].
    pub tune_settle_ms: u64,
    /// Frames of a sweep's new view dropped after the first one whose header
    /// names it (the server's down-converter may still hold the old view's
    /// samples). Default [`WF_SETTLE_FRAMES`].
    pub wf_settle_frames: u32,
    /// The receiver's status page, read once per connection for its position
    /// (bounded by `handshake_timeout_ms`); `None` reads nothing.
    pub status_url: Option<String>,
}

impl KiwiConfig {
    /// Defaults: reconnect after 1, 2, 4, 8 and 16 s; 10 s read and handshake
    /// timeouts; the status page at `http://<host>:<port>/status`. A password with a space, `=` or control character cannot be
    /// sent in `SET auth` and is refused (the error does not echo it).
    pub fn new(
        host: impl Into<String>,
        port: u16,
        password: Option<String>,
    ) -> Result<Self, String> {
        if password
            .as_deref()
            .is_some_and(|p| p.chars().any(|c| c == ' ' || c == '=' || c.is_control()))
        {
            return Err("kiwi password must not contain a space, '=' or control character".into());
        }
        let host = host.into();
        Ok(Self {
            status_url: Some(format!("http://{host}:{port}{KIWI_STATUS_PATH}")),
            host,
            port,
            password,
            reconnect_delays_ms: vec![1000, 2000, 4000, 8000, 16000],
            read_timeout_ms: 10_000,
            handshake_timeout_ms: 10_000,
            tune_settle_ms: TUNE_SETTLE_MS,
            wf_settle_frames: WF_SETTLE_FRAMES,
        })
    }
}

impl fmt::Debug for KiwiConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KiwiConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field(
                "password",
                &format_args!(
                    "{}",
                    self.password.as_ref().map_or("None", |_| "<redacted>")
                ),
            )
            .field("reconnect_delays_ms", &self.reconnect_delays_ms)
            .field("read_timeout_ms", &self.read_timeout_ms)
            .field("handshake_timeout_ms", &self.handshake_timeout_ms)
            .field("tune_settle_ms", &self.tune_settle_ms)
            .field("wf_settle_frames", &self.wf_settle_frames)
            .field("status_url", &self.status_url)
            .finish()
    }
}

impl fmt::Display for KiwiConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "kiwi {}:{}", self.host, self.port)
    }
}

/// A live session task; dropping it cancels the task, which closes the socket.
struct Session {
    band: Tuning,
    cmds: mpsc::UnboundedSender<Tuning>,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// A sweep's task (`wf::run`); dropping it cancels the task, which closes the
/// `W/F` socket.
struct LiveSweep {
    /// The task's rows: the reader still holds them while the task runs and the
    /// receiver is open.
    rows: mpsc::WeakSender<SweepRow>,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl LiveSweep {
    /// Whether the sweep still runs for a reader: its task not done with its
    /// rows, and their receiver not dropped.
    fn held(&self) -> bool {
        self.rows.upgrade().is_some_and(|tx| !tx.is_closed())
    }

    /// Cancel the task and wait for it to close its socket, bounded by
    /// [`WF_END_WAIT_MS`]; past that it is aborted, which drops the socket.
    /// Awaited in place: a caller dropped meanwhile leaves the handle where the
    /// next `sweep` finds it and waits again.
    async fn end(&mut self) {
        self.cancel.cancel();
        let wait = Duration::from_millis(WF_END_WAIT_MS);
        if tokio::time::timeout(wait, &mut self.task).await.is_err() {
            self.task.abort();
        }
    }
}

impl Drop for LiveSweep {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// A KiwiSDR receiver channel as a push [`Port`]. Nothing touches the network
/// until the first `tune`; dropping the source closes its session.
///
/// Blocks are stamped with the nominal `audio_rate` (the capture band's
/// width); the true rate is [`KiwiSource::reported_sample_rate`]. The
/// broadcast lives as long as the source, across sessions and faults, so
/// receivers taken before a fault (the node's ring feeder) resume after a
/// retune; a session's end is reported through [`Port::fault`], never by
/// closing the stream.
///
/// A sweep ([`Port::sweep`]) runs on a `W/F` socket of its own beside the IQ
/// session, one at a time, and touches nothing of it (`wf`'s module doc).
pub struct KiwiSource {
    id: SourceId,
    caps: Capabilities,
    cfg: Arc<KiwiConfig>,
    tx: broadcast::Sender<IqBlock>,
    status: Arc<Mutex<Status>>,
    session: Mutex<Option<Session>>,
    /// Serialises `tune`, which waits on the network.
    gate: tokio::sync::Mutex<()>,
    /// The sweep, one at a time; the lock serialises `sweep` and never `tune`.
    sweep: tokio::sync::Mutex<Option<LiveSweep>>,
}

impl KiwiSource {
    pub fn new(id: SourceId, cfg: KiwiConfig) -> Self {
        let caps = Capabilities {
            tune_min_hz: Claim::Asserted(Hz(0)),
            tune_max_hz: Claim::Asserted(Hz(TUNE_MAX_HZ)),
            max_bandwidth_hz: Claim::Asserted(Hz(MAX_BANDWIDTH_HZ)),
            formats: vec![SampleFormat::Ci16],
            channels: 1,
            rx: true,
            tx: false,
            remote: true,
            push: true,
            usable_fraction: Claim::Asserted(1.0),
            depths: vec![],
            // Asserted: the status page names no waterfall channels, so a
            // refused `sweep` is what corrects a wrong hope. The finest pixel is
            // zoom 10's, 30 MHz / 2^10 / 1024.
            sweep: Some(SweepCaps {
                min_hz: Hz(0),
                max_hz: Hz(TUNE_MAX_HZ),
                bin_hz_min: Hz((TUNE_MAX_HZ >> WF_ZOOM_MAX).div_ceil(1024)),
                rows_per_s_max: WF_FPS,
                calibrated: true,
            }),
        };
        Self {
            id,
            caps,
            cfg: Arc::new(cfg),
            tx: broadcast::channel(BROADCAST_CAPACITY).0,
            status: Arc::new(Mutex::new(Status::default())),
            session: Mutex::new(None),
            gate: tokio::sync::Mutex::new(()),
            sweep: tokio::sync::Mutex::new(None),
        }
    }

    /// The nominal integer rate (`MSG audio_rate=`), once a session has it.
    pub fn audio_rate_hz(&self) -> Option<u32> {
        self.status.lock().unwrap().audio_rate
    }

    /// The true IQ rate the Kiwi reports (`MSG sample_rate=`).
    pub fn reported_sample_rate(&self) -> Option<f64> {
        self.status.lock().unwrap().sample_rate
    }

    /// The S-meter of the latest frame.
    pub fn rssi_dbm(&self) -> Option<f32> {
        self.status.lock().unwrap().rssi_dbm
    }

    /// Frames flagged with an ADC overflow.
    pub fn adc_overflows(&self) -> u64 {
        self.status.lock().unwrap().adc_overflows
    }

    /// Breaks in the frame sequence numbers (lost frames).
    pub fn seq_gaps(&self) -> u64 {
        self.status.lock().unwrap().seq_gaps
    }

    /// Status page reads that failed (each leaves the position as it was).
    pub fn status_errors(&self) -> u64 {
        self.status.lock().unwrap().status_errors
    }

    fn io(&self, reason: &str) -> PortError {
        PortError::Io(format!("{}: {reason}", self.cfg))
    }

    /// The band on the live, unfaulted session, sending the tuning commands
    /// when it changed; `None` when a new session is needed.
    fn retune_live(&self, t: Tuning) -> Option<Tuning> {
        let mut session = self.session.lock().unwrap();
        let s = session.as_mut()?;
        let (fault, rate) = {
            let st = self.status.lock().unwrap();
            (st.fault.is_some(), st.audio_rate)
        };
        let rate = rate.filter(|_| !fault && !s.task.is_finished())?;
        // The passband is always the full capture band, so only a new centre
        // needs commands.
        if s.band.center_hz != t.center_hz {
            s.cmds.send(t).ok()?;
            s.band = t;
        }
        let cur = Tuning {
            center_hz: t.center_hz,
            bandwidth_hz: Hz(u64::from(rate)),
        };
        self.status.lock().unwrap().current = Some(cur);
        Some(cur)
    }
}

impl Drop for KiwiSource {
    fn drop(&mut self) {
        // Dropping the session cancels it; it sends a close frame and exits.
        self.session.lock().unwrap().take();
        // So does the sweep's task.
        self.sweep.get_mut().take();
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

#[async_trait]
impl Port for KiwiSource {
    fn id(&self) -> &SourceId {
        &self.id
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    /// Opens a session when there is none or the port is faulted (clearing
    /// the fault once the Kiwi has answered `sample_rate` and `audio_rate`);
    /// on a live session sends the tuning commands unless the centre is
    /// unchanged (the passband is always the full capture band). After a new
    /// centre is sent, the session drops `SND` frames for `tune_settle_ms`
    /// (the header carries no frequency), so the first block delivered with
    /// that centre holds only samples taken after the tune settled; the first
    /// tune of a connection drops nothing. Returns the capture band: the requested centre, `audio_rate`
    /// wide.
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        let bw = t.bandwidth_hz.0;
        if !(1..=MAX_BANDWIDTH_HZ).contains(&bw) {
            return Err(out_of_range("bandwidth_hz", bw, 1, MAX_BANDWIDTH_HZ));
        }
        if t.center_hz.0 > TUNE_MAX_HZ {
            return Err(out_of_range("center_hz", t.center_hz.0, 0, TUNE_MAX_HZ));
        }
        let _gate = self.gate.lock().await;
        if let Some(cur) = self.retune_live(t) {
            return Ok(cur);
        }

        // A new session. The old one (finished or faulted) is cancelled.
        self.session.lock().unwrap().take();
        {
            let mut st = self.status.lock().unwrap();
            st.fault = None;
            st.audio_rate = None;
            st.sample_rate = None;
            st.freq_offset_khz = 0.0;
        }
        let (cmds, cmds_rx) = mpsc::unbounded_channel();
        let (ready_tx, ready) = oneshot::channel();
        let cancel = CancellationToken::new();
        let ctx = Ctx {
            cfg: self.cfg.clone(),
            tx: self.tx.clone(),
            status: self.status.clone(),
            cancel: cancel.clone(),
        };
        let task = tokio::spawn(session::run(ctx, t, cmds_rx, ready_tx));
        *self.session.lock().unwrap() = Some(Session {
            band: t,
            cmds,
            cancel,
            task,
        });

        let wait = Duration::from_millis(self.cfg.handshake_timeout_ms);
        let reason = match tokio::time::timeout(wait, ready).await {
            Ok(Ok(Ok(()))) => None,
            Ok(Ok(Err(reason))) => Some(reason),
            Ok(Err(_)) => Some("session ended before the handshake".to_string()),
            Err(_) => Some(format!(
                "no sample_rate and audio_rate within {} ms",
                self.cfg.handshake_timeout_ms
            )),
        };
        if let Some(reason) = reason {
            self.session.lock().unwrap().take();
            self.status.lock().unwrap().fault = Some(reason.clone());
            return Err(self.io(&reason));
        }
        let mut st = self.status.lock().unwrap();
        if st.freq_offset_khz != 0.0 {
            let off = st.freq_offset_khz;
            drop(st);
            self.session.lock().unwrap().take();
            return Err(PortError::Unsupported(format!(
                "kiwi freq_offset {off} kHz not supported"
            )));
        }
        let rate = st.audio_rate.unwrap_or_default();
        let cur = Tuning {
            center_hz: t.center_hz,
            bandwidth_hz: Hz(u64::from(rate)),
        };
        st.current = Some(cur);
        Ok(cur)
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

    /// The last successful status page read; `None` before the first.
    fn position(&self) -> Option<SourcePosition> {
        self.status.lock().unwrap().position
    }

    /// Opens a `W/F` socket of its own (never paired with the IQ session's) and
    /// answers once its setup has arrived: the plan, from the Kiwi's own
    /// `bandwidth`. A span outside 0-30 MHz (or the Kiwi's band) is
    /// `OutOfRange`; while another sweep's rows are held it is refused (one
    /// waterfall connection), but a sweep whose rows were dropped is ended first
    /// (its close awaited) and does not refuse this one. The Kiwi's refusals
    /// (`too_busy`, no waterfall channels, `badp`, `down`, a locked Kiwi, a
    /// close before the setup, a frequency offset) are `Unsupported` with the
    /// reason, transport failures `Io`; none touches the IQ session or
    /// [`Port::fault`]. The connect and handshake are bounded by
    /// `handshake_timeout_ms`, at most `wf::WF_HANDSHAKE_MAX_MS`.
    ///
    /// The rows carry a control: [`SweepRows::retarget`] re-aims the sweep on its
    /// open socket (one `SET zoom= cf=`, no reconnect), refusing a span outside the
    /// band as this call does. A plan of one view holds it (no odd-sweep shift):
    /// a row every frame the dwell takes, at every zoom.
    async fn sweep(&self, req: SweepRequest) -> Result<SweepRows, PortError> {
        wf::span_within(req.span, TUNE_MAX_HZ)?;
        let mut live = self.sweep.lock().await;
        if live.as_ref().is_some_and(LiveSweep::held) {
            return Err(PortError::Unsupported(
                "a sweep is already running on this Kiwi (one waterfall connection)".into(),
            ));
        }
        // Never two sockets: the last sweep's (its rows dropped, or ended) is
        // closed before this one connects.
        if let Some(old) = live.as_mut() {
            old.end().await;
        }
        *live = None;
        let (rows_tx, rows) = mpsc::channel(SWEEP_ROWS_CAPACITY);
        let (state_tx, state) = watch::channel(SweepState::Running);
        let (ready_tx, ready) = oneshot::channel();
        let (control, control_rx) = SweepControl::channel();
        let cancel = CancellationToken::new();
        let weak = rows_tx.downgrade();
        let ctx = wf::Ctx {
            cfg: self.cfg.clone(),
            cancel: cancel.clone(),
            rows: rows_tx,
            state: state_tx,
        };
        let task = tokio::spawn(wf::run(ctx, req, ready_tx, control_rx));
        *live = Some(LiveSweep {
            rows: weak,
            cancel,
            task,
        });
        // The task answers within its handshake bound; this is a backstop.
        let ms = wf::handshake_ms(&self.cfg);
        let wait = Duration::from_millis(ms + 500);
        let answer = match tokio::time::timeout(wait, ready).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(self.io("waterfall session ended before its setup")),
            Err(_) => Err(self.io(&format!("no wf_setup within {ms} ms"))),
        };
        match answer {
            Ok(plan) => Ok(SweepRows {
                plan: plan.sweep_plan(),
                rows,
                state,
                control: Some(control),
            }),
            Err(e) => {
                // The task ends by itself (its rows dropped here); its close is
                // awaited by the next sweep.
                if let Some(l) = live.as_ref() {
                    l.cancel.cancel();
                }
                Err(e)
            }
        }
    }
}
