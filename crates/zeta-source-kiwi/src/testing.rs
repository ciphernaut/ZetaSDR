//! A scripted fake KiwiSDR on `127.0.0.1:0` for tests (feature `test-fake`).
//!
//! Each accepted connection waits for the client's `SET auth`, then answers
//! with the [`FakeReply`] for that connection (the last one repeats). `Ok`
//! replays the live Kiwi's order of `MSG`s and then streams IQ `SND` frames.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

use crate::proto::{SND_FLAG_STEREO, encode_msg, encode_snd};

/// How the fake answers one connection's `SET auth`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FakeReply {
    /// The live handshake, then IQ frames.
    Ok,
    /// `MSG too_busy=<n>`, then close.
    TooBusy(u32),
    /// `MSG badp=<n>`, then close.
    BadPassword(u8),
    /// `MSG down`, then close.
    Down,
}

#[derive(Clone, Debug)]
pub struct FakeScript {
    /// One reply per connection, in order; the last one repeats.
    pub on_auth: Vec<FakeReply>,
    /// `Some(n)`: drop the connection abruptly after `n` SND frames.
    pub frames_per_connection: Option<usize>,
    pub frame_interval_ms: u64,
    /// When false the listener closes after the first connection ends, so
    /// every reconnect is refused.
    pub listen_after_drop: bool,
}

impl Default for FakeScript {
    fn default() -> Self {
        Self {
            on_auth: vec![FakeReply::Ok],
            frames_per_connection: None,
            frame_interval_ms: 10,
            listen_after_drop: true,
        }
    }
}

#[derive(Default)]
struct Seen {
    commands: Vec<String>,
    connections: usize,
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

/// The live version line, sent after `badp=0`.
const VERSION: &str = "version_maj=1 version_min=837 debian_ver=11 model=2 platform=0 hw=1 \
ext_clk=0 freq_offset=0.000 abyy=A26 dx_db_name=dx has_attn=1";

impl FakeKiwi {
    pub async fn start(script: FakeScript) -> FakeKiwi {
        assert!(!script.on_auth.is_empty(), "on_auth needs a reply");
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

    /// Websocket upgrades accepted.
    pub fn connections(&self) -> usize {
        self.seen.lock().unwrap().connections
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
    let mut n = 0;
    loop {
        let Ok((tcp, _)) = listener.accept().await else {
            return;
        };
        let reply = script.on_auth[n.min(script.on_auth.len() - 1)];
        n += 1;
        let conn = connection(tcp, reply, script.clone(), seen.clone());
        if script.listen_after_drop {
            tokio::spawn(conn);
        } else {
            conn.await;
            return;
        }
    }
}

// The handshake callback's error type is tungstenite's, large by design.
#[allow(clippy::result_large_err)]
async fn connection(tcp: TcpStream, reply: FakeReply, script: FakeScript, seen: Arc<Mutex<Seen>>) {
    let paths = seen.clone();
    let callback = move |req: &Request, resp: Response| {
        paths
            .lock()
            .unwrap()
            .paths
            .push(req.uri().path().to_string());
        Ok(resp)
    };
    let Ok(ws) = tokio_tungstenite::accept_hdr_async(tcp, callback).await else {
        return;
    };
    seen.lock().unwrap().connections += 1;
    let (mut tx, mut rx) = ws.split();
    let (authed_tx, authed) = oneshot::channel::<()>();
    let (closed_tx, mut closed) = oneshot::channel::<()>();
    let reader_seen = seen.clone();
    let reader = tokio::spawn(async move {
        let mut authed_tx = Some(authed_tx);
        while let Some(Ok(m)) = rx.next().await {
            match m {
                Message::Text(t) => {
                    if t.starts_with("SET auth")
                        && let Some(a) = authed_tx.take()
                    {
                        let _ = a.send(());
                    }
                    reader_seen.lock().unwrap().commands.push(t.to_string());
                }
                Message::Close(_) => {
                    reader_seen.lock().unwrap().clean_closes += 1;
                    break;
                }
                _ => {}
            }
        }
        let _ = closed_tx.send(());
    });
    if authed.await.is_err() {
        return;
    }
    let msg = |s: &str| Message::Binary(encode_msg(s).into());
    let refusal = match reply {
        FakeReply::Ok => None,
        FakeReply::TooBusy(n) => Some(format!("too_busy={n}")),
        FakeReply::BadPassword(n) => Some(format!("badp={n}")),
        FakeReply::Down => Some("down".to_string()),
    };
    if let Some(r) = refusal {
        let _ = tx.send(msg(&r)).await;
        let _ = tx.close().await;
        let _ = reader.await;
        return;
    }
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
    let mut tick = tokio::time::interval(Duration::from_millis(script.frame_interval_ms.max(1)));
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
