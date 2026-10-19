//! Per-source IQ history: a bounded ring addressed by time.

use std::collections::VecDeque;

use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::port::IqBlock;
use crate::types::{Hz, Span, Timestamp};

const NS: u128 = 1_000_000_000;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cause", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub enum RingError {
    #[error("requested {requested_ns:?} is older than earliest retained {earliest_ns:?}")]
    TooOld {
        requested_ns: Timestamp,
        earliest_ns: Timestamp,
    },
    #[error("requested {requested_ns:?} is later than latest retained {latest_ns:?}")]
    Future {
        requested_ns: Timestamp,
        latest_ns: Timestamp,
    },
    #[error("span end {end_ns:?} is not after its start {start_ns:?}")]
    BadSpan {
        start_ns: Timestamp,
        end_ns: Timestamp,
    },
    #[error("ring is empty")]
    Empty,
}

/// Sample history for one source, bounded to a number of seconds.
///
/// Blocks are assumed contiguous; a block that does not start where the ring
/// expects (within one sample) is treated as a gap and restarts the ring, as
/// is a change of sample rate or centre frequency.
pub struct IqRing {
    capacity_seconds: f64,
    sample_rate_hz: u64,
    /// Centre frequency of the retained samples; `None` until the first block.
    center_hz: Option<Hz>,
    capacity: usize,
    buf: VecDeque<Complex<f32>>,
    /// Timestamp of the first sample of the current contiguous run.
    origin_ns: u64,
    /// Samples dropped from the front of the run so far.
    dropped: u64,
}

impl IqRing {
    pub fn new(capacity_seconds: f64, sample_rate_hz: Hz) -> Self {
        Self {
            capacity_seconds,
            sample_rate_hz: sample_rate_hz.0,
            center_hz: None,
            capacity: Self::cap_for(capacity_seconds, sample_rate_hz.0),
            buf: VecDeque::new(),
            origin_ns: 0,
            dropped: 0,
        }
    }

    fn cap_for(seconds: f64, rate: u64) -> usize {
        (seconds * rate as f64).ceil() as usize
    }

    /// Time of the sample `n` samples after the run origin, rounded to the nearest ns.
    fn time_of(&self, n: u64) -> u64 {
        let rate = self.sample_rate_hz.max(1) as u128;
        self.origin_ns + ((n as u128 * NS + rate / 2) / rate) as u64
    }

    fn clear(&mut self) {
        self.buf.clear();
        self.dropped = 0;
    }

    pub fn push(&mut self, block: &IqBlock) {
        if block.samples.is_empty() {
            return;
        }
        if block.sample_rate_hz.0 != self.sample_rate_hz {
            self.sample_rate_hz = block.sample_rate_hz.0;
            self.capacity = Self::cap_for(self.capacity_seconds, self.sample_rate_hz);
            self.clear();
        }
        if self.center_hz != Some(block.center_hz) {
            self.center_hz = Some(block.center_hz);
            self.clear();
        }
        if !self.buf.is_empty() {
            let expected = self.time_of(self.dropped + self.buf.len() as u64);
            let tolerance = NS.div_ceil(self.sample_rate_hz.max(1) as u128) as u64;
            if block.t0.0.abs_diff(expected) > tolerance {
                self.clear();
            }
        }
        if self.buf.is_empty() {
            self.origin_ns = block.t0.0;
            self.dropped = 0;
        }
        self.buf.extend(block.samples.iter().copied());
        if self.buf.len() > self.capacity {
            let excess = self.buf.len() - self.capacity;
            self.buf.drain(..excess);
            self.dropped += excess as u64;
        }
    }

    /// Change the history length to `seconds` (at the current rate), keeping
    /// the newest samples when it shrinks.
    pub fn resize(&mut self, seconds: f64) {
        self.capacity_seconds = seconds;
        self.capacity = Self::cap_for(seconds, self.sample_rate_hz);
        if self.buf.len() > self.capacity {
            let excess = self.buf.len() - self.capacity;
            self.buf.drain(..excess);
            self.dropped += excess as u64;
        }
    }

    /// Most samples the ring retains at its current rate.
    pub fn capacity_samples(&self) -> usize {
        self.capacity
    }

    pub fn sample_rate_hz(&self) -> Hz {
        Hz(self.sample_rate_hz)
    }

    /// Centre frequency the retained samples were captured at.
    pub fn center_hz(&self) -> Option<Hz> {
        self.center_hz
    }

    /// Timestamp of the oldest retained sample.
    pub fn earliest(&self) -> Option<Timestamp> {
        (!self.buf.is_empty()).then(|| Timestamp(self.time_of(self.dropped)))
    }

    /// Timestamp of the newest retained sample.
    pub fn latest(&self) -> Option<Timestamp> {
        (!self.buf.is_empty())
            .then(|| Timestamp(self.time_of(self.dropped + self.buf.len() as u64 - 1)))
    }

    /// Samples covering the half-open span `[start_ns, end_ns)`, or an error;
    /// never partial. A span whose end is not after its start is `BadSpan`.
    pub fn read(&self, span: Span) -> Result<Vec<Complex<f32>>, RingError> {
        if span.end_ns.0 <= span.start_ns.0 {
            return Err(RingError::BadSpan {
                start_ns: span.start_ns,
                end_ns: span.end_ns,
            });
        }
        let (earliest, latest) = match (self.earliest(), self.latest()) {
            (Some(e), Some(l)) => (e, l),
            _ => return Err(RingError::Empty),
        };
        if span.start_ns.0 < earliest.0 {
            return Err(RingError::TooOld {
                requested_ns: span.start_ns,
                earliest_ns: earliest,
            });
        }
        let rate = self.sample_rate_hz as u128;
        let offset =
            |t: u64| -> usize { (((t - earliest.0) as u128 * rate + NS / 2) / NS) as usize };
        let (from, to) = (offset(span.start_ns.0), offset(span.end_ns.0));
        if to > self.buf.len() {
            return Err(RingError::Future {
                requested_ns: span.end_ns,
                latest_ns: latest,
            });
        }
        Ok(self.buf.range(from..to).copied().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const RATE: u64 = 1000;

    fn block(index: u64) -> IqBlock {
        let samples: Vec<Complex<f32>> = (0..1000)
            .map(|i| Complex::new((index * 1000 + i) as f32, 0.0))
            .collect();
        IqBlock {
            t0: Timestamp(index * 1_000_000_000),
            sample_rate_hz: Hz(RATE),
            center_hz: Hz(100_000_000),
            samples: Arc::from(samples),
        }
    }

    fn span(a: u64, b: u64) -> Span {
        Span {
            start_ns: Timestamp(a * 1_000_000_000),
            end_ns: Timestamp(b * 1_000_000_000),
        }
    }

    #[test]
    fn ring_resize_keeps_newest() {
        let mut ring = IqRing::new(10.0, Hz(RATE));
        for i in 0..3 {
            ring.push(&block(i));
        }
        ring.resize(1.0);
        assert_eq!(ring.capacity_samples(), 1000);
        assert_eq!(ring.earliest(), Some(Timestamp(2_000 * MS)));
        assert_eq!(ring.latest(), Some(Timestamp(2_999 * MS)));
        assert_eq!(ring.read(span(2, 3)).unwrap(), block(2).samples.to_vec());
        ring.resize(5.0);
        ring.push(&block(3));
        assert_eq!(ring.earliest(), Some(Timestamp(2_000 * MS)));
        assert_eq!(ring.read(span(2, 4)).unwrap().len(), 2000);
    }

    #[test]
    fn read_returns_exact_window() {
        let mut ring = IqRing::new(10.0, Hz(RATE));
        for i in 0..3 {
            ring.push(&block(i));
        }
        let got = ring.read(span(1, 2)).unwrap();
        assert_eq!(got.len(), 1000);
        assert_eq!(got.as_slice(), &block(1).samples[..]);
    }

    #[test]
    fn read_older_than_capacity_is_too_old() {
        let mut ring = IqRing::new(2.0, Hz(RATE));
        for i in 0..5 {
            ring.push(&block(i));
        }
        assert_eq!(
            ring.read(span(0, 1)),
            Err(RingError::TooOld {
                requested_ns: Timestamp(0),
                earliest_ns: Timestamp(3_000_000_000)
            })
        );
    }

    #[test]
    fn read_beyond_latest_is_future() {
        let mut ring = IqRing::new(10.0, Hz(RATE));
        ring.push(&block(0));
        assert_eq!(
            ring.read(span(0, 2)),
            Err(RingError::Future {
                requested_ns: Timestamp(2_000_000_000),
                latest_ns: Timestamp(999_000_000)
            })
        );
    }

    #[test]
    fn reversed_or_empty_span_is_bad_span() {
        let mut ring = IqRing::new(10.0, Hz(RATE));
        ring.push(&block(0));
        for (a, b) in [(500 * MS, 200 * MS), (300 * MS, 300 * MS)] {
            assert_eq!(
                ring.read(span_ns(a, b)),
                Err(RingError::BadSpan {
                    start_ns: Timestamp(a),
                    end_ns: Timestamp(b)
                })
            );
        }
        let bad = serde_json::to_value(RingError::BadSpan {
            start_ns: Timestamp(2),
            end_ns: Timestamp(1),
        })
        .unwrap();
        assert_eq!(
            bad,
            serde_json::json!({"cause": "bad_span", "start_ns": 2, "end_ns": 1})
        );
    }

    #[test]
    fn empty_ring_is_empty_error() {
        let ring = IqRing::new(10.0, Hz(RATE));
        assert_eq!(ring.read(span(0, 1)), Err(RingError::Empty));
    }

    const MS: u64 = 1_000_000;

    fn span_ns(a: u64, b: u64) -> Span {
        Span {
            start_ns: Timestamp(a),
            end_ns: Timestamp(b),
        }
    }

    fn block_at(t0_ns: u64, first: u64, n: u64) -> IqBlock {
        let samples: Vec<Complex<f32>> = (0..n)
            .map(|i| Complex::new((first + i) as f32, 0.0))
            .collect();
        IqBlock {
            t0: Timestamp(t0_ns),
            sample_rate_hz: Hz(RATE),
            center_hz: Hz(100_000_000),
            samples: Arc::from(samples),
        }
    }

    /// Capacity 2 s, 5 s pushed: retains samples 3000..=4999, earliest 3 s, latest 4.999 s.
    fn wrapped() -> IqRing {
        let mut ring = IqRing::new(2.0, Hz(RATE));
        for i in 0..5 {
            ring.push(&block(i));
        }
        ring
    }

    #[test]
    fn read_at_earliest_edge_succeeds() {
        let got = wrapped().read(span_ns(3000 * MS, 3001 * MS)).unwrap();
        assert_eq!(got, vec![Complex::new(3000.0, 0.0)]);
    }

    #[test]
    fn read_one_sample_before_earliest_is_too_old() {
        assert_eq!(
            wrapped().read(span_ns(2999 * MS, 3001 * MS)),
            Err(RingError::TooOld {
                requested_ns: Timestamp(2999 * MS),
                earliest_ns: Timestamp(3000 * MS)
            })
        );
    }

    #[test]
    fn read_to_one_past_latest_succeeds() {
        let got = wrapped().read(span_ns(4000 * MS, 5000 * MS)).unwrap();
        assert_eq!(got.len(), 1000);
        assert_eq!(got.last(), Some(&Complex::new(4999.0, 0.0)));
    }

    #[test]
    fn read_beyond_that_is_future() {
        assert_eq!(
            wrapped().read(span_ns(4000 * MS, 5001 * MS)),
            Err(RingError::Future {
                requested_ns: Timestamp(5001 * MS),
                latest_ns: Timestamp(4999 * MS)
            })
        );
    }

    #[test]
    fn read_after_wrap_straddles_blocks() {
        let got = wrapped().read(span_ns(3500 * MS, 4500 * MS)).unwrap();
        let want: Vec<Complex<f32>> = (3500..4500).map(|v| Complex::new(v as f32, 0.0)).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn gap_clears_ring() {
        let mut ring = IqRing::new(10.0, Hz(RATE));
        ring.push(&block(0));
        ring.push(&block(1));
        ring.push(&block_at(5_000 * MS, 5000, 1000));
        assert_eq!(ring.earliest(), Some(Timestamp(5_000 * MS)));
        assert_eq!(ring.latest(), Some(Timestamp(5_999 * MS)));
        assert!(matches!(
            ring.read(span(0, 1)),
            Err(RingError::TooOld { .. })
        ));
        assert_eq!(ring.read(span(5, 6)).unwrap().len(), 1000);
    }

    #[test]
    fn centre_change_clears_ring_and_is_tracked() {
        let mut ring = IqRing::new(10.0, Hz(RATE));
        assert_eq!(ring.center_hz(), None);
        ring.push(&block(0));
        assert_eq!(ring.center_hz(), Some(Hz(100_000_000)));
        let moved = IqBlock {
            center_hz: Hz(433_000_000),
            ..block(1)
        };
        ring.push(&moved);
        assert_eq!(ring.center_hz(), Some(Hz(433_000_000)));
        assert_eq!(ring.earliest(), Some(Timestamp(1_000 * MS)));
    }

    #[test]
    fn one_sample_jitter_is_tolerated() {
        for t0 in [1_000 * MS - MS, 1_000 * MS + MS] {
            let mut ring = IqRing::new(10.0, Hz(RATE));
            ring.push(&block(0));
            ring.push(&block_at(t0, 1000, 1000));
            assert_eq!(
                ring.earliest(),
                Some(Timestamp(0)),
                "jitter at {t0} cleared the ring"
            );
            assert_eq!(ring.read(span_ns(0, 2_000 * MS)).unwrap().len(), 2000);
        }
    }
}
