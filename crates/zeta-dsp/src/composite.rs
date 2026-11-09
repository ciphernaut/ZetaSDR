//! The `composite_decode` stage: composite video (volts, sync negative) to black-and-white
//! `gray8` fields and a `video` lock event. Sync is sliced at the midpoint of the sync tip
//! and the blanking level, a line PLL follows the sync edges, the vertical interval's
//! broad pulses number the lines, and each active line is sampled after a luma low-pass.
//! Every rule works in seconds, so any input rate serves.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus, VideoFormat};
use zeta_core::types::{StreamId, StreamKind, Timestamp};

use crate::Input;
use crate::util::{event, lowpass_taps};
use crate::video_fm::{StandardTable, VideoStandard, WHITE_V};
use crate::worker::{VIDEO_QUEUE_DEPTH, run_map_threaded};

/// Which line standard to decode; `auto` tells PAL from NTSC by the lines between
/// vertical intervals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(rename_all = "lowercase")]
pub enum StandardChoice {
    #[default]
    Auto,
    Pal,
    Ntsc,
}

/// The width of a decoded field: 720, 360 or 180 samples per line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(rename_all = "lowercase")]
pub enum Scale {
    #[default]
    Full,
    Half,
    Quarter,
}

impl Scale {
    /// Samples per active line.
    pub fn width(self) -> u32 {
        match self {
            Scale::Full => 720,
            Scale::Half => 360,
            Scale::Quarter => 180,
        }
    }
}

fn default_report_s() -> u64 {
    5
}

/// Parameters of `composite_decode`.
#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct CompositeParams {
    /// "auto" (tell PAL from NTSC), "pal" or "ntsc".
    #[serde(default)]
    pub standard: StandardChoice,
    /// "full" (720 samples per line), "half" (360) or "quarter" (180).
    #[serde(default)]
    pub scale: Scale,
    /// Seconds between lock events while nothing changes.
    #[serde(default = "default_report_s")]
    pub report_s: u64,
}

impl Default for CompositeParams {
    fn default() -> Self {
        CompositeParams {
            standard: StandardChoice::Auto,
            scale: Scale::Full,
            report_s: default_report_s(),
        }
    }
}

/// One decoded field: `height` rows of `width` luma bytes, row-major.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub width: u32,
    pub height: u32,
    pub luma: Vec<u8>,
    /// The time of the field's line 1 sync edge.
    pub start_ns: u64,
    /// The field's number; even for a field that starts on a whole line.
    pub index: u64,
}

/// The decoder's lock state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lock {
    /// The standard in use (`None` while `auto` has not decided).
    pub standard: Option<VideoStandard>,
    pub locked: bool,
    /// The line PLL's rate, Hz.
    pub line_hz: f64,
    /// Fields decoded since the start or the last reset.
    pub fields: u64,
    /// Sync amplitude over the back porch's noise, dB (0..=60).
    pub snr_db: i32,
}

/// Sync pulses shorter than this fraction of `sync_s` are noise.
const GLITCH_SYNC: f64 = 0.25;
/// A line-sync pulse lasts between these fractions of `sync_s`.
const SYNC_MIN: f64 = 0.6;
const SYNC_MAX: f64 = 1.6;
/// A broad pulse is held low for more than this fraction of a line.
const BROAD_LINE: f64 = 0.4;
/// The PLL accepts an edge this close to its prediction, seconds.
const PLL_WINDOW_S: f64 = 2e-6;
/// The PLL corrects its period by this fraction of each accepted edge's error.
const PLL_GAIN: f64 = 1.0 / 16.0;
/// The period stays within this fraction of the standard's.
const PLL_RANGE: f64 = 0.02;
/// Consecutive missed lines after which the PLL acquires afresh.
const PLL_LOST: u32 = 10;
/// Line records kept (more than a frame).
const LINES_KEPT: usize = 800;
/// Decay of the sync tip's running minimum, seconds.
const TIP_DECAY_S: f64 = 0.5;
/// Time constant of the blanking level, seconds.
const BLANK_TAU_S: f64 = 2e-3;
/// Margin inside the back porch, seconds.
const PORCH_MARGIN_S: f64 = 0.5e-6;
/// The sync path's two boxcars are each this long, seconds.
const SYNC_BOXCAR_S: f64 = 0.8e-6;
/// Taps of the luma low-pass.
const LUMA_TAPS: usize = 63;
/// The back porch's noise is measured over this many lines.
const PORCH_LINES: u32 = 300;

/// A running mean of the last `len` samples.
#[derive(Clone, Debug)]
struct Boxcar {
    ring: Vec<f32>,
    pos: usize,
    sum: f64,
}

impl Boxcar {
    fn new(len: usize) -> Self {
        Boxcar {
            ring: vec![0.0; len.max(1)],
            pos: 0,
            sum: 0.0,
        }
    }

    fn run(&mut self, x: f32) -> f32 {
        self.sum += f64::from(x) - f64::from(self.ring[self.pos]);
        self.ring[self.pos] = x;
        self.pos = (self.pos + 1) % self.ring.len();
        (self.sum / self.ring.len() as f64) as f32
    }
}

/// One line of a tracker's grid: its sync edge in absolute samples, and whether an edge
/// was seen there (else it is the PLL's prediction).
#[derive(Clone, Copy, Debug)]
struct LineRec {
    idx: u64,
    t: f64,
    accepted: bool,
}

/// A field whose active lines are all on the grid: its rows' edges.
struct Plan {
    line1_t: f64,
    rows: Vec<f64>,
    even: bool,
}

/// Horizontal and vertical lock for one standard, fed the sync pulses.
#[derive(Clone, Debug)]
struct Tracker {
    standard: VideoStandard,
    t: &'static StandardTable,
    rate: f64,
    nominal: f64,
    period: f64,
    pred: Option<f64>,
    misses: u32,
    next_idx: u64,
    lines: VecDeque<LineRec>,
    /// The rising edge of the latest broad pulse not yet resolved.
    broad_rise: Option<f64>,
    field_line1: Option<u64>,
    /// Where the last field closed (or counting started), for a missing vertical
    /// interval.
    last_close: Option<u64>,
    goods: u32,
    bads: u32,
    locked: bool,
    /// When (absolute samples) a field last closed with this standard's line count.
    matched_at: Option<f64>,
    /// The latest field's line 1 and parity, until its active lines are decoded.
    pending: Option<(u64, bool)>,
    field_counter: u64,
}

impl Tracker {
    fn new(standard: VideoStandard, rate: f64) -> Self {
        let t = standard.table();
        let nominal = t.line_s * rate;
        Tracker {
            standard,
            t,
            rate,
            nominal,
            period: nominal,
            pred: None,
            misses: 0,
            next_idx: 1000,
            lines: VecDeque::with_capacity(LINES_KEPT + 1),
            broad_rise: None,
            field_line1: None,
            last_close: None,
            goods: 0,
            bads: 0,
            locked: false,
            matched_at: None,
            pending: None,
            field_counter: 0,
        }
    }

    fn max_field_lines(&self) -> u64 {
        u64::from(self.t.lines_per_field[0].max(self.t.lines_per_field[1]))
    }

    /// A sync pulse low from `fall` to `rise` (absolute samples).
    fn on_pulse(&mut self, fall: f64, rise: f64) {
        let dur_s = (rise - fall) / self.rate;
        let sync_like = (SYNC_MIN * self.t.sync_s..=SYNC_MAX * self.t.sync_s).contains(&dur_s);
        let win = PLL_WINDOW_S * self.rate;
        match self.pred {
            None => {
                if sync_like {
                    self.acquire(fall);
                }
            }
            Some(mut pred) => {
                let mut lost = false;
                while fall > pred + win {
                    self.push(pred, false);
                    pred += self.period;
                    self.misses += 1;
                    if self.misses >= PLL_LOST {
                        lost = true;
                        break;
                    }
                }
                if lost {
                    self.pred = None;
                    if sync_like {
                        self.acquire(fall);
                    }
                } else {
                    if (fall - pred).abs() <= win {
                        let err = fall - pred;
                        self.period = (self.period + err * PLL_GAIN).clamp(
                            self.nominal * (1.0 - PLL_RANGE),
                            self.nominal * (1.0 + PLL_RANGE),
                        );
                        self.push(fall, true);
                        pred = fall + self.period;
                        self.misses = 0;
                    }
                    self.pred = Some(pred);
                }
            }
        }
        if dur_s > BROAD_LINE * self.t.line_s {
            self.broad_rise = Some(rise);
        }
    }

    /// Start the grid at `fall`, numbering on from the last line kept.
    fn acquire(&mut self, fall: f64) {
        self.period = self.nominal;
        let idx = match self.lines.back() {
            Some(l) => l.idx + ((fall - l.t) / self.period).round().max(1.0) as u64,
            None => self.next_idx,
        };
        self.push_at(idx, fall, true);
        self.pred = Some(fall + self.period);
        self.misses = 0;
    }

    fn push(&mut self, t: f64, accepted: bool) {
        self.push_at(self.next_idx, t, accepted);
    }

    fn push_at(&mut self, idx: u64, t: f64, accepted: bool) {
        self.lines.push_back(LineRec { idx, t, accepted });
        self.next_idx = idx + 1;
        while self.lines.len() > LINES_KEPT {
            self.lines.pop_front();
        }
        // The vertical interval is over a line after its last broad pulse.
        if let Some(r) = self.broad_rise
            && t > r + self.nominal
        {
            self.broad_rise = None;
            self.vertical(r);
        }
        let since = *self.last_close.get_or_insert(idx);
        if idx.saturating_sub(since) > self.max_field_lines() * 3 / 2 {
            let acc = self.accepted_in(since, idx);
            self.close_field(false, idx - since, acc, t);
            self.last_close = Some(idx);
            self.field_line1 = None;
        }
    }

    fn accepted_in(&self, from: u64, to: u64) -> u64 {
        self.lines
            .iter()
            .filter(|l| l.idx >= from && l.idx < to && l.accepted)
            .count() as u64
    }

    /// A vertical interval whose last broad pulse rose at `rise`: number the new field.
    fn vertical(&mut self, rise: f64) {
        // The broad pulses' half-line slot ends a sync width after the rise; the first
        // grid line at or after it is `broad_end`. A grid line lies either on the slot's
        // end or half a line from it, so anything from 10 us before the rise on finds it.
        let thr = rise - 10e-6 * self.rate;
        let Some(l) = self.lines.iter().find(|l| l.t >= thr).copied() else {
            return;
        };
        // Lines before the grid started (or across a gap) are extrapolated.
        let back = ((l.t - thr) / self.period).floor().max(0.0);
        let (end_idx, end_t) = (l.idx.saturating_sub(back as u64), l.t - back * self.period);
        let vbi = self.t.eq_lines + self.t.broad_lines;
        let slot_end = rise + self.t.sync_s * self.rate;
        let start_phase = ((end_t - slot_end) / self.period + vbi).rem_euclid(1.0);
        let even = !(0.25..0.75).contains(&start_phase);
        let line1 = end_idx.saturating_sub(vbi.round() as u64);
        if let Some(prev) = self.field_line1 {
            if line1 <= prev + self.max_field_lines() / 2 {
                return;
            }
            let acc = self.accepted_in(prev, line1);
            self.close_field(true, line1 - prev, acc, end_t);
        }
        self.field_line1 = Some(line1);
        self.last_close = Some(line1);
        self.pending = Some((line1, even));
    }

    /// The field just ended had `n` lines, `acc` of them with an edge; `vi` when its
    /// vertical interval was found.
    fn close_field(&mut self, vi: bool, n: u64, acc: u64, at: f64) {
        let frac = acc as f64 / n.max(1) as f64;
        let count_ok = vi && self.t.lines_per_field.iter().any(|&c| u64::from(c) == n);
        if count_ok {
            self.matched_at = Some(at);
        }
        if count_ok && frac >= 0.9 {
            self.goods += 1;
            self.bads = 0;
        } else if !vi || frac < 0.5 {
            self.bads += 1;
            self.goods = 0;
        } else {
            self.goods = 0;
            self.bads = 0;
        }
        if self.goods >= 2 {
            self.locked = true;
        }
        if self.bads >= 3 {
            self.locked = false;
        }
    }

    fn line_t(&self, idx: u64) -> Option<f64> {
        let first = self.lines.front()?;
        let guess = idx.checked_sub(first.idx)? as usize;
        // Records are contiguous unless the PLL re-acquired; search near the guess.
        match self.lines.get(guess) {
            Some(l) if l.idx == idx => Some(l.t),
            _ => self.lines.iter().find(|l| l.idx == idx).map(|l| l.t),
        }
    }

    /// The pending field once all its active lines are on the grid; `Some(None)` when
    /// they never will be (a gap in the grid), to drop it.
    fn plan(&self) -> Option<Option<Plan>> {
        let (line1, even) = self.pending?;
        let first = line1 + u64::from(self.t.first_active_line) - 1;
        let last = first + u64::from(self.t.active_lines) - 1;
        if self.lines.back()?.idx <= last {
            return None;
        }
        let rows: Option<Vec<f64>> = (first..=last).map(|i| self.line_t(i)).collect();
        let Some(rows) = rows else {
            return Some(None);
        };
        let line1_t = match self.line_t(line1) {
            Some(t) => t,
            None => rows[0] - (first - line1) as f64 * self.period,
        };
        Some(Some(Plan {
            line1_t,
            rows,
            even,
        }))
    }

    /// Take the pending field: its index.
    fn consume(&mut self, even: bool) -> u64 {
        self.pending = None;
        if self.field_counter.is_multiple_of(2) != even {
            self.field_counter += 1;
        }
        let i = self.field_counter;
        self.field_counter += 1;
        i
    }

    fn accepted_recent(&self) -> usize {
        self.lines
            .iter()
            .rev()
            .take(50)
            .filter(|l| l.accepted)
            .count()
    }
}

/// The rate-dependent state, rebuilt on a rate change or a reset.
#[derive(Clone, Debug)]
struct State {
    rate: f64,
    /// Raw samples from absolute index `base`.
    buf: Vec<f32>,
    base: u64,
    box1: Boxcar,
    box2: Boxcar,
    /// The sync path's delay, samples.
    delay: f64,
    prev: f32,
    chunk_len: usize,
    chunk_pos: usize,
    chunk_min: f32,
    chunk_max: f32,
    tip: Option<f32>,
    peak: f32,
    blank: Option<f32>,
    low: bool,
    cand: Option<f64>,
    fall: f64,
    /// Falls of line-sync pulses whose back porch is still to be measured.
    porch: VecDeque<f64>,
    porch_var: f64,
    porch_n: u64,
    porch_lines: u32,
    sigma: Option<f64>,
    trackers: Vec<Tracker>,
    taps: Vec<f32>,
    /// Samples a field needs kept behind the newest.
    keep: usize,
    block_t0_ns: u64,
    block_start: u64,
}

/// Streaming composite video decoder; see the module docs.
#[derive(Clone, Debug)]
pub struct CompositeDecoder {
    choice: StandardChoice,
    width: u32,
    fields: u64,
    st: Option<State>,
}

impl CompositeDecoder {
    pub fn new(p: &CompositeParams) -> Self {
        CompositeDecoder {
            choice: p.standard,
            width: p.scale.width(),
            fields: 0,
            st: None,
        }
    }

    fn standards(&self) -> Vec<VideoStandard> {
        match self.choice {
            StandardChoice::Auto => vec![VideoStandard::Pal, VideoStandard::Ntsc],
            StandardChoice::Pal => vec![VideoStandard::Pal],
            StandardChoice::Ntsc => vec![VideoStandard::Ntsc],
        }
    }

    fn build(&self, rate_hz: u64) -> State {
        let rate = rate_hz as f64;
        let n = ((SYNC_BOXCAR_S * rate).round() as usize).max(1);
        let trackers: Vec<Tracker> = self
            .standards()
            .into_iter()
            .map(|s| Tracker::new(s, rate))
            .collect();
        let field_s = 313.0 * VideoStandard::Pal.table().line_s;
        let lowpass = trackers
            .iter()
            .map(|t| t.t.luma_lowpass_hz)
            .fold(f64::MAX, f64::min)
            .min(0.45 * rate);
        State {
            rate,
            buf: Vec::new(),
            base: 0,
            box1: Boxcar::new(n),
            box2: Boxcar::new(n),
            delay: (n - 1) as f64,
            prev: 0.0,
            chunk_len: ((VideoStandard::Pal.table().line_s * rate).round() as usize).max(1),
            chunk_pos: 0,
            chunk_min: f32::MAX,
            chunk_max: f32::MIN,
            tip: None,
            peak: 0.0,
            blank: None,
            low: false,
            cand: None,
            fall: 0.0,
            porch: VecDeque::new(),
            porch_var: 0.0,
            porch_n: 0,
            porch_lines: 0,
            sigma: None,
            trackers,
            taps: lowpass_taps(lowpass, rate, LUMA_TAPS),
            keep: (1.5 * field_s * rate) as usize + LUMA_TAPS,
            block_t0_ns: 0,
            block_start: 0,
        }
    }

    /// Decode one block of composite volts at `rate_hz`, the first sample at `t0_ns`: the
    /// fields completed in it (only while locked). A rate change resets the decoder.
    pub fn push(&mut self, x: &[f32], rate_hz: u64, t0_ns: u64) -> Vec<Field> {
        if rate_hz == 0 {
            return Vec::new();
        }
        if self.st.as_ref().is_none_or(|s| s.rate != rate_hz as f64) {
            self.reset();
            self.st = Some(self.build(rate_hz));
        }
        let Some(mut st) = self.st.take() else {
            return Vec::new();
        };
        st.block_t0_ns = t0_ns;
        st.block_start = st.base + st.buf.len() as u64;
        st.buf.extend_from_slice(x);
        st.slice(x.len());
        st.measure_porches();
        let fields = self.collect(&mut st);
        st.trim();
        self.st = Some(st);
        fields
    }

    /// Decode the fields whose active lines are all in.
    fn collect(&mut self, st: &mut State) -> Vec<Field> {
        let sel = st.selected(self.choice);
        let mut out = Vec::new();
        for i in 0..st.trackers.len() {
            let Some(plan) = st.trackers[i].plan() else {
                continue;
            };
            let Some(plan) = plan else {
                st.trackers[i].pending = None;
                continue;
            };
            let t = st.trackers[i].t;
            match st.field(t, &plan, self.width) {
                Some(None) => continue, // samples still to come
                done => {
                    let index = st.trackers[i].consume(plan.even);
                    if let Some(Some(luma)) = done
                        && sel == Some(i)
                        && st.trackers[i].locked
                    {
                        self.fields += 1;
                        out.push(Field {
                            width: self.width,
                            height: t.active_lines,
                            luma,
                            start_ns: st.t_ns(plan.line1_t),
                            index,
                        });
                    }
                }
            }
        }
        out
    }

    pub fn lock(&self) -> Lock {
        let Some(st) = &self.st else {
            let standard = match self.choice {
                StandardChoice::Auto => None,
                StandardChoice::Pal => Some(VideoStandard::Pal),
                StandardChoice::Ntsc => Some(VideoStandard::Ntsc),
            };
            return Lock {
                standard,
                locked: false,
                line_hz: standard.map_or(0.0, |s| 1.0 / s.table().line_s),
                fields: self.fields,
                snr_db: 0,
            };
        };
        let sel = st.selected(self.choice);
        let tr = sel.map(|i| &st.trackers[i]).unwrap_or(&st.trackers[0]);
        let decided = sel.is_some_and(|i| {
            self.choice != StandardChoice::Auto
                || st.trackers[i].locked
                || st.trackers[i].matched_at.is_some()
        });
        Lock {
            standard: decided.then_some(tr.standard),
            locked: sel.is_some() && tr.locked,
            line_hz: st.rate / tr.period,
            fields: self.fields,
            snr_db: st.snr_db(),
        }
    }

    /// Forget all streaming state (after lost input or a rate change).
    pub fn reset(&mut self) {
        self.st = None;
    }
}

impl State {
    /// The tracker in use: the only one for a chosen standard; in `auto` a locked one,
    /// else the one whose line count matched last, else (`None` once neither) the one
    /// with more edges over the last 50 lines.
    fn selected(&self, choice: StandardChoice) -> Option<usize> {
        if choice != StandardChoice::Auto {
            return Some(0);
        }
        if let Some(i) = self.trackers.iter().position(|t| t.locked) {
            return Some(i);
        }
        let matched = self
            .trackers
            .iter()
            .enumerate()
            .filter_map(|(i, t)| t.matched_at.map(|m| (i, m)))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((i, _)) = matched {
            return Some(i);
        }
        self.trackers
            .iter()
            .enumerate()
            .max_by_key(|(_, t)| t.accepted_recent())
            .map(|(i, _)| i)
    }

    fn blank_level(&self, tip: f32) -> f32 {
        self.blank.unwrap_or(tip + 0.3 * (self.peak - tip))
    }

    /// Run the sync path over the newest `n` samples of `buf`.
    fn slice(&mut self, n: usize) {
        let start = self.buf.len() - n;
        let decay = (self.chunk_len as f64 / self.rate / TIP_DECAY_S) as f32;
        for j in start..self.buf.len() {
            let s = self.base + j as u64;
            let y = self.box2.run(self.box1.run(self.buf[j]));
            self.chunk_min = self.chunk_min.min(y);
            self.chunk_max = self.chunk_max.max(y);
            self.chunk_pos += 1;
            if self.chunk_pos >= self.chunk_len {
                let (m, mx) = (self.chunk_min, self.chunk_max);
                match self.tip {
                    None => {
                        self.tip = Some(m);
                        self.peak = mx;
                    }
                    Some(tip) => {
                        self.tip = Some(if m < tip { m } else { tip + (m - tip) * decay });
                        self.peak = if mx > self.peak {
                            mx
                        } else {
                            self.peak + (mx - self.peak) * decay
                        };
                    }
                }
                self.chunk_pos = 0;
                self.chunk_min = f32::MAX;
                self.chunk_max = f32::MIN;
            }
            let py = std::mem::replace(&mut self.prev, y);
            let Some(tip) = self.tip else {
                continue;
            };
            let blank = self.blank_level(tip);
            let slice = 0.5 * (tip + blank);
            let h = (0.1 * (blank - tip)).abs().max(1e-4);
            let at = |a: f32, b: f32| (s - 1) as f64 + f64::from((a - slice) / (a - b));
            if !self.low {
                if py >= slice && y < slice {
                    self.cand = Some(at(py, y));
                } else if y > slice + h {
                    self.cand = None;
                }
                if y < slice - h {
                    self.low = true;
                    self.fall = self.cand.take().unwrap_or(s as f64) - self.delay;
                }
            } else {
                if py <= slice && y > slice {
                    self.cand = Some(at(py, y));
                } else if y < slice - h {
                    self.cand = None;
                }
                if y > slice + h {
                    self.low = false;
                    let rise = self.cand.take().unwrap_or(s as f64) - self.delay;
                    self.pulse(self.fall, rise);
                }
            }
        }
    }

    fn pulse(&mut self, fall: f64, rise: f64) {
        let dur_s = (rise - fall) / self.rate;
        let sync_s = self.trackers[0].t.sync_s;
        if dur_s < GLITCH_SYNC * sync_s {
            return;
        }
        if (SYNC_MIN * sync_s..=SYNC_MAX * sync_s).contains(&dur_s) {
            self.porch.push_back(fall);
        }
        for t in &mut self.trackers {
            t.on_pulse(fall, rise);
        }
    }

    /// The back porch after each line-sync pulse whose porch is in: the blanking level
    /// and the porch's noise.
    fn measure_porches(&mut self) {
        let t = self.trackers[0].t;
        let start_s = t.sync_s + PORCH_MARGIN_S;
        let end_s = self
            .trackers
            .iter()
            .map(|t| t.t.active_start_s)
            .fold(f64::MAX, f64::min)
            - PORCH_MARGIN_S;
        let alpha = (t.line_s / BLANK_TAU_S) as f32;
        let end_abs = self.base + self.buf.len() as u64;
        while let Some(&fall) = self.porch.front() {
            let a = (fall + start_s * self.rate).ceil();
            let b = (fall + end_s * self.rate).floor();
            if b < 0.0 || b as u64 >= end_abs {
                break;
            }
            self.porch.pop_front();
            if a < self.base as f64 || b <= a {
                continue;
            }
            let (a, b) = (
                (a as u64 - self.base) as usize,
                (b as u64 - self.base) as usize,
            );
            let w = &self.buf[a..=b];
            let n = w.len() as f64;
            let mean = w.iter().map(|&v| f64::from(v)).sum::<f64>() / n;
            let var = w
                .iter()
                .map(|&v| (f64::from(v) - mean).powi(2))
                .sum::<f64>();
            self.blank = Some(match self.blank {
                None => mean as f32,
                Some(bl) => bl + (mean as f32 - bl) * alpha,
            });
            self.porch_var += var;
            self.porch_n += w.len() as u64;
            self.porch_lines += 1;
            if self.porch_lines >= PORCH_LINES {
                self.sigma = Some((self.porch_var / self.porch_n.max(1) as f64).sqrt());
                self.porch_var = 0.0;
                self.porch_n = 0;
                self.porch_lines = 0;
            }
        }
    }

    fn snr_db(&self) -> i32 {
        let (Some(tip), Some(blank), Some(sigma)) = (self.tip, self.blank, self.sigma) else {
            return 0;
        };
        let amp = f64::from(blank - tip);
        if amp <= 0.0 {
            return 0;
        }
        if sigma <= 0.0 {
            return 60;
        }
        (20.0 * (amp / sigma).log10()).round().clamp(0.0, 60.0) as i32
    }

    /// The luma of `plan`'s rows: `None` when its samples are gone, `Some(None)` when
    /// they are still to come.
    fn field(&self, t: &StandardTable, plan: &Plan, width: u32) -> Option<Option<Vec<u8>>> {
        let w = width as usize;
        let half = (self.taps.len() / 2) as i64;
        let step = t.active_s * self.rate / w as f64;
        let end_abs = (self.base + self.buf.len() as u64) as i64;
        let last = plan.rows.last()? + t.active_start_s * self.rate + w as f64 * step;
        if last.ceil() as i64 + half + 2 >= end_abs {
            return Some(None);
        }
        let blank = self.blank.or(self.tip.map(|tip| self.blank_level(tip)))?;
        let gain = 255.0 / WHITE_V;
        let mut luma = Vec::with_capacity(w * plan.rows.len());
        let mut y = Vec::new();
        for &edge in &plan.rows {
            let x0 = edge + t.active_start_s * self.rate;
            let a = (x0 + 0.5 * step).floor() as i64;
            let b = (x0 + (w as f64 - 0.5) * step).floor() as i64 + 1;
            if a - half < self.base as i64 {
                return None;
            }
            // The luma low-pass, centred, at samples a..=b.
            y.clear();
            for c in a..=b {
                let i0 = (c - half - self.base as i64) as usize;
                let v: f32 = self
                    .taps
                    .iter()
                    .zip(&self.buf[i0..])
                    .map(|(h, s)| h * s)
                    .sum();
                y.push(v);
            }
            for k in 0..w {
                let x = x0 + (k as f64 + 0.5) * step;
                let i = x.floor();
                let f = (x - i) as f32;
                let j = (i as i64 - a) as usize;
                let v = y[j] * (1.0 - f) + y[j + 1] * f;
                luma.push(((v - blank) * gain).round().clamp(0.0, 255.0) as u8);
            }
        }
        Some(Some(luma))
    }

    /// The time of absolute sample `s`, ns.
    fn t_ns(&self, s: f64) -> u64 {
        let dt = (s - self.block_start as f64) / self.rate * 1e9;
        (self.block_t0_ns as f64 + dt).max(0.0) as u64
    }

    /// Drop samples no field can need.
    fn trim(&mut self) {
        if self.buf.len() > 2 * self.keep {
            let cut = self.buf.len() - self.keep;
            self.buf.drain(..cut);
            self.base += cut as u64;
        }
    }
}

/// A field's length (half a frame), ns.
fn field_ns(s: VideoStandard) -> u64 {
    let t = s.table();
    let lines = f64::from(t.lines_per_field[0] + t.lines_per_field[1]) / 2.0;
    (lines * t.line_s * 1e9).round() as u64
}

/// The event's name for a standard.
fn system(s: VideoStandard) -> &'static str {
    match s {
        VideoStandard::Pal => "PAL 625/50",
        VideoStandard::Ntsc => "NTSC 525/60",
    }
}

/// The `video` event's fields for `l`.
fn lock_fields(l: &Lock, dropped: u64) -> serde_json::Value {
    let mut v = json!({
        "lock": if l.locked { "locked" } else { "no lock" },
        "standard": l.standard,
        "locked": l.locked,
        "line_hz": l.line_hz,
        "fields": l.fields,
        "snr_db": l.snr_db,
        "dropped": dropped,
    });
    if let Some(s) = l.standard {
        v["system"] = system(s).into();
    }
    v
}

/// When to send the lock event, in signal time: the first when the decoder first locks
/// or 2 s after the first block, whichever is first; then on every change of `locked` or
/// `standard`, and every `report_s` seconds.
#[derive(Clone, Debug)]
struct Reporter {
    report_ns: u64,
    first_ns: Option<u64>,
    last: Option<(bool, Option<VideoStandard>)>,
    last_ns: u64,
}

impl Reporter {
    fn new(report_s: u64) -> Self {
        Reporter {
            report_ns: report_s.max(1).saturating_mul(1_000_000_000),
            first_ns: None,
            last: None,
            last_ns: 0,
        }
    }

    /// Whether to send `l` at `now_ns` (a block's end, having started at `t0_ns`).
    fn due(&mut self, l: &Lock, t0_ns: u64, now_ns: u64) -> bool {
        let first = *self.first_ns.get_or_insert(t0_ns);
        let key = (l.locked, l.standard);
        let send = match self.last {
            None => l.locked || now_ns.saturating_sub(first) >= 2_000_000_000,
            Some(last) => last != key || now_ns.saturating_sub(self.last_ns) >= self.report_ns,
        };
        if send {
            self.last = Some(key);
            self.last_ns = now_ns;
        }
        send
    }
}

/// Decodes composite video to `gray8` fields (output 0) and a `video` lock event
/// (output 1), with the DSP on a thread of its own behind a drop-on-full queue.
pub struct CompositeDecodeStage {
    spec: StageSpec,
}

impl CompositeDecodeStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: "composite_decode",
                inputs: vec![StreamKind::Audio],
                outputs: vec![StreamKind::Video, StreamKind::Event],
                params_schema: schemars::schema_for!(CompositeParams),
            },
        }
    }
}

impl Default for CompositeDecodeStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for CompositeDecodeStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    fn check_params(&self, params: &serde_json::Value) -> Result<(), String> {
        serde_json::from_value::<CompositeParams>(params.clone())
            .map(|_| ())
            .map_err(|e| format!("composite_decode params: {e}"))
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
        let kind = self.spec().kind;
        let p: CompositeParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("{kind} params: {e}")))?;
        let [video_out, event_out] = outputs.as_slice() else {
            return Err(StageError::Failed("expected 2 outputs".into()));
        };
        let (video_out, event_out) = (video_out.clone(), event_out.clone());
        let mut dec = CompositeDecoder::new(&p);
        let mut reporter = Reporter::new(p.report_s);
        let dropped = Arc::new(AtomicU64::new(0));
        let seen = dropped.clone();
        let mut complained = false;
        let f = move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    dec.reset();
                    return Vec::new();
                }
            };
            let Payload::Audio {
                sample_rate_hz,
                channels,
                samples,
            } = &frame.payload
            else {
                if !std::mem::replace(&mut complained, true) {
                    eprintln!("zeta-dsp: {kind} expects audio frames; dropping others");
                }
                return Vec::new();
            };
            let rate = sample_rate_hz.0;
            let ch = usize::from((*channels).max(1));
            let mono: Vec<f32>;
            let x: &[f32] = if ch == 1 {
                samples
            } else {
                mono = samples.iter().step_by(ch).copied().collect();
                &mono
            };
            let t0 = frame.provenance.span.start_ns.0;
            let mut out = Vec::new();
            let fields = dec.push(x, rate, t0);
            let field_ns = dec.lock().standard.map_or(20_000_000, field_ns);
            for field in fields {
                let mut prov = frame.provenance.clone();
                prov.span.start_ns = Timestamp(field.start_ns);
                prov.span.end_ns = Timestamp(field.start_ns.saturating_add(field_ns));
                out.push(Frame::new(
                    video_out.clone(),
                    prov,
                    Payload::Video {
                        width: field.width,
                        height: field.height,
                        format: VideoFormat::Gray8,
                        frame: field.luma.into(),
                    },
                ));
            }
            let now = t0.saturating_add(if rate == 0 {
                0
            } else {
                (x.len() as u128 * 1_000_000_000 / u128::from(rate)) as u64
            });
            let l = dec.lock();
            if reporter.due(&l, t0, now) {
                let v = event(
                    &ctx,
                    t0,
                    "video",
                    lock_fields(&l, seen.load(Ordering::Relaxed)),
                );
                out.push(Frame::new(
                    event_out.clone(),
                    frame.provenance.clone(),
                    Payload::Event(v),
                ));
            }
            out
        };
        run_map_threaded(kind, inputs, bus, cancel, VIDEO_QUEUE_DEPTH, dropped, f).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock(locked: bool, standard: Option<VideoStandard>) -> Lock {
        Lock {
            standard,
            locked,
            line_hz: 15_625.0,
            fields: 0,
            snr_db: 40,
        }
    }

    const S: u64 = 1_000_000_000;

    #[test]
    fn the_first_event_waits_for_the_lock_or_two_seconds() {
        let mut r = Reporter::new(5);
        assert!(!r.due(&lock(false, None), 0, S / 1000));
        assert!(!r.due(&lock(false, None), S, S + S / 1000));
        assert!(r.due(&lock(false, None), 2 * S, 2 * S));
        let mut r = Reporter::new(5);
        assert!(r.due(&lock(true, Some(VideoStandard::Pal)), 0, S / 10));
    }

    #[test]
    fn then_on_a_change_or_every_report_s() {
        let mut r = Reporter::new(5);
        let pal = Some(VideoStandard::Pal);
        assert!(r.due(&lock(true, pal), 0, S));
        assert!(!r.due(&lock(true, pal), S, 2 * S));
        assert!(r.due(&lock(false, pal), 2 * S, 3 * S));
        assert!(!r.due(&lock(false, pal), 3 * S, 4 * S));
        assert!(r.due(&lock(false, pal), 7 * S, 8 * S));
        assert!(r.due(&lock(false, Some(VideoStandard::Ntsc)), 8 * S, 8 * S + 1));
    }

    #[test]
    fn the_event_names_the_system_only_with_a_standard() {
        let v = lock_fields(&lock(true, Some(VideoStandard::Ntsc)), 3);
        assert_eq!(v["system"], "NTSC 525/60");
        assert_eq!(v["lock"], "locked");
        assert_eq!(v["standard"], "ntsc");
        assert_eq!(v["dropped"], 3);
        let v = lock_fields(&lock(false, None), 0);
        assert!(v.get("system").is_none());
        assert_eq!(v["lock"], "no lock");
        assert!(v["standard"].is_null());
    }
}
