//! VITA 49.2 (VRT) packets over UDP as a push [`Port`].
//!
//! [`open`] binds a UDP socket and spawns a receive loop: each datagram is one VRT packet
//! (as `zeta_vrt::VrtEncoder` produces them) fed through one [`VrtDecoder`]. Data packets
//! become [`IqBlock`]s on a broadcast; the last context packet is the capture band
//! reported by [`Port::current`]. Packets that do not decode (garbage, data before any
//! context) are counted by [`VrtPort::errors`] and skipped; they are never a fault. The
//! loop ends when the port is dropped.
//!
//! The sender decides the frequency and rate, so [`Port::tune`] only accepts a request
//! that lies inside the band the sender is already on. Before the first context packet it
//! waits up to [`CONTEXT_WAIT`] for one: senders repeat context about once a second, and a
//! job is often started right after the port is opened.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use num_complex::Complex;
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::types::{Capabilities, Claim, Hz, SampleFormat, SourceId, Timestamp};
use zeta_vrt::VrtDecoder;

/// Largest band advertised, in Hz.
pub const VRT_MAX_BANDWIDTH_HZ: u64 = 20_000_000;
/// How long `tune` waits for the first context packet: the 1 s context interval of
/// `zeta_vrt::VrtEncoder`, plus slack.
pub const CONTEXT_WAIT: Duration = Duration::from_millis(1_500);
/// Blocks buffered per receiver.
const BROADCAST_CAPACITY: usize = 64;
/// Receive buffer: the largest UDP payload, rounded up.
const DATAGRAM_BYTES: usize = 65_536;

/// Where to listen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VrtSourceConfig {
    /// The id of this port.
    pub id: SourceId,
    /// The UDP address to bind; port 0 picks a free one (see [`VrtPort::local_addr`]).
    pub bind: SocketAddr,
}

/// Decode failures, shared with the receive loop.
#[derive(Default)]
struct Shared {
    errors: AtomicU64,
}

/// A VRT sender on UDP as a push [`Port`]; see the crate docs.
pub struct VrtPort {
    id: SourceId,
    caps: Capabilities,
    local: SocketAddr,
    tx: broadcast::Sender<IqBlock>,
    /// The band of the last context packet, set by the receive loop.
    current: watch::Receiver<Option<Tuning>>,
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

/// Bind `cfg.bind` and start receiving; a bind failure is [`PortError::Io`].
pub async fn open(cfg: VrtSourceConfig) -> Result<VrtPort, PortError> {
    let io = |e: std::io::Error| {
        PortError::Io(format!("vrt source {}: udp {}: {e}", cfg.id.0, cfg.bind))
    };
    let sock = UdpSocket::bind(cfg.bind).await.map_err(io)?;
    let local = sock.local_addr().map_err(io)?;
    let tx = broadcast::channel(BROADCAST_CAPACITY).0;
    let shared = Arc::new(Shared::default());
    let (band_tx, current) = watch::channel(None);
    let task = tokio::spawn(receive(
        sock,
        tx.clone(),
        band_tx,
        shared.clone(),
        cfg.id.0.clone(),
    ));
    Ok(VrtPort {
        id: cfg.id,
        caps: Capabilities {
            tune_min_hz: Claim::Asserted(Hz(0)),
            tune_max_hz: Claim::Asserted(Hz(u64::MAX / 2)),
            max_bandwidth_hz: Claim::Asserted(Hz(VRT_MAX_BANDWIDTH_HZ)),
            formats: vec![SampleFormat::Cf32],
            channels: 1,
            rx: true,
            tx: false,
            remote: true,
            push: true,
        },
        local,
        tx,
        current,
        shared,
        task,
    })
}

impl VrtPort {
    /// The bound UDP address (the real port when bound to port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// Datagrams that could not be received or decoded so far.
    pub fn errors(&self) -> u64 {
        self.shared.errors.load(Ordering::Relaxed)
    }
}

impl Drop for VrtPort {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[async_trait]
impl Port for VrtPort {
    fn id(&self) -> &SourceId {
        &self.id
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    /// A VRT sender is not tunable from here: succeeds, returning the sender's band,
    /// only when `t` lies inside it. Waits up to [`CONTEXT_WAIT`] for the first context
    /// packet.
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        let id = &self.id.0;
        let mut rx = self.current.clone();
        // A closed channel (receive loop gone) or the timeout both leave `None`.
        let _ = tokio::time::timeout(CONTEXT_WAIT, rx.wait_for(Option::is_some)).await;
        let Some(band) = self.current() else {
            return Err(PortError::Unsupported(format!(
                "vrt source {id}: no context packet received yet; retune the sender"
            )));
        };
        if contains(band, t) {
            Ok(band)
        } else {
            Err(PortError::Unsupported(format!(
                "vrt source {id}: sender is at {} ± {} Hz; retune the sender",
                band.center_hz.0,
                band.bandwidth_hz.0 / 2
            )))
        }
    }

    fn current(&self) -> Option<Tuning> {
        *self.current.borrow()
    }

    fn subscribe(&self) -> IqReceiver {
        self.tx.subscribe()
    }
}

/// Whether `t.center ± t.bandwidth/2` lies within `band.center ± band.bandwidth/2`
/// (compared doubled, so odd widths need no rounding).
fn contains(band: Tuning, t: Tuning) -> bool {
    let (bc, bw) = (
        i128::from(band.center_hz.0),
        i128::from(band.bandwidth_hz.0),
    );
    let (tc, tw) = (i128::from(t.center_hz.0), i128::from(t.bandwidth_hz.0));
    2 * tc - tw >= 2 * bc - bw && 2 * tc + tw <= 2 * bc + bw
}

/// Receive datagrams until aborted. Every datagram goes through one decoder; failures
/// are counted (the first is logged) and skipped. Blocks go out even with no receivers
/// (dropped), so a later subscriber still gets them.
async fn receive(
    sock: UdpSocket,
    tx: broadcast::Sender<IqBlock>,
    band: watch::Sender<Option<Tuning>>,
    shared: Arc<Shared>,
    id: String,
) {
    let mut buf = vec![0u8; DATAGRAM_BYTES];
    let mut decoder = VrtDecoder::new();
    let fail = |what: &dyn std::fmt::Display| {
        if shared.errors.fetch_add(1, Ordering::Relaxed) == 0 {
            eprintln!("vrt source {id}: skipping datagram: {what} (further ones only counted)");
        }
    };
    loop {
        let n = match sock.recv_from(&mut buf).await {
            Ok((n, _from)) => n,
            Err(e) => {
                fail(&e);
                tokio::task::yield_now().await;
                continue;
            }
        };
        let decoded = match decoder.feed(&buf[..n]) {
            Ok(d) => d,
            Err(e) => {
                fail(&e);
                continue;
            }
        };
        let Some(d) = decoded else {
            // A context packet: the sender's band, as a capture band.
            if let Some(meta) = decoder.meta() {
                let new = Some(Tuning {
                    center_hz: Hz(meta.center_hz),
                    bandwidth_hz: Hz(meta.sample_rate_hz),
                });
                band.send_if_modified(|c| std::mem::replace(c, new) != new);
            }
            continue;
        };
        let samples: Arc<[Complex<f32>]> = d.samples.into();
        // No receivers: the block is dropped and the loop keeps going.
        let _ = tx.send(IqBlock {
            t0: Timestamp(d.t0_ns),
            sample_rate_hz: Hz(d.meta.sample_rate_hz),
            center_hz: Hz(d.meta.center_hz),
            samples,
        });
    }
}
