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
