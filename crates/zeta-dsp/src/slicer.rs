//! Symbol slicing: bits out of one sample per symbol.
//!
//! Reads `clock_recover`'s output (one sample per symbol, `sample_rate_hz` the symbol
//! rate): 1 channel is a real decision value sliced against running thresholds, 2
//! channels interleaved I/Q are quantised by angle. Each symbol becomes `log2(M)` bits,
//! MSB first, published in `bits` events of `chunk_bits`.

use std::collections::VecDeque;
use std::f64::consts::TAU;
use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Provenance, Span, StreamId, StreamKind, Timestamp};

use crate::{Input, is_seam, run_map_ending};

/// The stage kind, as graphs name it.
pub const SLICER_KIND: &str = "slicer";

/// Default bits per `bits` event.
pub const CHUNK_BITS_DEFAULT: u32 = 4096;

/// Most bits per `bits` event: 32 KiB of hex, so the event stays under 64 KiB.
pub const CHUNK_BITS_MAX: u32 = 131_072;

/// Most points a `constellation` event carries (the newest of its window).
pub const CONSTELLATION_POINTS_MAX: usize = 512;

/// Input time between two `constellation` events, in milliseconds.
pub const CONSTELLATION_EVERY_MS: u64 = 1000;

/// Time constant of the running statistics (cluster levels, peak, ring radius), in
/// symbols.
const AVERAGE_SYMBOLS: f64 = 64.0;

/// Default number of levels on a 1-channel input.
const LEVELS_DEFAULT: u8 = 2;

/// The outer thresholds of 4 levels as a fraction of the running peak: midway between
/// the cluster centres `±1/3` and `±1` of it.
const OUTER_THRESHOLD: f64 = 2.0 / 3.0;

const NS_PER_S: u128 = 1_000_000_000;
const MS_PER_S: u64 = 1000;

fn default_levels() -> u8 {
    LEVELS_DEFAULT
}

fn default_chunk_bits() -> u32 {
    CHUNK_BITS_DEFAULT
}

/// The points of a 2-channel input: the first at angle 0, counter-clockwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Constellation {
    /// 2 points.
    Bpsk,
    /// 4 points.
    Qpsk,
    /// 8 points.
    #[serde(rename = "8psk")]
    Psk8,
}

impl Constellation {
    fn points(self) -> u32 {
        match self {
            Constellation::Bpsk => 2,
            Constellation::Qpsk => 4,
            Constellation::Psk8 => 8,
        }
    }
}

/// How a symbol index becomes its bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mapping {
    /// The index itself.
    #[default]
    Natural,
    /// The index through Gray code (`i ^ (i >> 1)`).
    Gray,
}

/// `slicer` parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct SlicerParams {
    /// Levels of a 1-channel input, 2 or 4; ignored on 2 channels.
    #[serde(default = "default_levels")]
    pub levels: u8,
    /// The points of a 2-channel input; required there, ignored on 1 channel.
    #[serde(default)]
    pub constellation: Option<Constellation>,
    /// Natural or Gray mapping of the symbol index.
    #[serde(default)]
    pub mapping: Mapping,
    /// Emit `(sym - prev) mod M`, the first symbol of a stream against 0.
    #[serde(default)]
    pub differential: bool,
    /// Bits per `bits` event, 1..=131072.
    #[serde(default = "default_chunk_bits")]
    pub chunk_bits: u32,
}

/// A `bits` event: `bits` packed MSB first as lowercase hex, the last byte zero-padded,
/// `count` the bits it holds, `offset_bits` the first bit's index since the last seam
/// and `t_ns` the time of the first bit's symbol.
pub fn bits_event(rate_baud: u32, bits: &[u8], offset_bits: u64, t_ns: u64) -> serde_json::Value {
    let mut hex = String::with_capacity(bits.len().div_ceil(8) * 2);
    for byte in bits.chunks(8) {
        let b = byte
            .iter()
            .enumerate()
            .fold(0u8, |acc, (i, &v)| acc | ((v & 1) << (7 - i)));
        let _ = write!(hex, "{b:02x}");
    }
    serde_json::json!({
        "protocol": "bits",
        "rate_baud": rate_baud,
        "bits": hex,
        "count": bits.len(),
        "offset_bits": offset_bits,
        "t_ns": t_ns,
        "summary": format!("{} bits at {rate_baud} Bd", bits.len()),
    })
}

/// The first `count` bits of `hex` (MSB first per digit, upper or lower case), fewer
/// when the hex runs out or reaches a character that is not a hex digit.
pub fn bits_from_hex(hex: &str, count: usize) -> Vec<u8> {
    hex.chars()
        .map_while(|c| c.to_digit(16))
        .flat_map(|d| (0..4).rev().map(move |b| ((d >> b) & 1) as u8))
        .take(count)
        .collect()
}

/// What one note window (between two `constellation` events) has seen.
struct Window {
    symbols: u64,
    first_ns: Option<u64>,
    last_ns: u64,
    points: VecDeque<[f32; 2]>,
    /// Per threshold `k` (between decisions `k` and `k + 1`): the largest value decided
    /// `k` and the smallest decided `k + 1`.
    below_max: Vec<f64>,
    above_min: Vec<f64>,
    min: f64,
    max: f64,
    err2: f64,
}

impl Window {
    fn new(thresholds: usize) -> Window {
        Window {
            symbols: 0,
            first_ns: None,
            last_ns: 0,
            points: VecDeque::with_capacity(CONSTELLATION_POINTS_MAX),
            below_max: vec![f64::NEG_INFINITY; thresholds],
            above_min: vec![f64::INFINITY; thresholds],
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            err2: 0.0,
        }
    }

    /// The eye opening of a 1-channel window: the smallest, over the thresholds with
    /// symbols on both sides, of the gap between the inner edges of the two clusters
    /// beside it (the smallest value decided above less the largest decided below),
    /// over the peak-to-peak divided by `levels - 1` (one cluster spacing), clamped to
    /// 0..1. A clean ±1 stream reads 1; noise that closes the eye reads 0. `None` with
    /// no threshold seen from both sides.
    fn eye_open(&self, levels: u32) -> Option<f64> {
        let spacing = (self.max - self.min) / (levels - 1) as f64;
        let gap = self
            .below_max
            .iter()
            .zip(&self.above_min)
            .filter(|(b, a)| b.is_finite() && a.is_finite())
            .map(|(b, a)| a - b)
            .reduce(f64::min)?;
        Some(if spacing > 0.0 {
            (gap / spacing).clamp(0.0, 1.0)
        } else {
            0.0
        })
    }

    /// The error vector magnitude of a 2-channel window: the RMS distance of each
    /// symbol to its nearest point, over the ring radius at the time.
    fn evm(&self) -> Option<f64> {
        (self.symbols > 0).then(|| (self.err2 / self.symbols as f64).sqrt())
    }
}

/// The slicer over one run of contiguous input.
struct Slicer {
    rate_hz: u64,
    channels: usize,
    /// Levels (1 channel) or points (2 channels).
    m: u32,
    bits_per_symbol: u32,
    /// Running lower and upper cluster levels (2 levels).
    lo: f64,
    hi: f64,
    /// Running outer cluster magnitude (4 levels).
    peak: f64,
    /// Running ring radius (2 channels).
    radius: f64,
    prev_sym: u32,
    offset_bits: u64,
    pending: Vec<u8>,
    pending_t0: u64,
    pending_end: u64,
    window: Window,
    window_symbols: u64,
    prov: Provenance,
}

impl Slicer {
    fn new(
        p: &SlicerParams,
        rate_hz: u64,
        channels: usize,
        first: &[f32],
        prov: Provenance,
    ) -> Result<Slicer, StageError> {
        let m = if channels == 2 {
            p.constellation
                .ok_or_else(|| {
                    StageError::Failed(format!(
                        "{SLICER_KIND}: 2-channel input needs a constellation"
                    ))
                })?
                .points()
        } else {
            p.levels as u32
        };
        let values = first.iter().map(|&v| v as f64);
        let lo = values.clone().fold(f64::INFINITY, f64::min);
        let hi = values.clone().fold(f64::NEG_INFINITY, f64::max);
        let peak = values.map(f64::abs).fold(0.0, f64::max);
        let n = (first.len() / channels).max(1) as f64;
        let radius = first
            .chunks(channels)
            .map(|s| s.iter().map(|&v| (v as f64).powi(2)).sum::<f64>().sqrt())
            .sum::<f64>()
            / n;
        let thresholds = if channels == 1 { m as usize - 1 } else { 0 };
        Ok(Slicer {
            rate_hz,
            channels,
            m,
            bits_per_symbol: m.trailing_zeros(),
            lo,
            hi,
            peak,
            radius,
            prev_sym: 0,
            offset_bits: 0,
            pending: Vec::new(),
            pending_t0: 0,
            pending_end: 0,
            window: Window::new(thresholds),
            window_symbols: (rate_hz * CONSTELLATION_EVERY_MS / MS_PER_S).max(1),
            prov,
        })
    }

    /// The symbol index of a real decision value, low to high, the statistics updated:
    /// the cluster the value falls in moves toward it by `1/AVERAGE_SYMBOLS`.
    fn slice_real(&mut self, x: f64) -> u32 {
        let a = 1.0 / AVERAGE_SYMBOLS;
        if self.m == 2 {
            if x >= (self.lo + self.hi) / 2.0 {
                self.hi += (x - self.hi) * a;
                1
            } else {
                self.lo += (x - self.lo) * a;
                0
            }
        } else {
            let t = self.peak * OUTER_THRESHOLD;
            let s = if x < -t {
                0
            } else if x < 0.0 {
                1
            } else if x < t {
                2
            } else {
                3
            };
            if s == 0 || s == self.m - 1 {
                self.peak += (x.abs() - self.peak) * a;
            }
            s
        }
    }

    /// The point index of an I/Q sample by angle, the ring radius and the window's
    /// error updated.
    fn slice_iq(&mut self, i: f64, q: f64) -> u32 {
        let step = TAU / self.m as f64;
        let k = ((q.atan2(i) / step).round() as i64).rem_euclid(self.m as i64) as u32;
        let r = self.radius.max(f64::MIN_POSITIVE);
        let (ps, pc) = (k as f64 * step).sin_cos();
        self.window.err2 += ((i - r * pc).powi(2) + (q - r * ps).powi(2)) / (r * r);
        self.radius += ((i * i + q * q).sqrt() - self.radius) / AVERAGE_SYMBOLS;
        k
    }

    fn time_ns(&self, start_ns: u64, k: usize) -> u64 {
        start_ns + (k as u128 * NS_PER_S / self.rate_hz as u128) as u64
    }

    /// Slice one frame's symbols; the events that came due.
    fn step(
        &mut self,
        p: &SlicerParams,
        start_ns: u64,
        samples: &[f32],
        events: &StreamId,
        totals: &mut Totals,
        ctx: &StageContext,
    ) -> Vec<Frame> {
        let mut out = Vec::new();
        for (k, s) in samples.chunks(self.channels).enumerate() {
            let t = self.time_ns(start_ns, k);
            let (x, y) = (s[0] as f64, s.get(1).copied().unwrap_or(0.0) as f64);
            let raw = if self.channels == 1 {
                let r = self.slice_real(x);
                let w = &mut self.window;
                if (r as usize) < w.below_max.len() {
                    w.below_max[r as usize] = w.below_max[r as usize].max(x);
                }
                if r > 0 {
                    w.above_min[r as usize - 1] = w.above_min[r as usize - 1].min(x);
                }
                w.min = w.min.min(x);
                w.max = w.max.max(x);
                r
            } else {
                self.slice_iq(x, y)
            };
            let mut sym = raw;
            if p.differential {
                sym = (raw + self.m - self.prev_sym) % self.m;
                self.prev_sym = raw;
            }
            if p.mapping == Mapping::Gray {
                sym ^= sym >> 1;
            }
            let end = self.time_ns(start_ns, k + 1);
            for b in (0..self.bits_per_symbol).rev() {
                if self.pending.is_empty() {
                    self.pending_t0 = t;
                }
                self.pending.push(((sym >> b) & 1) as u8);
                self.pending_end = end;
                if self.pending.len() >= p.chunk_bits as usize {
                    out.push(self.bits_frame(events));
                }
            }
            totals.symbols += 1;
            let w = &mut self.window;
            w.symbols += 1;
            w.first_ns.get_or_insert(t);
            w.last_ns = end;
            if w.points.len() == CONSTELLATION_POINTS_MAX {
                w.points.pop_front();
            }
            w.points.push_back([x as f32, y as f32]);
            if w.symbols >= self.window_symbols {
                out.push(self.constellation_frame(events, totals, ctx));
            }
        }
        out
    }

    /// The pending bits as a `bits` event frame, the chunk emptied.
    fn bits_frame(&mut self, events: &StreamId) -> Frame {
        let bits = std::mem::take(&mut self.pending);
        let ev = bits_event(
            self.rate_hz as u32,
            &bits,
            self.offset_bits,
            self.pending_t0,
        );
        self.offset_bits += bits.len() as u64;
        let mut prov = self.prov.clone();
        prov.span = Span {
            start_ns: Timestamp(self.pending_t0),
            end_ns: Timestamp(self.pending_end),
        };
        Frame::new(events.clone(), prov, Payload::Event(ev))
    }

    /// The window closed: its figure of merit into `totals` and the note; the window
    /// reopened and the closed one returned.
    fn close_window(
        &mut self,
        events: &StreamId,
        totals: &mut Totals,
        ctx: &StageContext,
    ) -> Window {
        let thresholds = self.window.below_max.len();
        let w = std::mem::replace(&mut self.window, Window::new(thresholds));
        if self.channels == 1 {
            totals.evm = None;
            if let Some(e) = w.eye_open(self.m) {
                totals.eye_open = Some(e);
            }
        } else {
            totals.eye_open = None;
            if let Some(e) = w.evm() {
                totals.evm = Some(e);
            }
        }
        totals.note(events, ctx);
        w
    }

    /// The window's `constellation` event frame, noted to the job, the window reopened.
    fn constellation_frame(
        &mut self,
        events: &StreamId,
        totals: &mut Totals,
        ctx: &StageContext,
    ) -> Frame {
        let w = self.close_window(events, totals, ctx);
        let start = w.first_ns.unwrap_or(w.last_ns);
        let merit = match (totals.eye_open, totals.evm) {
            (Some(e), _) => format!("eye {e:.2}"),
            (_, Some(e)) => format!("EVM {e:.3}"),
            _ => "no eye".to_string(),
        };
        let points: Vec<[f32; 2]> = w.points.into_iter().collect();
        let ev = serde_json::json!({
            "protocol": "constellation",
            "channels": self.channels,
            "points": points,
            "t_ns": start,
            "summary": format!("{} points, {merit}", points.len()),
        });
        let mut prov = self.prov.clone();
        prov.span = Span {
            start_ns: Timestamp(start),
            end_ns: Timestamp(w.last_ns),
        };
        Frame::new(events.clone(), prov, Payload::Event(ev))
    }

    /// The pending chunk as a short `bits` event, and the partial window noted.
    fn flush(
        &mut self,
        events: &StreamId,
        totals: &mut Totals,
        ctx: &StageContext,
    ) -> Option<Frame> {
        if self.window.symbols > 0 {
            self.close_window(events, totals, ctx);
        }
        (!self.pending.is_empty()).then(|| self.bits_frame(events))
    }
}

/// What the note carries across runs.
#[derive(Default)]
struct Totals {
    symbols: u64,
    eye_open: Option<f64>,
    evm: Option<f64>,
}

impl Totals {
    fn note(&self, events: &StreamId, ctx: &StageContext) {
        ctx.note(serde_json::json!({"slicer": {
            "symbols": self.symbols,
            "eye_open": self.eye_open,
            "evm": self.evm,
            "stream": events.0,
        }}));
    }
}

/// Slices one sample per symbol into bits: 1 channel against running thresholds
/// (`levels` 2: the midpoint of the running lower and upper cluster levels; `levels` 4:
/// 0 and ±2/3 of the running outer cluster magnitude, the midpoints between cluster
/// centres ±1 and ±1/3 of it; each an exponential average over [`AVERAGE_SYMBOLS`]
/// symbols of the values falling in that cluster, started from the run's first frame's
/// minimum, maximum and largest magnitude), 2 channels by angle onto `constellation`.
/// Publishes a `bits` event per `chunk_bits` (and the short chunk at a seam, a lag, a
/// change of rate or channels and the input's end, each of which restarts the
/// statistics, the differential reference and `offset_bits`), a `constellation` event
/// every [`CONSTELLATION_EVERY_MS`] of input, and notes `slicer` to the job.
pub struct SlicerStage {
    spec: StageSpec,
}

impl SlicerStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: SLICER_KIND,
                inputs: vec![StreamKind::Audio],
                outputs: vec![StreamKind::Event],
                params_schema: schemars::schema_for!(SlicerParams),
            },
        }
    }
}

impl Default for SlicerStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for SlicerStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    fn check_params(&self, params: &serde_json::Value) -> Result<(), String> {
        let p: SlicerParams = serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
        if !matches!(p.levels, 2 | 4) {
            return Err("levels: 2 or 4".into());
        }
        if !(1..=CHUNK_BITS_MAX).contains(&p.chunk_bits) {
            return Err(format!("chunk_bits: 1..={CHUNK_BITS_MAX}"));
        }
        Ok(())
    }

    async fn run(
        &self,
        params: serde_json::Value,
        inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        ctx: StageContext,
    ) -> Result<(), StageError> {
        self.check_params(&params).map_err(StageError::Failed)?;
        let p: SlicerParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("{SLICER_KIND} params: {e}")))?;
        let [events] = <[StreamId; 1]>::try_from(outputs)
            .map_err(|o| StageError::Failed(format!("expected 1 output, got {}", o.len())))?;
        let mut sl: Option<Slicer> = None;
        let mut prev_end: Option<u64> = None;
        let mut totals = Totals::default();
        run_map_ending(SLICER_KIND, inputs, bus, cancel, move |input| {
            let mut published = Vec::new();
            let frame = match input {
                Some(Input::Frame(f)) => f,
                Some(Input::Lagged { .. }) | None => {
                    prev_end = None;
                    published.extend(
                        sl.take()
                            .and_then(|mut s| s.flush(&events, &mut totals, &ctx)),
                    );
                    return Ok(published);
                }
            };
            let Payload::Audio {
                sample_rate_hz,
                channels,
                samples,
            } = &frame.payload
            else {
                return Err(StageError::Failed(format!(
                    "{SLICER_KIND} stage expects audio frames"
                )));
            };
            let (rate, channels) = (sample_rate_hz.0, *channels as usize);
            if !(1..=2).contains(&channels) {
                return Err(StageError::Failed(format!(
                    "{SLICER_KIND}: expects 1 or 2 channels, got {channels}"
                )));
            }
            if rate == 0 || rate > u32::MAX as u64 {
                return Err(StageError::Failed(format!(
                    "{SLICER_KIND}: symbol rate {rate} Hz out of range"
                )));
            }
            if samples.len() % channels != 0 {
                return Err(StageError::Failed(format!(
                    "{SLICER_KIND}: {} samples is not a whole number of {channels}-channel frames",
                    samples.len()
                )));
            }
            let span = &frame.provenance.span;
            let seam = prev_end.is_some_and(|e| is_seam(e, span.start_ns.0, rate));
            prev_end = Some(span.end_ns.0);
            if seam
                || sl
                    .as_ref()
                    .is_some_and(|s| s.rate_hz != rate || s.channels != channels)
            {
                published.extend(
                    sl.take()
                        .and_then(|mut s| s.flush(&events, &mut totals, &ctx)),
                );
            }
            if samples.is_empty() {
                return Ok(published);
            }
            let s = match &mut sl {
                Some(s) => s,
                None => sl.insert(Slicer::new(
                    &p,
                    rate,
                    channels,
                    samples,
                    frame.provenance.clone(),
                )?),
            };
            published.extend(s.step(&p, span.start_ns.0, samples, &events, &mut totals, &ctx));
            Ok(published)
        })
        .await
    }
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use std::f64::consts::PI;
    use std::sync::{Arc, Mutex};

    use serde_json::Value;
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;
    use zeta_core::stage::{Stage, StageContext, StageError};
    use zeta_core::stream::{Frame, Payload, StreamBus};
    use zeta_core::types::{Hz, JobId, Provenance, SourceId, StreamId, StreamKind, Timestamp};

    use super::{CHUNK_BITS_MAX, CONSTELLATION_POINTS_MAX, SlicerStage, bits_event, bits_from_hex};
    use crate::fixtures::awgn_real;
    use crate::frame_span;

    const BAUD: u32 = 4_800;
    const T0: u64 = 1_000_000_000;
    const BLOCK_MS: u64 = 20;
    const SEAM_GAP_NS: u64 = 2_000_000_000;

    /// Deterministic random bits.
    fn random_bits(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (s >> 63) as u8
            })
            .collect()
    }

    fn nrz(bits: &[u8]) -> Vec<f32> {
        bits.iter()
            .map(|&b| if b == 1 { 1.0 } else { -1.0 })
            .collect()
    }

    /// `symbols` (interleaved, `channels`) at one sample per symbol from `t0`, as 20 ms
    /// audio frames whose spans meet exactly.
    fn frames(symbols: &[f32], channels: u16, t0: u64) -> Vec<Frame> {
        let per = (BAUD as u64 * BLOCK_MS / 1000) as usize * channels as usize;
        let id = StreamId::new();
        let mut start = t0;
        symbols
            .chunks(per)
            .map(|c| {
                let span = frame_span(Timestamp(start), c.len() / channels as usize, BAUD as u64);
                start = span.end_ns.0;
                Frame::new(
                    id.clone(),
                    Provenance {
                        source: SourceId("s".into()),
                        job: Some(JobId("job".into())),
                        channel: None,
                        span,
                    },
                    Payload::Audio {
                        sample_rate_hz: Hz(BAUD as u64),
                        channels,
                        samples: Arc::from(c.to_vec()),
                    },
                )
            })
            .collect()
    }

    struct Run {
        events: Vec<Frame>,
        context: Value,
        event_stream: String,
    }

    impl Run {
        /// The payloads of the events of `protocol`.
        fn of(&self, protocol: &str) -> Vec<Value> {
            self.events
                .iter()
                .filter_map(|f| match &f.payload {
                    Payload::Event(v) if v["protocol"] == protocol => Some(v.clone()),
                    _ => None,
                })
                .collect()
        }

        /// Every `bits` event's bits, in order.
        fn bits(&self) -> Vec<u8> {
            self.of("bits")
                .iter()
                .flat_map(|e| {
                    bits_from_hex(
                        e["bits"].as_str().unwrap(),
                        e["count"].as_u64().unwrap() as usize,
                    )
                })
                .collect()
        }
    }

    async fn try_run(params: Value, input: Vec<Frame>) -> Result<Run, StageError> {
        let bus = Arc::new(StreamBus::new(4096));
        let ev = bus.register(
            StreamKind::Event,
            Provenance {
                source: SourceId("s".into()),
                job: None,
                channel: None,
                span: frame_span(Timestamp(0), 0, 1),
            },
        );
        let mut rx_ev = bus.subscribe(&ev).unwrap();
        let (tx, rx) = broadcast::channel(input.len().max(1));
        let notes = Arc::new(Mutex::new(Vec::new()));
        let sink = notes.clone();
        let ctx = StageContext::new(SourceId("s".into()))
            .with_note(Arc::new(move |v| sink.lock().unwrap().push(v)));
        for f in input {
            tx.send(f).unwrap();
        }
        drop(tx);
        SlicerStage::new()
            .run(
                params,
                vec![rx],
                vec![ev.clone()],
                bus.clone(),
                CancellationToken::new(),
                ctx,
            )
            .await?;
        let mut events = Vec::new();
        while let Ok(f) = rx_ev.try_recv() {
            events.push(f);
        }
        let mut context = serde_json::Map::new();
        for n in notes.lock().unwrap().iter() {
            for (k, v) in n.as_object().unwrap() {
                context.insert(k.clone(), v.clone());
            }
        }
        Ok(Run {
            events,
            context: Value::Object(context),
            event_stream: ev.0,
        })
    }

    async fn run(params: Value, input: Vec<Frame>) -> Run {
        try_run(params, input).await.unwrap()
    }

    /// MSB-first packing of `bits` as lowercase hex, the last byte zero-padded.
    fn hex_of(bits: &[u8]) -> String {
        bits.chunks(8)
            .map(|c| {
                let b = c
                    .iter()
                    .enumerate()
                    .fold(0u8, |acc, (i, &v)| acc | (v << (7 - i)));
                format!("{b:02x}")
            })
            .collect()
    }

    #[tokio::test]
    async fn two_levels_give_the_planted_bits_in_chunks() {
        let bits = random_bits(10_000, 1);
        let r = run(
            serde_json::json!({"chunk_bits": 4096}),
            frames(&nrz(&bits), 1, T0),
        )
        .await;
        let ev = r.of("bits");
        let counts: Vec<u64> = ev.iter().map(|e| e["count"].as_u64().unwrap()).collect();
        assert_eq!(counts, [4096, 4096, 1808]);
        let offsets: Vec<u64> = ev
            .iter()
            .map(|e| e["offset_bits"].as_u64().unwrap())
            .collect();
        assert_eq!(offsets, [0, 4096, 8192]);
        for (e, want) in ev.iter().zip(bits.chunks(4096)) {
            assert_eq!(e["bits"], hex_of(want));
            assert_eq!(e["rate_baud"], BAUD);
        }
        assert_eq!(r.bits(), bits);
        assert_eq!(ev[0]["t_ns"], T0);
        let period = 1_000_000_000 / BAUD as u64;
        let t1 = ev[1]["t_ns"].as_u64().unwrap();
        assert!(t1.abs_diff(T0 + 4096 * period) <= 4096, "{t1}");
    }

    #[tokio::test]
    async fn four_levels_with_gray_mapping() {
        let levels = [-1.0f32, -1.0 / 3.0, 1.0 / 3.0, 1.0];
        let x: Vec<f32> = (0..400).map(|i| levels[i % 4]).collect();
        let r = run(
            serde_json::json!({"levels": 4, "mapping": "gray"}),
            frames(&x, 1, T0),
        )
        .await;
        let want: Vec<u8> = (0..100).flat_map(|_| [0, 0, 0, 1, 1, 1, 1, 0]).collect();
        assert_eq!(r.bits(), want);
    }

    #[tokio::test]
    async fn qpsk_differential_undoes_a_phase_rotation() {
        let bits = random_bits(2000, 3);
        let mut s = 0u8;
        let iq: Vec<f32> = bits
            .chunks(2)
            .flat_map(|d| {
                s = (s + 2 * d[0] + d[1]) % 4;
                // Rotated by 90 degrees: one point on.
                let a = (s as f64 + 1.0) * PI / 2.0;
                [a.cos() as f32, a.sin() as f32]
            })
            .collect();
        let r = run(
            serde_json::json!({"constellation": "qpsk", "differential": true}),
            frames(&iq, 2, T0),
        )
        .await;
        let got = r.bits();
        assert_eq!(got.len(), bits.len());
        // The first symbol is against 0, so the rotation shows there only.
        assert_eq!(got[2..], bits[2..]);
        assert_ne!(got[..2], bits[..2]);
        assert!(r.context["slicer"]["evm"].as_f64().unwrap() < 0.01);
    }

    #[tokio::test]
    async fn a_seam_flushes_the_chunk_and_restarts_the_offset() {
        let a = random_bits(3000, 5);
        let b = random_bits(3000, 6);
        let mut input = frames(&nrz(&a), 1, T0);
        let a_end = input.last().unwrap().provenance.span.end_ns.0;
        let t1 = a_end + SEAM_GAP_NS;
        input.extend(frames(&nrz(&b), 1, t1));
        let r = run(serde_json::json!({}), input).await;
        let ev = r.of("bits");
        let counts: Vec<u64> = ev.iter().map(|e| e["count"].as_u64().unwrap()).collect();
        assert_eq!(counts, [3000, 3000]);
        let offsets: Vec<u64> = ev
            .iter()
            .map(|e| e["offset_bits"].as_u64().unwrap())
            .collect();
        assert_eq!(offsets, [0, 0]);
        assert_eq!(ev[0]["t_ns"], T0);
        assert_eq!(ev[1]["t_ns"], t1);
        let first = r
            .events
            .iter()
            .find(|f| matches!(&f.payload, Payload::Event(v) if v["protocol"] == "bits"))
            .unwrap();
        assert_eq!(first.provenance.span.start_ns.0, T0);
        assert!(
            first.provenance.span.end_ns.0.abs_diff(a_end) <= 3000,
            "{:?}",
            first.provenance.span
        );
        let mut want = a.clone();
        want.extend(&b);
        assert_eq!(r.bits(), want);
    }

    #[tokio::test]
    async fn a_constellation_event_and_the_note_appear() {
        let bits = random_bits(2 * BAUD as usize, 7);
        let r = run(serde_json::json!({}), frames(&nrz(&bits), 1, T0)).await;
        let c = r.of("constellation");
        assert!(!c.is_empty(), "{} events", r.events.len());
        for e in &c {
            let points = e["points"].as_array().unwrap();
            assert!(!points.is_empty() && points.len() <= CONSTELLATION_POINTS_MAX);
            assert_eq!(e["channels"], 1);
            assert!(points.iter().all(|p| p[1] == 0.0));
            assert!(e["t_ns"].is_u64() && e["summary"].is_string());
        }
        let s = &r.context["slicer"];
        assert_eq!(s["symbols"], bits.len());
        assert_eq!(s["stream"], r.event_stream);
        let eye = s["eye_open"].as_f64().unwrap();
        assert!(eye > 0.95, "{eye}");
    }

    #[tokio::test]
    async fn a_noisy_stream_reads_a_smaller_eye() {
        let mut x = nrz(&random_bits(2 * BAUD as usize, 8));
        awgn_real(&mut x, 12.0, 9);
        let r = run(serde_json::json!({}), frames(&x, 1, T0)).await;
        let eye = r.context["slicer"]["eye_open"].as_f64().unwrap();
        assert!((0.0..0.8).contains(&eye), "{eye}");
    }

    #[tokio::test]
    async fn two_channels_without_a_constellation_fail() {
        let Err(StageError::Failed(e)) =
            try_run(serde_json::json!({}), frames(&[1.0, 0.0, -1.0, 0.0], 2, T0)).await
        else {
            panic!("a failure")
        };
        assert_eq!(e, "slicer: 2-channel input needs a constellation");
    }

    #[test]
    fn chunk_bits_over_the_cap_is_refused_naming_the_field() {
        let st = SlicerStage::new();
        assert_eq!(
            st.check_params(&serde_json::json!({"chunk_bits": 200_000})),
            Err("chunk_bits: 1..=131072".to_string())
        );
        assert_eq!(
            st.check_params(&serde_json::json!({"chunk_bits": 0})),
            Err("chunk_bits: 1..=131072".to_string())
        );
        assert_eq!(
            st.check_params(&serde_json::json!({"levels": 3})),
            Err("levels: 2 or 4".to_string())
        );
        assert!(
            st.check_params(&serde_json::json!({"constellation": "8psk"}))
                .is_ok()
        );
        assert!(st.check_params(&serde_json::json!({"bogus": 1})).is_err());
    }

    #[test]
    fn a_full_chunk_event_is_under_the_cap() {
        let bits = vec![1u8; CHUNK_BITS_MAX as usize];
        assert!(bits_event(4800, &bits, 0, 0).to_string().len() < 65_536);
    }

    #[test]
    fn hex_round_trips_with_padding() {
        let all: Vec<u8> = (0..16)
            .map(|i| ((0xa5c0u16 >> (15 - i)) & 1) as u8)
            .collect();
        assert_eq!(bits_from_hex("a5c", 11), all[..11]);
        let e = bits_event(1200, &all[..11], 0, 5);
        assert_eq!(e["bits"], "a5c0");
        assert_eq!(e["count"], 11);
        assert_eq!(e["offset_bits"], 0);
        assert_eq!(e["t_ns"], 5);
        assert_eq!(e["summary"], "11 bits at 1200 Bd");
        assert_eq!(bits_from_hex(e["bits"].as_str().unwrap(), 11), all[..11]);
    }
}
