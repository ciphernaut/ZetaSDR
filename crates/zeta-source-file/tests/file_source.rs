mod common;

use std::f64::consts::PI;

use zeta_core::port::{Port, PortError, Tuning};
use zeta_core::types::{Hz, SourceId, Timestamp};
use zeta_source_file::{FileOpts, FileSource, read_meta};

#[test]
fn reads_meta_fields() {
    let dir = tempfile::tempdir().unwrap();
    let meta = read_meta(&common::tone(dir.path())).unwrap();
    assert_eq!(meta.datatype, "cf32_le");
    assert_eq!(meta.sample_rate_hz, Hz(48_000));
    assert_eq!(meta.center_hz, Hz(100_000_000));
    assert_eq!(meta.start, Timestamp(0));
}

#[test]
fn unsupported_datatype_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("bad.sigmf-meta");
    std::fs::write(
        &p,
        r#"{"global":{"core:datatype":"rf32_le","core:sample_rate":48000},
            "captures":[{"core:sample_start":0,"core:frequency":100000000}],"annotations":[]}"#,
    )
    .unwrap();
    match FileSource::open(SourceId("f".into()), &p, FileOpts { realtime: false, block_samples: 4800, repeat: false }) {
        Err(PortError::Unsupported(s)) => assert!(s.contains("rf32_le")),
        other => panic!("expected Unsupported, got {:?}", other.map(|_| ())),
    }
}

fn bin_mag(s: &[num_complex::Complex<f32>], k: usize) -> f64 {
    let n = s.len() as f64;
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (i, x) in s.iter().enumerate() {
        let a = -2.0 * PI * k as f64 * i as f64 / n;
        let (sa, ca) = a.sin_cos();
        re += x.re as f64 * ca - x.im as f64 * sa;
        im += x.re as f64 * sa + x.im as f64 * ca;
    }
    (re * re + im * im).sqrt()
}

#[tokio::test]
async fn streams_all_samples_in_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let path = common::tone(dir.path());
    let meta = read_meta(&path).unwrap();
    let src = FileSource::open(SourceId("f".into()), &path, FileOpts { realtime: false, block_samples: 4800, repeat: false }).unwrap();
    let mut rx = src.subscribe();
    let _h = src.start();
    let mut total = 0;
    let mut first = None;
    for _ in 0..10 {
        let b = rx.recv().await.unwrap();
        assert_eq!(b.sample_rate_hz, Hz(48_000));
        assert_eq!(b.center_hz, Hz(100_000_000));
        total += b.samples.len();
        if first.is_none() {
            first = Some(b);
        }
    }
    assert_eq!(total, 48_000);
    let first = first.unwrap();
    assert_eq!(first.t0, meta.start);
    let m100 = bin_mag(&first.samples, 100);
    assert!(m100 > 10.0 * bin_mag(&first.samples, 0));
    assert!(m100 > 10.0 * bin_mag(&first.samples, 200));
}

#[tokio::test]
async fn tune_off_center_is_out_of_range() {
    let dir = tempfile::tempdir().unwrap();
    let src = FileSource::open(SourceId("f".into()), &common::tone(dir.path()), FileOpts { realtime: false, block_samples: 4800, repeat: false }).unwrap();
    match src.tune(Tuning { center_hz: Hz(101_000_000), bandwidth_hz: Hz(48_000) }).await {
        Err(PortError::OutOfRange { field: "center_hz", .. }) => {}
        other => panic!("got {other:?}"),
    }
    let ok = Tuning { center_hz: Hz(100_000_000), bandwidth_hz: Hz(48_000) };
    assert_eq!(src.tune(ok).await.unwrap(), ok);
}
