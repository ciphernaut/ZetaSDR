//! Per-source IQ history: a bounded ring addressed by time.

use std::collections::VecDeque;

use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::port::IqBlock;
use crate::types::{Hz, Span, Timestamp};

const NS: u128 = 1_000_000_000;
/// The most segments the ring keeps. A 10 s history sees far fewer breaks than
/// this from overflows, and the linear search for a segment stays cheap.
const MAX_SEGMENTS: usize = 256;

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
    #[error(
        "span {start_ns:?}..{end_ns:?} crosses a gap in the history, between {gap_start_ns:?} and {gap_end_ns:?}"
    )]
    Gap {
        start_ns: Timestamp,
        end_ns: Timestamp,
        gap_start_ns: Timestamp,
        gap_end_ns: Timestamp,
    },
    #[error("ring is empty")]
    Empty,
}

/// A contiguous run of the retained samples: where its first sample sits in
/// the stream (counted from the ring's start, as `dropped + index`) and when
/// that sample was taken.
#[derive(Clone, Copy, Debug)]
struct Segment {
    origin_ns: u64,
    start: u64,
}

/// Sample history for one input, bounded to a number of seconds.
///
/// Blocks are assumed contiguous. A block that does not start where the ring
/// expects (within one sample) is a gap: it starts a new segment with its own
/// origin, and the history before it stays, so a read inside either side is
/// served and only a read across the gap is refused. A change of sample rate
/// or centre frequency restarts the ring.
pub struct IqRing {
    capacity_seconds: f64,
    sample_rate_hz: u64,
    /// Centre frequency of the retained samples; `None` until the first block.
    center_hz: Option<Hz>,
    capacity: usize,
    buf: VecDeque<Complex<f32>>,
    /// The segments of `buf`, oldest first; never empty while `buf` is not.
    /// The front one may start before the retained samples (its front was
    /// trimmed); every other starts at or after them.
    segments: VecDeque<Segment>,
    /// Samples dropped from the front of the buffer so far.
    dropped: u64,
}

impl IqRing {
    pub fn new(capacity_seconds: f64, sample_rate_hz: Hz) -> Self {
        Self {
            capacity_seconds,
            sample_rate_hz: sample_rate_hz.0,
            center_hz: None,
            capacity: Self::capacity_for(capacity_seconds, sample_rate_hz.0),
            buf: VecDeque::new(),
            segments: VecDeque::new(),
            dropped: 0,
        }
    }

    /// The samples a ring of `seconds` holds at `rate`.
    pub fn capacity_for(seconds: f64, rate: u64) -> usize {
        (seconds * rate as f64).ceil() as usize
    }

    /// Time of the stream sample `idx` (counted as `Segment::start` is), taken
    /// from its segment `seg`, rounded to the nearest ns.
    fn time_of(&self, seg: &Segment, idx: u64) -> u64 {
        let rate = self.sample_rate_hz.max(1) as u128;
        seg.origin_ns + (((idx - seg.start) as u128 * NS + rate / 2) / rate) as u64
    }

    /// Segment `i`'s retained samples as stream indices `[first, end)`.
    fn bounds(&self, i: usize) -> (u64, u64) {
        let first = self.segments[i].start.max(self.dropped);
        let end = self
            .segments
            .get(i + 1)
            .map_or(self.dropped + self.buf.len() as u64, |s| s.start);
        (first, end)
    }

    fn clear(&mut self) {
        self.buf.clear();
        self.segments.clear();
        self.dropped = 0;
    }

    /// Drop the oldest `excess` samples, and the segments they leave empty.
    fn trim(&mut self, excess: usize) {
        self.buf.drain(..excess);
        self.dropped += excess as u64;
        while self.segments.len() > 1 && self.segments[1].start <= self.dropped {
            self.segments.pop_front();
        }
    }

    pub fn push(&mut self, block: &IqBlock) {
        if block.samples.is_empty() {
            return;
        }
        if block.sample_rate_hz.0 != self.sample_rate_hz {
            self.sample_rate_hz = block.sample_rate_hz.0;
            self.capacity = Self::capacity_for(self.capacity_seconds, self.sample_rate_hz);
            // Drop the old allocation first, then grow: assigning a new buffer in one
            // expression would allocate the new one while the old one is still live.
            self.buf = VecDeque::new();
            self.buf.reserve(self.capacity);
            self.segments.clear();
            self.dropped = 0;
        }
        if self.center_hz != Some(block.center_hz) {
            self.center_hz = Some(block.center_hz);
            self.clear();
        }
        let next = self.dropped + self.buf.len() as u64;
        let new_segment = match self.segments.back() {
            None => true,
            Some(last) => {
                let expected = self.time_of(last, next);
                let tolerance = NS.div_ceil(self.sample_rate_hz.max(1) as u128) as u64;
                if block.t0.0 + tolerance < expected {
                    // Time went backwards: segments stay ordered in time, so
                    // the history before it cannot stay.
                    self.clear();
                    true
                } else {
                    block.t0.0 > expected + tolerance
                }
            }
        };
        let next = self.dropped + self.buf.len() as u64;
        if new_segment {
            self.segments.push_back(Segment {
                origin_ns: block.t0.0,
                start: next,
            });
            // An input that gaps on every block must not grow the list without
            // limit: past the cap the oldest segment goes, samples and all.
            if self.segments.len() > MAX_SEGMENTS {
                let keep_from = self.segments[1].start;
                self.trim((keep_from - self.dropped) as usize);
            }
        }
        self.buf.extend(block.samples.iter().copied());
        if self.buf.len() > self.capacity {
            self.trim(self.buf.len() - self.capacity);
        }
    }

    /// Change the history length to `seconds` at the current rate, keeping the
    /// newest samples when it shrinks and releasing the freed memory; a block
    /// with a new rate recomputes the capacity (from `seconds`) and restarts
    /// the ring. Before a rate change use [`IqRing::set_capacity_seconds`].
    pub fn resize(&mut self, seconds: f64) {
        self.capacity_seconds = seconds;
        self.capacity = Self::capacity_for(seconds, self.sample_rate_hz);
        if self.buf.len() > self.capacity {
            self.trim(self.buf.len() - self.capacity);
        }
        self.buf.shrink_to_fit();
    }

    /// Set the history length a block with a new rate sizes the ring to,
    /// touching nothing now: for a retune, where [`IqRing::push`] restarts
    /// the ring at the new rate anyway (and [`IqRing::resize`] would trim
    /// and reallocate the old rate's samples first, for nothing).
    pub fn set_capacity_seconds(&mut self, seconds: f64) {
        self.capacity_seconds = seconds;
    }

    /// The history length the ring is sized to, in seconds.
    pub fn capacity_seconds(&self) -> f64 {
        self.capacity_seconds
    }

    /// Bytes currently allocated for samples.
    pub fn capacity_bytes(&self) -> usize {
        self.buf.capacity() * std::mem::size_of::<Complex<f32>>()
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
        let seg = self.segments.front()?;
        Some(Timestamp(self.time_of(seg, self.bounds(0).0)))
    }

    /// Timestamp of the newest retained sample.
    pub fn latest(&self) -> Option<Timestamp> {
        let i = self.segments.len().checked_sub(1)?;
        Some(Timestamp(
            self.time_of(&self.segments[i], self.bounds(i).1 - 1),
        ))
    }

    /// Timestamp of the first sample of the newest segment: the start of the
    /// longest span ending at [`IqRing::latest`] that a read can serve.
    pub fn latest_run_start(&self) -> Option<Timestamp> {
        let i = self.segments.len().checked_sub(1)?;
        Some(Timestamp(self.time_of(&self.segments[i], self.bounds(i).0)))
    }

    /// The gaps inside the retained history: the segments less one.
    pub fn gaps(&self) -> usize {
        self.segments.len().saturating_sub(1)
    }

    /// Seconds of samples the ring holds (samples held over the rate): never
    /// more than the ring's size, whatever gaps lie inside it.
    pub fn held_s(&self) -> f64 {
        self.buf.len() as f64 / self.sample_rate_hz.max(1) as f64
    }

    /// Seconds of samples in the newest contiguous run: the longest "last N
    /// seconds" a read can serve, since a read across a gap is refused.
    pub fn run_s(&self) -> f64 {
        let Some(i) = self.segments.len().checked_sub(1) else {
            return 0.0;
        };
        let (first, end) = self.bounds(i);
        (end - first) as f64 / self.sample_rate_hz.max(1) as f64
    }

    /// The first `samples` samples at `center` starting at `after` (at the
    /// ring's earliest when `after` is `None` or older), as one block; `None`
    /// while the ring holds fewer from there or holds another centre.
    ///
    /// A block is contiguous: it never straddles a gap. When the segment
    /// `after` falls in holds fewer than `samples` from there and a gap follows
    /// (or `after` lies inside the gap), the answer is the first `samples` of
    /// the next segment that holds them, so the caller sees `t0` jump, as a
    /// scan's FFT steps need whole blocks. Within one segment a block ending
    /// at `t` is followed by the one asked for `after: t`.
    pub fn run_after(
        &self,
        center: Hz,
        after: Option<Timestamp>,
        samples: usize,
    ) -> Option<IqBlock> {
        if self.center_hz != Some(center) || self.buf.is_empty() {
            return None;
        }
        // The segment `after` falls in (the first when it is older than all),
        // and the sample there, rounded to the nearest so a time a nanosecond
        // either side of a sample's lands on it.
        let (seg, from) = match after.and_then(|t| self.segment_at(t.0).map(|i| (i, t.0))) {
            Some((i, t)) => {
                let at = self.time_of(&self.segments[i], self.bounds(i).0);
                (i, self.offset(t - at))
            }
            None => (0, 0),
        };
        for i in seg..self.segments.len() {
            let (first, end) = self.bounds(i);
            let skip = if i == seg { from } else { 0 };
            let len = (end - first) as usize;
            if len.saturating_sub(skip) >= samples {
                let at = first + skip as u64;
                let lo = (at - self.dropped) as usize;
                return Some(IqBlock {
                    t0: Timestamp(self.time_of(&self.segments[i], at)),
                    sample_rate_hz: Hz(self.sample_rate_hz),
                    center_hz: center,
                    samples: self.buf.range(lo..lo + samples).copied().collect(),
                });
            }
        }
        None
    }

    /// The newest segment whose first retained sample is at or before `t`.
    fn segment_at(&self, t: u64) -> Option<usize> {
        (0..self.segments.len())
            .rev()
            .find(|&i| self.time_of(&self.segments[i], self.bounds(i).0) <= t)
    }

    /// Samples in `ns` nanoseconds, rounded to the nearest.
    fn offset(&self, ns: u64) -> usize {
        ((ns as u128 * self.sample_rate_hz as u128 + NS / 2) / NS) as usize
    }

    /// Samples covering the half-open span `[start_ns, end_ns)`, or an error;
    /// never partial. A span whose end is not after its start is `BadSpan`. A
    /// span wholly inside one segment is served from it; one that crosses a gap,
    /// or starts inside one, is `Gap`.
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
        let too_old = RingError::TooOld {
            requested_ns: span.start_ns,
            earliest_ns: earliest,
        };
        if span.start_ns.0 < earliest.0 {
            return Err(too_old);
        }
        let Some(i) = self.segment_at(span.start_ns.0) else {
            return Err(too_old);
        };
        let (first, end) = self.bounds(i);
        let seg = &self.segments[i];
        let at = self.time_of(seg, first);
        let len = (end - first) as usize;
        let (from, to) = (
            self.offset(span.start_ns.0 - at),
            self.offset(span.end_ns.0.saturating_sub(at)),
        );
        if to > len || from >= len {
            if i + 1 == self.segments.len() {
                return Err(RingError::Future {
                    requested_ns: span.end_ns,
                    latest_ns: latest,
                });
            }
            return Err(RingError::Gap {
                start_ns: span.start_ns,
                end_ns: span.end_ns,
                gap_start_ns: Timestamp(self.time_of(seg, end - 1)),
                gap_end_ns: Timestamp(self.time_of(&self.segments[i + 1], end)),
            });
        }
        let lo = (first - self.dropped) as usize;
        Ok(self.buf.range(lo + from..lo + to).copied().collect())
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
    fn capacity_seconds_apply_at_the_next_rate() {
        let mut ring = IqRing::new(10.0, Hz(RATE));
        for i in 0..3 {
            ring.push(&block(i));
        }
        let bytes = ring.capacity_bytes();
        ring.set_capacity_seconds(2.0);
        // Nothing trimmed or reallocated at the old rate.
        assert_eq!(ring.capacity_samples(), 10_000);
        assert_eq!(ring.capacity_bytes(), bytes);
        assert_eq!(ring.read(span(0, 3)).unwrap().len(), 3000);
        let mut b = block(3);
        b.sample_rate_hz = Hz(2 * RATE);
        ring.push(&b);
        assert_eq!(ring.capacity_samples(), 4000);
        assert_eq!(ring.earliest(), Some(Timestamp(3_000 * MS)));
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

    /// The sample values of `b`, as the `n` the test blocks number them by.
    fn values(b: &IqBlock) -> Vec<u64> {
        b.samples.iter().map(|c| c.re as u64).collect()
    }

    #[test]
    fn run_after_gives_the_first_samples_of_the_centre() {
        let mut ring = IqRing::new(10.0, Hz(RATE));
        ring.push(&block(0));
        ring.push(&block(1));
        let got = ring.run_after(Hz(100_000_000), None, 1500).unwrap();
        assert_eq!(got.t0, Timestamp(0));
        assert_eq!(got.center_hz, Hz(100_000_000));
        assert_eq!(got.sample_rate_hz, Hz(RATE));
        assert_eq!(values(&got), (0..1500).collect::<Vec<u64>>());
        // A time older than the ring's earliest starts at the earliest.
        let mut ring = wrapped();
        let got = ring.run_after(Hz(100_000_000), Some(Timestamp(MS)), 10);
        assert_eq!(got.map(|b| b.t0), Some(Timestamp(3_000 * MS)));
        // A gap starts a segment; a run that does not fit before it is the next one's.
        ring.push(&block_at(9_000 * MS, 9000, 500));
        let got = ring.run_after(Hz(100_000_000), None, 1500).unwrap();
        assert_eq!(got.t0, Timestamp(3_500 * MS));
        assert!(ring.run_after(Hz(100_000_000), None, 1501).is_none());
    }

    #[test]
    fn run_after_is_none_for_another_centre_or_too_few() {
        let empty = IqRing::new(10.0, Hz(RATE));
        assert!(empty.run_after(Hz(100_000_000), None, 1).is_none());
        let mut ring = IqRing::new(10.0, Hz(RATE));
        ring.push(&block(0));
        ring.push(&block(1));
        assert!(ring.run_after(Hz(433_000_000), None, 10).is_none());
        assert!(ring.run_after(Hz(100_000_000), None, 2001).is_none());
        assert!(ring.run_after(Hz(100_000_000), None, 2000).is_some());
        // Past the newest sample: nothing yet.
        assert!(
            ring.run_after(Hz(100_000_000), Some(Timestamp(5_000 * MS)), 1)
                .is_none()
        );
    }

    #[test]
    fn run_after_skips_what_was_taken() {
        let mut ring = IqRing::new(10.0, Hz(RATE));
        ring.push(&block(0));
        ring.push(&block(1));
        let c = Hz(100_000_000);
        let first = ring.run_after(c, None, 600).unwrap();
        let end = |b: &IqBlock| Timestamp(b.t0.0 + b.samples.len() as u64 * MS);
        let second = ring.run_after(c, Some(end(&first)), 600).unwrap();
        assert_eq!(second.t0, Timestamp(600 * MS));
        assert_eq!(values(&second), (600..1200).collect::<Vec<u64>>());
        // 800 samples are left after the second: too few for another 1000, until more arrive.
        assert!(ring.run_after(c, Some(end(&second)), 1000).is_none());
        ring.push(&block(2));
        let third = ring.run_after(c, Some(end(&second)), 1000).unwrap();
        assert_eq!(values(&third), (1200..2200).collect::<Vec<u64>>());
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

    /// A ring of 10 s: blocks 0 and 1, a gap, then blocks 5 and 6.
    fn gapped() -> IqRing {
        let mut ring = IqRing::new(10.0, Hz(RATE));
        ring.push(&block(0));
        ring.push(&block(1));
        ring.push(&block(5));
        ring.push(&block(6));
        ring
    }

    #[test]
    fn a_gap_keeps_the_history_before_it() {
        let ring = gapped();
        assert_eq!(ring.earliest(), Some(Timestamp(0)));
        assert_eq!(ring.latest(), Some(Timestamp(6_999 * MS)));
        assert_eq!(ring.gaps(), 1);
        // Each side is served from its own segment.
        assert_eq!(ring.read(span(0, 2)).unwrap().len(), 2000);
        assert_eq!(ring.read(span(5, 7)).unwrap().len(), 2000);
        let got = ring.read(span_ns(1_500 * MS, 1_600 * MS)).unwrap();
        assert_eq!(got[0], Complex::new(1500.0, 0.0));
        let got = ring.read(span_ns(5_500 * MS, 5_600 * MS)).unwrap();
        assert_eq!(got[0], Complex::new(5500.0, 0.0));
        assert_eq!(ring.held_s(), 4.0);
    }

    #[test]
    fn a_read_across_a_gap_is_refused_with_its_bounds() {
        let ring = gapped();
        let gap = RingError::Gap {
            start_ns: Timestamp(1_500 * MS),
            end_ns: Timestamp(5_500 * MS),
            gap_start_ns: Timestamp(1_999 * MS),
            gap_end_ns: Timestamp(5_000 * MS),
        };
        assert_eq!(ring.read(span_ns(1_500 * MS, 5_500 * MS)), Err(gap));
        // One sample past the first segment's end is already across.
        assert!(matches!(
            ring.read(span_ns(1_500 * MS, 2_001 * MS)),
            Err(RingError::Gap { .. })
        ));
        // A start inside the gap has no samples.
        assert!(matches!(
            ring.read(span_ns(3_000 * MS, 3_100 * MS)),
            Err(RingError::Gap {
                gap_start_ns: Timestamp(t0),
                gap_end_ns: Timestamp(t1),
                ..
            }) if t0 == 1_999 * MS && t1 == 5_000 * MS
        ));
        // Too old and future keep their meaning against the whole ring.
        assert!(matches!(
            ring.read(span_ns(7_000 * MS, 7_100 * MS)),
            Err(RingError::Future { .. })
        ));
        let json = serde_json::to_value(RingError::Gap {
            start_ns: Timestamp(1),
            end_ns: Timestamp(9),
            gap_start_ns: Timestamp(2),
            gap_end_ns: Timestamp(5),
        })
        .unwrap();
        assert_eq!(json["cause"], "gap");
        assert_eq!(json["gap_start_ns"], 2);
        assert_eq!(json["gap_end_ns"], 5);
        assert_eq!(json["start_ns"], 1);
    }

    #[test]
    fn trimming_drops_whole_old_segments() {
        let mut ring = IqRing::new(3.0, Hz(RATE));
        ring.push(&block(0));
        ring.push(&block(5));
        ring.push(&block(6));
        assert_eq!(ring.gaps(), 1);
        // 3000 samples held: all three blocks fit.
        assert_eq!(ring.earliest(), Some(Timestamp(0)));
        ring.push(&block(7));
        // Block 0 went: its segment with it, and the ring reads as unbroken.
        assert_eq!(ring.gaps(), 0);
        assert_eq!(ring.earliest(), Some(Timestamp(5_000 * MS)));
        assert_eq!(ring.latest(), Some(Timestamp(7_999 * MS)));
        assert!(matches!(
            ring.read(span(0, 1)),
            Err(RingError::TooOld { .. })
        ));
        assert_eq!(ring.read(span(5, 8)).unwrap().len(), 3000);
        // A partly trimmed first segment keeps its own times.
        ring.push(&block_at(20_000 * MS, 0, 500));
        assert_eq!(ring.gaps(), 1);
        assert_eq!(ring.earliest(), Some(Timestamp(5_500 * MS)));
        assert_eq!(
            ring.read(span_ns(5_500 * MS, 5_600 * MS)).unwrap()[0],
            Complex::new(5500.0, 0.0)
        );
    }

    #[test]
    fn a_new_rate_or_centre_still_restarts_the_ring() {
        let mut ring = gapped();
        let moved = IqBlock {
            center_hz: Hz(433_000_000),
            ..block(8)
        };
        ring.push(&moved);
        assert_eq!(ring.gaps(), 0);
        assert_eq!(ring.earliest(), Some(Timestamp(8_000 * MS)));
        let mut ring = gapped();
        let mut b = block(8);
        b.sample_rate_hz = Hz(2 * RATE);
        ring.push(&b);
        assert_eq!(ring.gaps(), 0);
        assert_eq!(ring.earliest(), Some(Timestamp(8_000 * MS)));
    }

    #[test]
    fn the_segment_list_is_bounded() {
        let mut ring = IqRing::new(10_000.0, Hz(RATE));
        // A gap every block, far more blocks than the cap.
        for i in 0..(MAX_SEGMENTS as u64 * 3) {
            ring.push(&block_at(i * 10_000 * MS, i * 100, 100));
        }
        assert_eq!(ring.gaps(), MAX_SEGMENTS - 1);
        let last = MAX_SEGMENTS as u64 * 3 - 1;
        assert_eq!(ring.latest(), Some(Timestamp(last * 10_000 * MS + 99 * MS)));
        let first = last + 1 - MAX_SEGMENTS as u64;
        assert_eq!(ring.earliest(), Some(Timestamp(first * 10_000 * MS)));
        // The samples of dropped segments went with them.
        assert_eq!(
            ring.read(span_ns(first * 10_000 * MS, first * 10_000 * MS + 100 * MS))
                .unwrap()[0],
            Complex::new((first * 100) as f32, 0.0)
        );
    }

    #[test]
    fn run_after_never_straddles_a_gap() {
        let ring = gapped();
        let c = Hz(100_000_000);
        let end = |b: &IqBlock| Timestamp(b.t0.0 + b.samples.len() as u64 * MS);
        let first = ring.run_after(c, None, 1500).unwrap();
        assert_eq!(first.t0, Timestamp(0));
        assert_eq!(values(&first), (0..1500).collect::<Vec<u64>>());
        // 500 samples are left before the gap: too few for 1500, so the next
        // run is the next segment's first 1500, and the caller sees t0 jump.
        let next = ring.run_after(c, Some(end(&first)), 1500).unwrap();
        assert_eq!(next.t0, Timestamp(5_000 * MS));
        assert_eq!(values(&next), (5000..6500).collect::<Vec<u64>>());
        // What fits before the gap is still served from there.
        let fits = ring.run_after(c, Some(end(&first)), 500).unwrap();
        assert_eq!(fits.t0, Timestamp(1_500 * MS));
        // A time inside the gap continues in the next segment.
        let from_gap = ring.run_after(c, Some(Timestamp(3_000 * MS)), 100).unwrap();
        assert_eq!(from_gap.t0, Timestamp(5_000 * MS));
        // Nothing more past the last segment's end.
        assert!(
            ring.run_after(c, Some(Timestamp(6_500 * MS)), 1000)
                .is_none()
        );
    }

    #[test]
    fn held_s_counts_samples_and_run_s_the_newest_run() {
        // 10 s ring: 4 s, a 5 s pause, 4 s. Earliest to latest is about 13 s.
        let mut ring = IqRing::new(10.0, Hz(RATE));
        for i in 0..4 {
            ring.push(&block(i));
        }
        for i in 9..13 {
            ring.push(&block(i));
        }
        let span_s = (ring.latest().unwrap().0 - ring.earliest().unwrap().0) as f64 / 1e9;
        assert!(span_s > ring.capacity_seconds(), "{span_s}");
        assert_eq!(ring.gaps(), 1);
        assert_eq!(ring.held_s(), 8.0);
        assert!(ring.held_s() <= ring.capacity_seconds());
        assert_eq!(ring.run_s(), 4.0);
        assert_eq!(IqRing::new(10.0, Hz(RATE)).run_s(), 0.0);
        assert_eq!(IqRing::new(10.0, Hz(RATE)).held_s(), 0.0);
    }

    #[test]
    fn a_block_earlier_than_expected_restarts_the_ring() {
        let mut ring = IqRing::new(10.0, Hz(RATE));
        ring.push(&block(5));
        ring.push(&block(6));
        // Time went backwards: segments must stay ordered in time, so restart.
        ring.push(&block(2));
        assert_eq!(ring.gaps(), 0);
        assert_eq!(ring.earliest(), Some(Timestamp(2_000 * MS)));
        assert_eq!(ring.latest(), Some(Timestamp(2_999 * MS)));
        assert!(matches!(
            ring.read(span(5, 6)),
            Err(RingError::Future { .. })
        ));
    }
}
