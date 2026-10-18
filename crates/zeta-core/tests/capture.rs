use num_complex::Complex;
use zeta_core::capture::{CaptureMeta, write_cf32};
use zeta_core::port::Port;
use zeta_core::types::{Hz, SourceId, Timestamp};
use zeta_source_file::{FileOpts, FileSource, read_meta};

#[tokio::test]
async fn write_cf32_roundtrips_through_read_meta_and_file_source() {
    let dir = tempfile::tempdir().unwrap();
    let samples: Vec<Complex<f32>> = (0..480)
        .map(|n| Complex::new(n as f32 * 0.001, -(n as f32) * 0.002))
        .collect();
    let meta = CaptureMeta {
        sample_rate_hz: Hz(48_000),
        center_hz: Hz(100_000_000),
        start: Timestamp(5_000_000_000),
        annotations: vec![serde_json::json!({"core:sample_start": 0, "core:label": "x"})],
    };
    let path = write_cf32(dir.path(), "cap", &meta, &samples).unwrap();
    assert_eq!(path, dir.path().join("cap.sigmf-meta"));
    assert!(dir.path().join("cap.sigmf-data").exists());

    let m = read_meta(&path).unwrap();
    assert_eq!(m.datatype, "cf32_le");
    assert_eq!(m.sample_rate_hz, Hz(48_000));
    assert_eq!(m.center_hz, Hz(100_000_000));
    assert_eq!(m.start, Timestamp(5_000_000_000));

    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(json["annotations"][0]["core:label"], "x");

    let src = FileSource::open(
        SourceId("c".into()),
        &path,
        FileOpts {
            realtime: false,
            block_samples: 100,
            repeat: false,
        },
    )
    .unwrap();
    let mut rx = src.subscribe();
    let _h = src.start();
    let mut got = Vec::new();
    while let Ok(b) = rx.recv().await {
        got.extend_from_slice(&b.samples);
    }
    assert_eq!(got.len(), 480);
    for (a, b) in got.iter().zip(&samples) {
        assert!((a - b).norm() < 1e-6);
    }
}

#[test]
fn iso8601_formats_utc_nanos() {
    use zeta_core::capture::iso8601;
    assert_eq!(iso8601(Timestamp(0)), "1970-01-01T00:00:00.000000000Z");
    assert_eq!(
        iso8601(Timestamp(1_760_000_000_000_000_001)),
        "2025-10-09T08:53:20.000000001Z"
    );
}
