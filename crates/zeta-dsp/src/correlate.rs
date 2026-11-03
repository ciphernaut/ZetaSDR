//! Sync-word correlation over a bit stream.
//!
//! Reads `slicer`'s `bits` events and publishes one `sync` event wherever `pattern`
//! appears with at most `max_errors` bit errors, the hit's offset counted in bits from
//! the start of its segment (the slicer's `offset_bits` origin).

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Provenance, Span, StreamId, StreamKind, Timestamp};

use crate::slicer::bits_from_hex;
use crate::{Input, run_map};

/// The stage kind, as graphs name it.
pub const CORRELATE_KIND: &str = "correlate";

/// Longest pattern, in bits.
pub const PATTERN_BITS_MAX: u32 = 256;

/// Bits per hex digit.
const HEX_DIGIT_BITS: u64 = 4;

const NS_PER_S: f64 = 1e9;

/// `correlate` parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct CorrelateParams {
    /// The sync word as hex, MSB first.
    pub pattern: String,
    /// How many of the pattern's leading bits to match, 1..=256.
    pub bits: u32,
    /// Most bit errors a hit may have, under `bits`.
    #[serde(default)]
    pub max_errors: u32,
}

impl CorrelateParams {
    fn check(&self) -> Result<(), String> {
        if self.pattern.is_empty() || !self.pattern.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("pattern: hex".into());
        }
        let have = self.pattern.len() as u64 * HEX_DIGIT_BITS;
        if !(1..=PATTERN_BITS_MAX).contains(&self.bits) || u64::from(self.bits) > have {
            return Err(format!(
                "bits: 1..={PATTERN_BITS_MAX} and at most the pattern's bits"
            ));
        }
        if self.max_errors >= self.bits {
            return Err("max_errors: under bits".into());
        }
        Ok(())
    }
}

/// One `bits` event as the bit stages read it.
pub(crate) struct BitsIn {
    pub bits: Vec<u8>,
    pub offset_bits: u64,
    pub t_ns: u64,
    /// One bit's duration: the frame's span over its bits, else one symbol at
    /// `rate_baud`.
    pub bit_ns: f64,
    pub prov: Provenance,
}

impl BitsIn {
    /// The `bits` event of `frame`; `None` for any other event; an error for a frame
    /// that is not an event.
    pub fn parse(kind: &str, frame: Frame) -> Result<Option<BitsIn>, StageError> {
        let Payload::Event(ev) = &frame.payload else {
            return Err(StageError::Failed(format!(
                "{kind} stage expects event frames"
            )));
        };
        if ev["protocol"] != "bits" {
            return Ok(None);
        }
        let (Some(hex), Some(count), Some(offset_bits), Some(t_ns)) = (
            ev["bits"].as_str(),
            ev["count"].as_u64(),
            ev["offset_bits"].as_u64(),
            ev["t_ns"].as_u64(),
        ) else {
            return Ok(None);
        };
        let bits = bits_from_hex(hex, count as usize);
        let span = &frame.provenance.span;
        let dur = span.end_ns.0.saturating_sub(span.start_ns.0);
        let bit_ns = if dur > 0 && !bits.is_empty() {
            dur as f64 / bits.len() as f64
        } else {
            ev["rate_baud"]
                .as_f64()
                .filter(|&r| r > 0.0)
                .map_or(0.0, |r| NS_PER_S / r)
        };
        Ok(Some(BitsIn {
            bits,
            offset_bits,
            t_ns,
            bit_ns,
            prov: frame.provenance,
        }))
    }

    /// The offset one past this event's last bit.
    pub fn end(&self) -> u64 {
        self.offset_bits + self.bits.len() as u64
    }

    /// The time of this event's bit `i`.
    pub fn time(&self, i: usize) -> u64 {
        self.t_ns + (i as f64 * self.bit_ns) as u64
    }
}

/// The correlator's state over one segment: the last `bits - 1` bits seen and their
/// times.
#[derive(Default)]
struct Carry {
    bits: Vec<u8>,
    times: Vec<u64>,
    /// The offset one past the last bit seen; `None` before any.
    end: Option<u64>,
}

/// Publishes a `sync` event (`offset_bits`, `errors`, `t_ns` of the hit's first bit,
/// `summary`) for every position of a `bits` segment where the first `bits` bits of
/// `pattern` match with at most `max_errors` errors, the last `bits - 1` bits carried
/// across events so a pattern split between two is found. A `bits` event whose
/// `offset_bits` is not the previous one's end (a seam restarts it at 0), and a lag,
/// start a new segment. Nothing else passes through.
pub struct CorrelateStage {
    spec: StageSpec,
}

impl CorrelateStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: CORRELATE_KIND,
                inputs: vec![StreamKind::Event],
                outputs: vec![StreamKind::Event],
                params_schema: schemars::schema_for!(CorrelateParams),
            },
        }
    }
}

impl Default for CorrelateStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for CorrelateStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    fn check_params(&self, params: &serde_json::Value) -> Result<(), String> {
        let p: CorrelateParams =
            serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
        p.check()
    }

    async fn run(
        &self,
        params: serde_json::Value,
        inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        self.check_params(&params).map_err(StageError::Failed)?;
        let p: CorrelateParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("{CORRELATE_KIND} params: {e}")))?;
        let [events] = <[StreamId; 1]>::try_from(outputs)
            .map_err(|o| StageError::Failed(format!("expected 1 output, got {}", o.len())))?;
        let pattern = bits_from_hex(&p.pattern, p.bits as usize);
        let label = p.pattern.to_ascii_lowercase();
        let n = pattern.len();
        let mut carry = Carry::default();
        run_map(CORRELATE_KIND, inputs, bus, cancel, move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    carry = Carry::default();
                    return Ok(Vec::new());
                }
            };
            let Some(b) = BitsIn::parse(CORRELATE_KIND, frame)? else {
                return Ok(Vec::new());
            };
            if carry.end != Some(b.offset_bits) {
                carry = Carry::default();
            }
            let base = b.offset_bits - carry.bits.len() as u64;
            carry.bits.extend_from_slice(&b.bits);
            carry.times.extend((0..b.bits.len()).map(|i| b.time(i)));
            carry.end = Some(b.end());
            let mut out = Vec::new();
            for start in 0..(carry.bits.len() + 1).saturating_sub(n) {
                let mut errors = 0u32;
                for (x, y) in carry.bits[start..start + n].iter().zip(&pattern) {
                    errors += u32::from(x != y);
                    if errors > p.max_errors {
                        break;
                    }
                }
                if errors > p.max_errors {
                    continue;
                }
                let offset = base + start as u64;
                let t = carry.times[start];
                let ev = serde_json::json!({
                    "protocol": "sync",
                    "offset_bits": offset,
                    "errors": errors,
                    "t_ns": t,
                    "summary": format!("sync 0x{label} at {offset} ({errors} errors)"),
                });
                let mut prov = b.prov.clone();
                prov.span = Span {
                    start_ns: Timestamp(t),
                    end_ns: Timestamp(t + (n as f64 * b.bit_ns) as u64),
                };
                out.push(Frame::new(events.clone(), prov, Payload::Event(ev)));
            }
            let drop = carry.bits.len() - carry.bits.len().min(n - 1);
            carry.bits.drain(..drop);
            carry.times.drain(..drop);
            Ok(out)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use zeta_core::stage::Stage;

    use super::CorrelateStage;
    use crate::bit_view::test_streams::{bits_frames, framed_stream, run_events};

    const PERIOD: usize = 1120;
    const SYNC_BITS: usize = 16;

    #[tokio::test]
    async fn sync_hits_land_at_the_planted_offsets() {
        let bits = framed_stream(8, 136, 7);
        let split = 4 * PERIOD + SYNC_BITS / 2;
        let (events, _) = run_events(
            &CorrelateStage::new(),
            json!({"pattern": "2dd4", "bits": 16}),
            bits_frames(&bits, &[split]),
        )
        .await;
        let offsets: Vec<u64> = events
            .iter()
            .map(|e| {
                assert_eq!(e["protocol"], "sync");
                assert_eq!(e["errors"], 0);
                e["offset_bits"].as_u64().unwrap()
            })
            .collect();
        let want: Vec<u64> = (0..8).map(|k| (k * PERIOD) as u64).collect();
        assert_eq!(offsets, want);
        assert_eq!(events[1]["summary"], "sync 0x2dd4 at 1120 (0 errors)");
    }

    #[tokio::test]
    async fn max_errors_admits_a_damaged_sync() {
        let mut bits = framed_stream(3, 136, 3);
        bits[PERIOD + 3] ^= 1;
        let at = |events: &[serde_json::Value]| {
            events
                .iter()
                .find(|e| e["offset_bits"] == PERIOD as u64)
                .cloned()
        };
        let (strict, _) = run_events(
            &CorrelateStage::new(),
            json!({"pattern": "2dd4", "bits": 16}),
            bits_frames(&bits, &[]),
        )
        .await;
        assert!(at(&strict).is_none(), "{strict:?}");
        let (loose, _) = run_events(
            &CorrelateStage::new(),
            json!({"pattern": "2dd4", "bits": 16, "max_errors": 1}),
            bits_frames(&bits, &[]),
        )
        .await;
        assert_eq!(at(&loose).expect("damaged sync found")["errors"], 1);
    }

    #[test]
    fn bad_params_are_refused_naming_the_field() {
        let s = CorrelateStage::new();
        assert_eq!(
            s.check_params(&json!({"pattern": "zz", "bits": 8})),
            Err("pattern: hex".into())
        );
        let e = s
            .check_params(&json!({"pattern": "2dd4", "bits": 300}))
            .unwrap_err();
        assert!(e.starts_with("bits: 1..=256"), "{e}");
        let e = s
            .check_params(&json!({"pattern": "2dd4", "bits": 20}))
            .unwrap_err();
        assert!(e.starts_with("bits: 1..=256"), "{e}");
        assert_eq!(
            s.check_params(&json!({"pattern": "2dd4", "bits": 16, "max_errors": 16})),
            Err("max_errors: under bits".into())
        );
        assert!(
            s.check_params(&json!({"pattern": "2dd4", "bits": 16, "extra": 1}))
                .is_err()
        );
        assert!(
            s.check_params(&json!({"pattern": "2dd4", "bits": 16}))
                .is_ok()
        );
    }
}
