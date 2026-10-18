//! SigMF capture writer: a `cf32_le` data file plus its `.sigmf-meta`.

use std::path::{Path, PathBuf};

use num_complex::Complex;

use crate::types::{Hz, Timestamp};

/// What goes into a capture's `.sigmf-meta` besides the fixed fields.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureMeta {
    pub sample_rate_hz: Hz,
    pub center_hz: Hz,
    pub start: Timestamp,
    /// SigMF annotation objects, written verbatim.
    pub annotations: Vec<serde_json::Value>,
}

/// `YYYY-MM-DDTHH:MM:SS.nnnnnnnnnZ` (UTC) for nanoseconds since the epoch.
pub fn iso8601(t: Timestamp) -> String {
    let (secs, nanos) = (t.0 / 1_000_000_000, t.0 % 1_000_000_000);
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil from days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{nanos:09}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Write `<stem>.sigmf-data` (cf32_le) and `<stem>.sigmf-meta` into `dir`
/// (which must exist) and return the meta path.
pub fn write_cf32(
    dir: &Path,
    stem: &str,
    meta: &CaptureMeta,
    samples: &[Complex<f32>],
) -> std::io::Result<PathBuf> {
    let mut bytes = Vec::with_capacity(samples.len() * 8);
    for s in samples {
        bytes.extend_from_slice(&s.re.to_le_bytes());
        bytes.extend_from_slice(&s.im.to_le_bytes());
    }
    std::fs::write(dir.join(format!("{stem}.sigmf-data")), bytes)?;
    let json = serde_json::json!({
        "global": {
            "core:datatype": "cf32_le",
            "core:sample_rate": meta.sample_rate_hz.0,
            "core:version": "1.0.0",
        },
        "captures": [{
            "core:sample_start": 0,
            "core:frequency": meta.center_hz.0,
            "core:datetime": iso8601(meta.start),
        }],
        "annotations": meta.annotations,
    });
    let path = dir.join(format!("{stem}.sigmf-meta"));
    std::fs::write(&path, serde_json::to_vec_pretty(&json)?)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_formats_utc_nanos() {
        assert_eq!(iso8601(Timestamp(0)), "1970-01-01T00:00:00.000000000Z");
        assert_eq!(
            iso8601(Timestamp(951_868_801_500_000_000)),
            "2000-03-01T00:00:01.500000000Z"
        );
        assert_eq!(
            iso8601(Timestamp(1_760_000_000_000_000_001)),
            "2025-10-09T08:53:20.000000001Z"
        );
    }
}
