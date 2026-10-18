//! One KiwiSDR session: a tokio task that owns the websocket for the life of
//! a tuning, reconnecting after transport errors and ending on a refusal.
//!
//! Per connection, [`Conn`] is the protocol state machine (no I/O):
//!
//! ```text
//! connect ──► SET auth ──► AwaitRates ──(sample_rate)──► setup + tuning + keepalive
//!                              │      ──(audio_rate)───► SET AR OK
//!                              │      both known ──────► Ready ──► SND → IqBlock
//!                              └─ too_busy / badp≠0 / down / redirect / bad rate ─► Terminal
//! ```
//!
//! [`run`] drives connections: a [`End::Lost`] connection (dropped, silent,
//! refused) is retried after each of `reconnect_delays_ms`, the budget reset
//! by every connection that reached `Ready`; an [`End::Terminal`] one is not.
//! Before the first `Ready` nothing is retried, so `tune` answers promptly.
//! A terminal failure sets the port's fault before the task lets go of its
//! broadcast sender.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::{Instant, interval_at, sleep, sleep_until, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_util::sync::CancellationToken;
use zeta_core::port::{IqBlock, Tuning};
use zeta_core::types::{Hz, Timestamp};

use crate::proto::{
    CMD_AGC, CMD_COMPRESSION_OFF, CMD_GEN, CMD_GENATTN, CMD_IDENT, CMD_KEEPALIVE, CMD_SQUELCH,
    KiwiMsg, SND_FLAG_ADC_OVFL, Snd, Tag, cmd_ar_ok, cmd_auth, cmd_mod_iq, parse_msg, parse_snd,
    snd_url, tag,
};
use crate::source::KiwiConfig;

/// Largest websocket message or frame accepted; larger ones end the connection.
pub(crate) const MAX_FRAME_BYTES: usize = 1 << 20;
/// `MSG` payloads longer than this are ignored unparsed (`load_cfg=` is huge
/// and carries nothing this client reads).
pub(crate) const MAX_MSG_BYTES: usize = 64 * 1024;
/// Consecutive unparseable `SND` frames that end the session.
pub(crate) const MAX_BAD_SND: u32 = 10;
/// Accepted `sample_rate`/`audio_rate`, in Hz.
const RATE_MIN_HZ: f64 = 1_000.0;
const RATE_MAX_HZ: f64 = 1_000_000.0;
/// Wall-clock drift beyond which block timestamps are re-anchored.
pub(crate) const MAX_DRIFT_NS: u64 = 500_000_000;
/// Longest redirect target echoed in a fault.
const MAX_REDIRECT_CHARS: usize = 200;

/// What the port shows of its sessions; one per port, shared with each session.
#[derive(Debug, Default)]
pub(crate) struct Status {
    pub fault: Option<String>,
    pub current: Option<Tuning>,
    pub audio_rate: Option<u32>,
    pub sample_rate: Option<f64>,
    pub freq_offset_khz: f64,
    pub rssi_dbm: Option<f32>,
    pub adc_overflows: u64,
    pub seq_gaps: u64,
    /// Malformed `MSG`s, each skipped as a warning.
    pub msg_errors: u64,
}

/// What a frame asks the driver to do, in order.
#[derive(Debug, PartialEq)]
pub(crate) enum Event {
    Send(String),
    /// Both rates are known: the handshake is complete.
    Ready,
    /// An IQ frame; `gap` when its sequence number does not follow the last.
    Block {
        snd: Snd,
        gap: bool,
    },
    /// A refusal or protocol failure; the session ends without retrying.
    Terminal(String),
    MsgError,
}

fn badp_meaning(n: u8) -> &'static str {
    match n {
        1 => "bad password or all channels busy that don't require a password",
        2 => "still determining local interface address, try again in a few moments",
        3 => "admin connection not allowed from this ip address",
        4 => "no admin password set, can only connect from the Kiwi's local network",
        5 => "no multiple connections from the same ip address",
        6 => "database update in progress, try again after one minute",
        7 => "another admin connection already open",
        _ => "unknown refusal",
    }
}

fn tuning_cmds(t: Tuning) -> [String; 3] {
    [
        cmd_mod_iq(t.center_hz.0, t.bandwidth_hz.0),
        CMD_AGC.to_string(),
        CMD_COMPRESSION_OFF.to_string(),
    ]
}

fn rate_ok(r: f64) -> bool {
    (RATE_MIN_HZ..=RATE_MAX_HZ).contains(&r)
}

/// One connection's protocol state.
#[derive(Debug, Default)]
pub(crate) struct Conn {
    pub sample_rate: Option<f64>,
    pub audio_rate: Option<u32>,
    pub freq_offset_khz: f64,
    pub ready: bool,
    last_seq: Option<u32>,
    bad_snd: u32,
}

impl Conn {
    /// Handle one websocket payload; `tuning` is sent on `sample_rate`.
    pub fn on_frame(&mut self, payload: &[u8], tuning: Tuning, out: &mut Vec<Event>) {
        match tag(payload) {
            Some(Tag::Msg) if payload.len() <= MAX_MSG_BYTES => self.on_msg(payload, tuning, out),
            Some(Tag::Snd) if self.ready => self.on_snd(payload, out),
            _ => {}
        }
    }

    fn on_msg(&mut self, payload: &[u8], tuning: Tuning, out: &mut Vec<Event>) {
        let Ok(msgs) = parse_msg(payload) else {
            out.push(Event::MsgError);
            return;
        };
        for m in msgs {
            match m {
                KiwiMsg::SampleRate(r) => {
                    if !rate_ok(r) {
                        out.push(Event::Terminal(format!(
                            "protocol: sample_rate {r} outside 1000..=1000000 Hz"
                        )));
                        return;
                    }
                    let first = self.sample_rate.is_none();
                    self.sample_rate = Some(r);
                    if first {
                        for c in [CMD_SQUELCH, CMD_GENATTN, CMD_GEN, CMD_IDENT] {
                            out.push(Event::Send(c.to_string()));
                        }
                        out.extend(tuning_cmds(tuning).map(Event::Send));
                        out.push(Event::Send(CMD_KEEPALIVE.to_string()));
                    }
                }
                KiwiMsg::AudioRate(n) => {
                    if !rate_ok(f64::from(n)) {
                        out.push(Event::Terminal(format!(
                            "protocol: audio_rate {n} outside 1000..=1000000 Hz"
                        )));
                        return;
                    }
                    self.audio_rate = Some(n);
                    out.push(Event::Send(cmd_ar_ok(n)));
                }
                KiwiMsg::BadPassword(0) => {}
                KiwiMsg::BadPassword(n) => {
                    out.push(Event::Terminal(format!("badp={n}: {}", badp_meaning(n))));
                    return;
                }
                KiwiMsg::TooBusy(n) => {
                    out.push(Event::Terminal(format!(
                        "too_busy: all {n} client slots taken"
                    )));
                    return;
                }
                KiwiMsg::Down => {
                    out.push(Event::Terminal("down: server is down".into()));
                    return;
                }
                KiwiMsg::Redirect(url) => {
                    let url: String = url.chars().take(MAX_REDIRECT_CHARS).collect();
                    out.push(Event::Terminal(format!("redirect to {url} not supported")));
                    return;
                }
                KiwiMsg::FreqOffsetKhz(k) => self.freq_offset_khz = k,
                KiwiMsg::Other(_) => {}
            }
            if !self.ready && self.sample_rate.is_some() && self.audio_rate.is_some() {
                self.ready = true;
                out.push(Event::Ready);
            }
        }
    }

    fn on_snd(&mut self, payload: &[u8], out: &mut Vec<Event>) {
        match parse_snd(payload) {
            Err(e) => {
                self.bad_snd += 1;
                if self.bad_snd >= MAX_BAD_SND {
                    out.push(Event::Terminal(format!("protocol: {e}")));
                }
            }
            Ok(snd) => {
                self.bad_snd = 0;
                let gap = self.last_seq.is_some_and(|l| snd.seq != l.wrapping_add(1));
                self.last_seq = Some(snd.seq);
                out.push(Event::Block { snd, gap });
            }
        }
    }
}

/// Block timestamps: `anchor + count · 1e9 / rate`, re-anchored to
/// `max(now, last_end)` after a reconnect or a sequence gap, on a rate change,
/// and when the stamps drift more than [`MAX_DRIFT_NS`] from the wall clock.
/// The live Kiwi runs about 91 ppm slow of its nominal 12000 Hz, so this
/// happens about every 1.5 h; the ring sees it as a gap once.
#[derive(Debug, Default)]
pub(crate) struct Clock {
    rate: u64,
    anchor: u64,
    count: u64,
    last_end: u64,
    stale: bool,
}

impl Clock {
    pub fn new() -> Self {
        Self {
            stale: true,
            ..Self::default()
        }
    }

    /// The next block's `t0` will not follow the last one.
    pub fn gap(&mut self) {
        self.stale = true;
    }

    fn at(&self, count: u64) -> u64 {
        self.anchor + (u128::from(count) * 1_000_000_000 / u128::from(self.rate.max(1))) as u64
    }

    /// `t0` of a block of `n` samples at `rate` arriving at `now_ns`.
    pub fn stamp(&mut self, rate: u64, n: usize, now_ns: u64) -> u64 {
        if self.stale || rate != self.rate || self.at(self.count).abs_diff(now_ns) > MAX_DRIFT_NS {
            self.rate = rate;
            self.anchor = now_ns.max(self.last_end);
            self.count = 0;
            self.stale = false;
        }
        let t0 = self.at(self.count);
        self.count += n as u64;
        self.last_end = self.at(self.count);
        t0
    }
}

/// How one connection ended.
#[derive(Debug)]
pub(crate) enum End {
    /// Cancelled, the port dropped, or no receivers: nothing to report.
    Stop,
    Terminal(String),
    /// A transport failure; `ready` when the connection had completed its handshake.
    Lost {
        err: String,
        ready: bool,
    },
}

/// Everything a session shares with its port.
pub(crate) struct Ctx {
    pub cfg: Arc<KiwiConfig>,
    pub tx: broadcast::Sender<IqBlock>,
    pub status: Arc<Mutex<Status>>,
    pub cancel: CancellationToken,
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
}

/// The `<ts>` path element: unix seconds plus the pid, as the reference
/// keeps it unique per client.
fn path_ts() -> u32 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    secs.wrapping_add(u64::from(std::process::id())) as u32
}

/// The session task: connections until a stop or a terminal failure.
/// `ready` answers the first handshake (or its failure) to `tune`.
pub(crate) async fn run(
    ctx: Ctx,
    mut band: Tuning,
    mut cmds: mpsc::UnboundedReceiver<Tuning>,
    ready: oneshot::Sender<Result<(), String>>,
) {
    let mut ready = Some(ready);
    let mut clock = Clock::new();
    let mut attempts = 0usize;
    let reason = loop {
        match connection(&ctx, &mut band, &mut cmds, &mut clock, &mut ready).await {
            End::Stop => return,
            End::Terminal(r) => break r,
            End::Lost { err, .. } if ready.is_some() => break err,
            End::Lost {
                err,
                ready: was_ready,
            } => {
                if was_ready {
                    attempts = 0;
                }
                let Some(&delay) = ctx.cfg.reconnect_delays_ms.get(attempts) else {
                    break format!("connection lost ({err}); {attempts} reconnect attempts failed");
                };
                attempts += 1;
                clock.gap();
                tokio::select! {
                    _ = ctx.cancel.cancelled() => return,
                    _ = sleep(Duration::from_millis(delay)) => {}
                }
            }
        }
    };
    // Fault first: a reader that sees the stream end must already see why.
    ctx.status.lock().unwrap().fault = Some(reason.clone());
    if let Some(r) = ready.take() {
        let _ = r.send(Err(reason));
    }
}

type Sink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;

/// Send a close frame (bounded wait), freeing the Kiwi's slot.
async fn close(mut sink: Sink) {
    let _ = timeout(Duration::from_secs(1), sink.close()).await;
}

/// One websocket connection, from connect to its end.
async fn connection(
    ctx: &Ctx,
    band: &mut Tuning,
    cmds: &mut mpsc::UnboundedReceiver<Tuning>,
    clock: &mut Clock,
    ready: &mut Option<oneshot::Sender<Result<(), String>>>,
) -> End {
    let cfg = &ctx.cfg;
    let lost = |err: String, ready: bool| End::Lost { err, ready };
    let url = snd_url(&cfg.host, cfg.port, path_ts());
    let ws_cfg = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME_BYTES))
        .max_frame_size(Some(MAX_FRAME_BYTES));
    let connect = tokio_tungstenite::connect_async_with_config(url, Some(ws_cfg), true);
    let handshake = Duration::from_millis(cfg.handshake_timeout_ms);
    let ws = tokio::select! {
        _ = ctx.cancel.cancelled() => return End::Stop,
        r = timeout(handshake, connect) => match r {
            Ok(Ok((ws, _))) => ws,
            Ok(Err(e)) => return lost(format!("connect: {e}"), false),
            Err(_) => {
                let ms = cfg.handshake_timeout_ms;
                return lost(format!("connect: no answer in {ms} ms"), false);
            }
        },
    };
    let (mut sink, mut stream) = ws.split();
    if let Err(e) = sink
        .send(Message::text(cmd_auth(cfg.password.as_deref())))
        .await
    {
        return lost(format!("send: {e}"), false);
    }
    let second = Duration::from_secs(1);
    let mut keepalive = interval_at(Instant::now() + second, second);
    let read_timeout = Duration::from_millis(cfg.read_timeout_ms);
    let mut deadline = Instant::now() + read_timeout;
    let mut conn = Conn::default();
    let mut events = Vec::new();
    loop {
        let msg = tokio::select! {
            _ = ctx.cancel.cancelled() => {
                close(sink).await;
                return End::Stop;
            }
            t = cmds.recv() => {
                let Some(t) = t else {
                    close(sink).await;
                    return End::Stop;
                };
                *band = t;
                if conn.sample_rate.is_some() {
                    for c in tuning_cmds(t) {
                        if let Err(e) = sink.send(Message::text(c)).await {
                            return lost(format!("send: {e}"), conn.ready);
                        }
                    }
                }
                continue;
            }
            _ = keepalive.tick() => {
                if let Err(e) = sink.send(Message::text(CMD_KEEPALIVE)).await {
                    return lost(format!("send: {e}"), conn.ready);
                }
                continue;
            }
            _ = sleep_until(deadline) => {
                let ms = cfg.read_timeout_ms;
                return lost(format!("no message for {ms} ms"), conn.ready);
            }
            m = stream.next() => m,
        };
        deadline = Instant::now() + read_timeout;
        let payload = match msg {
            None | Some(Ok(Message::Close(_))) => {
                return lost("closed by the kiwi".into(), conn.ready);
            }
            Some(Err(e)) => return lost(e.to_string(), conn.ready),
            Some(Ok(Message::Binary(b))) => b,
            Some(Ok(Message::Text(t))) => t.into(),
            Some(Ok(_)) => continue,
        };
        events.clear();
        conn.on_frame(&payload, *band, &mut events);
        for ev in events.drain(..) {
            match ev {
                Event::Send(c) => {
                    if let Err(e) = sink.send(Message::text(c)).await {
                        return lost(format!("send: {e}"), conn.ready);
                    }
                }
                Event::Ready => {
                    let rate = conn.audio_rate.unwrap_or_default();
                    {
                        let mut s = ctx.status.lock().unwrap();
                        s.audio_rate = conn.audio_rate;
                        s.sample_rate = conn.sample_rate;
                        s.freq_offset_khz = conn.freq_offset_khz;
                        if let Some(cur) = s.current.as_mut() {
                            cur.bandwidth_hz = Hz(u64::from(rate));
                        }
                    }
                    if let Some(r) = ready.take() {
                        let _ = r.send(Ok(()));
                    }
                }
                Event::Block { snd, gap } => {
                    let rate = u64::from(conn.audio_rate.unwrap_or_default());
                    {
                        let mut s = ctx.status.lock().unwrap();
                        s.rssi_dbm = Some(snd.rssi_dbm);
                        if snd.flags & SND_FLAG_ADC_OVFL != 0 {
                            s.adc_overflows += 1;
                        }
                        if gap {
                            s.seq_gaps += 1;
                        }
                    }
                    if gap {
                        clock.gap();
                    }
                    let t0 = clock.stamp(rate, snd.samples.len(), now_ns());
                    let block = IqBlock {
                        t0: Timestamp(t0),
                        sample_rate_hz: Hz(rate),
                        center_hz: band.center_hz,
                        samples: snd.samples.into(),
                    };
                    if ctx.tx.send(block).is_err() {
                        // No receivers: free the Kiwi's slot.
                        close(sink).await;
                        return End::Stop;
                    }
                }
                Event::Terminal(r) => {
                    close(sink).await;
                    return End::Terminal(r);
                }
                Event::MsgError => ctx.status.lock().unwrap().msg_errors += 1,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{encode_msg, encode_snd};

    fn band() -> Tuning {
        Tuning {
            center_hz: Hz(7_100_000),
            bandwidth_hz: Hz(10_000),
        }
    }

    fn feed(c: &mut Conn, payload: &[u8]) -> Vec<Event> {
        let mut out = Vec::new();
        c.on_frame(payload, band(), &mut out);
        out
    }

    fn ready_conn() -> Conn {
        let mut c = Conn::default();
        let setup = feed(&mut c, &encode_msg("sample_rate=11998.902088"));
        assert_eq!(setup.len(), 8, "{setup:?}");
        assert_eq!(
            feed(&mut c, &encode_msg("audio_init=1 audio_rate=12000")),
            vec![
                Event::Send("SET AR OK in=12000 out=44100".into()),
                Event::Ready
            ]
        );
        c
    }

    #[test]
    fn malformed_msg_is_a_warning_not_fatal() {
        let mut c = Conn::default();
        assert_eq!(
            feed(&mut c, &encode_msg("audio_rate=fast")),
            vec![Event::MsgError]
        );
        assert_eq!(feed(&mut c, b"MSG \xff"), vec![Event::MsgError]);
        assert_eq!(feed(&mut c, &encode_msg("audio_rate=12000")).len(), 1);
        assert_eq!(c.audio_rate, Some(12_000));
    }

    #[test]
    fn insane_rates_are_terminal() {
        for m in [
            "sample_rate=0",
            "sample_rate=-12000",
            "sample_rate=2000000",
            "audio_rate=0",
            "audio_rate=999",
            "audio_rate=4000000",
        ] {
            let out = feed(&mut Conn::default(), &encode_msg(m));
            assert!(
                matches!(out.last(), Some(Event::Terminal(r)) if r.starts_with("protocol: ")),
                "{m}: {out:?}"
            );
        }
    }

    #[test]
    fn oversized_msg_is_ignored() {
        let mut c = Conn::default();
        let big = format!("load_cfg={} audio_rate=12000", "x".repeat(MAX_MSG_BYTES));
        assert_eq!(feed(&mut c, &encode_msg(&big)), vec![]);
        assert_eq!(c.audio_rate, None);
    }

    #[test]
    fn refusals_are_named() {
        for (m, want) in [
            (
                "badp=1",
                "badp=1: bad password or all channels busy that don't require a password",
            ),
            ("badp=7", "badp=7: another admin connection already open"),
            ("badp=9", "badp=9: unknown refusal"),
            ("too_busy=8", "too_busy: all 8 client slots taken"),
            ("down", "down: server is down"),
            (
                "redirect=http%3A%2F%2Fk2%3A8073",
                "redirect to http://k2:8073 not supported",
            ),
        ] {
            let out = feed(&mut Conn::default(), &encode_msg(m));
            assert_eq!(out, vec![Event::Terminal(want.into())], "{m}");
        }
        assert_eq!(feed(&mut Conn::default(), &encode_msg("badp=0")), vec![]);
    }

    #[test]
    fn snd_before_ready_is_ignored_and_ten_bad_frames_are_terminal() {
        let mut c = Conn::default();
        assert_eq!(feed(&mut c, &encode_snd(0x08, 0, 0, &[(1, 2)])), vec![]);
        let mut c = ready_conn();
        let bad = b"SND\x00 not iq frame at all";
        for _ in 0..MAX_BAD_SND - 1 {
            assert_eq!(feed(&mut c, bad), vec![]);
        }
        // A good frame resets the run.
        let good = feed(&mut c, &encode_snd(0x08, 0, 0, &[(1, 2)]));
        assert!(matches!(good[..], [Event::Block { .. }]), "{good:?}");
        for _ in 0..MAX_BAD_SND - 1 {
            assert_eq!(feed(&mut c, bad), vec![]);
        }
        let end = feed(&mut c, bad);
        assert!(
            matches!(&end[..], [Event::Terminal(r)] if r.starts_with("protocol: ")),
            "{end:?}"
        );
    }

    #[test]
    fn sequence_gaps_are_flagged() {
        let mut c = ready_conn();
        let gaps: Vec<bool> = [5u32, 6, 8, 9, u32::MAX, 0]
            .iter()
            .map(
                |&s| match feed(&mut c, &encode_snd(0x08, s, 0, &[(0, 0)])).pop() {
                    Some(Event::Block { gap, .. }) => gap,
                    e => panic!("{e:?}"),
                },
            )
            .collect();
        assert_eq!(gaps, [false, false, true, false, true, false]);
    }

    #[test]
    fn clock_is_contiguous_and_reanchors_on_drift_and_gaps() {
        const S: u64 = 1_000_000_000;
        let mut k = Clock::new();
        assert_eq!(k.stamp(12_000, 512, 100 * S), 100 * S);
        assert_eq!(k.stamp(12_000, 512, 100 * S), 100 * S + 42_666_666);
        assert_eq!(k.stamp(12_000, 512, 100 * S), 100 * S + 85_333_333);
        // Wall clock 600 ms past the next stamp: re-anchor to now.
        let now = 100 * S + 128_000_000 + 600_000_000;
        assert_eq!(k.stamp(12_000, 512, now), now);
        // A gap with the wall clock behind the last end: never backwards.
        k.gap();
        let last_end = now + 42_666_666;
        assert_eq!(k.stamp(12_000, 512, now), last_end);
        // Within the drift bound nothing moves.
        assert_eq!(
            k.stamp(12_000, 512, last_end + 400_000_000),
            last_end + 42_666_666
        );
    }
}
