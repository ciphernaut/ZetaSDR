//! Needs a HackRF and libsoapysdr-dev; run from this crate's directory:
//! `cargo test -- --ignored`.

use std::time::{Duration, Instant};

use tokio::sync::broadcast::error::RecvError;
use zeta_core::port::{Port, Tuning};
use zeta_core::types::{Hz, SourceId};
use zeta_source_soapy_driver::{find, open};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a HackRF and libsoapysdr-dev"]
async fn hackrf_streams_one_second_at_100_mhz() {
    if find("driver=hackrf").expect("enumerate").is_empty() {
        eprintln!("skipped: no hackrf attached");
        return;
    }
    let src = open(SourceId("hackrf0".into()), "driver=hackrf").expect("open");
    let mut rx = src.subscribe();
    let got = src
        .tune(Tuning {
            center_hz: Hz(100_000_000),
            bandwidth_hz: Hz(2_000_000),
        })
        .await
        .expect("tune");
    assert_eq!(got.bandwidth_hz, Hz(2_000_000));
    let start = Instant::now();
    let mut last_t0 = None;
    let mut blocks = 0;
    while start.elapsed() < Duration::from_secs(1) {
        let b = match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Ok(b)) => b,
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed)) => panic!("stream closed: {:?}", src.fault()),
            Err(_) => panic!("no block within 2 s: {:?}", src.fault()),
        };
        assert_eq!(b.sample_rate_hz, Hz(2_000_000));
        if let Some(prev) = last_t0 {
            assert!(b.t0.0 > prev, "t0 not strictly increasing");
        }
        last_t0 = Some(b.t0.0);
        let p: f32 = b.samples.iter().map(|s| s.norm_sqr()).sum::<f32>() / b.samples.len() as f32;
        assert!(p.is_finite(), "power {p}");
        blocks += 1;
    }
    assert!(blocks > 0);
    assert_eq!(src.fault(), None);
    eprintln!(
        "{blocks} blocks, {} overflows, measured {:?} S/s",
        src.overflows(),
        src.measured_rate_hz()
    );
}
