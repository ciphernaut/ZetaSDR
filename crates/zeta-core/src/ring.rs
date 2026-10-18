//! Per-source IQ history: a bounded ring addressed by time.

use std::collections::VecDeque;

use num_complex::Complex;

use crate::port::IqBlock;
use crate::types::{Hz, Span, Timestamp};

const NS: u128 = 1_000_000_000;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RingError {
    #[error("requested {requested:?} is older than earliest retained {earliest:?}")]
    TooOld { requested: Timestamp, earliest: Timestamp },
    #[error("requested {requested:?} is later than latest retained {latest:?}")]
    Future { requested: Timestamp, latest: Timestamp },
    #[error("ring is empty or span is empty")]
    Empty,
}

/// Sample history for one source, bounded to a number of seconds.
///
/// Blocks are assumed contiguous; a block that does not start where the ring
/// expects (within one sample) is treated as a gap and restarts the ring.
pub struct IqRing {
    capacity_seconds: f64,
    sample_rate_hz: u64,
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

    /// Timestamp of the oldest retained sample.
    pub fn earliest(&self) -> Option<Timestamp> {
        (!self.buf.is_empty()).then(|| Timestamp(self.time_of(self.dropped)))
    }

    /// Timestamp of the newest retained sample.
    pub fn latest(&self) -> Option<Timestamp> {
        (!self.buf.is_empty()).then(|| Timestamp(self.time_of(self.dropped + self.buf.len() as u64 - 1)))
    }

    /// Samples covering the half-open span `[start, end)`, or an error; never partial.
    pub fn read(&self, span: Span) -> Result<Vec<Complex<f32>>, RingError> {
        let (earliest, latest) = match (self.earliest(), self.latest()) {
            (Some(e), Some(l)) => (e, l),
            _ => return Err(RingError::Empty),
        };
        if span.end.0 <= span.start.0 {
            return Err(RingError::Empty);
        }
        if span.start.0 < earliest.0 {
            return Err(RingError::TooOld { requested: span.start, earliest });
        }
        let rate = self.sample_rate_hz as u128;
        let offset = |t: u64| -> usize {
            (((t - earliest.0) as u128 * rate + NS / 2) / NS) as usize
        };
        let (from, to) = (offset(span.start.0), offset(span.end.0));
        if to > self.buf.len() {
            return Err(RingError::Future { requested: span.end, latest });
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
        let samples: Vec<Complex<f32>> =
            (0..1000).map(|i| Complex::new((index * 1000 + i) as f32, 0.0)).collect();
        IqBlock {
            t0: Timestamp(index * 1_000_000_000),
            sample_rate_hz: Hz(RATE),
            center_hz: Hz(100_000_000),
            samples: Arc::from(samples),
        }
    }

    fn span(a: u64, b: u64) -> Span {
        Span { start: Timestamp(a * 1_000_000_000), end: Timestamp(b * 1_000_000_000) }
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
                requested: Timestamp(0),
                earliest: Timestamp(3_000_000_000)
            })
        );
    }

    #[test]
    fn read_beyond_latest_is_future() {
        let mut ring = IqRing::new(10.0, Hz(RATE));
        ring.push(&block(0));
        assert!(matches!(ring.read(span(0, 2)), Err(RingError::Future { .. })));
    }

    #[test]
    fn empty_ring_is_empty_error() {
        let ring = IqRing::new(10.0, Hz(RATE));
        assert_eq!(ring.read(span(0, 1)), Err(RingError::Empty));
    }
}
