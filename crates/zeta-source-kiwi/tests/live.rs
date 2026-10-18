//! Against the real KiwiSDR at kiwisdr.local:8073. Run by hand:
//! `cargo test -p zeta-source-kiwi --test live -- --ignored --nocapture`.

use std::time::{Duration, Instant};

use tokio::sync::broadcast::error::RecvError;
use zeta_core::port::{Port, Tuning};
use zeta_core::types::{Hz, SourceId};
use zeta_source_kiwi::{KiwiConfig, KiwiSource};

const HOST: &str = "kiwisdr.local";
const PORT: u16 = 8073;

#[tokio::test]
#[ignore = "needs ws://kiwisdr.local:8073"]
async fn live_kiwi_7100_khz_iq() {
    let reach = tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::TcpStream::connect((HOST, PORT)),
    )
    .await;
    if !matches!(reach, Ok(Ok(_))) {
        eprintln!("skipped: {HOST}:{PORT} unreachable");
        return;
    }
    let started = Instant::now();
    let src = KiwiSource::new(
        SourceId("kiwi0".into()),
        KiwiConfig::new(HOST, PORT, None).unwrap(),
    );
    let mut rx = src.subscribe();
    let want = Tuning {
        center_hz: Hz(7_100_000),
        bandwidth_hz: Hz(10_000),
    };
    let got = match src.tune(want).await {
        Ok(t) => t,
        Err(e) => {
            // A full or refusing receiver is not this code's failure.
            eprintln!("skipped: {HOST}:{PORT} refused: {e}");
            return;
        }
    };
    let rate = src.audio_rate_hz().expect("audio_rate after tune");
    eprintln!(
        "tuned {got:?}; audio_rate {rate}; sample_rate {:?}",
        src.reported_sample_rate()
    );
    assert_eq!(rate, 12_000);
    assert_eq!(got.bandwidth_hz, Hz(rate as u64));
    let sr = src.reported_sample_rate().unwrap();
    assert!((sr - 12_000.0).abs() < 50.0, "sample_rate {sr}");

    let (mut n, mut power, mut blocks) = (0usize, 0f64, 0usize);
    let mut last_t0: Option<zeta_core::types::Timestamp> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while n < 12_000 {
        let b = match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(b)) => b,
            Ok(Err(RecvError::Lagged(k))) => panic!("lagged {k}"),
            Ok(Err(RecvError::Closed)) => panic!("closed: {:?}", src.fault()),
            Err(_) => panic!("only {n} samples in 15 s; fault {:?}", src.fault()),
        };
        assert_eq!(b.sample_rate_hz, Hz(rate as u64));
        assert_eq!(b.center_hz, Hz(7_100_000));
        if let Some(t) = last_t0 {
            assert!(b.t0.0 > t.0, "t0 strictly increasing");
        }
        last_t0 = Some(b.t0);
        power += b.samples.iter().map(|s| s.norm_sqr() as f64).sum::<f64>();
        n += b.samples.len();
        blocks += 1;
    }
    let mean = power / n as f64;
    eprintln!(
        "{blocks} blocks, {n} samples, mean |s|^2 {mean:.3e}, rssi {:?} dBm, \
         adc_overflows {}, seq_gaps {}, fault {:?}, {:.2} s",
        src.rssi_dbm(),
        src.adc_overflows(),
        src.seq_gaps(),
        src.fault(),
        started.elapsed().as_secs_f64()
    );
    assert!(mean.is_finite() && mean > 0.0, "power {mean}");
    assert_eq!(src.fault(), None);
    drop(src);
    // Let the session send its close frame and free the slot.
    tokio::time::sleep(Duration::from_millis(300)).await;
    eprintln!("closed after {:.2} s", started.elapsed().as_secs_f64());
}
