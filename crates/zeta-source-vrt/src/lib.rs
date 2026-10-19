//! VITA 49.2 (VRT) packets over UDP as a push [`Port`].
//!
//! [`open`] binds a UDP socket and spawns a receive loop: each datagram is one VRT packet
//! (as `zeta_vrt::VrtEncoder` produces them) fed through one [`VrtDecoder`]. Data packets
//! become [`IqBlock`]s on a broadcast; the last context packet is the capture band
//! reported by [`Port::current`]. Packets that do not decode (garbage, data before any
//! context) are counted by [`VrtPort::errors`] and skipped; they are never a fault. The
//! loop ends when the port is dropped.
//!
//! The port follows one sender: the stream id of the first context packet is pinned.
//! Until the first data packet arrives a context with another id re-pins (a sender
//! that restarted under a new id); after that, packets of any other stream id are
//! dropped. A data packet whose `t0` is earlier than the end of the previous block
//! (a duplicated or reordered datagram) is dropped too, so `t0` never goes backwards.
//! Both kinds of drop are counted by [`VrtPort::dropped`], apart from decode errors.
//!
//! The ingest is unauthenticated: anyone who can reach the UDP port can feed it, so bind
//! a specific LAN address.
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
use zeta_vrt::{Header, PacketKind, VrtDecoder};

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
    dropped: AtomicU64,
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

    /// Well-formed packets dropped so far: from a stream other than the pinned one,
    /// or data whose `t0` runs backwards (duplicated or reordered datagrams).
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
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
    // The pinned stream id, whether any data was forwarded, and where the last block ended.
    let mut pinned: Option<u32> = None;
    let mut seen_data = false;
    let mut next_t0: Option<u64> = None;
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
                // Back off so a persistent socket error cannot spin the core.
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                continue;
            }
        };
        let packet = &buf[..n];
        // Stream identity: drop other senders once data has flowed from the pinned one.
        let head = peek(packet);
        if let (Some(pin), Some((kind, sid))) = (pinned, head)
            && sid != pin
            && (kind == PacketKind::Data || seen_data)
        {
            drop_one(&shared, &id, "another stream id");
            continue;
        }
        let decoded = match decoder.feed(packet) {
            Ok(d) => d,
            Err(e) => {
                fail(&e);
                continue;
            }
        };
        let Some(d) = decoded else {
            // A context packet: pin its sender and report its band as the capture band.
            pinned = head.map(|(_, sid)| sid);
            if let Some(meta) = decoder.meta() {
                let new = Some(Tuning {
                    center_hz: Hz(meta.center_hz),
                    bandwidth_hz: Hz(meta.sample_rate_hz),
                });
                band.send_if_modified(|c| std::mem::replace(c, new) != new);
            }
            continue;
        };
        if next_t0.is_some_and(|end| d.t0_ns < end) {
            drop_one(&shared, &id, "t0 before the previous block's end");
            continue;
        }
        seen_data = true;
        let step = (d.samples.len() as u128 * 1_000_000_000)
            .checked_div(u128::from(d.meta.sample_rate_hz))
            .unwrap_or(0);
        next_t0 = Some(
            d.t0_ns
                .saturating_add(u64::try_from(step).unwrap_or(u64::MAX)),
        );
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

/// The kind and stream id of a packet whose header decodes and that is long enough to
/// carry a stream id (both packet kinds put it in word 1).
fn peek(packet: &[u8]) -> Option<(PacketKind, u32)> {
    let head = Header::decode(u32::from_be_bytes(*packet.first_chunk::<4>()?)).ok()?;
    let sid = packet.get(4..8)?;
    Some((head.kind, u32::from_be_bytes(sid.try_into().ok()?)))
}

/// Count a dropped packet; the first one is logged.
fn drop_one(shared: &Shared, id: &str, why: &str) {
    if shared.dropped.fetch_add(1, Ordering::Relaxed) == 0 {
        eprintln!("vrt source {id}: dropping packet: {why} (further ones only counted)");
    }
}

#[cfg(test)]
mod contains_tests {
    use super::*;
    use zeta_core::types::Hz;

    fn t(c: u64, w: u64) -> Tuning {
        Tuning {
            center_hz: Hz(c),
            bandwidth_hz: Hz(w),
        }
    }

    #[test]
    fn exact_edge_fit() {
        assert!(contains(t(1000, 100), t(1000, 100)));
        assert!(contains(t(1000, 100), t(1025, 50)));
        assert!(contains(t(1000, 100), t(975, 50)));
    }

    #[test]
    fn odd_widths_on_both_sides() {
        // width 99 means half-width 49.5 on each side
        assert!(contains(t(1000, 99), t(1000, 99)));
        assert!(contains(t(1000, 100), t(1000, 99)));
        assert!(!contains(t(1000, 99), t(1000, 100)));
        // odd request touching the low edge of an even band
        assert!(contains(t(1000, 100), t(976, 49)));
        assert!(!contains(t(1000, 100), t(975, 51)));
    }

    #[test]
    fn one_hz_over_the_edge() {
        assert!(!contains(t(1000, 100), t(1025, 51)));
        assert!(!contains(t(1000, 100), t(1026, 50)));
        assert!(!contains(t(1000, 100), t(974, 50)));
    }

    #[test]
    fn centre_near_u64_max() {
        let m = u64::MAX;
        assert!(contains(t(m - 10, 20), t(m - 10, 20)));
        assert!(!contains(t(m - 10, 20), t(m - 10, 22)));
        assert!(!contains(t(m - 10, 20), t(m, 2)));
    }
}
