//! SigMF recording playback as a [`Port`].

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use num_complex::Complex;
use tokio::sync::broadcast;
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::types::{Capabilities, Claim, Hz, SampleFormat, SourceId, Timestamp};

/// The metadata fields this source needs from a `.sigmf-meta` file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SigmfMeta {
    pub datatype: String,
    pub sample_rate_hz: Hz,
    pub center_hz: Hz,
    pub start: Timestamp,
}

#[derive(Clone, Copy, Debug)]
pub struct FileOpts {
    pub realtime: bool,
    pub block_samples: usize,
    pub repeat: bool,
}

fn io(msg: impl Into<String>) -> PortError {
    PortError::Io(msg.into())
}

fn number(v: Option<&serde_json::Value>, key: &str) -> Result<u64, PortError> {
    let f = v.and_then(|v| v.as_f64()).ok_or_else(|| io(format!("{key} missing")))?;
    if f < 0.0 || !f.is_finite() {
        return Err(io(format!("{key} invalid")));
    }
    Ok(f as u64)
}

/// Parse `YYYY-MM-DDTHH:MM:SS(.fraction)?Z` into nanoseconds since the epoch.
fn parse_datetime(s: &str) -> Result<Timestamp, PortError> {
    let bad = || io(format!("core:datetime unsupported form: {s}"));
    let body = s.strip_suffix('Z').ok_or_else(bad)?;
    let (main, frac) = match body.split_once('.') {
        Some((m, f)) => (m, Some(f)),
        None => (body, None),
    };
    let b = main.as_bytes();
    if b.len() != 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return Err(bad());
    }
    let n = |r: std::ops::Range<usize>| main[r].parse::<i64>().map_err(|_| bad());
    let (y, mo, d, h, mi, sec) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return Err(bad());
    }
    // Days from civil (Howard Hinnant).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + sec;
    let mut nanos = 0u64;
    if let Some(f) = frac {
        if f.is_empty() || !f.bytes().all(|c| c.is_ascii_digit()) {
            return Err(bad());
        }
        let digits: String = f.chars().take(9).collect();
        nanos = format!("{digits:0<9}").parse().map_err(|_| bad())?;
    }
    u64::try_from(secs).map_err(|_| bad()).map(|s| Timestamp(s * 1_000_000_000 + nanos))
}

/// Read the fields this source needs from a `.sigmf-meta` file.
pub fn read_meta(path: &Path) -> Result<SigmfMeta, PortError> {
    let text = std::fs::read_to_string(path).map_err(|e| io(format!("{}: {e}", path.display())))?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| io(format!("meta json: {e}")))?;
    let global = &v["global"];
    let datatype = global["core:datatype"]
        .as_str()
        .ok_or_else(|| io("core:datatype missing"))?
        .to_string();
    let sample_rate_hz = Hz(number(global.get("core:sample_rate"), "core:sample_rate")?);
    let cap = v["captures"].get(0);
    let center_hz = Hz(number(cap.and_then(|c| c.get("core:frequency")), "core:frequency")?);
    let start = match cap.and_then(|c| c.get("core:datetime")).and_then(|d| d.as_str()) {
        Some(s) => parse_datetime(s)?,
        None => Timestamp(0),
    };
    Ok(SigmfMeta { datatype, sample_rate_hz, center_hz, start })
}

fn format_of(datatype: &str) -> Result<SampleFormat, PortError> {
    match datatype {
        "cf32_le" => Ok(SampleFormat::Cf32),
        "ci16_le" => Ok(SampleFormat::Ci16),
        "ci8" => Ok(SampleFormat::Ci8),
        other => Err(PortError::Unsupported(other.to_string())),
    }
}

fn convert(fmt: SampleFormat, raw: &[u8]) -> Vec<Complex<f32>> {
    match fmt {
        SampleFormat::Cf32 => raw
            .chunks_exact(8)
            .map(|c| {
                Complex::new(
                    f32::from_le_bytes(c[0..4].try_into().unwrap()),
                    f32::from_le_bytes(c[4..8].try_into().unwrap()),
                )
            })
            .collect(),
        SampleFormat::Ci16 => raw
            .chunks_exact(4)
            .map(|c| {
                Complex::new(
                    i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0,
                    i16::from_le_bytes([c[2], c[3]]) as f32 / 32768.0,
                )
            })
            .collect(),
        SampleFormat::Ci8 => raw
            .chunks_exact(2)
            .map(|c| Complex::new(c[0] as i8 as f32 / 128.0, c[1] as i8 as f32 / 128.0))
            .collect(),
    }
}

/// Plays a SigMF recording as if it were a live source.
pub struct FileSource {
    id: SourceId,
    meta: SigmfMeta,
    caps: Capabilities,
    opts: FileOpts,
    samples: Arc<[Complex<f32>]>,
    tx: broadcast::Sender<IqBlock>,
    stopped: Arc<AtomicBool>,
}

impl Drop for FileSource {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
    }
}

impl FileSource {
    pub fn open(id: SourceId, meta_path: &Path, opts: FileOpts) -> Result<Self, PortError> {
        let meta = read_meta(meta_path)?;
        let fmt = format_of(&meta.datatype)?;
        if opts.block_samples == 0 {
            return Err(io("block_samples must be > 0"));
        }
        if meta.sample_rate_hz.0 == 0 {
            return Err(io("core:sample_rate invalid"));
        }
        let data_path: PathBuf = meta_path.with_extension("sigmf-data");
        let raw = std::fs::read(&data_path).map_err(|e| io(format!("{}: {e}", data_path.display())))?;
        let unit = match fmt {
            SampleFormat::Cf32 => 8,
            SampleFormat::Ci16 => 4,
            SampleFormat::Ci8 => 2,
        };
        if raw.len() % unit != 0 {
            return Err(io(format!("{}: length not a multiple of {unit} bytes", data_path.display())));
        }
        let samples: Arc<[Complex<f32>]> = convert(fmt, &raw).into();
        let caps = Capabilities {
            tune_min_hz: Claim::Asserted(meta.center_hz),
            tune_max_hz: Claim::Asserted(meta.center_hz),
            max_bandwidth_hz: Claim::Asserted(meta.sample_rate_hz),
            formats: vec![fmt],
            channels: 1,
            rx: true,
            tx: false,
            remote: false,
            push: true,
        };
        let (tx, _) = broadcast::channel(64);
        Ok(Self { id, meta, caps, opts, samples, tx, stopped: Arc::new(AtomicBool::new(false)) })
    }

    /// Begin broadcasting blocks to subscribers. Call `subscribe()` first: the task
    /// exits as soon as there are no receivers, or when this source is dropped.
    pub fn start(&self) -> tokio::task::JoinHandle<()> {
        let tx = self.tx.clone();
        let samples = self.samples.clone();
        let (meta, opts) = (self.meta.clone(), self.opts);
        let stopped = self.stopped.clone();
        tokio::spawn(async move {
            let rate = meta.sample_rate_hz.0;
            let mut elapsed: u64 = 0;
            if samples.is_empty() {
                return;
            }
            loop {
                for chunk in samples.chunks(opts.block_samples) {
                    if stopped.load(Ordering::Relaxed) || tx.receiver_count() == 0 {
                        return;
                    }
                    let t0 = Timestamp(meta.start.0 + (elapsed as u128 * 1_000_000_000 / rate as u128) as u64);
                    elapsed += chunk.len() as u64;
                    let _ = tx.send(IqBlock {
                        t0,
                        sample_rate_hz: meta.sample_rate_hz,
                        center_hz: meta.center_hz,
                        samples: chunk.into(),
                    });
                    if opts.realtime {
                        tokio::time::sleep(Duration::from_secs_f64(chunk.len() as f64 / rate as f64)).await;
                    } else {
                        tokio::task::yield_now().await;
                    }
                }
                if !opts.repeat {
                    break;
                }
            }
        })
    }
}

#[async_trait]
impl Port for FileSource {
    fn id(&self) -> &SourceId {
        &self.id
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        let c = self.meta.center_hz.0;
        if t.center_hz.0 != c {
            return Err(PortError::OutOfRange { field: "center_hz", value: t.center_hz.0, min: c, max: c, suggested: None });
        }
        let bw = self.meta.sample_rate_hz.0;
        if t.bandwidth_hz.0 > bw {
            return Err(PortError::OutOfRange { field: "bandwidth_hz", value: t.bandwidth_hz.0, min: 0, max: bw, suggested: None });
        }
        Ok(t)
    }

    fn current(&self) -> Option<Tuning> {
        Some(Tuning { center_hz: self.meta.center_hz, bandwidth_hz: self.meta.sample_rate_hz })
    }

    fn subscribe(&self) -> IqReceiver {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datetime_parses_to_epoch_nanos() {
        assert_eq!(parse_datetime("1970-01-01T00:00:00Z").unwrap(), Timestamp(0));
        assert_eq!(parse_datetime("2000-03-01T00:00:01.5Z").unwrap(), Timestamp(951_868_801_500_000_000));
        assert!(parse_datetime("2000-03-01T00:00:01+01:00").is_err());
    }
}
