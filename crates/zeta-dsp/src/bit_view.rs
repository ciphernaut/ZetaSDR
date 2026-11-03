//! Hypotheses about a bit stream with no known decoder.
//!
//! Reads `slicer`'s `bits` events into a sliding window and publishes what the window
//! suggests: run lengths, a frame period, sync words, the line code and CRCs that
//! check. [`hypotheses`] is the pure analysis of one window.

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Provenance, Span, StreamId, StreamKind, Timestamp};

use crate::correlate::BitsIn;
use crate::crc::{CRCS, CrcSpec, crc};
use crate::{Input, run_map_ending};

/// The stage kind, as graphs name it.
pub const BIT_VIEW_KIND: &str = "bit_view";

/// Default window, in bits.
pub const WINDOW_BITS_DEFAULT: u32 = 65_536;

/// Largest window, in bits: [`hypotheses`] runs on the async worker, so the window stays
/// where it keeps to [`HYPOTHESES_BUDGET_MS`]. Measured in a release build on the field host (2026-11-04,
/// `hypotheses_timing_on_64_and_256_kbit`): 64 kbit 19.6 ms framed, 8.6 ms random,
/// 9.5 ms repeating; 256 kbit 55.3 ms framed, 37.5 ms random, 47.7 ms repeating.
pub const WINDOW_BITS_MAX: u32 = 262_144;

/// Smallest window, in bits.
pub const WINDOW_BITS_MIN: u32 = 64;

/// What [`hypotheses`] takes on a [`WINDOW_BITS_DEFAULT`] window in a release build, at
/// most: the CRC search is bounded by [`CRC_MAX_FRAMES`] frames per hypothesis and
/// [`SYNC_CANDIDATES_MAX`] × ([`PERIOD_CANDIDATES_MAX`] + 1) hypotheses to keep it so.
/// Documented, not enforced by a timer.
pub const HYPOTHESES_BUDGET_MS: u64 = 200;

/// Most sync candidates listed.
pub const SYNC_CANDIDATES_MAX: usize = 5;

/// Most frame periods listed.
pub const PERIOD_CANDIDATES_MAX: usize = 3;

/// Least frames a CRC must check on to be listed.
pub const CRC_MIN_FRAMES: usize = 3;

/// Most frames tried per (sync, period) hypothesis.
pub const CRC_MAX_FRAMES: usize = 64;

/// Most CRC candidates listed.
pub const CRC_CANDIDATES_MAX: usize = 8;

/// The `period_bits` of a CRC candidate whose frames run from each sync to the next.
pub const NEXT_SYNC_PERIOD: u32 = 0;

/// Run lengths counted one by one; longer runs fall in the last bin.
pub const RUN_HISTOGRAM_MAX: usize = 32;

/// The sync word widths searched.
pub const SYNC_WORD_BITS: [u32; 3] = [16, 24, 32];

/// Shortest frame period considered, in bits.
pub const PERIOD_MIN_LAG: u32 = 8;

/// The fraction of the window the longest period considered spans.
const PERIOD_MAX_LAG_DIVISOR: usize = 4;

/// The word width the period search matches: the narrowest sync width.
const PERIOD_WORD_BITS: u32 = SYNC_WORD_BITS[0];

/// Later occurrences of the same word each occurrence is paired with in the period
/// search.
const PERIOD_NEIGHBOURS: usize = 8;

/// Least chains (below) a lag needs to be a period candidate.
const PERIOD_MIN_CHAINS: u32 = 2;

/// Least occurrences of a sync candidate, over what chance gives.
const SYNC_MIN_COUNT: f64 = 3.0;

/// Standard deviations of the chance count a sync candidate must clear.
const SYNC_CHANCE_SIGMAS: f64 = 6.0;

/// Longest run the Manchester score calls short.
const MANCHESTER_RUN_MAX: u32 = 2;

/// How much another line code must outscore `nrz` to be the likely one.
const ENCODING_MARGIN: f32 = 0.02;

const BYTE_BITS: usize = 8;

/// Bits per hex digit.
const HEX_DIGIT_BITS: u32 = 4;

/// The chance of either value of a fair random bit: a run of `k` has chance `2^-k`.
const FAIR_BIT: f64 = 0.5;

/// The window grows by `window_bits / PUBLISH_DIVISOR` between two events.
const PUBLISH_DIVISOR: usize = 4;

fn default_window_bits() -> u32 {
    WINDOW_BITS_DEFAULT
}

/// `bit_view` parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct BitViewParams {
    /// The window, in bits, 64..=262144.
    #[serde(default = "default_window_bits")]
    pub window_bits: u32,
}

/// The longest run of the window.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Longest {
    pub value: u8,
    pub length: u32,
}

/// The run-length histogram: `zeros[k]` and `ones[k]` count the runs of `k + 1` equal
/// bits, the last bin every run of [`RUN_HISTOGRAM_MAX`] or more.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Runs {
    pub zeros: Vec<u32>,
    pub ones: Vec<u32>,
    pub longest: Longest,
}

/// A frame period candidate: `lag` bits, `score` 0..1.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Period {
    pub lag: u32,
    pub score: f32,
}

/// A sync word candidate: `word` as `bits / 4` hex digits, `count` occurrences at any
/// bit offset, `mean_gap_bits` from one to the next.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SyncCandidate {
    pub word: String,
    pub bits: u32,
    pub count: u32,
    pub mean_gap_bits: f32,
}

/// Line code scores, 0..1, and the `likely` one.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Encoding {
    pub nrz: f32,
    pub nrzi: f32,
    pub manchester: f32,
    pub differential: f32,
    pub likely: String,
}

/// A CRC that checks on `frames` of the `checked` frames of `period_bits` cut at `sync`
/// ([`NEXT_SYNC_PERIOD`] for frames that run from each sync to the next).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CrcCandidate {
    pub crc: String,
    pub sync: String,
    pub period_bits: u32,
    pub frames: u32,
    pub checked: u32,
}

/// What one window of bits suggests; see [`hypotheses`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hypotheses {
    pub runs: Runs,
    pub period_bits: Vec<Period>,
    pub sync_candidates: Vec<SyncCandidate>,
    pub encoding: Encoding,
    pub crc_candidates: Vec<CrcCandidate>,
    pub summary: String,
}

/// The runs of `bits`: (value, length), in order.
fn run_lengths(bits: &[u8]) -> Vec<(u8, u32)> {
    let mut runs: Vec<(u8, u32)> = Vec::new();
    for &b in bits {
        match runs.last_mut() {
            Some((v, l)) if *v == b => *l += 1,
            _ => runs.push((b, 1)),
        }
    }
    runs
}

fn run_bin(length: u32) -> usize {
    (length as usize).min(RUN_HISTOGRAM_MAX) - 1
}

fn runs_of(runs: &[(u8, u32)]) -> Runs {
    let mut zeros = vec![0; RUN_HISTOGRAM_MAX];
    let mut ones = vec![0; RUN_HISTOGRAM_MAX];
    let mut longest = Longest::default();
    for &(v, l) in runs {
        if v == 0 {
            zeros[run_bin(l)] += 1;
        } else {
            ones[run_bin(l)] += 1;
        }
        if l > longest.length {
            longest = Longest {
                value: v,
                length: l,
            };
        }
    }
    Runs {
        zeros,
        ones,
        longest,
    }
}

/// The window's segments: `starts[k]..ends[k]` of the concatenated bits is segment `k`.
/// Words, runs, periods and frames never span two segments.
struct Segments {
    starts: Vec<usize>,
    ends: Vec<usize>,
}

impl Segments {
    fn of(segments: &[&[u8]]) -> Self {
        let mut starts = Vec::with_capacity(segments.len());
        let mut ends = Vec::with_capacity(segments.len());
        let mut at = 0;
        for s in segments {
            starts.push(at);
            at += s.len();
            ends.push(at);
        }
        Self { starts, ends }
    }

    /// The segment holding bit `p` of the concatenation.
    fn index_of(&self, p: usize) -> usize {
        self.starts.partition_point(|&s| s <= p) - 1
    }

    /// One past the last bit of the segment holding bit `p`.
    fn end_of(&self, p: usize) -> usize {
        self.ends[self.index_of(p)]
    }
}

/// How much the run lengths of `segments` look like those of fair random bits: 1 less
/// the total variation distance between the share of runs of each length (zeros and
/// ones together, binned as [`Runs`], each segment's runs its own) and the geometric
/// `2^-length` that random bits give, the last bin taking the tail. 0 with no runs.
fn flatness(segments: &[Vec<u8>]) -> f32 {
    let mut hist = [0u32; RUN_HISTOGRAM_MAX];
    let mut total = 0usize;
    for s in segments {
        for (_, l) in run_lengths(s) {
            hist[run_bin(l)] += 1;
            total += 1;
        }
    }
    if total == 0 {
        return 0.0;
    }
    let total = total as f64;
    let tv: f64 = hist
        .iter()
        .enumerate()
        .map(|(k, &c)| {
            let expected = FAIR_BIT.powi(k.min(RUN_HISTOGRAM_MAX - 2) as i32 + 1);
            (c as f64 / total - expected).abs()
        })
        .sum();
    (1.0 - tv / 2.0).clamp(0.0, 1.0) as f32
}

/// The line code scores of `segments`: `nrz` the [`flatness`] of the bits as they are,
/// `nrzi` that of the bits decoded as NRZI (a bit is a change of level), `differential`
/// that of the bits XORed with the previous one (on two levels the same decoding as
/// NRZI, so the same score), `manchester` the share of runs of at most
/// [`MANCHESTER_RUN_MAX`] bits (1 when no run exceeds it). `likely` is the highest,
/// `nrz` unless another beats it by more than [`ENCODING_MARGIN`] (random-looking bits
/// read about the same under every decoding), then the first of `manchester`, `nrzi`,
/// `differential` on a tie. Each decoding is taken within a segment.
fn encoding_of(segments: &[&[u8]], runs: &[(u8, u32)]) -> Encoding {
    let plain: Vec<Vec<u8>> = segments.iter().map(|s| s.to_vec()).collect();
    let changes: Vec<Vec<u8>> = segments
        .iter()
        .map(|s| s.windows(2).map(|w| u8::from(w[0] != w[1])).collect())
        .collect();
    let xored: Vec<Vec<u8>> = segments
        .iter()
        .map(|s| s.windows(2).map(|w| w[0] ^ w[1]).collect())
        .collect();
    let nrz = flatness(&plain);
    let nrzi = flatness(&changes);
    let differential = flatness(&xored);
    let manchester = if runs.is_empty() {
        0.0
    } else {
        runs.iter().filter(|r| r.1 <= MANCHESTER_RUN_MAX).count() as f32 / runs.len() as f32
    };
    let mut likely = ("nrz", nrz + ENCODING_MARGIN);
    for c in [
        ("manchester", manchester),
        ("nrzi", nrzi),
        ("differential", differential),
    ] {
        if c.1 > likely.1 {
            likely = c;
        }
    }
    Encoding {
        nrz,
        nrzi,
        manchester,
        differential,
        likely: likely.0.to_string(),
    }
}

fn mask(width: u32) -> u64 {
    u64::MAX >> (u64::BITS - width)
}

/// The `width`-bit words of the segments, MSB first, at every bit offset where the
/// word lies inside one segment: `values[i]` starts at bit `pos[i]` of the
/// concatenation.
struct Words {
    values: Vec<u64>,
    pos: Vec<usize>,
}

fn words(segments: &[&[u8]], width: u32) -> Words {
    let w = width as usize;
    let m = mask(width);
    let mut values = Vec::new();
    let mut pos = Vec::new();
    let mut at = 0;
    for s in segments {
        let mut v = 0u64;
        for (i, &b) in s.iter().enumerate() {
            v = ((v << 1) | u64::from(b)) & m;
            if i + 1 >= w {
                values.push(v);
                pos.push(at + i + 1 - w);
            }
        }
        at += s.len();
    }
    Words { values, pos }
}

/// A run (all zeros, all ones) or one of the two alternations, which every lag and
/// every offset repeat and which say nothing about framing.
fn uninformative(v: u64, width: u32) -> bool {
    let m = mask(width);
    let alt = m / 3; // 0101...
    v == 0 || v == m || v == alt || v == (alt << 1) & m
}

/// Whether the `short_w`-bit `short` lies inside the `long_w`-bit `long`.
fn contains(long: u64, long_w: u32, short: u64, short_w: u32) -> bool {
    (0..=long_w - short_w).any(|s| (long >> s) & mask(short_w) == short)
}

/// Frame period candidates by word recurrence. A lone sync word in a random payload is
/// lost in the ±1 autocorrelation (16 agreeing bits per period against the payload's
/// noise), so the autocorrelation is taken over word identity instead: a chain at lag
/// `L` is an offset whose [`PERIOD_WORD_BITS`]-bit word (not [`uninformative`]) recurs
/// at `+L` and `+2L` in the same segment, which random bits give about once per 2^32
/// offsets. Each occurrence is paired with its next [`PERIOD_NEIGHBOURS`] occurrences,
/// lags [`PERIOD_MIN_LAG`]..=(the longest segment)/4. `score` is the chains at `L`
/// over the chains segments repeating every `L` bits would hold (the sum over the
/// segments of `floor((len - 16) / L) - 1`, the zero-lag count scaled to the lag),
/// capped at 1. A lag needs [`PERIOD_MIN_CHAINS`]; the strongest are listed (the
/// shorter on a tie), each skipping a multiple of one already listed, at most
/// [`PERIOD_CANDIDATES_MAX`].
fn periods(segs: &Segments, w16: &Words) -> Vec<Period> {
    let lens: Vec<usize> = segs
        .starts
        .iter()
        .zip(&segs.ends)
        .map(|(a, b)| b - a)
        .collect();
    let max_lag = lens.iter().copied().max().unwrap_or(0) / PERIOD_MAX_LAG_DIVISOR;
    if max_lag < PERIOD_MIN_LAG as usize {
        return Vec::new();
    }
    let mut keyed: Vec<u64> = w16
        .values
        .iter()
        .zip(&w16.pos)
        .filter(|(v, _)| !uninformative(**v, PERIOD_WORD_BITS))
        .map(|(&v, &p)| (v << u32::BITS) | p as u64)
        .collect();
    keyed.sort_unstable();
    let mut chains = vec![0u32; max_lag + 1];
    let mut start = 0;
    while start < keyed.len() {
        let word = keyed[start] >> u32::BITS;
        let end = start + keyed[start..].partition_point(|k| k >> u32::BITS == word);
        let pos: Vec<usize> = keyed[start..end]
            .iter()
            .map(|k| (k & u64::from(u32::MAX)) as usize)
            .collect();
        for j in 0..pos.len() {
            let seg_end = segs.end_of(pos[j]);
            for k in j + 1..pos.len().min(j + 1 + PERIOD_NEIGHBOURS) {
                let lag = pos[k] - pos[j];
                if lag > max_lag || pos[k] >= seg_end {
                    break;
                }
                let third = pos[k] + lag;
                if lag >= PERIOD_MIN_LAG as usize
                    && third < seg_end
                    && pos[k + 1..].binary_search(&third).is_ok()
                {
                    chains[lag] += 1;
                }
            }
        }
        start = end;
    }
    let mut scored: Vec<Period> = chains
        .iter()
        .enumerate()
        .filter(|&(_, &c)| c >= PERIOD_MIN_CHAINS)
        .map(|(lag, &c)| {
            let possible: usize = lens
                .iter()
                .map(|&n| (n.saturating_sub(PERIOD_WORD_BITS as usize) / lag).saturating_sub(1))
                .sum::<usize>()
                .max(1);
            Period {
                lag: lag as u32,
                score: (c as f32 / possible as f32).min(1.0),
            }
        })
        .collect();
    scored.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.lag.cmp(&b.lag)));
    let mut out: Vec<Period> = Vec::new();
    for p in scored {
        if out.len() == PERIOD_CANDIDATES_MAX {
            break;
        }
        if out.iter().all(|k| p.lag % k.lag != 0) {
            out.push(p);
        }
    }
    out
}

/// A sync candidate with its occurrences (bit offsets of the concatenation, ascending)
/// and the segment of each.
struct Sync {
    value: u64,
    width: u32,
    positions: Vec<usize>,
    segments: Vec<usize>,
}

impl Sync {
    fn word(&self) -> String {
        format!(
            "{:0w$x}",
            self.value,
            w = (self.width / HEX_DIGIT_BITS) as usize
        )
    }

    /// The mean gap from one occurrence to the next within a segment; 0 when no
    /// segment holds two.
    fn mean_gap(&self) -> f32 {
        let (mut span, mut gaps) = (0usize, 0usize);
        for k in 1..self.positions.len() {
            if self.segments[k] == self.segments[k - 1] {
                span += self.positions[k] - self.positions[k - 1];
                gaps += 1;
            }
        }
        if gaps == 0 {
            0.0
        } else {
            span as f32 / gaps as f32
        }
    }
}

/// Sync candidates: for each width of [`SYNC_WORD_BITS`], the words (not
/// [`uninformative`]) at every bit offset of every segment that occur, over all the
/// segments, at least `chance + 6 sqrt(chance) + 3` times, `chance` being the words of
/// that width over `2^width`, what random bits give; listed by count, then width
/// (wider first), a word skipped when a listed wider word contains it (its occurrences
/// are that word's), at most [`SYNC_CANDIDATES_MAX`].
fn syncs(segs: &Segments, by_width: &[(u32, Words)]) -> Vec<Sync> {
    let mut pool: Vec<(u32, u32, u64)> = Vec::new(); // (count, width, value)
    for (width, ws) in by_width {
        if ws.values.is_empty() {
            continue;
        }
        let chance = ws.values.len() as f64 / 2f64.powi(*width as i32);
        let need = chance + SYNC_CHANCE_SIGMAS * chance.sqrt() + SYNC_MIN_COUNT;
        let mut sorted: Vec<u64> = ws
            .values
            .iter()
            .copied()
            .filter(|&v| !uninformative(v, *width))
            .collect();
        sorted.sort_unstable();
        for group in sorted.chunk_by(|a, b| a == b) {
            if group.len() as f64 >= need {
                pool.push((group.len() as u32, *width, group[0]));
            }
        }
    }
    pool.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    let mut kept: Vec<(u32, u64)> = Vec::new();
    for &(_, width, value) in &pool {
        if kept.len() == SYNC_CANDIDATES_MAX {
            break;
        }
        if kept
            .iter()
            .any(|&(kw, kv)| kw > width && contains(kv, kw, value, width))
        {
            continue;
        }
        kept.push((width, value));
    }
    kept.into_iter()
        .map(|(width, value)| {
            let ws = &by_width.iter().find(|(w, _)| *w == width).unwrap().1;
            let positions: Vec<usize> = ws
                .values
                .iter()
                .zip(&ws.pos)
                .filter(|&(&v, _)| v == value)
                .map(|(_, &p)| p)
                .collect();
            let segments = positions.iter().map(|&p| segs.index_of(p)).collect();
            Sync {
                value,
                width,
                positions,
                segments,
            }
        })
        .collect()
}

/// `bits` packed MSB first into bytes; with `reflect` each byte bit-reversed (the
/// bytes of an LSB-first stream).
fn pack(bits: &[u8], reflect: bool) -> Vec<u8> {
    bits.chunks(BYTE_BITS)
        .map(|c| {
            let b = c.iter().fold(0u8, |acc, &v| (acc << 1) | v);
            if reflect { b.reverse_bits() } else { b }
        })
        .collect()
}

fn value_of(bits: &[u8]) -> u64 {
    bits.iter().fold(0u64, |acc, &v| (acc << 1) | u64::from(v))
}

/// The frames of `period` bits cut at the occurrences of `sync` that fit inside their
/// segment, at most [`CRC_MAX_FRAMES`].
fn period_frames<'a>(bits: &'a [u8], segs: &Segments, sync: &Sync, period: usize) -> Vec<&'a [u8]> {
    sync.positions
        .iter()
        .zip(&sync.segments)
        .filter(|&(&s, &k)| s + period <= segs.ends[k])
        .take(CRC_MAX_FRAMES)
        .map(|(&s, _)| &bits[s..s + period])
        .collect()
}

/// The frames from each occurrence of `sync` to the next in the same segment, for
/// frames of varying length that abut, at most [`CRC_MAX_FRAMES`].
fn next_sync_frames<'a>(bits: &'a [u8], sync: &Sync) -> Vec<&'a [u8]> {
    (1..sync.positions.len())
        .filter(|&k| sync.segments[k] == sync.segments[k - 1])
        .take(CRC_MAX_FRAMES)
        .map(|k| &bits[sync.positions[k - 1]..sync.positions[k]])
        .collect()
}

/// The CRCs that check on `frames` cut at `sync` (listed with `period_bits`): a frame's
/// last `width` bits are the CRC, the bits between the sync and it the data, packed MSB
/// first; a frame whose data is not whole bytes (or empty) is not tried for that spec.
/// Tried as sent and LSB first (every data byte bit-reversed, the CRC field read
/// reversed); a spec is listed when it checks on at least [`CRC_MIN_FRAMES`], with
/// `checked` the frames it was tried on.
fn crcs_for(frames: &[&[u8]], sync: &Sync, period_bits: u32, out: &mut Vec<CrcCandidate>) {
    if frames.len() < CRC_MIN_FRAMES {
        return;
    }
    let sync_w = sync.width as usize;
    for spec in CRCS {
        let width = spec.width as usize;
        let (mut plain, mut lsb, mut checked) = (0u32, 0u32, 0u32);
        for f in frames {
            let Some(data_bits) = f.len().checked_sub(sync_w + width) else {
                continue;
            };
            if data_bits == 0 || data_bits % BYTE_BITS != 0 {
                continue;
            }
            checked += 1;
            let data = &f[sync_w..sync_w + data_bits];
            let field = value_of(&f[sync_w + data_bits..]);
            if crc(spec, &pack(data, false)) == field {
                plain += 1;
            }
            if crc(spec, &pack(data, true)) == reflect(field, spec) {
                lsb += 1;
            }
        }
        for (name, passed) in [
            (spec.name.to_string(), plain),
            (format!("{} lsb-first", spec.name), lsb),
        ] {
            if passed as usize >= CRC_MIN_FRAMES {
                out.push(CrcCandidate {
                    crc: name,
                    sync: sync.word(),
                    period_bits,
                    frames: passed,
                    checked,
                });
            }
        }
    }
}

fn reflect(v: u64, spec: &CrcSpec) -> u64 {
    v.reverse_bits() >> (u64::BITS - u32::from(spec.width))
}

/// What one window of bits suggests (see each field's type), the window given as its
/// segments (the stretches of contiguous bits, one per capture of a set): the
/// run-length histogram, the frame periods (see `periods`), the sync candidates, the
/// line code scores and the CRC candidates. No word, run, period or frame spans two
/// segments; the counts sum over all of them, and a gap or a next-sync frame is taken
/// within one. The CRC search tries each sync candidate with frames of each listed
/// period and with frames that run from each occurrence to the next (listed with
/// `period_bits` 0), so at most [`SYNC_CANDIDATES_MAX`] × ([`PERIOD_CANDIDATES_MAX`] + 1)
/// hypotheses of [`CRC_MAX_FRAMES`] frames, within [`HYPOTHESES_BUDGET_MS`] on a
/// [`WINDOW_BITS_DEFAULT`] window; the most frames first, at most
/// [`CRC_CANDIDATES_MAX`]. `summary` reads `sync 0x<word> ×<count> every <gap> bits,
/// <likely>, <crc> <frames>/<checked>` for the first sync and CRC candidates (the CRC
/// part only when one checks), or `no sync candidate`.
pub fn hypotheses(segments: &[&[u8]]) -> Hypotheses {
    let segs = Segments::of(segments);
    let bits: Vec<u8> = segments.concat();
    let runs: Vec<(u8, u32)> = segments.iter().flat_map(|s| run_lengths(s)).collect();
    let encoding = encoding_of(segments, &runs);
    let by_width: Vec<(u32, Words)> = SYNC_WORD_BITS
        .iter()
        .map(|&w| (w, words(segments, w)))
        .collect();
    let w16 = &by_width
        .iter()
        .find(|(w, _)| *w == PERIOD_WORD_BITS)
        .unwrap()
        .1;
    let period_bits = periods(&segs, w16);
    let syncs = syncs(&segs, &by_width);
    let mut crc_candidates = Vec::new();
    for s in &syncs {
        for p in &period_bits {
            crcs_for(
                &period_frames(&bits, &segs, s, p.lag as usize),
                s,
                p.lag,
                &mut crc_candidates,
            );
        }
        crcs_for(
            &next_sync_frames(&bits, s),
            s,
            NEXT_SYNC_PERIOD,
            &mut crc_candidates,
        );
    }
    crc_candidates.sort_by_key(|c| std::cmp::Reverse(c.frames));
    crc_candidates.truncate(CRC_CANDIDATES_MAX);
    let sync_candidates: Vec<SyncCandidate> = syncs
        .iter()
        .map(|s| SyncCandidate {
            word: s.word(),
            bits: s.width,
            count: s.positions.len() as u32,
            mean_gap_bits: s.mean_gap(),
        })
        .collect();
    let summary = match sync_candidates.first() {
        None => "no sync candidate".to_string(),
        Some(s) => {
            let mut parts = vec![
                format!(
                    "sync 0x{} ×{} every {:.0} bits",
                    s.word, s.count, s.mean_gap_bits
                ),
                encoding.likely.clone(),
            ];
            if let Some(c) = crc_candidates.first() {
                parts.push(format!("{} {}/{}", c.crc, c.frames, c.checked));
            }
            parts.join(", ")
        }
    };
    Hypotheses {
        runs: runs_of(&runs),
        period_bits,
        sync_candidates,
        encoding,
        crc_candidates,
        summary,
    }
}

/// Where one `bits` event's bits sit in the window (positions count the bits pushed
/// since the window opened), for the window's times.
struct Piece {
    start: u64,
    t_ns: u64,
    bit_ns: f64,
}

impl Piece {
    fn time(&self, at: u64) -> u64 {
        self.t_ns + ((at - self.start) as f64 * self.bit_ns) as u64
    }
}

/// The window over the segments of a set.
struct Window {
    size: usize,
    bits: VecDeque<u8>,
    pieces: VecDeque<Piece>,
    /// Where each segment starts, in window positions; the first may lie before the
    /// window's first bit.
    segments: VecDeque<u64>,
    /// The position one past the last bit.
    end: u64,
    /// The slicer's `offset_bits` one past the current segment's last bit; `None` when
    /// the next `bits` event starts a segment (at the first and after a lag).
    seg_end: Option<u64>,
    /// Bits added toward the next quarter: the publishing cadence only.
    grown: usize,
    /// Bits added since the last event, which a segment's or the input's end publishes.
    unpublished: usize,
    prov: Provenance,
}

impl Window {
    fn new(size: usize, prov: Provenance) -> Self {
        Self {
            size,
            bits: VecDeque::with_capacity(size),
            pieces: VecDeque::new(),
            segments: VecDeque::new(),
            end: 0,
            seg_end: None,
            grown: 0,
            unpublished: 0,
            prov,
        }
    }

    fn start(&self) -> u64 {
        self.end - self.bits.len() as u64
    }

    /// The time of the window's first bit.
    fn first_t(&self) -> u64 {
        let first = &self.pieces[0];
        first.time(self.start().max(first.start))
    }

    fn push(&mut self, b: &BitsIn) {
        if self.seg_end != Some(b.offset_bits) {
            self.segments.push_back(self.end);
        }
        self.pieces.push_back(Piece {
            start: self.end,
            t_ns: b.t_ns,
            bit_ns: b.bit_ns,
        });
        self.bits.extend(&b.bits);
        self.end += b.bits.len() as u64;
        self.seg_end = Some(b.end());
        let excess = self.bits.len().saturating_sub(self.size);
        self.bits.drain(..excess);
        self.grown += b.bits.len();
        self.unpublished += b.bits.len();
        self.prov = b.prov.clone();
        let start = self.start();
        while self.pieces.len() > 1 && self.pieces[1].start <= start {
            self.pieces.pop_front();
        }
        while self.segments.len() > 1 && self.segments[1] <= start {
            self.segments.pop_front();
        }
    }

    /// The window's `hypotheses` event, noted to the job; nothing left unpublished.
    fn publish(&mut self, events: &StreamId, ctx: &StageContext) -> Frame {
        self.unpublished = 0;
        let start = self.start();
        let t0 = self.first_t();
        let t1 = self.pieces.back().unwrap().time(self.end);
        let mut cuts: Vec<usize> = self
            .segments
            .iter()
            .map(|&s| (s.max(start) - start) as usize)
            .collect();
        let bits = self.bits.make_contiguous();
        cuts.push(bits.len());
        let segments: Vec<&[u8]> = cuts.windows(2).map(|c| &bits[c[0]..c[1]]).collect();
        let h = hypotheses(&segments);
        let mut ev = serde_json::to_value(&h).expect("hypotheses serialise");
        let obj = ev.as_object_mut().expect("an object");
        obj.insert("protocol".into(), "hypotheses".into());
        obj.insert("window_bits".into(), bits.len().into());
        obj.insert("t_ns".into(), t0.into());
        ctx.note(serde_json::json!({ "bit_view": ev }));
        let mut prov = self.prov.clone();
        prov.span = Span {
            start_ns: Timestamp(t0),
            end_ns: Timestamp(t1.max(t0)),
        };
        Frame::new(events.clone(), prov, Payload::Event(ev))
    }
}

/// Keeps the last `window_bits` bits of the `bits` segments of a set (a `bits` event
/// whose `offset_bits` is not the previous one's end, or the first after a lag, starts
/// a new segment, one per capture of the set) and publishes a `hypotheses` event
/// ([`Hypotheses`]' fields over the window's segments, `protocol`, `window_bits` the
/// bits it covers and `t_ns` its first bit's time) each time the window has grown by a
/// quarter of `window_bits` since the last, and once more when a segment or the input
/// ends with growth unpublished. The window clears only by sliding past `window_bits`
/// and when a looping replay restarts (a segment starting at `offset_bits` 0 at or
/// before the window's first bit's time), so a pass is never counted twice. Notes the
/// latest under `bit_view`: the set-wide view.
pub struct BitViewStage {
    spec: StageSpec,
}

impl BitViewStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: BIT_VIEW_KIND,
                inputs: vec![StreamKind::Event],
                outputs: vec![StreamKind::Event],
                params_schema: schemars::schema_for!(BitViewParams),
            },
        }
    }
}

impl Default for BitViewStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for BitViewStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    fn check_params(&self, params: &serde_json::Value) -> Result<(), String> {
        let p: BitViewParams = serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
        if !(WINDOW_BITS_MIN..=WINDOW_BITS_MAX).contains(&p.window_bits) {
            return Err(format!(
                "window_bits: {WINDOW_BITS_MIN}..={WINDOW_BITS_MAX}"
            ));
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
        let p: BitViewParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("{BIT_VIEW_KIND} params: {e}")))?;
        let [events] = <[StreamId; 1]>::try_from(outputs)
            .map_err(|o| StageError::Failed(format!("expected 1 output, got {}", o.len())))?;
        let size = p.window_bits as usize;
        let quarter = size / PUBLISH_DIVISOR;
        let mut win: Option<Window> = None;
        run_map_ending(BIT_VIEW_KIND, inputs, bus, cancel, move |input| {
            let mut out = Vec::new();
            let b = match input {
                Some(Input::Frame(f)) => match BitsIn::parse(BIT_VIEW_KIND, f)? {
                    Some(b) => Some(b),
                    None => return Ok(out),
                },
                Some(Input::Lagged { .. }) | None => None,
            };
            let Some(b) = b else {
                // A lag or the input's end: publish what is pending; after a lag the
                // next bits start a new segment.
                if let Some(w) = win.as_mut() {
                    if w.unpublished > 0 {
                        out.push(w.publish(&events, &ctx));
                    }
                    w.seg_end = None;
                }
                return Ok(out);
            };
            if b.bits.is_empty() {
                return Ok(out);
            }
            let w = win.get_or_insert_with(|| Window::new(size, b.prov.clone()));
            if !w.bits.is_empty() && w.seg_end != Some(b.offset_bits) {
                if w.unpublished > 0 {
                    out.push(w.publish(&events, &ctx));
                }
                if b.offset_bits == 0 && b.t_ns <= w.first_t() {
                    *w = Window::new(size, b.prov.clone());
                }
            }
            w.push(&b);
            if w.grown >= quarter {
                w.grown %= quarter;
                out.push(w.publish(&events, &ctx));
            }
            Ok(out)
        })
        .await
    }
}

/// Bit streams and a stage runner shared by this module's and `correlate`'s tests.
#[cfg(test)]
pub(crate) mod test_streams {
    use std::sync::{Arc, Mutex};

    use serde_json::Value;
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;
    use zeta_core::stage::{Stage, StageContext};
    use zeta_core::stream::{Frame, Payload, StreamBus};
    use zeta_core::types::{JobId, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};

    use crate::crc::{CRC16_CCITT, crc};
    use crate::slicer::bits_event;

    /// The frame sync `framed_stream` plants.
    pub const SYNC: u16 = 0x2DD4;
    /// The bit rate of the test streams.
    pub const BAUD: u32 = 4800;
    /// The first bit's time.
    pub const T0: u64 = 1_000_000_000;

    /// Deterministic random bits.
    pub fn random_bits(n: usize, seed: u64) -> Vec<u8> {
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

    fn push_msb(out: &mut Vec<u8>, v: u64, bits: u32) {
        out.extend((0..bits).rev().map(|b| ((v >> b) & 1) as u8));
    }

    /// `frames` frames of [`SYNC`], a random body of `body_bytes` and the CRC-16-CCITT
    /// of the body, MSB first, concatenated: the period is `16 + 8 × body_bytes + 16`.
    /// A body that would put [`SYNC`] anywhere but at a frame start is drawn again, so
    /// the sync occurs exactly `frames` times.
    pub fn framed_stream(frames: usize, body_bytes: usize, seed: u64) -> Vec<u8> {
        framed_stream_of(&vec![body_bytes; frames], seed)
    }

    /// [`framed_stream`] with a body of `body_bytes[k]` in frame `k`.
    pub fn framed_stream_of(body_bytes: &[usize], seed: u64) -> Vec<u8> {
        const SYNC_W: usize = 16;
        let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut out = Vec::new();
        let mut made = 0;
        while made < body_bytes.len() {
            let body: Vec<u8> = (0..body_bytes[made])
                .map(|_| {
                    s ^= s << 13;
                    s ^= s >> 7;
                    s ^= s << 17;
                    (s >> 24) as u8
                })
                .collect();
            let start = out.len();
            push_msb(&mut out, u64::from(SYNC), SYNC_W as u32);
            for &b in &body {
                push_msb(&mut out, u64::from(b), 8);
            }
            push_msb(&mut out, crc(&CRC16_CCITT, &body), 16);
            let stray = (start.saturating_sub(SYNC_W - 1)..=out.len() - SYNC_W)
                .filter(|&i| i != start)
                .any(|i| {
                    out[i..i + SYNC_W]
                        .iter()
                        .fold(0u16, |acc, &v| (acc << 1) | u16::from(v))
                        == SYNC
                });
            if stray {
                out.truncate(start);
            } else {
                made += 1;
            }
        }
        out
    }

    /// `bits` as `bits` event frames cut at `splits` (ascending), one segment from
    /// [`T0`] at [`BAUD`].
    pub fn bits_frames(bits: &[u8], splits: &[usize]) -> Vec<Frame> {
        bits_frames_at(bits, splits, T0)
    }

    /// [`bits_frames`] with the segment's first bit at `t0`.
    pub fn bits_frames_at(bits: &[u8], splits: &[usize], t0: u64) -> Vec<Frame> {
        let id = StreamId::new();
        let mut cuts = vec![0];
        cuts.extend_from_slice(splits);
        cuts.push(bits.len());
        let t = |k: usize| t0 + (k as u128 * 1_000_000_000 / BAUD as u128) as u64;
        cuts.windows(2)
            .map(|w| {
                Frame::new(
                    id.clone(),
                    Provenance {
                        source: SourceId("s".into()),
                        job: Some(JobId("job".into())),
                        channel: None,
                        span: Span {
                            start_ns: Timestamp(t(w[0])),
                            end_ns: Timestamp(t(w[1])),
                        },
                    },
                    Payload::Event(bits_event(BAUD, &bits[w[0]..w[1]], w[0] as u64, t(w[0]))),
                )
            })
            .collect()
    }

    /// Run `stage` over `input`: the event payloads it published and the merged notes.
    pub async fn run_events(
        stage: &dyn Stage,
        params: Value,
        input: Vec<Frame>,
    ) -> (Vec<Value>, Value) {
        let bus = Arc::new(StreamBus::new(4096));
        let ev = bus.register(
            StreamKind::Event,
            Provenance {
                source: SourceId("s".into()),
                job: None,
                channel: None,
                span: Span {
                    start_ns: Timestamp(0),
                    end_ns: Timestamp(0),
                },
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
        stage
            .run(
                params,
                vec![rx],
                vec![ev],
                bus.clone(),
                CancellationToken::new(),
                ctx,
            )
            .await
            .unwrap();
        let mut events = Vec::new();
        while let Ok(f) = rx_ev.try_recv() {
            if let Payload::Event(v) = f.payload {
                events.push(v);
            }
        }
        let mut context = serde_json::Map::new();
        for n in notes.lock().unwrap().iter() {
            for (k, v) in n.as_object().unwrap() {
                context.insert(k.clone(), v.clone());
            }
        }
        (events, Value::Object(context))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use serde_json::json;
    use zeta_core::stage::Stage;
    use zeta_core::stream::{Frame, Payload};

    use super::test_streams::{
        T0, bits_frames, bits_frames_at, framed_stream, framed_stream_of, random_bits, run_events,
    };

    const SECOND_NS: u64 = 1_000_000_000;
    use super::{
        BitViewStage, CRC_CANDIDATES_MAX, CRC_MAX_FRAMES, CRC_MIN_FRAMES, NEXT_SYNC_PERIOD,
        hypotheses,
    };

    #[test]
    fn a_framed_stream_gives_its_sync_period_and_crc() {
        let h = hypotheses(&[&framed_stream(14, 136, 1)]);
        assert_eq!(h.sync_candidates[0].word, "2dd4", "{:?}", h.sync_candidates);
        assert!(
            h.period_bits.iter().any(|p| p.lag == 1120),
            "{:?}",
            h.period_bits
        );
        assert!(
            h.crc_candidates
                .iter()
                .any(|c| c.crc == "crc16-ccitt" && c.frames >= 12),
            "{:?}",
            h.crc_candidates
        );
        assert!(
            h.summary.starts_with("sync 0x2dd4 ×14 every 1120 bits"),
            "{}",
            h.summary
        );
    }

    #[test]
    fn a_manchester_stream_scores_manchester_highest() {
        let bits: Vec<u8> = random_bits(8192, 5)
            .into_iter()
            .flat_map(|b| [b, b ^ 1])
            .collect();
        let h = hypotheses(&[&bits]);
        assert_eq!(h.encoding.likely, "manchester", "{:?}", h.encoding);
    }

    #[test]
    fn random_bits_give_no_sync_candidate_and_say_so() {
        let h = hypotheses(&[&random_bits(65_536, 11)]);
        assert!(h.crc_candidates.is_empty(), "{:?}", h.crc_candidates);
        assert!(
            h.period_bits.iter().all(|p| p.score <= 0.5),
            "{:?}",
            h.period_bits
        );
        assert!(
            h.summary == "no sync candidate"
                || (h.summary.starts_with("sync") && h.sync_candidates[0].count < 4),
            "{}",
            h.summary
        );
    }

    #[tokio::test]
    async fn the_stage_publishes_at_each_quarter_and_notes_the_latest() {
        let bits = random_bits(10_000, 9);
        let splits: Vec<usize> = (1..10).map(|k| k * 1000).collect();
        let (events, context) = run_events(
            &BitViewStage::new(),
            json!({"window_bits": 4096}),
            bits_frames(&bits, &splits),
        )
        .await;
        assert_eq!(events.len(), 9);
        assert!(events.windows(2).all(|w| w[0] != w[1]));
        assert!(events.iter().all(|e| e["protocol"] == "hypotheses"));
        assert!(
            events
                .iter()
                .all(|e| e["window_bits"].as_u64().unwrap() <= 4096)
        );
        assert_eq!(context["bit_view"], *events.last().unwrap());
    }

    #[tokio::test]
    async fn a_sync_event_among_the_bits_changes_nothing() {
        let bits = random_bits(10_000, 9);
        let splits: Vec<usize> = (1..10).map(|k| k * 1000).collect();
        let plain = bits_frames(&bits, &splits);
        // A correlator's `sync` event after every bits event, as `correlate` forwards them.
        let mut mixed = Vec::new();
        for f in &plain {
            mixed.push(f.clone());
            let mut sync = f.clone();
            sync.payload = Payload::Event(json!({
                "protocol": "sync", "offset_bits": 0, "errors": 0, "t_ns": 0, "summary": "sync"
            }));
            mixed.push(sync);
        }
        let params = json!({"window_bits": 4096});
        let (want, _) = run_events(&BitViewStage::new(), params.clone(), plain).await;
        let (got, _) = run_events(&BitViewStage::new(), params, mixed).await;
        assert_eq!(got, want);
    }

    #[tokio::test]
    async fn a_seam_starts_a_segment_and_a_restart_clears_the_window() {
        let bits = random_bits(3000, 4);
        // The next capture of a set: offset_bits restarts at 0, later in time.
        let mut input = bits_frames(&bits[..2000], &[1000]);
        input.extend(bits_frames_at(&bits[2000..], &[], T0 + 10 * SECOND_NS));
        let (events, _) = run_events(&BitViewStage::new(), json!({}), input).await;
        assert_eq!(events.last().unwrap()["window_bits"], 3000);
        // A looping replay back at the set's start: the window clears.
        let mut input = bits_frames(&bits[..2000], &[1000]);
        input.extend(bits_frames(&bits[2000..], &[]));
        let (events, _) = run_events(&BitViewStage::new(), json!({}), input).await;
        assert_eq!(events.last().unwrap()["window_bits"], 1000);
    }

    /// `segments` segments of two framed frames (a 30-byte body) each.
    fn set_of_segments(segments: usize) -> Vec<Vec<u8>> {
        (0..segments)
            .map(|k| framed_stream(2, 30, 100 + k as u64))
            .collect()
    }

    /// The segments as `bits` frames, two per segment, the segments ten seconds apart.
    fn set_frames(segments: &[Vec<u8>]) -> Vec<Frame> {
        segments
            .iter()
            .enumerate()
            .flat_map(|(k, s)| bits_frames_at(s, &[s.len() / 2], T0 + 10 * k as u64 * SECOND_NS))
            .collect()
    }

    #[tokio::test]
    async fn the_window_spans_the_segments_of_a_set() {
        let segments = set_of_segments(6);
        for s in &segments {
            let alone = hypotheses(&[s]);
            assert!(
                alone.sync_candidates.is_empty(),
                "{:?}",
                alone.sync_candidates
            );
            assert!(
                alone.crc_candidates.is_empty(),
                "{:?}",
                alone.crc_candidates
            );
        }
        let (_, context) = run_events(&BitViewStage::new(), json!({}), set_frames(&segments)).await;
        let view = &context["bit_view"];
        assert_eq!(view["sync_candidates"][0]["word"], "2dd4", "{view}");
        assert_eq!(view["sync_candidates"][0]["count"], 12, "{view}");
        // The gap is taken within a segment: one frame of 16 + 240 + 16 bits.
        assert_eq!(view["sync_candidates"][0]["mean_gap_bits"], 272.0, "{view}");
        let crcs = view["crc_candidates"].as_array().unwrap();
        assert!(
            crcs.iter().any(|c| c["crc"] == "crc16-ccitt"
                && c["period_bits"] == NEXT_SYNC_PERIOD
                && c["frames"].as_u64().unwrap() as usize >= CRC_MIN_FRAMES),
            "{view}"
        );
    }

    #[tokio::test]
    async fn a_looping_replay_does_not_count_a_pass_twice() {
        let segments = set_of_segments(3);
        let mut input = set_frames(&segments);
        input.extend(set_frames(&segments));
        let (_, context) = run_events(&BitViewStage::new(), json!({}), input).await;
        assert_eq!(
            context["bit_view"]["sync_candidates"][0]["count"], 6,
            "{}",
            context["bit_view"]
        );
    }

    #[tokio::test]
    async fn too_few_bits_say_no_sync_candidate() {
        let (events, _) = run_events(
            &BitViewStage::new(),
            json!({"window_bits": 64}),
            bits_frames(&random_bits(8, 2), &[]),
        )
        .await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["summary"], "no sync candidate");
        assert_eq!(events[0]["sync_candidates"], json!([]));
    }

    #[test]
    fn bad_window_is_refused_naming_the_field() {
        let s = BitViewStage::new();
        assert_eq!(
            s.check_params(&json!({"window_bits": 32})),
            Err("window_bits: 64..=262144".into())
        );
        assert!(s.check_params(&json!({"window": 4096})).is_err());
        assert!(s.check_params(&json!({})).is_ok());
    }

    #[test]
    fn abutting_frames_of_varying_length_give_their_crc() {
        let lengths: Vec<usize> = (0..12).map(|k| [100, 120, 136][k % 3]).collect();
        let h = hypotheses(&[&framed_stream_of(&lengths, 6)]);
        assert_eq!(h.sync_candidates[0].word, "2dd4", "{:?}", h.sync_candidates);
        assert!(
            h.crc_candidates.iter().any(|c| c.crc == "crc16-ccitt"
                && c.period_bits == NEXT_SYNC_PERIOD
                && c.frames as usize >= CRC_MIN_FRAMES),
            "{:?}",
            h.crc_candidates
        );
    }

    #[test]
    fn the_crc_search_is_bounded() {
        let h = hypotheses(&[&framed_stream(200, 136, 2)]);
        assert!(!h.crc_candidates.is_empty());
        assert!(
            h.crc_candidates
                .iter()
                .all(|c| c.checked as usize <= CRC_MAX_FRAMES),
            "{:?}",
            h.crc_candidates
        );
        assert!(h.crc_candidates.len() <= CRC_CANDIDATES_MAX);
    }

    /// The [`super::HYPOTHESES_BUDGET_MS`] budget on 64 kbit and the time on a
    /// [`super::WINDOW_BITS_MAX`] window; run with
    /// `cargo test --release -p zeta-dsp hypotheses_timing -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing, release build"]
    fn hypotheses_timing_on_64_and_256_kbit() {
        for n in [65_536, super::WINDOW_BITS_MAX as usize] {
            let mut framed = framed_stream(n / 1120 + 1, 136, 1);
            framed.truncate(n);
            let repeating: Vec<u8> = (0..n).map(|i| u8::from(i % 8 >= 4)).collect();
            for (name, bits) in [
                ("framed", framed),
                ("random", random_bits(n, 11)),
                ("repeating 0x0f", repeating),
            ] {
                let t = Instant::now();
                let h = hypotheses(&[&bits]);
                eprintln!(
                    "hypotheses on {} {name} bits: {:?} ({})",
                    bits.len(),
                    t.elapsed(),
                    h.summary
                );
            }
        }
    }
}
