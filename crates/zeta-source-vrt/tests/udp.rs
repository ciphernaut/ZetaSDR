//! The VRT port fed by real UDP datagrams on loopback.

use std::net::SocketAddr;
use std::time::Duration;

use num_complex::Complex;
use tokio::net::UdpSocket;
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::types::{Hz, SourceId};
use zeta_source_vrt::{VrtPort, VrtSourceConfig, open};
use zeta_vrt::{IqMeta, VrtEncoder};

const WAIT: Duration = Duration::from_secs(5);

const META: IqMeta = IqMeta {
    center_hz: 1_000_000,
    sample_rate_hz: 48_000,
    bandwidth_hz: 48_000,
};

async fn port() -> VrtPort {
    open(VrtSourceConfig {
        id: SourceId("v".into()),
        bind: "127.0.0.1:0".parse().unwrap(),
    })
    .await
    .expect("bind loopback")
}

async fn sender() -> UdpSocket {
    UdpSocket::bind("127.0.0.1:0").await.unwrap()
}

/// Encode block `n` (480 samples, 10 ms at 48 kHz) and send its packets to `to`.
async fn send_block(sock: &UdpSocket, enc: &mut VrtEncoder, to: SocketAddr, n: u64) {
    let samples = vec![Complex::new(0.5f32, 0.0); 480];
    for p in enc.encode(n * 10_000_000, META, &samples) {
        sock.send_to(&p, to).await.unwrap();
    }
}

async fn next_block(rx: &mut IqReceiver) -> IqBlock {
    tokio::time::timeout(WAIT, rx.recv())
        .await
        .expect("a block within 5 s")
        .expect("receiver open")
}

fn tuning(center: u64, bw: u64) -> Tuning {
    Tuning {
        center_hz: Hz(center),
        bandwidth_hz: Hz(bw),
    }
}

#[tokio::test]
async fn udp_packets_become_blocks_and_tune_follows_context() {
    let port = port().await;
    let caps = port.capabilities();
    assert!(caps.remote && caps.push);
    assert_eq!(caps.max_bandwidth_hz.value().0, 20_000_000);
    let mut rx = port.subscribe();
    let to = port.local_addr();
    let sock = sender().await;
    let mut enc = VrtEncoder::new(7);
    for n in 0..5 {
        send_block(&sock, &mut enc, to, n).await;
    }
    let mut last_t0 = None;
    for _ in 0..5 {
        let b = next_block(&mut rx).await;
        assert_eq!(b.samples.len(), 480);
        assert_eq!(b.sample_rate_hz, Hz(48_000));
        assert_eq!(b.center_hz, Hz(1_000_000));
        if let Some(prev) = last_t0 {
            assert!(b.t0.0 > prev, "t0 {} not after {prev}", b.t0.0);
        }
        last_t0 = Some(b.t0.0);
    }
    let band = tuning(1_000_000, 48_000);
    assert_eq!(port.current(), Some(band));
    assert_eq!(port.tune(tuning(1_000_000, 24_000)).await.unwrap(), band);
    match port.tune(tuning(2_000_000, 24_000)).await {
        Err(PortError::Unsupported(m)) => {
            assert!(m.contains("vrt source v"), "{m}");
            assert!(m.contains("retune the sender"), "{m}");
            assert!(m.contains("1000000"), "{m}");
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
    assert_eq!(port.errors(), 0);
    assert_eq!(port.fault(), None);
}

#[tokio::test]
async fn garbage_datagrams_are_counted_not_fatal() {
    let port = port().await;
    let mut rx = port.subscribe();
    let to = port.local_addr();
    let sock = sender().await;
    for _ in 0..3 {
        sock.send_to(b"nope", to).await.unwrap();
    }
    let mut enc = VrtEncoder::new(1);
    for n in 0..3 {
        send_block(&sock, &mut enc, to, n).await;
    }
    for _ in 0..3 {
        assert_eq!(next_block(&mut rx).await.samples.len(), 480);
    }
    assert_eq!(port.errors(), 3);
    assert_eq!(port.fault(), None);
}

#[tokio::test]
async fn tune_before_any_context_is_unsupported_and_says_so() {
    let port = port().await;
    assert_eq!(port.current(), None);
    match port.tune(tuning(1_000_000, 24_000)).await {
        Err(PortError::Unsupported(m)) => {
            assert!(m.contains("no context packet received yet"), "{m}");
            assert!(m.contains("vrt source v"), "{m}");
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

/// Encode block `n` with `enc`, returning its packets (context first when due).
fn packets(enc: &mut VrtEncoder, meta: IqMeta, n: u64) -> Vec<Vec<u8>> {
    let samples = vec![Complex::new(0.5f32, 0.0); 480];
    enc.encode(n * 10_000_000, meta, &samples)
}

/// Wait until `port.dropped()` reaches `n` or 5 s pass.
async fn wait_dropped(port: &VrtPort, n: u64) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while port.dropped() < n {
        assert!(
            tokio::time::Instant::now() < deadline,
            "dropped {} < {n}",
            port.dropped()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn duplicate_and_reordered_packets_are_dropped() {
    let port = port().await;
    let mut rx = port.subscribe();
    let to = port.local_addr();
    let sock = sender().await;
    let mut enc = VrtEncoder::new(7);
    let b0 = packets(&mut enc, META, 0); // context + data
    let b1 = packets(&mut enc, META, 1);
    let b2 = packets(&mut enc, META, 2);
    assert_eq!((b0.len(), b1.len(), b2.len()), (2, 1, 1));
    // b1 twice (duplicate), then b0's data again (reordered), then b2.
    for p in [&b0[0], &b0[1], &b1[0], &b1[0], &b0[1], &b2[0]] {
        sock.send_to(p, to).await.unwrap();
    }
    let t0s: Vec<u64> = [
        next_block(&mut rx).await,
        next_block(&mut rx).await,
        next_block(&mut rx).await,
    ]
    .iter()
    .map(|b| b.t0.0)
    .collect();
    assert_eq!(t0s, vec![0, 10_000_000, 20_000_000]);
    wait_dropped(&port, 2).await;
    assert_eq!(port.dropped(), 2);
    assert_eq!(port.errors(), 0);
    assert!(rx.try_recv().is_err(), "no further block");
}

#[tokio::test]
async fn second_sender_is_ignored() {
    let port = port().await;
    let mut rx = port.subscribe();
    let to = port.local_addr();
    let (a, b) = (sender().await, sender().await);
    let mut enc_a = VrtEncoder::new(7);
    let mut enc_b = VrtEncoder::new(9);
    let other = IqMeta {
        center_hz: 2_000_000,
        ..META
    };
    for p in packets(&mut enc_a, META, 0) {
        a.send_to(&p, to).await.unwrap();
    }
    assert_eq!(next_block(&mut rx).await.center_hz, Hz(1_000_000));
    // Sender B: its context and its data (later t0, so only identity can reject it).
    for p in packets(&mut enc_b, other, 5) {
        b.send_to(&p, to).await.unwrap();
    }
    wait_dropped(&port, 2).await;
    for p in packets(&mut enc_a, META, 1) {
        a.send_to(&p, to).await.unwrap();
    }
    let blk = next_block(&mut rx).await;
    assert_eq!(blk.center_hz, Hz(1_000_000));
    assert_eq!(blk.t0.0, 10_000_000);
    assert_eq!(port.current(), Some(tuning(1_000_000, 48_000)));
    assert_eq!(port.dropped(), 2);
    assert_eq!(port.errors(), 0);
}

#[tokio::test]
async fn context_from_another_sender_before_any_data_repins() {
    let port = port().await;
    let mut rx = port.subscribe();
    let to = port.local_addr();
    let (a, b) = (sender().await, sender().await);
    let mut enc_a = VrtEncoder::new(7);
    let mut enc_b = VrtEncoder::new(9);
    // A's context only, then B takes over before any data arrived.
    let pa = packets(&mut enc_a, META, 0);
    a.send_to(&pa[0], to).await.unwrap();
    let other = IqMeta {
        center_hz: 2_000_000,
        ..META
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    for p in packets(&mut enc_b, other, 1) {
        b.send_to(&p, to).await.unwrap();
    }
    assert_eq!(next_block(&mut rx).await.center_hz, Hz(2_000_000));
    assert_eq!(port.dropped(), 0);
}
