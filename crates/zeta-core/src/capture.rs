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

/// Parse `2000-03-01T00:00:01.500000000Z` back to nanoseconds; `None` for any other shape.
pub(crate) fn parse_iso8601(s: &str) -> Option<Timestamp> {
    let (date, time) = s.strip_suffix('Z')?.split_once('T')?;
    let mut d = date.splitn(3, '-');
    let (y, m, day): (i64, i64, i64) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let (hms, frac) = time.split_once('.').unwrap_or((time, ""));
    let mut t = hms.splitn(3, ':');
    let (h, mi, sec): (i64, i64, i64) = (
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
    );
    let mut nanos: u64 = 0;
    for (i, c) in frac.chars().take(9).enumerate() {
        nanos += u64::from(c.to_digit(10)?) * 10u64.pow(8 - i as u32);
    }
    // Days from civil (Howard Hinnant).
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = u64::try_from(days * 86_400 + h * 3600 + mi * 60 + sec).ok()?;
    Some(Timestamp(secs * 1_000_000_000 + nanos))
}

/// Read a `cf32_le` SigMF pair written by [`write_cf32`] (or any like it): the sample
/// rate, centre frequency and start from the meta, the annotations verbatim, and the
/// samples from the sibling `.sigmf-data`.
pub fn read_cf32(meta_path: &Path) -> std::io::Result<(CaptureMeta, Vec<Complex<f32>>)> {
    let bad = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, m.to_string());
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(meta_path)?)?;
    if json["global"]["core:datatype"].as_str() != Some("cf32_le") {
        return Err(bad("core:datatype is not cf32_le"));
    }
    let rate = json["global"]["core:sample_rate"]
        .as_f64()
        .filter(|r| *r >= 1.0)
        .ok_or_else(|| bad("core:sample_rate missing"))?;
    let cap = &json["captures"][0];
    let center = cap["core:frequency"]
        .as_f64()
        .filter(|f| *f >= 0.0)
        .ok_or_else(|| bad("core:frequency missing"))?;
    let start = cap["core:datetime"]
        .as_str()
        .and_then(parse_iso8601)
        .unwrap_or(Timestamp(0));
    let bytes = std::fs::read(meta_path.with_extension("sigmf-data"))?;
    let samples = bytes
        .chunks_exact(8)
        .map(|c| {
            Complex::new(
                f32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                f32::from_le_bytes([c[4], c[5], c[6], c[7]]),
            )
        })
        .collect();
    let meta = CaptureMeta {
        sample_rate_hz: Hz(rate as u64),
        center_hz: Hz(center as u64),
        start,
        annotations: json["annotations"].as_array().cloned().unwrap_or_default(),
    };
    Ok((meta, samples))
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

    #[test]
    fn read_cf32_round_trips_write_cf32() {
        let dir = tempfile::tempdir().unwrap();
        let meta = CaptureMeta {
            sample_rate_hz: Hz(48_000),
            center_hz: Hz(433_920_000),
            start: Timestamp(1_760_000_000_000_000_001),
            annotations: vec![serde_json::json!({"core:sample_start": 0})],
        };
        let iq = vec![Complex::new(0.5, -0.25), Complex::new(1.0, 2.0)];
        let path = write_cf32(dir.path(), "x", &meta, &iq).unwrap();
        let (m, s) = read_cf32(&path).unwrap();
        assert_eq!(m, meta);
        assert_eq!(s, iq);
    }
}
