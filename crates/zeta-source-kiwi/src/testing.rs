//! A scripted fake KiwiSDR on `127.0.0.1:0` for tests (feature `test-fake`).
//!
//! Each accepted connection waits for the client's `SET auth`, then answers
//! with the [`FakeReply`] for that connection (the last one repeats): an `SND`
//! socket from [`FakeScript::on_auth`], a `W/F` socket (its path ends `/W/F`)
//! from [`FakeWf::on_auth`], each counted apart. `Ok` replays the live Kiwi's
//! order of `MSG`s and then streams IQ `SND` frames, or on `W/F` the server's
//! waterfall setup and then frames of the view last asked for.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

use crate::proto::{
    SND_FLAG_STEREO, WF_FLAG_COMPRESSED, WF_PIXELS, encode_msg, encode_snd, encode_wf,
};

/// How the fake answers one connection's `SET auth`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FakeReply {
    /// The live handshake, then IQ (or waterfall) frames.
    Ok,
    /// `MSG too_busy=<n>`, then close.
    TooBusy(u32),
    /// `MSG badp=<n>`, then close.
    BadPassword(u8),
    /// `MSG down`, then close.
    Down,
    /// `MSG sample_rate=…` and `badp=0` (on `W/F`, `badp=0` only), then a
    /// filler `MSG` every frame interval and never `audio_rate` (`wf_setup`): a
    /// handshake that never completes.
    NoAudioRate,
    /// A close with no message, as the server refuses a `W/F` when every
    /// connection slot is taken.
    Close,
}

#[derive(Clone, Debug)]
pub struct FakeScript {
    /// One reply per `SND` connection, in order; the last one repeats.
    pub on_auth: Vec<FakeReply>,
    /// `Some(n)`: drop the connection abruptly after `n` SND frames.
    pub frames_per_connection: Option<usize>,
    /// The interval of both streams' frames.
    pub frame_interval_ms: u64,
    /// When false the listener closes after the first connection ends, so
    /// every reconnect is refused.
    pub listen_after_drop: bool,
    /// How `W/F` connections are answered.
    pub wf: FakeWf,
}

impl Default for FakeScript {
    fn default() -> Self {
        Self {
            on_auth: vec![FakeReply::Ok],
            frames_per_connection: None,
            frame_interval_ms: 10,
            listen_after_drop: true,
            wf: FakeWf::default(),
        }
    }
}

/// How the fake answers a `W/F` connection. `Ok` sends the server's messages
/// (`badp=0`, the version line with `freq_offset=<freq_offset_khz>`, then
/// `bandwidth=30000000` and the `wf_setup` line with `wf_chans` and `wf_cal`),
/// then, once a view, `maxdb` and `wf_speed` have been set, a frame every
/// `frame_interval_ms`. A frame's header names the last `SET zoom= cf=` (its
/// `x_bin` truncated as the server's is), but only after `lag_frames` more
/// frames of the view before it; with `lag_frames` over 0 the first frame
/// naming a new view still carries the old view's pixels, as a down-converter
/// that has not flushed would. Pixels 0-1 are 55 and the rest
/// `100 + (x_bin / 4096) % 100`, unless `carriers` lists some: then the rest is a floor
/// near -110 dB with a little ripple, and each carrier is drawn at the pixels its band
/// covers in the view painted (at least the one pixel its centre falls in), so a picture
/// of the frames shows where the view is.
#[derive(Clone, Debug)]
pub struct FakeWf {
    /// One reply per `W/F` connection, in order; the last one repeats.
    pub on_auth: Vec<FakeReply>,
    pub wf_chans: u32,
    pub wf_cal: i32,
    pub freq_offset_khz: f64,
    pub lag_frames: usize,
    /// `Some(n)`: drop the connection abruptly after `n` frames.
    pub frames_per_connection: Option<usize>,
    /// Every frame flagged ADPCM.
    pub compressed: bool,
    /// Frames always name zoom 0 at 0 Hz, whatever is asked.
    pub ignore_view: bool,
    /// Carriers drawn by frequency in every frame (none: the flat `x_bin` paint).
    pub carriers: Vec<FakeCarrier>,
}

/// A carrier the fake's waterfall draws where it sits in the view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FakeCarrier {
    pub hz: u64,
    /// The band drawn, centred on `hz`, Hz.
    pub width_hz: u64,
    /// Its level, dB (the byte is `dB + 255`).
    pub db: i16,
}

impl Default for FakeWf {
    fn default() -> Self {
        Self {
            on_auth: vec![FakeReply::Ok],
            wf_chans: 4,
            wf_cal: 0,
            freq_offset_khz: 0.0,
            lag_frames: 0,
            frames_per_connection: None,
            compressed: false,
            ignore_view: false,
            carriers: Vec::new(),
        }
    }
}

#[derive(Default)]
struct Seen {
    commands: Vec<String>,
    connections: usize,
    wf_connections: usize,
    paths: Vec<String>,
    clean_closes: usize,
}

pub struct FakeKiwi {
    addr: SocketAddr,
    seen: Arc<Mutex<Seen>>,
    task: JoinHandle<()>,
}

impl Drop for FakeKiwi {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Samples per SND frame, as on the live Kiwi.
pub const FAKE_FRAME_SAMPLES: usize = 512;
/// The fake's ADC span, Hz.
pub const FAKE_WF_BANDWIDTH_HZ: u64 = 30_000_000;
/// The fake's finest zoom, as the server's.
const FAKE_ZOOM_MAX: u8 = 14;

/// The live version line, sent after `badp=0`.
const VERSION: &str = "version_maj=1 version_min=837 debian_ver=11 model=2 platform=0 hw=1 \
ext_clk=0 freq_offset=0.000 abyy=A26 dx_db_name=dx has_attn=1";

impl FakeKiwi {
    pub async fn start(script: FakeScript) -> FakeKiwi {
        assert!(!script.on_auth.is_empty(), "on_auth needs a reply");
        assert!(!script.wf.on_auth.is_empty(), "wf.on_auth needs a reply");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind fake");
        let addr = listener.local_addr().expect("fake addr");
        let seen = Arc::new(Mutex::new(Seen::default()));
        let task = tokio::spawn(serve(listener, script, seen.clone()));
        FakeKiwi { addr, seen, task }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Every text frame received, over all connections, in order.
    pub fn commands(&self) -> Vec<String> {
        self.seen.lock().unwrap().commands.clone()
    }

    /// Websocket upgrades accepted, of both streams.
    pub fn connections(&self) -> usize {
        self.seen.lock().unwrap().connections
    }

    /// `W/F` websocket upgrades accepted.
    pub fn wf_connections(&self) -> usize {
        self.seen.lock().unwrap().wf_connections
    }

    /// The request path of each accepted upgrade.
    pub fn paths(&self) -> Vec<String> {
        self.seen.lock().unwrap().paths.clone()
    }

    /// Connections the client ended with a close frame.
    pub fn clean_closes(&self) -> usize {
        self.seen.lock().unwrap().clean_closes
    }
}

async fn serve(listener: TcpListener, script: FakeScript, seen: Arc<Mutex<Seen>>) {
    let script = Arc::new(script);
    let counts = Arc::new(Mutex::new((0usize, 0usize)));
    loop {
        let Ok((tcp, _)) = listener.accept().await else {
            return;
        };
        let conn = connection(tcp, script.clone(), counts.clone(), seen.clone());
        if script.listen_after_drop {
            tokio::spawn(conn);
        } else {
            conn.await;
            return;
        }
    }
}

type Tx = SplitSink<WebSocketStream<TcpStream>, Message>;
type Rx = SplitStream<WebSocketStream<TcpStream>>;

/// The view a `W/F` client asked for, as the server reads its commands.
#[derive(Default)]
struct WfAsked {
    /// Zoom and `x_bin` of the last `SET zoom= cf=`.
    view: Option<(u8, u32)>,
    db: bool,
    speed: bool,
}

/// The server's start of `SET zoom=<z> cf=<kHz>`: `(cf - half a view) /
/// (bandwidth / (1024 << 14))`, clamped to the band and truncated.
fn wf_view(cmd: &str) -> Option<(u8, u32)> {
    let rest = cmd.strip_prefix("SET zoom=")?;
    let (zoom, cf) = rest.split_once(" cf=")?;
    let zoom: u8 = zoom.parse::<u8>().ok()?.min(FAKE_ZOOM_MAX);
    let cf_hz = cf.parse::<f64>().ok()? * 1000.0;
    let units = (WF_PIXELS as f64) * f64::from(1u32 << FAKE_ZOOM_MAX);
    let half = (FAKE_WF_BANDWIDTH_HZ >> zoom) as f64 / 2.0;
    let start = (cf_hz - half) * units / FAKE_WF_BANDWIDTH_HZ as f64;
    let max = (WF_PIXELS << FAKE_ZOOM_MAX) - (WF_PIXELS << (FAKE_ZOOM_MAX - zoom));
    Some((zoom, start.clamp(0.0, max as f64) as u32))
}

/// Reads the client's frames until it closes: every text frame recorded, the
/// first `SET auth` answered on `authed`, a close frame counted, and on a `W/F`
/// socket the view commands kept in `asked`.
fn spawn_reader(
    mut rx: Rx,
    seen: Arc<Mutex<Seen>>,
    asked: Arc<Mutex<WfAsked>>,
    authed_tx: oneshot::Sender<()>,
    closed_tx: oneshot::Sender<()>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut authed_tx = Some(authed_tx);
        while let Some(Ok(m)) = rx.next().await {
            match m {
                Message::Text(t) => {
                    if t.starts_with("SET auth")
                        && let Some(a) = authed_tx.take()
                    {
                        let _ = a.send(());
                    }
                    {
                        let mut a = asked.lock().unwrap();
                        if let Some(v) = wf_view(&t) {
                            a.view = Some(v);
                        }
                        a.db |= t.starts_with("SET maxdb=");
                        a.speed |= t.starts_with("SET wf_speed=");
                    }
                    seen.lock().unwrap().commands.push(t.to_string());
                }
                Message::Close(_) => {
                    seen.lock().unwrap().clean_closes += 1;
                    break;
                }
                _ => {}
            }
        }
        let _ = closed_tx.send(());
    })
}

// The handshake callback's error type is tungstenite's, large by design.
#[allow(clippy::result_large_err)]
async fn connection(
    tcp: TcpStream,
    script: Arc<FakeScript>,
    counts: Arc<Mutex<(usize, usize)>>,
    seen: Arc<Mutex<Seen>>,
) {
    let path = Arc::new(Mutex::new(String::new()));
    let seen_path = path.clone();
    let callback = move |req: &Request, resp: Response| {
        *seen_path.lock().unwrap() = req.uri().path().to_string();
        Ok(resp)
    };
    let Ok(ws) = tokio_tungstenite::accept_hdr_async(tcp, callback).await else {
        return;
    };
    let path = path.lock().unwrap().clone();
    let is_wf = path.ends_with("/W/F");
    let reply = {
        let mut c = counts.lock().unwrap();
        let (n, replies) = if is_wf {
            (&mut c.1, &script.wf.on_auth)
        } else {
            (&mut c.0, &script.on_auth)
        };
        let r = replies[(*n).min(replies.len() - 1)];
        *n += 1;
        r
    };
    {
        let mut s = seen.lock().unwrap();
        s.paths.push(path);
        s.connections += 1;
        if is_wf {
            s.wf_connections += 1;
        }
    }
    let (mut tx, rx) = ws.split();
    let (authed_tx, authed) = oneshot::channel::<()>();
    let (closed_tx, closed) = oneshot::channel::<()>();
    let asked = Arc::new(Mutex::new(WfAsked::default()));
    let reader = spawn_reader(rx, seen.clone(), asked.clone(), authed_tx, closed_tx);
    if authed.await.is_err() {
        return;
    }
    let refusal = match reply {
        FakeReply::Ok | FakeReply::NoAudioRate => None,
        FakeReply::TooBusy(n) => Some(Some(format!("too_busy={n}"))),
        FakeReply::BadPassword(n) => Some(Some(format!("badp={n}"))),
        FakeReply::Down => Some(Some("down".to_string())),
        FakeReply::Close => Some(None),
    };
    if let Some(r) = refusal {
        if let Some(r) = r {
            let _ = tx.send(msg(&r)).await;
        }
        let _ = tx.close().await;
        let _ = reader.await;
        return;
    }
    let interval = Duration::from_millis(script.frame_interval_ms.max(1));
    if reply == FakeReply::NoAudioRate {
        let first: &[&str] = if is_wf {
            &["badp=0"]
        } else {
            &["sample_rate=11998.902088", "badp=0"]
        };
        never_ready(tx, closed, first, interval).await;
        return;
    }
    if is_wf {
        waterfall(tx, reader, closed, asked, &script.wf, interval).await;
    } else {
        sound(tx, reader, closed, &script, interval).await;
    }
}

fn msg(s: &str) -> Message {
    Message::Binary(encode_msg(s).into())
}

/// `first`, then a filler `MSG` every `interval` until the client closes.
async fn never_ready(
    mut tx: Tx,
    mut closed: oneshot::Receiver<()>,
    first: &[&str],
    interval: Duration,
) {
    for m in first {
        if tx.send(msg(m)).await.is_err() {
            return;
        }
    }
    let mut tick = tokio::time::interval(interval);
    loop {
        tokio::select! {
            _ = &mut closed => return,
            _ = tick.tick() => {}
        }
        if tx.send(msg("cfg_loaded")).await.is_err() {
            return;
        }
    }
}

async fn sound(
    mut tx: Tx,
    reader: JoinHandle<()>,
    mut closed: oneshot::Receiver<()>,
    script: &FakeScript,
    interval: Duration,
) {
    for m in [
        "sample_rate=11998.902088",
        "badp=0",
        VERSION,
        "audio_init=1 audio_rate=12000",
    ] {
        if tx.send(msg(m)).await.is_err() {
            return;
        }
    }
    let ramp: Vec<(i16, i16)> = (0..FAKE_FRAME_SAMPLES as i32)
        .map(|k| ((k * 60 - 15_360) as i16, (15_360 - k * 60) as i16))
        .collect();
    let mut tick = tokio::time::interval(interval);
    let mut seq: u32 = 0;
    loop {
        if script
            .frames_per_connection
            .is_some_and(|n| seq as usize >= n)
        {
            // Abrupt drop: no close frame, as a lost link would look.
            reader.abort();
            return;
        }
        tokio::select! {
            _ = &mut closed => return,
            _ = tick.tick() => {}
        }
        let frame = encode_snd(SND_FLAG_STEREO, seq, 564, &ramp);
        if tx.send(Message::Binary(frame.into())).await.is_err() {
            return;
        }
        seq = seq.wrapping_add(1);
    }
}

/// The value the fake paints a view starting at `x_bin` with.
fn painted(x_bin: u32) -> u8 {
    (100 + (x_bin / 4096) % 100) as u8
}

/// The floor the carriers are drawn over: about -110 dB, rippling by frame and pixel.
const CARRIER_FLOOR_DB: i16 = -110;

/// The pixels of the view `(zoom, x_bin)` with `carriers` drawn where their bands fall
/// (a carrier narrower than a pixel takes the pixel its centre is in), frame `seq`'s ripple
/// on the floor; pixels 0-1 left for the caller.
fn carrier_pixels(view: (u8, u32), carriers: &[FakeCarrier], seq: u32) -> Vec<u8> {
    let (zoom, x_bin) = view;
    let units = (WF_PIXELS as f64) * f64::from(1u32 << FAKE_ZOOM_MAX);
    let start = f64::from(x_bin) * FAKE_WF_BANDWIDTH_HZ as f64 / units;
    let pixel = (FAKE_WF_BANDWIDTH_HZ >> zoom) as f64 / WF_PIXELS as f64;
    let byte = |db: i16| (db + 255).clamp(0, 255) as u8;
    let mut px: Vec<u8> = (0..WF_PIXELS)
        .map(|i| {
            let ripple = ((i as u32)
                .wrapping_mul(7)
                .wrapping_add(seq.wrapping_mul(13)))
                % 7;
            byte(CARRIER_FLOOR_DB + ripple as i16 - 3)
        })
        .collect();
    for c in carriers {
        let lo = c.hz as f64 - c.width_hz as f64 / 2.0;
        let hi = c.hz as f64 + c.width_hz as f64 / 2.0;
        let centre = ((c.hz as f64 - start) / pixel).floor();
        let first = ((lo - start) / pixel).floor().min(centre);
        let last = ((hi - start) / pixel).ceil().max(centre + 1.0);
        let n = WF_PIXELS as f64;
        let (first, last) = (first.clamp(0.0, n), last.clamp(0.0, n));
        let lit = (first as usize)..(last.max(first) as usize);
        px[lit].fill(byte(c.db));
    }
    px
}

async fn waterfall(
    mut tx: Tx,
    reader: JoinHandle<()>,
    mut closed: oneshot::Receiver<()>,
    asked: Arc<Mutex<WfAsked>>,
    wf: &FakeWf,
    interval: Duration,
) {
    let version = format!(
        "version_maj=1 version_min=837 debian_ver=11 model=2 platform=0 ext_clk=0 abyy=A26 \
freq_offset={:.3}",
        wf.freq_offset_khz
    );
    let setup = format!(
        "wf_fft_size=1024 wf_fps=23 wf_fps_max=23 zoom_max={FAKE_ZOOM_MAX} rx_chans=8 \
wf_chans={c} wf_chans_real={c} wf_cal={cal} wf_setup",
        c = wf.wf_chans,
        cal = wf.wf_cal
    );
    let bandwidth = format!(
        "center_freq={} bandwidth={FAKE_WF_BANDWIDTH_HZ} adc_clk_nom=66666600",
        FAKE_WF_BANDWIDTH_HZ / 2
    );
    for m in [
        "badp=0",
        version.as_str(),
        bandwidth.as_str(),
        setup.as_str(),
    ] {
        if tx.send(msg(m)).await.is_err() {
            return;
        }
    }
    let flags = if wf.compressed { WF_FLAG_COMPRESSED } else { 0 };
    let mut tick = tokio::time::interval(interval);
    let mut seq: u32 = 0;
    // The view frames name, and a newer one with the frames of lag left.
    let mut shown: Option<(u8, u32)> = None;
    let mut pending: Option<((u8, u32), usize)> = None;
    loop {
        if wf.frames_per_connection.is_some_and(|n| seq as usize >= n) {
            // Abrupt drop: no close frame, as a lost link would look.
            reader.abort();
            return;
        }
        tokio::select! {
            _ = &mut closed => return,
            _ = tick.tick() => {}
        }
        let target = {
            let a = asked.lock().unwrap();
            match a.view {
                Some(v) if a.db && a.speed => v,
                _ => continue,
            }
        };
        let (header, paint) = if wf.ignore_view {
            ((0, 0), (0, 0))
        } else {
            match shown {
                None => {
                    shown = Some(target);
                    (target, target)
                }
                Some(cur) if cur == target => {
                    pending = None;
                    (cur, cur)
                }
                Some(cur) => {
                    let left = match pending {
                        Some((t, left)) if t == target => left,
                        _ => wf.lag_frames,
                    };
                    if left > 0 {
                        pending = Some((target, left - 1));
                        (cur, cur)
                    } else {
                        pending = None;
                        shown = Some(target);
                        let paint = if wf.lag_frames > 0 { cur } else { target };
                        (target, paint)
                    }
                }
            }
        };
        let mut pixels = if wf.carriers.is_empty() {
            vec![painted(paint.1); WF_PIXELS]
        } else {
            carrier_pixels(paint, &wf.carriers, seq)
        };
        pixels[..2].fill(55);
        let frame = encode_wf(header.1, header.0, flags, seq, &pixels);
        if tx.send(Message::Binary(frame.into())).await.is_err() {
            return;
        }
        seq = seq.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fake_reads_a_view_as_the_server_does() {
        assert_eq!(wf_view("SET zoom=3 cf=1875.000"), Some((3, 0)));
        assert_eq!(wf_view("SET zoom=3 cf=5625.000"), Some((3, 2_097_152)));
        // Clamped to the band at both edges.
        assert_eq!(wf_view("SET zoom=3 cf=1000.000"), Some((3, 0)));
        let max = (1024 << 14) - (1024 << 11);
        assert_eq!(wf_view("SET zoom=3 cf=29999.000"), Some((3, max)));
        assert_eq!(wf_view("SET maxdb=0 mindb=-100"), None);
    }

    #[test]
    fn carriers_are_drawn_where_they_sit_in_the_view() {
        let carrier = FakeCarrier {
            hz: 7_510_000,
            width_hz: 3_000,
            db: -40,
        };
        let high = (-40i16 + 255) as u8;
        let x_bin_of = |hz: u64| (hz * (1024 << 14) / FAKE_WF_BANDWIDTH_HZ) as u32;
        // Zoom 0, the whole band: 29 296.875 Hz a pixel, 7.51 MHz in pixel 256.
        let px = carrier_pixels((0, 0), &[carrier], 0);
        let lit: Vec<usize> = (0..WF_PIXELS).filter(|i| px[*i] == high).collect();
        assert_eq!(lit, vec![256]);
        // Zoom 2 from 7.5 MHz: about 7.3 kHz a pixel, 10 kHz up is pixel 1.
        let px = carrier_pixels((2, x_bin_of(7_500_000)), &[carrier], 0);
        let lit: Vec<usize> = (0..WF_PIXELS).filter(|i| px[*i] == high).collect();
        assert_eq!(lit, vec![1]);
        // Zoom 10 centred on it: some 105 pixels of a 28.6 Hz grid.
        let start = 7_510_000 - (FAKE_WF_BANDWIDTH_HZ >> 10) / 2;
        let px = carrier_pixels((10, x_bin_of(start)), &[carrier], 0);
        let lit = (0..WF_PIXELS).filter(|i| px[*i] == high).count();
        assert!((100..=110).contains(&lit), "{lit}");
        assert_eq!(px[512], high);
        // A carrier out of the view draws nothing.
        assert!(
            carrier_pixels((10, 0), &[carrier], 0)
                .iter()
                .all(|p| *p < 150)
        );
    }
}
