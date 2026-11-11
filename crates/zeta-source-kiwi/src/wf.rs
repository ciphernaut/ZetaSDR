//! The KiwiSDR waterfall sweep: its plan (which zoom, which views, how long a
//! dwell) and its session (one `W/F` websocket, stepping the view across the span).
//!
//! The plan is pure arithmetic over the server's facts (`docs/kiwi-waterfall.md`):
//! a frame is 1024 pixels over a view of `bandwidth >> zoom` Hz starting at an
//! `x_bin` that counts in `bandwidth / (1024 << zoom_max)` Hz, the server zeroes
//! the first pixels, and up to zoom 10 a frame is sampled after the change that
//! asked for it.
//!
//! **The `W/F` socket always stands alone.** A Kiwi pairs an `SND` and a `W/F`
//! whose path timestamps match into one user, and closing a paired `W/F` stops its
//! `SND` (`docs/kiwi-waterfall.md`, "Socket and slot rules"). So the sweep's socket
//! carries a timestamp of its own ([`wf_ts`]: never an `SND` timestamp of this
//! process, and never a previous `W/F`'s, which the server would drop silently),
//! costs one user slot of the Kiwi beyond the IQ session's while a sweep runs and
//! none otherwise, and never reads or writes the IQ session's state: a sweep opens,
//! fails and ends with the demodulator beside it untouched.
//!
//! Per connection, [`run`] sends `SET auth`; on `wf_setup` (with `bandwidth`) it
//! sends the setup commands and the first view, then steps: a frame belongs to the
//! step when its zoom is the plan's and its `x_bin` the asked view's; the first
//! `wf_settle_frames` that belong after a change are dropped, the next
//! `plan.frames` averaged in linear power into one [`SweepRow`]. A step with no row
//! after [`WF_STEP_SPARE_FRAMES`] frame times more than it needs is skipped; a
//! sweep of skips, ten bad frames in a row, a close or silence lose the connection,
//! which is retried after each of `reconnect_delays_ms` (the budget reset by a
//! whole sweep delivered) while the state says `Waiting`. A refusal on a reconnect,
//! or the budget spent, ends the sweep (`Ended`, then the rows close). The rows'
//! receiver dropped, or the source dropped, closes the socket cleanly.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, interval_at, sleep, sleep_until, timeout_at};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_util::sync::CancellationToken;
use zeta_core::port::{PortError, SweepPlan, SweepRequest, SweepRow, SweepState, Tuning};
use zeta_core::types::{Hz, Span, Timestamp};

use crate::proto::{
    CMD_IDENT, CMD_KEEPALIVE, CMD_WF_COMP_OFF, CMD_WF_DB, CMD_WF_INTERP, CMD_WF_SPEED, KiwiMsg,
    Tag, WF_PIXELS, Wf, cmd_auth, cmd_wf_view, parse_msg, parse_wf, tag, wf_url,
};
use crate::session::{
    MAX_BAD_SND, MAX_FRAME_BYTES, MAX_MSG_BYTES, Sink, badp_meaning, close, path_ts,
};
use crate::source::KiwiConfig;

/// The finest zoom the sweep uses: above it the server samples overlapped.
pub(crate) const WF_ZOOM_MAX: u8 = 10;
/// Frames a second at `wf_speed=4`.
pub(crate) const WF_FPS: u32 = 23;
/// Pixels the odd sweeps shift the views by, over the server's zeroed pixels (4 at
/// most, at zoom 0, where nothing shifts).
pub(crate) const WF_SHIFT_PIXELS: u64 = 8;
/// Frames dropped after the first one that names a new view.
pub(crate) const WF_SETTLE_FRAMES: u32 = 1;

/// Pixels as `u64`, for arithmetic on Hz.
const PIXELS: u64 = WF_PIXELS as u64;

/// A sweep's zoom, views and dwell.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WfPlan {
    pub zoom: u8,
    /// The width of one view, Hz.
    pub view_hz: u64,
    /// The width of one pixel, Hz.
    pub bin_hz: f64,
    /// The first Hz of each view of an even sweep, ascending and distinct.
    pub starts: Vec<u64>,
    /// How far an odd sweep's views sit below an even sweep's, Hz.
    pub shift_hz: u64,
    /// Frames read at each view.
    pub frames: u32,
    /// The dwell those frames make, ms.
    pub dwell_ms: u64,
}

impl WfPlan {
    /// The first Hz of the view at `step` in sweep number `sweep`: odd sweeps
    /// sit `shift_hz` lower (never under 0), so a pixel the server zeroed in the
    /// even sweep is read clean in the odd one.
    pub(crate) fn start(&self, sweep: u64, step: usize) -> u64 {
        let start = self.starts[step];
        if sweep % 2 == 1 {
            start.saturating_sub(self.shift_hz)
        } else {
            start
        }
    }

    /// The server's `x_bin` of a view starting at `start_hz`:
    /// `round(start × (1024 << zoom_max) / bandwidth)`.
    pub(crate) fn x_bin(&self, start_hz: u64, bandwidth_hz: u64, zoom_max: u8) -> u32 {
        let num = u128::from(start_hz) * (u128::from(PIXELS) << zoom_max);
        let den = u128::from(bandwidth_hz.max(1));
        ((2 * num + den) / (2 * den)).min(u128::from(u32::MAX)) as u32
    }
}

/// Plans a sweep of `span` at pixels of about `bin_hz` or finer, dwelling about
/// `dwell_ms` at each view, on a Kiwi whose ADC covers `bandwidth_hz`.
///
/// The zoom is the larger of the coarsest one whose pixel is at or under
/// `bin_hz` and the finest one whose view still holds the whole span, never over
/// [`WF_ZOOM_MAX`]. A view that holds the span is centred on it; otherwise the
/// views tile it from its low edge. Every start is clamped to the band and a
/// repeated start is dropped.
pub(crate) fn wf_plan(span: Tuning, bin_hz: u64, dwell_ms: u64, bandwidth_hz: u64) -> WfPlan {
    let view = |z: u8| bandwidth_hz >> z;
    let width = span.bandwidth_hz.0;
    let z_target = (0..=WF_ZOOM_MAX)
        .find(|z| view(*z) <= bin_hz.saturating_mul(PIXELS))
        .unwrap_or(WF_ZOOM_MAX);
    let z_fit = (0..=WF_ZOOM_MAX)
        .rev()
        .find(|z| view(*z) >= width)
        .unwrap_or(0);
    let zoom = z_target.max(z_fit);
    let view_hz = view(zoom);
    let lo = span.center_hz.0.saturating_sub(width / 2);
    let raw: Vec<u64> = if view_hz >= width {
        vec![span.center_hz.0.saturating_sub(view_hz / 2)]
    } else {
        (0..width.div_ceil(view_hz))
            .map(|k| lo.saturating_add(k * view_hz))
            .collect()
    };
    let last = bandwidth_hz.saturating_sub(view_hz);
    let mut starts: Vec<u64> = Vec::with_capacity(raw.len());
    for start in raw.into_iter().map(|s| s.min(last)) {
        if !starts.contains(&start) {
            starts.push(start);
        }
    }
    let frames = (dwell_ms * u64::from(WF_FPS))
        .div_ceil(1000)
        .clamp(1, u64::from(u32::MAX)) as u32;
    WfPlan {
        zoom,
        view_hz,
        bin_hz: view_hz as f64 / WF_PIXELS as f64,
        starts,
        // Zoom 0 is the whole band: no view to shift to.
        shift_hz: if zoom == 0 {
            0
        } else {
            WF_SHIFT_PIXELS * view_hz / PIXELS
        },
        frames,
        dwell_ms: (u64::from(frames) * 2000 + u64::from(WF_FPS)) / (2 * u64::from(WF_FPS)),
    }
}

impl WfPlan {
    /// The plan as the node sees it.
    pub(crate) fn sweep_plan(&self) -> SweepPlan {
        SweepPlan {
            steps: u32::try_from(self.starts.len()).unwrap_or(u32::MAX),
            step_hz: Hz(self.view_hz),
            size: WF_PIXELS as u32,
            bin_hz: self.bin_hz,
            dwell_ms: self.dwell_ms,
            shift_hz: Hz(self.shift_hz),
        }
    }
}

// ---- The session ----

/// The longest a `W/F` connect and handshake may take, whatever
/// `handshake_timeout_ms` says, so `sweep` answers well inside the node's
/// [`zeta_core::ops::SWEEP_START_TIMEOUT_MS`] (5 s) even after waiting for a
/// previous sweep's close ([`WF_END_WAIT_MS`]).
pub(crate) const WF_HANDSHAKE_MAX_MS: u64 = 3_000;
/// How long a new sweep waits for the previous sweep's task to end: its close is
/// bounded by 1 s; past this the task is aborted (its socket dropped).
pub(crate) const WF_END_WAIT_MS: u64 = 1_200;
/// Frame times a step waits beyond its settle and dwell before it is skipped.
pub(crate) const WF_STEP_SPARE_FRAMES: u32 = 10;
/// How far a frame's `x_bin` may sit from the asked view's: the server parses
/// `cf` as a 32-bit float (a 2 Hz unit at 30 MHz, over one `x_bin`) and truncates
/// the start, at most 2 units off in an emulation of its maths; 4 leaves a margin.
/// Two views of one zoom, an odd sweep's shifted ones included, are at least 128
/// units apart (at zoom 10).
const WF_X_BIN_SLACK: u32 = 4;
/// The server's finest zoom when the setup does not say (`zoom_max=14`).
const ZOOM_MAX_DEFAULT: u8 = 14;
/// Longest redirect target echoed in a reason.
const MAX_REDIRECT_CHARS: usize = 200;

/// The connect-and-handshake bound in force: `handshake_timeout_ms`, at most
/// [`WF_HANDSHAKE_MAX_MS`].
pub(crate) fn handshake_ms(cfg: &KiwiConfig) -> u64 {
    cfg.handshake_timeout_ms.min(WF_HANDSHAKE_MAX_MS)
}

/// A `W/F` path timestamp: the `SND` one with its top bit flipped, so never the
/// timestamp of an `SND` socket of this process (a Kiwi would pair the two).
pub(crate) fn wf_ts() -> u32 {
    path_ts() ^ 0x8000_0000
}

/// The last `W/F` timestamp this process used.
static LAST_WF_TS: AtomicU32 = AtomicU32::new(0);

/// [`wf_ts`], or one past the last `W/F` timestamp used when that is not
/// earlier: two `W/F` sockets of one timestamp, the second is dropped silently
/// by the server, and a sweep restarted within the second would be.
fn fresh_wf_ts() -> u32 {
    let ts = wf_ts();
    let next = |last: u32| if ts > last { ts } else { last.wrapping_add(1) };
    let last = LAST_WF_TS
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |l| Some(next(l)))
        .unwrap_or_else(|l| l);
    next(last)
}

/// Whether `span` lies inside `0..=max_hz` (both edges, `center ± bandwidth / 2`,
/// as [`zeta_core::types::SweepCaps::holds`] reads them), and is not empty.
pub(crate) fn span_within(span: Tuning, max_hz: u64) -> Result<(), PortError> {
    let (center, width) = (span.center_hz.0, span.bandwidth_hz.0);
    let out = |field, value, min, max| PortError::OutOfRange {
        field,
        value,
        min,
        max,
        suggested: None,
    };
    if !(1..=max_hz).contains(&width) {
        return Err(out("bandwidth_hz", width, 1, max_hz));
    }
    let half = width / 2;
    if center < half || center.saturating_add(half) > max_hz {
        return Err(out("center_hz", center, half, max_hz - half));
    }
    Ok(())
}

/// What the waterfall's handshake has said so far, and the `wf_cal` in force.
#[derive(Debug, Default)]
pub(crate) struct Setup {
    pub bandwidth: Option<u64>,
    pub zoom_max: Option<u8>,
    pub wf_chans: Option<u32>,
    /// The last `wf_cal`, dB added to every pixel.
    pub wf_cal: i32,
    pub freq_offset_khz: f64,
    /// `wf_setup` has arrived.
    setup: bool,
    /// The setup was reported complete.
    complete: bool,
}

impl Setup {
    /// One `MSG` payload: `Some(Ok)` the first time the setup is complete
    /// (`wf_setup` and `bandwidth` both arrived, and nothing refuses the sweep),
    /// `Some(Err)` a refusal with its reason; `None` otherwise (a malformed
    /// `MSG` is skipped).
    pub fn on_msg(&mut self, payload: &[u8]) -> Option<Result<(), String>> {
        let msgs = parse_msg(payload).ok()?;
        for m in msgs {
            match m {
                KiwiMsg::BadPassword(0) => {}
                KiwiMsg::BadPassword(n) => {
                    return Some(Err(format!("badp={n}: {}", badp_meaning(n))));
                }
                KiwiMsg::TooBusy(n) => {
                    return Some(Err(format!(
                        "no waterfall channel free (too_busy: {n} channels)"
                    )));
                }
                KiwiMsg::Down => return Some(Err("down: server is down".into())),
                KiwiMsg::Redirect(url) => {
                    let url: String = url.chars().take(MAX_REDIRECT_CHARS).collect();
                    return Some(Err(format!("redirect to {url} not supported")));
                }
                KiwiMsg::Other(o) if o == "exclusive_use" => {
                    return Some(Err(
                        "exclusive_use: the Kiwi is locked for exclusive use".into()
                    ));
                }
                KiwiMsg::FreqOffsetKhz(k) => self.freq_offset_khz = k,
                KiwiMsg::WfChans(n) => self.wf_chans = Some(n),
                KiwiMsg::WfCal(c) => self.wf_cal = c,
                KiwiMsg::ZoomMax(z) => self.zoom_max = Some(z),
                KiwiMsg::Bandwidth(b) => self.bandwidth = Some(b),
                KiwiMsg::WfSetup => self.setup = true,
                KiwiMsg::SampleRate(_) | KiwiMsg::AudioRate(_) | KiwiMsg::Other(_) => {}
            }
        }
        if self.complete || !self.setup {
            return None;
        }
        let bandwidth = self.bandwidth?;
        self.complete = true;
        Some(if self.wf_chans == Some(0) {
            Err("this Kiwi is configured without waterfall channels".into())
        } else if self.freq_offset_khz != 0.0 {
            // As `tune` refuses it: the rows would be painted at the wrong frequencies.
            Err(format!(
                "kiwi freq_offset {} kHz not supported",
                self.freq_offset_khz
            ))
        } else if bandwidth == 0 {
            Err("protocol: bandwidth 0".into())
        } else {
            Ok(())
        })
    }
}

/// What one frame did to the step.
#[derive(Debug, PartialEq)]
pub(crate) enum FrameOut {
    /// Not of the asked view: dropped.
    Other,
    /// Of the asked view, dropped to settle or kept for the dwell.
    Belongs,
    /// The step's row: the dwell's frames averaged.
    Row(SweepRow),
}

/// One connection's stepping through the plan's views: which view is asked,
/// which frames belong to it, and the dwell's average.
#[derive(Debug)]
pub(crate) struct Stepper {
    plan: WfPlan,
    bandwidth: u64,
    zoom_max: u8,
    settle: u32,
    sweep: u64,
    step: usize,
    /// The `x_bin` of the step's view.
    want: u32,
    /// The `x_bin` of the view last sent to the server.
    asked: Option<u32>,
    settle_left: u32,
    /// Linear power summed per pixel over the kept frames.
    acc: Vec<f64>,
    kept: u32,
    first_ns: u64,
    last_ns: u64,
    last_x: u32,
    /// Rows of the sweep under way.
    rows: usize,
}

impl Stepper {
    pub fn new(plan: WfPlan, bandwidth: u64, zoom_max: u8, settle: u32, sweep: u64) -> Self {
        Stepper {
            plan,
            bandwidth,
            zoom_max,
            settle,
            sweep,
            step: 0,
            want: 0,
            asked: None,
            settle_left: 0,
            acc: vec![0.0; WF_PIXELS],
            kept: 0,
            first_ns: 0,
            last_ns: 0,
            last_x: 0,
            rows: 0,
        }
    }

    /// The sweep a new connection carries on with: this one's, or the next when
    /// this one had begun.
    pub fn next_sweep(&self) -> u64 {
        if self.step > 0 || self.rows > 0 {
            self.sweep + 1
        } else {
            self.sweep
        }
    }

    /// Point the step at its view: the `SET zoom= cf=` to send, or `None` when
    /// the server shows that view already (nothing to settle then).
    pub fn view(&mut self) -> Option<String> {
        let start = self.plan.start(self.sweep, self.step);
        self.want = self.plan.x_bin(start, self.bandwidth, self.zoom_max);
        self.kept = 0;
        if self.asked == Some(self.want) {
            self.settle_left = 0;
            return None;
        }
        self.asked = Some(self.want);
        self.settle_left = self.settle;
        Some(cmd_wf_view(self.plan.zoom, start + self.plan.view_hz / 2))
    }

    /// One frame, arrived at `now_ns`; `wf_cal` is added to the row's levels.
    pub fn on_frame(&mut self, wf: &Wf, wf_cal: i32, now_ns: u64) -> FrameOut {
        if wf.zoom != self.plan.zoom || wf.x_bin.abs_diff(self.want) > WF_X_BIN_SLACK {
            return FrameOut::Other;
        }
        if self.settle_left > 0 {
            self.settle_left -= 1;
            return FrameOut::Belongs;
        }
        if self.kept == 0 {
            self.acc.fill(0.0);
            self.first_ns = now_ns;
        }
        for (a, d) in self.acc.iter_mut().zip(&wf.db) {
            *a += 10f64.powf(f64::from(*d) / 10.0);
        }
        self.kept += 1;
        self.last_ns = now_ns;
        self.last_x = wf.x_bin;
        if self.kept < self.plan.frames {
            return FrameOut::Belongs;
        }
        FrameOut::Row(self.row(wf_cal))
    }

    /// The kept frames' mean power per pixel, in dB with `wf_cal`, centred where
    /// the last frame's own `x_bin` says.
    fn row(&mut self, wf_cal: i32) -> SweepRow {
        let n = f64::from(self.kept.max(1));
        let db = self
            .acc
            .iter()
            .map(|a| (10.0 * (a / n).log10() + f64::from(wf_cal)) as f32)
            .collect();
        let units = u128::from(WF_PIXELS as u64) << self.zoom_max;
        let bw = u128::from(self.bandwidth);
        let start = (2 * u128::from(self.last_x) * bw + units) / (2 * units);
        let center = start as u64 + self.plan.view_hz / 2;
        self.kept = 0;
        self.rows += 1;
        SweepRow {
            sweep: self.sweep,
            step: u32::try_from(self.step).unwrap_or(u32::MAX),
            center_hz: Hz(center),
            bin_hz: self.plan.bin_hz,
            db,
            masked: vec![[0, if self.plan.zoom == 0 { 4 } else { 2 }]],
            span: Span {
                start_ns: Timestamp(self.first_ns),
                end_ns: Timestamp(self.last_ns),
            },
        }
    }

    /// End the step (its row sent, or skipped) and point at the next: the view's
    /// command to send, and at a sweep's end its rows and steps.
    pub fn next(&mut self) -> (Option<String>, Option<(usize, usize)>) {
        self.step += 1;
        let mut end = None;
        if self.step >= self.plan.starts.len() {
            end = Some((self.rows, self.plan.starts.len()));
            self.rows = 0;
            self.step = 0;
            self.sweep += 1;
        }
        (self.view(), end)
    }

    /// How long the step may go without a row: its settle and dwell, and
    /// [`WF_STEP_SPARE_FRAMES`] more, in frame times.
    pub fn patience(&self) -> Duration {
        let frames = self.settle + self.plan.frames + WF_STEP_SPARE_FRAMES;
        Duration::from_micros(u64::from(frames) * 1_000_000 / u64::from(WF_FPS))
    }
}

/// Everything a sweep's task shares with its source.
pub(crate) struct Ctx {
    pub cfg: Arc<KiwiConfig>,
    pub cancel: CancellationToken,
    pub rows: mpsc::Sender<SweepRow>,
    pub state: watch::Sender<SweepState>,
}

/// How one connection ended.
enum End {
    /// Cancelled, the source dropped, or the rows dropped: nothing to report.
    Stop,
    /// The Kiwi refused the sweep: nothing to retry.
    Refused(PortError),
    /// A transport or stream failure; `whole` when the connection delivered a
    /// whole sweep (which resets the reconnect budget).
    Lost { err: String, whole: bool },
}

/// What [`run`] carries from one connection to the next.
struct Carry {
    /// The plan, from the first connection's `bandwidth`.
    plan: Option<(WfPlan, u64)>,
    sweep: u64,
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
}

fn reason_of(e: &PortError) -> String {
    match e {
        PortError::Unsupported(m) | PortError::Io(m) => m.clone(),
        e => e.to_string(),
    }
}

/// The sweep's task: connections until a stop, a refusal or the reconnect
/// budget spent, each `Ended` with its reason before the rows close. `ready`
/// answers the first connection's setup (the plan) or its failure to `sweep`;
/// before it, nothing is retried.
pub(crate) async fn run(
    ctx: Ctx,
    req: SweepRequest,
    ready: oneshot::Sender<Result<WfPlan, PortError>>,
) {
    let mut ready = Some(ready);
    let mut carry = Carry {
        plan: None,
        sweep: 0,
    };
    let mut attempts = 0usize;
    let stopped = || SweepState::Ended("the sweep was stopped".into());
    let reason = loop {
        let (end, sink) = connection(&ctx, req, &mut carry, &mut ready).await;
        // Answer `sweep` before the (up to 1 s) close, so its bound holds.
        let end = match (end, ready.take()) {
            (End::Refused(e), Some(r)) => {
                let _ = r.send(Err(e));
                End::Stop
            }
            (End::Lost { err, .. }, Some(r)) => {
                let _ = r.send(Err(PortError::Io(format!("{}: {err}", ctx.cfg))));
                End::Stop
            }
            (end, r) => {
                ready = r;
                end
            }
        };
        if let Some(sink) = sink {
            close(sink).await;
        }
        match end {
            End::Stop => break stopped(),
            End::Refused(e) => break SweepState::Ended(reason_of(&e)),
            End::Lost { err, whole } => {
                if whole {
                    attempts = 0;
                }
                let Some(&delay) = ctx.cfg.reconnect_delays_ms.get(attempts) else {
                    let why =
                        format!("connection lost ({err}); {attempts} reconnect attempts failed");
                    break SweepState::Ended(why);
                };
                attempts += 1;
                ctx.state
                    .send_replace(SweepState::Waiting(format!("connection lost ({err})")));
                tokio::select! {
                    _ = ctx.cancel.cancelled() => break stopped(),
                    _ = ctx.rows.closed() => break stopped(),
                    _ = sleep(Duration::from_millis(delay)) => {}
                }
            }
        }
    };
    // The state first: a reader that sees the rows close must already see why.
    ctx.state.send_replace(reason);
}

/// One `W/F` websocket, from connect to its end; the socket, when it is still to
/// be closed, goes back with the end.
async fn connection(
    ctx: &Ctx,
    req: SweepRequest,
    carry: &mut Carry,
    ready: &mut Option<oneshot::Sender<Result<WfPlan, PortError>>>,
) -> (End, Option<Sink>) {
    let cfg = &ctx.cfg;
    let lost = |err: String, whole: bool| End::Lost { err, whole };
    let hs_ms = handshake_ms(cfg);
    let handshake_deadline = Instant::now() + Duration::from_millis(hs_ms);
    let url = wf_url(&cfg.host, cfg.port, fresh_wf_ts());
    let ws_cfg = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME_BYTES))
        .max_frame_size(Some(MAX_FRAME_BYTES));
    let connect = tokio_tungstenite::connect_async_with_config(url, Some(ws_cfg), true);
    let ws = tokio::select! {
        _ = ctx.cancel.cancelled() => return (End::Stop, None),
        _ = ctx.rows.closed() => return (End::Stop, None),
        r = timeout_at(handshake_deadline, connect) => match r {
            Ok(Ok((ws, _))) => ws,
            Ok(Err(e)) => return (lost(format!("connect: {e}"), false), None),
            Err(_) => return (lost(format!("no wf_setup within {hs_ms} ms"), false), None),
        },
    };
    let (mut sink, mut stream) = ws.split();
    if let Err(e) = sink
        .send(Message::text(cmd_auth(cfg.password.as_deref())))
        .await
    {
        return (lost(format!("send: {e}"), false), None);
    }
    let second = Duration::from_secs(1);
    let mut keepalive = interval_at(Instant::now() + second, second);
    let read_timeout = Duration::from_millis(cfg.read_timeout_ms);
    let mut read_deadline = Instant::now() + read_timeout;
    let mut setup = Setup::default();
    let mut stepper: Option<Stepper> = None;
    let mut step_deadline = Instant::now();
    let mut bad: u32 = 0;
    // A whole sweep delivered on this connection.
    let mut whole = false;
    macro_rules! send {
        ($cmd:expr) => {
            if let Err(e) = sink.send(Message::text($cmd)).await {
                let err = format!("send: {e}");
                return (lost(err, whole), None);
            }
        };
    }
    macro_rules! end {
        ($end:expr) => {
            return ($end, Some(sink))
        };
    }
    loop {
        let msg = tokio::select! {
            _ = ctx.cancel.cancelled() => end!(End::Stop),
            _ = ctx.rows.closed() => end!(End::Stop),
            _ = keepalive.tick() => {
                send!(CMD_KEEPALIVE);
                continue;
            }
            _ = sleep_until(handshake_deadline), if stepper.is_none() => {
                end!(lost(format!("no wf_setup within {hs_ms} ms"), false))
            }
            _ = sleep_until(step_deadline), if stepper.is_some() => {
                // The step's view never came (or not enough of it): skip it.
                let s = stepper.as_mut().expect("stepping");
                let (cmd, sweep_end) = s.next();
                carry.sweep = s.next_sweep();
                if let Some((0, _)) = sweep_end {
                    end!(lost("no frame of the asked view in a whole sweep".into(), whole));
                }
                step_deadline = Instant::now() + s.patience();
                if let Some(cmd) = cmd {
                    send!(cmd);
                }
                continue;
            }
            _ = sleep_until(read_deadline) => {
                let ms = cfg.read_timeout_ms;
                end!(lost(format!("no message for {ms} ms"), whole))
            }
            m = stream.next() => m,
        };
        read_deadline = Instant::now() + read_timeout;
        let payload = match msg {
            None | Some(Ok(Message::Close(_))) if stepper.is_none() => {
                // A refusal can be a bare close: every connection slot taken.
                let why = "the kiwi closed the waterfall socket before its setup \
                           (every connection slot taken?)";
                return (End::Refused(PortError::Unsupported(why.into())), None);
            }
            None | Some(Ok(Message::Close(_))) => {
                end!(lost("closed by the kiwi".into(), whole))
            }
            Some(Err(e)) => return (lost(e.to_string(), whole), None),
            Some(Ok(Message::Binary(b))) => b,
            Some(Ok(Message::Text(t))) => t.into(),
            Some(Ok(_)) => continue,
        };
        match tag(&payload) {
            Some(Tag::Msg) if payload.len() <= MAX_MSG_BYTES => match setup.on_msg(&payload) {
                None => {}
                Some(Err(why)) if stepper.is_none() => {
                    end!(End::Refused(PortError::Unsupported(why)))
                }
                // A refusal or a kick mid-run: the connection is lost.
                Some(Err(why)) => end!(lost(why, whole)),
                Some(Ok(())) => {
                    let bandwidth = setup.bandwidth.unwrap_or_default();
                    let plan = match &carry.plan {
                        None => {
                            if let Err(e) = span_within(req.span, bandwidth) {
                                end!(End::Refused(e));
                            }
                            let plan = wf_plan(req.span, req.bin_hz.0, req.dwell_ms, bandwidth);
                            carry.plan = Some((plan.clone(), bandwidth));
                            plan
                        }
                        Some((_, bw)) if *bw != bandwidth => {
                            let why =
                                format!("the kiwi's bandwidth changed from {bw} to {bandwidth} Hz");
                            end!(End::Refused(PortError::Unsupported(why)))
                        }
                        Some((plan, _)) => plan.clone(),
                    };
                    if let Some(r) = ready.take()
                        && r.send(Ok(plan.clone())).is_err()
                    {
                        // `sweep` gave up on this connection.
                        end!(End::Stop);
                    }
                    ctx.state.send_replace(SweepState::Running);
                    for c in [
                        CMD_IDENT,
                        CMD_WF_COMP_OFF,
                        CMD_WF_DB,
                        CMD_WF_INTERP,
                        CMD_WF_SPEED,
                    ] {
                        send!(c);
                    }
                    let zoom_max = setup.zoom_max.unwrap_or(ZOOM_MAX_DEFAULT);
                    let settle = cfg.wf_settle_frames;
                    let mut s = Stepper::new(plan, bandwidth, zoom_max, settle, carry.sweep);
                    let cmd = s.view();
                    step_deadline = Instant::now() + s.patience();
                    stepper = Some(s);
                    if let Some(cmd) = cmd {
                        send!(cmd);
                    }
                }
            },
            Some(Tag::Wf) => {
                let Some(s) = stepper.as_mut() else {
                    continue;
                };
                let wf = match parse_wf(&payload) {
                    Ok(wf) => wf,
                    Err(e) => {
                        bad += 1;
                        if bad >= MAX_BAD_SND {
                            end!(lost(format!("protocol: {e}"), whole));
                        }
                        continue;
                    }
                };
                bad = 0;
                match s.on_frame(&wf, setup.wf_cal, now_ns()) {
                    FrameOut::Other => {}
                    FrameOut::Belongs => step_deadline = Instant::now() + s.patience(),
                    FrameOut::Row(row) => {
                        // A full channel drops the row: the node counts it skipped.
                        if let Err(TrySendError::Closed(_)) = ctx.rows.try_send(row) {
                            end!(End::Stop);
                        }
                        let (cmd, sweep_end) = s.next();
                        carry.sweep = s.next_sweep();
                        if let Some((rows, steps)) = sweep_end {
                            whole |= rows == steps;
                        }
                        step_deadline = Instant::now() + s.patience();
                        if let Some(cmd) = cmd {
                            send!(cmd);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeta_core::types::Hz;

    const BW: u64 = 30_000_000;

    fn span(lo: u64, hi: u64) -> Tuning {
        Tuning {
            center_hz: Hz((lo + hi) / 2),
            bandwidth_hz: Hz(hi - lo),
        }
    }

    #[test]
    fn the_whole_band_at_6_khz_is_zoom_3_in_eight_steps() {
        let p = wf_plan(span(0, BW), 6000, 100, BW);
        assert_eq!(p.zoom, 3);
        assert_eq!(p.view_hz, 3_750_000);
        assert!((p.bin_hz - 3662.1).abs() < 0.1, "{}", p.bin_hz);
        let want: Vec<u64> = (0..8).map(|k| k * 3_750_000).collect();
        assert_eq!(p.starts, want);
        assert_eq!(p.shift_hz, 29_296);
    }

    #[test]
    fn at_8_khz_it_is_zoom_2_in_four() {
        let p = wf_plan(span(0, BW), 8000, 100, BW);
        assert_eq!(p.zoom, 2);
        let want: Vec<u64> = (0..4).map(|k| k * 7_500_000).collect();
        assert_eq!(p.starts, want);
    }

    #[test]
    fn the_coarsest_bins_are_zoom_0_in_one_step_with_no_shift() {
        let p = wf_plan(span(0, BW), 30_000, 100, BW);
        assert_eq!(p.zoom, 0);
        assert_eq!(p.starts, vec![0]);
        assert_eq!(p.shift_hz, 0);
        assert_eq!(p.start(1, 0), 0);
    }

    #[test]
    fn a_narrow_span_takes_the_finest_zoom_that_holds_it() {
        // 300 kHz wide at 7.1 MHz: view(6) = 468 750 holds it, view(7) does not.
        let p = wf_plan(span(6_950_000, 7_250_000), 5000, 100, BW);
        assert_eq!(p.zoom, 6);
        assert_eq!(p.view_hz, 468_750);
        assert_eq!(p.starts, vec![7_100_000 - 234_375]);
    }

    #[test]
    fn views_clamp_at_the_band_edges_without_a_repeated_step() {
        let p = wf_plan(span(29_000_000, 30_000_000), 500, 100, BW);
        assert_eq!((p.zoom, p.view_hz), (6, 468_750));
        assert_eq!(p.starts, vec![29_000_000, 29_468_750, 29_531_250]);
        let p = wf_plan(span(29_000_000, 31_000_000), 500, 100, BW);
        assert_eq!((p.zoom, p.view_hz), (6, 468_750));
        assert_eq!(p.starts, vec![29_000_000, 29_468_750, 29_531_250]);
    }

    #[test]
    fn the_zoom_never_passes_10() {
        let p = wf_plan(span(10_000_000, 10_010_000), 1, 100, BW);
        assert_eq!(p.zoom, WF_ZOOM_MAX);
        assert_eq!(p.view_hz, BW >> 10);
    }

    #[test]
    fn the_odd_sweep_sees_each_zeroed_pixel_clean() {
        for (lo, hi, bin) in [
            (0, BW, 6000),
            (1_000_000, 29_000_000, 6000),
            (7_000_000, 14_000_000, 3000),
            (14_000_000, 14_200_000, 200),
            // Clamped at the top edge: the last start is 37 500 Hz after the one before.
            (29_000_000, 30_000_000, 500),
            (29_000_000, 31_000_000, 500),
        ] {
            let p = wf_plan(span(lo, hi), bin, 100, BW);
            // Exact, in 1/1024 Hz: a pixel is `view_hz` of them, the server zeroes
            // 4. No slack: this compares the plan's own Hz, not an `x_bin` (whose
            // rounding is under a unit of bandwidth / (1024 << zoom_max), well under
            // a pixel, and never moves a view the plan placed).
            let zeroed = 4 * p.view_hz;
            // The zeroed pixels of the view at `z` lie in the clean ones of the
            // view at `s`: pixels 4.. of 1024.
            let clean = |z: u64, s: u64| {
                z * 1024 >= s * 1024 + zeroed && z * 1024 + zeroed <= (s + p.view_hz) * 1024
            };
            for step in 0..p.starts.len() {
                let even = p.start(0, step);
                let odd = p.start(1, step);
                if even > 0 {
                    assert!(clean(even, odd), "{lo}-{hi} step {step}: even in odd");
                }
                if step > 0 && odd > 0 {
                    // The preceding step's own even view must cover them.
                    assert!(
                        clean(odd, p.start(0, step - 1)),
                        "{lo}-{hi} step {step}: odd in the preceding even view"
                    );
                }
            }
        }
    }

    #[test]
    fn a_dwell_is_whole_frames_at_least_one() {
        let at = |ms| {
            let p = wf_plan(span(0, BW), 6000, ms, BW);
            (p.frames, p.dwell_ms)
        };
        assert_eq!(at(20), (1, 43));
        assert_eq!(at(250), (6, 261));
        assert_eq!(at(0), (1, 43));
    }

    #[test]
    fn x_bin_counts_in_the_servers_units() {
        let p = wf_plan(span(0, BW), 6000, 100, BW);
        assert_eq!(p.x_bin(0, BW, 14), 0);
        assert_eq!(p.x_bin(3_750_000, BW, 14), 2_097_152);
        assert_eq!(p.x_bin(BW, BW, 14), 16_777_216);
        // A unit is 1.79 Hz on 30 MHz: 1 Hz is 0.56 of one and rounds up, 2 Hz
        // is 1.12 and rounds down.
        assert_eq!(p.x_bin(1, BW, 14), 1);
        assert_eq!(p.x_bin(2, BW, 14), 1);
        assert_eq!(p.x_bin(3, BW, 14), 2);
    }

    // ---- The session's pure parts ----

    use crate::proto::encode_msg;

    fn setup_of(lines: &[&str]) -> (Setup, Vec<Option<Result<(), String>>>) {
        let mut s = Setup::default();
        let out = lines.iter().map(|l| s.on_msg(&encode_msg(l))).collect();
        (s, out)
    }

    const SETUP: &str = "wf_fft_size=1024 wf_fps=23 wf_fps_max=23 zoom_max=14 rx_chans=8 \
wf_chans=4 wf_chans_real=4 wf_cal=-13 wf_setup";
    const BANDWIDTH: &str = "center_freq=15000000 bandwidth=30000000 adc_clk_nom=66666600";

    #[test]
    fn the_setup_completes_once_on_wf_setup_and_bandwidth() {
        let (s, out) = setup_of(&["badp=0", "freq_offset=0.000", BANDWIDTH, SETUP, SETUP]);
        assert_eq!(out, vec![None, None, None, Some(Ok(())), None]);
        assert_eq!(
            (s.bandwidth, s.zoom_max, s.wf_cal),
            (Some(BW), Some(14), -13)
        );
        // `wf_setup` before `bandwidth`: complete when the bandwidth comes.
        let (_, out) = setup_of(&[SETUP, BANDWIDTH]);
        assert_eq!(out, vec![None, Some(Ok(()))]);
        // A later `wf_cal` is the one in force.
        let mut s = Setup::default();
        s.on_msg(&encode_msg("wf_cal=-13"));
        s.on_msg(&encode_msg("wf_cal=-7"));
        assert_eq!(s.wf_cal, -7);
    }

    #[test]
    fn waterfall_refusals_are_named() {
        for (line, want) in [
            (
                "too_busy=8",
                "no waterfall channel free (too_busy: 8 channels)",
            ),
            (
                "badp=5",
                "badp=5: no multiple connections from the same ip address",
            ),
            ("down", "down: server is down"),
            (
                "exclusive_use",
                "exclusive_use: the Kiwi is locked for exclusive use",
            ),
            (
                "redirect=http%3A%2F%2Fk2%3A8073",
                "redirect to http://k2:8073 not supported",
            ),
        ] {
            let (_, out) = setup_of(&[line]);
            assert_eq!(out, vec![Some(Err(want.to_string()))], "{line}");
        }
        let no_wf = SETUP.replace("wf_chans=4", "wf_chans=0");
        let (_, out) = setup_of(&[BANDWIDTH, &no_wf]);
        assert_eq!(
            out[1],
            Some(Err(
                "this Kiwi is configured without waterfall channels".into()
            ))
        );
        let (_, out) = setup_of(&["freq_offset=116000.000", BANDWIDTH, SETUP]);
        assert_eq!(
            out[2],
            Some(Err("kiwi freq_offset 116000 kHz not supported".into()))
        );
    }

    fn frame(zoom: u8, x_bin: u32, db: f32) -> Wf {
        Wf {
            x_bin,
            zoom,
            seq: 0,
            db: vec![db; WF_PIXELS],
        }
    }

    #[test]
    fn a_step_drops_other_views_and_its_settle_then_averages_in_linear_power() {
        // Zoom 3, two frames a step (50 ms), one settle frame.
        let plan = wf_plan(span(0, BW), 6000, 50, BW);
        assert_eq!((plan.zoom, plan.frames), (3, 2));
        let mut s = Stepper::new(plan, BW, 14, 1, 0);
        assert_eq!(s.view().as_deref(), Some("SET zoom=3 cf=1875.000"));
        assert_eq!(s.on_frame(&frame(2, 0, -50.0), 0, 1), FrameOut::Other);
        assert_eq!(
            s.on_frame(&frame(3, 2_097_152, -50.0), 0, 2),
            FrameOut::Other
        );
        assert_eq!(s.on_frame(&frame(3, 5, -50.0), 0, 3), FrameOut::Other);
        // The settle frame, then the dwell's two.
        assert_eq!(s.on_frame(&frame(3, 0, -50.0), 0, 4), FrameOut::Belongs);
        assert_eq!(s.on_frame(&frame(3, 1, -100.0), -13, 5), FrameOut::Belongs);
        let FrameOut::Row(row) = s.on_frame(&frame(3, 0, -90.0), -13, 6) else {
            panic!("a row");
        };
        let want = 10.0 * ((1e-10 + 1e-9) / 2.0f64).log10() - 13.0;
        assert!((f64::from(row.db[7]) - want).abs() < 1e-3, "{}", row.db[7]);
        assert_eq!((row.sweep, row.step, row.center_hz), (0, 0, Hz(1_875_000)));
        assert_eq!(row.masked, vec![[0, 2]]);
        assert_eq!(
            (row.span.start_ns, row.span.end_ns),
            (Timestamp(5), Timestamp(6))
        );
        assert_eq!(row.db.len(), WF_PIXELS);
    }

    #[test]
    fn a_frame_within_the_slack_counts_and_one_past_it_does_not() {
        let plan = wf_plan(span(0, BW), 6000, 20, BW);
        let want = plan.x_bin(plan.start(0, 1), BW, 14);
        let mut s = Stepper::new(plan, BW, 14, 0, 0);
        s.view();
        s.next();
        for x in [want - 5, want + 5] {
            assert_eq!(
                s.on_frame(&frame(3, x, -50.0), 0, 1),
                FrameOut::Other,
                "{x}"
            );
        }
        assert!(matches!(
            s.on_frame(&frame(3, want + 4, -50.0), 0, 2),
            FrameOut::Row(_)
        ));
        let want = want * 2;
        s.next();
        assert!(matches!(
            s.on_frame(&frame(3, want - 4, -50.0), 0, 3),
            FrameOut::Row(_)
        ));
    }

    #[test]
    fn a_row_is_centred_where_its_frames_say() {
        let plan = wf_plan(span(0, BW), 6000, 20, BW);
        let mut s = Stepper::new(plan, BW, 14, 0, 0);
        s.view();
        let (cmd, end) = s.next();
        assert_eq!(cmd.as_deref(), Some("SET zoom=3 cf=5625.000"));
        assert_eq!(end, None);
        // One unit low: 1.79 Hz on 30 MHz.
        let FrameOut::Row(row) = s.on_frame(&frame(3, 2_097_151, -50.0), 0, 1) else {
            panic!("a row");
        };
        assert_eq!((row.step, row.center_hz), (1, Hz(5_624_998)));
    }

    #[test]
    fn a_sweep_ends_with_its_rows_and_the_odd_one_is_shifted() {
        let plan = wf_plan(span(0, BW), 6000, 20, BW);
        let mut s = Stepper::new(plan.clone(), BW, 14, 0, 0);
        s.view();
        for step in 0..8 {
            if step % 2 == 0 {
                let x = plan.x_bin(plan.start(0, step), BW, 14);
                assert!(matches!(
                    s.on_frame(&frame(3, x, -50.0), 0, 1),
                    FrameOut::Row(_)
                ));
            }
            let (cmd, end) = s.next();
            assert_eq!(end.is_some(), step == 7, "step {step}");
            if step == 7 {
                assert_eq!(end, Some((4, 8)));
                // The odd sweep's first view is the even one's: 0 Hz is not shifted.
                assert_eq!(cmd.as_deref(), Some("SET zoom=3 cf=1875.000"));
            }
        }
        let (cmd, _) = s.next();
        assert_eq!(cmd.as_deref(), Some("SET zoom=3 cf=5595.704"));
        assert_eq!(s.next_sweep(), 2);
    }

    #[test]
    fn an_unchanged_view_is_not_asked_again_or_settled() {
        // Zoom 0: the whole band in one view, no shift.
        let plan = wf_plan(span(0, BW), 30_000, 20, BW);
        let mut s = Stepper::new(plan, BW, 14, 1, 0);
        assert_eq!(s.view().as_deref(), Some("SET zoom=0 cf=15000.000"));
        assert_eq!(s.on_frame(&frame(0, 0, -50.0), 0, 1), FrameOut::Belongs);
        let FrameOut::Row(row) = s.on_frame(&frame(0, 0, -50.0), 0, 2) else {
            panic!("a row");
        };
        assert_eq!(row.masked, vec![[0, 4]]);
        assert_eq!(s.next(), (None, Some((1, 1))));
        // No settle: the next frame is the next row.
        assert!(matches!(
            s.on_frame(&frame(0, 0, -50.0), 0, 3),
            FrameOut::Row(_)
        ));
        // A whole step's patience: settle, dwell and ten spare frames.
        assert_eq!(s.patience(), Duration::from_micros(12 * 1_000_000 / 23));
    }

    #[test]
    fn the_wf_timestamp_is_never_the_snd_one_and_never_repeats() {
        // Bit 31 differs whatever second each call reads (a tick moves the low bits).
        assert_eq!((wf_ts() ^ path_ts()) & 0x8000_0000, 0x8000_0000);
        let a = fresh_wf_ts();
        let b = fresh_wf_ts();
        assert!(b > a, "{a} {b}");
    }

    #[test]
    fn a_span_must_lie_inside_the_band() {
        assert!(span_within(span(0, BW), BW).is_ok());
        assert!(span_within(span(7_000_000, 7_300_000), BW).is_ok());
        for (t, field) in [
            (span(29_000_000, 31_000_000), "center_hz"),
            (span(0, BW + 2), "bandwidth_hz"),
            (span(7_000_000, 7_000_000), "bandwidth_hz"),
        ] {
            let Err(PortError::OutOfRange { field: f, .. }) = span_within(t, BW) else {
                panic!("{t:?}");
            };
            assert_eq!(f, field, "{t:?}");
        }
        let low = Tuning {
            center_hz: Hz(100_000),
            bandwidth_hz: Hz(400_000),
        };
        assert!(span_within(low, BW).is_err());
        // A Kiwi's own band: a 30 MHz span on a 32 MHz Kiwi, not on a 0 Hz one.
        assert!(span_within(span(0, BW), 32_000_000).is_ok());
        assert!(span_within(span(0, BW), 0).is_err());
    }
}
