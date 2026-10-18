//! Test fixture generator: a one-second 1 kHz tone as SigMF cf32_le.

use std::f64::consts::PI;
use std::path::{Path, PathBuf};

/// Write `tone.sigmf-meta` and `tone.sigmf-data` into `dir` if absent; return the meta path.
pub fn write_tone(dir: &Path) -> PathBuf {
    const RATE: usize = 48_000;
    let meta = dir.join("tone.sigmf-meta");
    let data = dir.join("tone.sigmf-data");
    if !data.exists() {
        let mut bytes = Vec::with_capacity(RATE * 8);
        for n in 0..RATE {
            let (s, c) = (2.0 * PI * 1000.0 * n as f64 / RATE as f64).sin_cos();
            bytes.extend_from_slice(&((0.5 * c) as f32).to_le_bytes());
            bytes.extend_from_slice(&((0.5 * s) as f32).to_le_bytes());
        }
        std::fs::write(&data, bytes).expect("write tone data");
    }
    if !meta.exists() {
        std::fs::write(
            &meta,
            r#"{"global":{"core:datatype":"cf32_le","core:sample_rate":48000,"core:version":"1.0.0"},
"captures":[{"core:sample_start":0,"core:frequency":100000000}],"annotations":[]}"#,
        )
        .expect("write tone meta");
    }
    meta
}
