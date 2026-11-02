//! Signal detection over spectrum rows: contiguous runs of bins over the row's
//! median with enough energy, tracked across rows, announced `new` once they persist
//! and `gone` once they fall silent, at most [`SIGNAL_EVENTS_PER_S`] a second.

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{SourceId, StreamId, StreamKind};

use crate::{Input, run_map};

/// Default `threshold_db`: a bin is occupied more than this many dB over the row's
/// median. An RS41 at 6 dB channel SNR stands 7-11 dB over the median in 1 kHz bins,
/// so 10 missed it; at 6 it is found down to 4 dB channel SNR, while white noise
/// averaged into 20 rows a second never stands 3 dB over its median.
pub const THRESHOLD_DB_DEFAULT: f32 = 6.0;
/// Default `energy_db`: a run counts when the sum over its bins of dB over the row's
/// median reaches this. Set against the synthesised inputs (the watch tests and an
/// RS41 at 6 dB channel SNR, still found with up to 60); the B210 noise recording
/// that should bound it from the other side is pending.
pub const ENERGY_DB_DEFAULT: f32 = 30.0;
/// Default `width_hz`: the real RS41 of 2026-11-01 read 4.5 kHz wide.
pub const WIDTH_HZ_DEFAULT: [u64; 2] = [4000, 30000];
/// A track is announced `new` only after this many rows have matched it.
pub const SIGHTINGS_MIN: u32 = 3;
/// At most this many `signal` events a second per stage; the rest wait their turn.
pub const SIGNAL_EVENTS_PER_S: usize = 10;

/// `detect` parameters; every field may be left out for its default.
#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct DetectParams {
    /// A bin is occupied when it stands more than this many dB over the row's median bin.
    pub threshold_db: f32,
    /// A run is a signal when the sum over its bins of dB over the row's median is at
    /// least this.
    pub energy_db: f32,
    /// A run is announced `new` once it has been seen this long.
    pub persist_ms: u64,
    /// Runs narrower than the first or wider than the second width (Hz) are ignored.
    pub width_hz: [u64; 2],
    /// Bins within this many Hz of the capture's centre (the DC spike) are ignored;
    /// a fifth of it is the half-width masked around each of `spurs_hz`.
    pub dc_mask_hz: u64,
    /// This fraction of the row's bins on each edge is ignored.
    #[schemars(default = "edge_mask_default")]
    pub edge_mask: f32,
    /// Absolute frequencies (Hz) of known spurs, masked like the DC spike.
    pub spurs_hz: Vec<u64>,
    /// An announced signal unmatched for this long is reported `gone`.
    pub gone_ms: u64,
    /// Watch policy, ignored by the stage: where a `watch` keeps a proved decoder,
    /// `capture` (inside its own capture) or `free` (on a free source, else `capture`).
    pub decode_on: DecodeOn,
    /// Watch policy, ignored by the stage: the source ids a `watch` with
    /// `decode_on: free` tries first, in order.
    pub prefer: Vec<SourceId>,
    /// Watch policy, ignored by the stage: the feed sink's settings, kept as given.
    pub sondehub: Option<serde_json::Value>,
}

impl DetectParams {
    /// What the schema cannot say: `threshold_db` and `energy_db` finite, `edge_mask`
    /// in `0..0.5`, `width_hz` ordered `[min, max]`. The error names the field.
    pub fn check(&self) -> Result<(), String> {
        if !self.threshold_db.is_finite() {
            return Err("threshold_db must be finite".into());
        }
        if !self.energy_db.is_finite() {
            return Err("energy_db must be finite".into());
        }
        if !(0.0..0.5).contains(&self.edge_mask) {
            return Err("edge_mask must be in 0..0.5".into());
        }
        if self.width_hz[0] > self.width_hz[1] {
            return Err("width_hz must be [min, max]".into());
        }
        Ok(())
    }
}

/// Where a `watch` keeps a decoder it proved (see [`DetectParams::decode_on`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecodeOn {
    #[default]
    Capture,
    Free,
}

/// The schema's `edge_mask` default, written as the decimal it is (the `f32` 0.05
/// would print as 0.05000000074505806).
fn edge_mask_default() -> f64 {
    0.05
}

impl Default for DetectParams {
    fn default() -> Self {
        Self {
            threshold_db: THRESHOLD_DB_DEFAULT,
            energy_db: ENERGY_DB_DEFAULT,
            persist_ms: 3000,
            width_hz: WIDTH_HZ_DEFAULT,
            dc_mask_hz: 25000,
            edge_mask: 0.05,
            spurs_hz: Vec::new(),
            gone_ms: zeta_core::ops::DETECT_GONE_MS_DEFAULT,
            decode_on: DecodeOn::Capture,
            prefer: Vec::new(),
            sondehub: None,
        }
    }
}

/// The fields of one spectrum row (`fft` stage) that detection reads: bin `k` lies at
/// `center_hz + (k - q.len() / 2) * bin_hz` with level `floor_db + q[k] * step_db`.
#[derive(Clone, Copy, Debug)]
pub struct SpectrumRowView<'a> {
    pub bin_hz: f64,
    pub center_hz: u64,
    pub floor_db: f32,
    pub step_db: f32,
    pub q: &'a [u8],
    /// Bin ranges `[lo, hi)` (lo inclusive, hi exclusive) that hold no data: a bin
    /// inside one takes no part in a run.
    pub masked: &'a [[usize; 2]],
}

/// One contiguous run of occupied bins in a row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Run {
    /// Power-weighted centre.
    pub center_hz: u64,
    /// Bin count times `bin_hz`.
    pub bandwidth_hz: u64,
    /// Peak over the row's `floor_db`.
    pub snr_db: f32,
}

/// The runs of `row`: maximal sets of contiguous unmasked bins more than `threshold_db`
/// over the median of the row's unmasked bins, inside `width_hz`, whose energy (the sum
/// over their bins of dB over that median) is at least `energy_db`. Masked are the bins inside
/// one of the row's `masked` ranges, the bins within `dc_mask_hz` of the row's centre, the outer `edge_mask` fraction on each side and
/// the bins within `dc_mask_hz / 5` of a spur; a masked bin ends a run. A run's centre
/// is weighted by its bins' linear power, its width is its bin count times `bin_hz`
/// and its SNR is its peak over `floor_db`.
pub fn runs(row: &SpectrumRowView, p: &DetectParams) -> Vec<Run> {
    let size = row.q.len();
    if size == 0 || row.bin_hz.is_nan() || row.bin_hz <= 0.0 {
        return Vec::new();
    }
    let center = row.center_hz as f64;
    let half = (size / 2) as f64;
    let freq = |k: usize| center + (k as f64 - half) * row.bin_hz;
    let edge = (f64::from(p.edge_mask.clamp(0.0, 0.5)) * size as f64) as usize;
    let spur_hz = p.dc_mask_hz as f64 / 5.0;
    let masked = |k: usize| {
        let f = freq(k);
        k < edge
            || row.masked.iter().any(|&[lo, hi]| lo <= k && k < hi)
            || k >= size - edge
            || (f - center).abs() <= p.dc_mask_hz as f64
            || p.spurs_hz.iter().any(|&s| (f - s as f64).abs() <= spur_hz)
    };
    let mut levels: Vec<u8> = (0..size)
        .filter(|&k| !masked(k))
        .map(|k| row.q[k])
        .collect();
    if levels.is_empty() {
        return Vec::new();
    }
    let mid = levels.len() / 2;
    let median = *levels.select_nth_unstable(mid).1;
    let over = |k: usize| (f32::from(row.q[k]) - f32::from(median)) * row.step_db;
    let occupied = |k: usize| !masked(k) && over(k) > p.threshold_db;
    let mut out = Vec::new();
    let mut k = 0;
    while k < size {
        if !occupied(k) {
            k += 1;
            continue;
        }
        let k0 = k;
        while k < size && occupied(k) {
            k += 1;
        }
        let bandwidth_hz = ((k - k0) as f64 * row.bin_hz).round() as u64;
        if bandwidth_hz < p.width_hz[0] || bandwidth_hz > p.width_hz[1] {
            continue;
        }
        if (k0..k).map(over).sum::<f32>() < p.energy_db {
            continue;
        }
        let (mut power, mut moment, mut peak) = (0.0f64, 0.0f64, 0u8);
        for (j, &q) in row.q.iter().enumerate().take(k).skip(k0) {
            let w = 10f64.powf(f64::from(q) * f64::from(row.step_db) / 10.0);
            power += w;
            moment += w * freq(j);
            peak = peak.max(q);
        }
        out.push(Run {
            center_hz: (moment / power).round() as u64,
            bandwidth_hz,
            snr_db: f32::from(peak) * row.step_db,
        });
    }
    out
}

/// Whether a tracked signal has just appeared or has gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignalState {
    New,
    Gone,
}

impl SignalState {
    fn as_str(self) -> &'static str {
        match self {
            SignalState::New => "new",
            SignalState::Gone => "gone",
        }
    }
}

/// A tracked signal's announcement: its latest run's centre, width and SNR, and the
/// times of its first and last matching rows.
#[derive(Clone, Debug, PartialEq)]
pub struct Signal {
    pub state: SignalState,
    pub center_hz: u64,
    pub bandwidth_hz: u64,
    pub snr_db: f32,
    pub first_ns: u64,
    pub last_ns: u64,
}

impl Signal {
    /// `<state> <centre MHz> MHz <width kHz> kHz <snr> dB`.
    pub fn summary(&self) -> String {
        format!(
            "{} {:.4} MHz {:.1} kHz {:.1} dB",
            self.state.as_str(),
            self.center_hz as f64 / 1e6,
            self.bandwidth_hz as f64 / 1e3,
            self.snr_db
        )
    }

    /// The `signal` event published for this announcement in the row of time `t_ns`.
    pub fn payload(&self, t_ns: u64) -> serde_json::Value {
        serde_json::json!({
            "protocol": "signal",
            "state": self.state.as_str(),
            "center_hz": self.center_hz,
            "bandwidth_hz": self.bandwidth_hz,
            "snr_db": (f64::from(self.snr_db) * 10.0).round() / 10.0,
            "first_ns": self.first_ns,
            "last_ns": self.last_ns,
            "summary": self.summary(),
            "t_ns": t_ns,
        })
    }
}

/// One signal followed by a [`Tracker`].
struct Track {
    center_hz: u64,
    bandwidth_hz: u64,
    snr_db: f32,
    first_ns: u64,
    last_ns: u64,
    /// Time of the first row since the last match that did not match it.
    unmatched_since: Option<u64>,
    /// Rows that matched it, the first included.
    sightings: u32,
    announced: bool,
}

impl Track {
    fn signal(&self, state: SignalState) -> Signal {
        Signal {
            state,
            center_hz: self.center_hz,
            bandwidth_hz: self.bandwidth_hz,
            snr_db: self.snr_db,
            first_ns: self.first_ns,
            last_ns: self.last_ns,
        }
    }
}

/// Follows runs across rows. A run matches the nearest tracked signal (not yet matched
/// in that row) whose last centre lies within `max(bandwidth_hz, 2 * bin_hz)` of it,
/// which takes the run's centre, width and SNR, so a drifting signal is followed; an
/// unmatched run starts a new signal. A signal matched in a row at least `persist_ms`
/// after its first, by at least [`SIGHTINGS_MIN`] rows in all, is announced
/// [`SignalState::New`] once; a signal no row has matched
/// for `gone_ms` (counted from the first row that missed it) is dropped, announced
/// [`SignalState::Gone`] when it had been announced new, forgotten silently otherwise.
pub struct Tracker {
    persist_ns: u64,
    gone_ns: u64,
    bin_hz: f64,
    tracks: Vec<Track>,
}

impl Tracker {
    pub fn new(p: &DetectParams, bin_hz: f64) -> Self {
        Self {
            persist_ns: p.persist_ms.saturating_mul(1_000_000),
            gone_ns: p.gone_ms.saturating_mul(1_000_000),
            bin_hz,
            tracks: Vec::new(),
        }
    }

    /// The bin width of the rows observed from now on.
    pub fn set_bin_hz(&mut self, bin_hz: f64) {
        self.bin_hz = bin_hz;
    }

    /// Match the `runs` of the row at `t_ns`; the announcements it causes.
    pub fn observe(&mut self, t_ns: u64, runs: &[Run]) -> Vec<Signal> {
        let mut matched = vec![false; self.tracks.len()];
        for r in runs {
            let near = self
                .tracks
                .iter()
                .enumerate()
                .filter(|&(i, tr)| {
                    let reach = (tr.bandwidth_hz as f64).max(2.0 * self.bin_hz);
                    !matched[i] && r.center_hz.abs_diff(tr.center_hz) as f64 <= reach
                })
                .min_by_key(|(_, tr)| r.center_hz.abs_diff(tr.center_hz))
                .map(|(i, _)| i);
            match near {
                Some(i) => {
                    let tr = &mut self.tracks[i];
                    tr.center_hz = r.center_hz;
                    tr.bandwidth_hz = r.bandwidth_hz;
                    tr.snr_db = r.snr_db;
                    tr.last_ns = t_ns;
                    tr.sightings = tr.sightings.saturating_add(1);
                    matched[i] = true;
                }
                None => {
                    self.tracks.push(Track {
                        center_hz: r.center_hz,
                        bandwidth_hz: r.bandwidth_hz,
                        snr_db: r.snr_db,
                        first_ns: t_ns,
                        last_ns: t_ns,
                        unmatched_since: None,
                        sightings: 1,
                        announced: false,
                    });
                    matched.push(true);
                }
            }
        }
        let mut out = Vec::new();
        let mut keep = Vec::with_capacity(self.tracks.len());
        for (mut tr, m) in std::mem::take(&mut self.tracks).into_iter().zip(matched) {
            if m {
                tr.unmatched_since = None;
                if !tr.announced
                    && tr.sightings >= SIGHTINGS_MIN
                    && t_ns.saturating_sub(tr.first_ns) >= self.persist_ns
                {
                    tr.announced = true;
                    out.push(tr.signal(SignalState::New));
                }
            } else if t_ns.saturating_sub(*tr.unmatched_since.get_or_insert(t_ns)) >= self.gone_ns {
                if tr.announced {
                    out.push(tr.signal(SignalState::Gone));
                }
                continue;
            }
            keep.push(tr);
        }
        self.tracks = keep;
        out
    }
}

/// Holds announcements to at most `per_s` in each second of row time (`t_ns / 1 s`);
/// the rest wait, in order, for the following seconds' rows.
pub struct Pacer {
    per_s: usize,
    second: u64,
    sent: usize,
    queue: VecDeque<Signal>,
}

impl Pacer {
    pub fn new(per_s: usize) -> Self {
        Self {
            per_s,
            second: 0,
            sent: 0,
            queue: VecDeque::new(),
        }
    }

    /// Queue `signals` from the row at `t_ns`; the ones to publish with that row.
    pub fn pace(&mut self, t_ns: u64, signals: Vec<Signal>) -> Vec<Signal> {
        self.queue.extend(signals);
        let second = t_ns / 1_000_000_000;
        if second != self.second {
            self.second = second;
            self.sent = 0;
        }
        let n = self.queue.len().min(self.per_s.saturating_sub(self.sent));
        self.sent += n;
        self.queue.drain(..n).collect()
    }
}

/// The `q` levels of a spectrum row's payload; `None` for any other event.
fn row_levels(v: &serde_json::Value) -> Option<Vec<u8>> {
    v.get("q")?
        .as_array()?
        .iter()
        .map(|q| q.as_u64().and_then(|q| u8::try_from(q).ok()))
        .collect()
}

/// The view of a spectrum row's payload `v` over its levels `q`.
fn row_view<'a>(
    v: &serde_json::Value,
    q: &'a [u8],
    masked: &'a [[usize; 2]],
) -> Option<SpectrumRowView<'a>> {
    Some(SpectrumRowView {
        bin_hz: v.get("bin_hz")?.as_f64()?,
        center_hz: v.get("center_hz")?.as_u64()?,
        floor_db: v.get("floor_db")?.as_f64()? as f32,
        step_db: v.get("step_db")?.as_f64()? as f32,
        q,
        masked,
    })
}

/// The payload's `masked` bin ranges as `[lo, hi)` pairs; an absent field or any
/// malformed shape gives none (the mask is ignored).
fn row_masked(v: &serde_json::Value) -> Vec<[usize; 2]> {
    let pairs = || -> Option<Vec<[usize; 2]>> {
        v.get("masked")?
            .as_array()?
            .iter()
            .map(|p| {
                let p = p.as_array()?;
                match p.as_slice() {
                    [lo, hi] => Some([lo.as_u64()? as usize, hi.as_u64()? as usize]),
                    _ => None,
                }
            })
            .collect()
    };
    pairs().unwrap_or_default()
}

/// Detects signals in spectrum rows (the `fft` stage's events): each row's [`runs`] go
/// through a [`Tracker`] and every announcement is published, at most
/// [`SIGNAL_EVENTS_PER_S`] a second ([`Pacer`]), as one `signal` event,
/// `{protocol, state, center_hz, bandwidth_hz, snr_db, first_ns, last_ns, summary,
/// t_ns}`, `t_ns` the row's start; events that are not spectrum rows are ignored.
pub struct DetectStage {
    spec: StageSpec,
}

impl DetectStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: "detect",
                inputs: vec![StreamKind::Event],
                outputs: vec![StreamKind::Event],
                params_schema: schemars::schema_for!(DetectParams),
            },
        }
    }
}

impl Default for DetectStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for DetectStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    fn check_params(&self, params: &serde_json::Value) -> Result<(), String> {
        serde_json::from_value::<DetectParams>(params.clone())
            .map_err(|e| e.to_string())?
            .check()
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
        let p: DetectParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("detect params: {e}")))?;
        p.check()
            .map_err(|e| StageError::Failed(format!("detect params: {e}")))?;
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        let mut tracker = Tracker::new(&p, 0.0);
        let mut pacer = Pacer::new(SIGNAL_EVENTS_PER_S);
        run_map(self.spec().kind, inputs, bus, cancel, move |input| {
            // Rows carry their own time, so lost rows only thin the evidence.
            let Input::Frame(frame) = input else {
                return Ok(Vec::new());
            };
            let Payload::Event(v) = &frame.payload else {
                return Err(StageError::Failed(
                    "detect stage expects event frames".into(),
                ));
            };
            let Some(q) = row_levels(v) else {
                return Ok(Vec::new());
            };
            let masked = row_masked(v);
            let Some(row) = row_view(v, &q, &masked) else {
                return Ok(Vec::new());
            };
            let t_ns = frame.provenance.span.start_ns.0;
            tracker.set_bin_hz(row.bin_hz);
            let found = runs(&row, &p);
            Ok(pacer
                .pace(t_ns, tracker.observe(t_ns, &found))
                .into_iter()
                .map(|s| {
                    Frame::new(
                        out.clone(),
                        frame.provenance.clone(),
                        Payload::Event(s.payload(t_ns)),
                    )
                })
                .collect())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIN_HZ: f64 = 3_500.0;
    const SIZE: usize = 1024;
    const CENTER_HZ: u64 = 403_000_000;
    const S: u64 = 1_000_000_000;

    /// Bin index of `offset_hz` from the centre.
    fn bin(offset_hz: f64) -> usize {
        (SIZE as f64 / 2.0 + (offset_hz / BIN_HZ).round()) as usize
    }

    /// A row at q = 10 (5 dB over the floor) everywhere but `bumps` (`(first bin, bins, q)`).
    fn row(bumps: &[(usize, usize, u8)]) -> Vec<u8> {
        let mut q = vec![10u8; SIZE];
        for &(k0, n, level) in bumps {
            q[k0..k0 + n].fill(level);
        }
        q
    }

    fn view(q: &[u8]) -> SpectrumRowView<'_> {
        view_masked(q, &[])
    }

    fn view_masked<'a>(q: &'a [u8], masked: &'a [[usize; 2]]) -> SpectrumRowView<'a> {
        SpectrumRowView {
            bin_hz: BIN_HZ,
            center_hz: CENTER_HZ,
            floor_db: -100.0,
            step_db: 0.5,
            q,
            masked,
        }
    }

    #[test]
    fn runs_ignore_bins_inside_a_masked_range() {
        // The 10 kHz bump of runs_find_a_10_khz_bump_and_skip_the_dc_spike, wholly masked.
        let up = 10 + 24;
        let k = bin(1_000_000.0) - 1;
        let q = row(&[(k, 3, up)]);
        assert_eq!(runs(&view(&q), &DetectParams::default()).len(), 1);
        assert!(runs(&view_masked(&q, &[[k, k + 3]]), &DetectParams::default()).is_empty());
    }

    #[test]
    fn a_masked_range_cuts_a_run() {
        // A 6-bin (21 kHz) bump with its first 3 bins masked: one 3-bin run on the unmasked half.
        let up = 10 + 24;
        let k = bin(1_000_000.0) - 1;
        let q = row(&[(k, 6, up)]);
        let found = runs(&view_masked(&q, &[[k, k + 3]]), &DetectParams::default());
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].bandwidth_hz, (3.0 * BIN_HZ) as u64);
        let centre = CENTER_HZ as f64 + (k as f64 + 4.0 - (SIZE / 2) as f64) * BIN_HZ;
        assert!(
            (found[0].center_hz as f64 - centre).abs() <= BIN_HZ,
            "{found:?}"
        );
    }

    #[test]
    fn a_malformed_masked_field_is_ignored() {
        let ok = serde_json::json!({"masked": [[1, 2], [5, 9]]});
        assert_eq!(row_masked(&ok), vec![[1, 2], [5, 9]]);
        for bad in [
            serde_json::json!({}),
            serde_json::json!({"masked": null}),
            serde_json::json!({"masked": [[1]]}),
            serde_json::json!({"masked": [[1, "x"]]}),
            serde_json::json!({"masked": "no"}),
        ] {
            assert!(row_masked(&bad).is_empty(), "{bad}");
        }
    }

    #[test]
    fn runs_find_a_10_khz_bump_and_skip_the_dc_spike() {
        // 12 dB over the q = 10 noise; the spike far stronger, on the centre.
        let up = 10 + 24;
        let k = bin(1_000_000.0) - 1;
        let q = row(&[(SIZE / 2 - 1, 2, 200), (k, 3, up)]);
        let found = runs(&view(&q), &DetectParams::default());
        assert_eq!(found.len(), 1, "{found:?}");
        let r = found[0];
        assert!(
            r.center_hz.abs_diff(CENTER_HZ + 1_000_000) <= BIN_HZ as u64,
            "{r:?}"
        );
        assert_eq!(r.bandwidth_hz, (3.0 * BIN_HZ) as u64);
        assert!((r.snr_db - 17.0).abs() < 1e-3, "{r:?}");
    }

    #[test]
    fn runs_respect_the_width_limits_and_spurs() {
        let up = 40;
        let wide = bin(-500_000.0);
        let spur_k = bin(700_000.0) - 1;
        let ok_k = bin(200_000.0) - 1;
        let q = row(&[(wide, 60, up), (spur_k, 3, up), (ok_k, 3, up)]);
        let spur_hz = (CENTER_HZ as f64 + (spur_k + 1 - SIZE / 2) as f64 * BIN_HZ) as u64;
        let p = DetectParams {
            spurs_hz: vec![spur_hz],
            ..DetectParams::default()
        };
        let found = runs(&view(&q), &p);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].center_hz.abs_diff(CENTER_HZ + 200_000) <= BIN_HZ as u64,
            "{found:?}"
        );
        // Without the spur listed, its run is found too.
        assert_eq!(runs(&view(&q), &DetectParams::default()).len(), 2);
        // A narrow run (one bin, 3.5 kHz) is under width_hz[0].
        let q = row(&[(ok_k, 1, up)]);
        assert!(runs(&view(&q), &DetectParams::default()).is_empty());
    }

    fn run_at(center_hz: u64) -> Run {
        Run {
            center_hz,
            bandwidth_hz: 10_500,
            snr_db: 18.0,
        }
    }

    #[test]
    fn tracker_reports_new_after_persist_and_gone_after_silence() {
        let mut t = Tracker::new(&DetectParams::default(), BIN_HZ);
        let r = [run_at(404_000_000)];
        for s in 0..3 {
            assert!(t.observe(s * S, &r).is_empty(), "at {s} s");
        }
        let new = t.observe(3 * S, &r);
        assert_eq!(new.len(), 1, "{new:?}");
        assert_eq!(new[0].state, SignalState::New);
        assert_eq!((new[0].first_ns, new[0].last_ns), (0, 3 * S));
        assert_eq!(new[0].center_hz, 404_000_000);
        for s in 4..34 {
            assert!(t.observe(s * S, &[]).is_empty(), "at {s} s");
        }
        let gone = t.observe(34 * S, &[]);
        assert_eq!(gone.len(), 1, "{gone:?}");
        assert_eq!(gone[0].state, SignalState::Gone);
        assert_eq!((gone[0].first_ns, gone[0].last_ns), (0, 3 * S));
        assert!(t.observe(35 * S, &[]).is_empty());
    }

    #[test]
    fn tracker_forgets_a_run_that_never_persisted() {
        let mut t = Tracker::new(&DetectParams::default(), BIN_HZ);
        assert!(t.observe(0, &[run_at(404_000_000)]).is_empty());
        for s in 1..40 {
            assert!(t.observe(s * S, &[]).is_empty(), "at {s} s");
        }
    }

    #[test]
    fn tracker_follows_a_drifting_centre() {
        let mut t = Tracker::new(&DetectParams::default(), BIN_HZ);
        let mut events = Vec::new();
        for s in 0..10u64 {
            let c = 404_000_000 + s * BIN_HZ as u64;
            events.extend(t.observe(s * S, &[run_at(c)]));
        }
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].state, SignalState::New);
        assert_eq!(events[0].first_ns, 0);
        assert_eq!(events[0].center_hz, 404_000_000 + 3 * BIN_HZ as u64);
    }

    fn bin_center(k: usize) -> i64 {
        CENTER_HZ as i64 + ((k as f64 - (SIZE / 2) as f64) * BIN_HZ) as i64
    }

    #[test]
    fn occupancy_is_over_the_median_and_a_run_needs_energy() {
        // A 2-bin bump 3 dB over the q = 10 median (under the threshold) and a 3-bin bump
        // 12 dB over it (36 dB of energy, over 30).
        let p = DetectParams {
            threshold_db: 10.0,
            energy_db: 30.0,
            ..Default::default()
        };
        let r = runs(&view(&row(&[(100, 2, 16), (300, 3, 34)])), &p);
        assert_eq!(r.len(), 1, "{r:?}");
        assert!((r[0].center_hz as i64 - bin_center(301)).abs() <= BIN_HZ as i64);
        // A row whose median stands 15 dB over floor_db: the floor rule would call every
        // bin occupied (one run too wide to count); the median rule finds the bump.
        let mut q = vec![30u8; SIZE];
        q[300..303].fill(30 + 24);
        let r = runs(&view(&q), &p);
        assert_eq!(r.len(), 1, "{r:?}");
        assert!((r[0].center_hz as i64 - bin_center(301)).abs() <= BIN_HZ as i64);
    }

    #[test]
    fn a_wide_weak_run_under_the_energy_floor_is_not_a_signal() {
        let p = DetectParams {
            threshold_db: 10.0,
            energy_db: 30.0,
            ..Default::default()
        };
        // +11 dB is q 22 over the median.
        let wide = runs(&view(&row(&[(300, 6, 32)])), &p);
        assert_eq!(wide.len(), 1, "6 bins at +11 dB = 66 dB: {wide:?}");
        let narrow = runs(&view(&row(&[(300, 2, 32)])), &p);
        assert!(narrow.is_empty(), "2 bins at +11 dB = 22 dB: {narrow:?}");
    }

    #[test]
    fn the_tracker_needs_three_sightings_before_new() {
        let p = DetectParams {
            persist_ms: 1500,
            ..Default::default()
        };
        let r = [run_at(404_000_000)];
        let mut t = Tracker::new(&p, BIN_HZ);
        assert!(t.observe(0, &r).is_empty());
        assert!(t.observe(S, &r).is_empty());
        let new = t.observe(2 * S, &r);
        assert_eq!(new.len(), 1, "{new:?}");
        assert_eq!(new[0].state, SignalState::New);
        // Two sightings past persist_ms are not enough: the third announces it.
        let mut t = Tracker::new(&p, BIN_HZ);
        assert!(t.observe(0, &r).is_empty());
        assert!(t.observe(2 * S, &r).is_empty());
        assert_eq!(t.observe(5 * S / 2, &r).len(), 1);
    }

    #[test]
    fn a_5_ms_burst_every_second_is_new_after_three_sightings() {
        let p = DetectParams {
            persist_ms: 3000,
            gone_ms: 30000,
            ..Default::default()
        };
        let mut t = Tracker::new(&p, BIN_HZ);
        let row_ns = S / 20;
        let mut news = Vec::new();
        for n in 0..200u64 {
            let r: &[Run] = if n % 20 == 0 {
                &[run_at(404_000_000)]
            } else {
                &[]
            };
            for s in t.observe(n * row_ns, r) {
                news.push((n * row_ns, s));
            }
        }
        assert_eq!(news.len(), 1, "{news:?}");
        assert_eq!(news[0].0, 3 * S);
        assert_eq!(news[0].1.state, SignalState::New);
    }

    #[test]
    fn signal_events_are_rate_limited_to_ten_per_second() {
        let p = DetectParams {
            persist_ms: 1000,
            ..Default::default()
        };
        let mut t = Tracker::new(&p, BIN_HZ);
        let mut pacer = Pacer::new(SIGNAL_EVENTS_PER_S);
        let many: Vec<Run> = (0..100u64)
            .map(|i| run_at(402_000_000 + i * 20_000))
            .collect();
        let row_ns = S / 20;
        let mut per_s = std::collections::BTreeMap::<u64, usize>::new();
        let mut total = 0;
        for n in 0..400u64 {
            let t_ns = n * row_ns;
            let out = pacer.pace(t_ns, t.observe(t_ns, &many));
            total += out.len();
            *per_s.entry(t_ns / S).or_default() += out.len();
        }
        assert_eq!(total, 100);
        assert!(per_s.values().all(|&n| n <= 10), "{per_s:?}");
        assert_eq!(per_s.get(&1), Some(&10), "{per_s:?}");
        assert_eq!(per_s.get(&10), Some(&10), "{per_s:?}");
    }

    /// Spectrum rows (`(floor_db, q)`) of `iq` at `rate_hz`, `size` bins, 20 a second.
    fn fft_rows(
        iq: &[num_complex::Complex<f32>],
        rate_hz: usize,
        size: usize,
    ) -> Vec<(f32, Vec<u8>)> {
        let fft = crate::fft::CpuFft::new();
        iq.chunks_exact(rate_hz / 20)
            .map(|chunk| {
                let power =
                    crate::fft::FftBackend::power_sum(&fft, chunk, size, crate::fft::Window::Hann);
                crate::fft::quantise_row(&power.db())
            })
            .collect()
    }

    const RS41_OFFSET_HZ: f64 = 200_000.0;

    /// Eight synthesised RS41 frames at `RS41_OFFSET_HZ` in a capture of `rate_hz`, under
    /// white noise `channel_snr_db` below the sonde's power while it transmits, measured
    /// in a 12.5 kHz channel.
    fn rs41_capture(rate_hz: usize, channel_snr_db: f64) -> Vec<num_complex::Complex<f32>> {
        use crate::synth::{Impairments, SynthSpec, awgn, synthesise};
        let spec = SynthSpec::Rs41 {
            serial: "S1234567".into(),
            lat: -27.38,
            lon: 153.12,
            alt_m: 12_000.0,
            vel_v: 5.0,
            frames: 8,
        };
        let imp = Impairments {
            offset_hz: RS41_OFFSET_HZ,
            ..Default::default()
        };
        let mut iq = synthesise(&spec, &imp, rate_hz as u32, CENTER_HZ);
        // The noise over the whole capture is the burst power less `channel_snr_db`,
        // spread over rate / 12 500 channels; `awgn` takes its SNR against the mean
        // power, gaps between frames included.
        let power = |s: &num_complex::Complex<f32>| f64::from(s.norm_sqr());
        let on: Vec<f64> = iq.iter().map(power).filter(|&p| p > 0.0).collect();
        let burst_p = on.iter().sum::<f64>() / on.len() as f64;
        let mean_p = iq.iter().map(power).sum::<f64>() / iq.len() as f64;
        let noise_p = burst_p / 10f64.powf(channel_snr_db / 10.0) * rate_hz as f64 / 12_500.0;
        awgn(&mut iq, (10.0 * (mean_p / noise_p).log10()) as f32, 7);
        iq
    }

    /// The announcements of `runs` and a [`Tracker`] over `iq`'s rows of `size` bins.
    fn detect_iq(
        iq: &[num_complex::Complex<f32>],
        rate_hz: usize,
        size: usize,
        p: &DetectParams,
    ) -> Vec<Signal> {
        let bin_hz = rate_hz as f64 / size as f64;
        let mut t = Tracker::new(p, bin_hz);
        let mut news = Vec::new();
        for (n, (floor_db, q)) in fft_rows(iq, rate_hz, size).iter().enumerate() {
            let row = SpectrumRowView {
                bin_hz,
                center_hz: CENTER_HZ,
                floor_db: *floor_db,
                step_db: crate::fft::STEP_DB,
                q,
                masked: &[],
            };
            news.extend(t.observe(n as u64 * S / 20, &runs(&row, p)));
        }
        news
    }

    #[test]
    fn a_6_db_rs41_is_detected() {
        const RATE: usize = 1_024_000;
        let news = detect_iq(
            &rs41_capture(RATE, 6.0),
            RATE,
            1024,
            &DetectParams::default(),
        );
        let want = CENTER_HZ + RS41_OFFSET_HZ as u64;
        assert!(
            news.iter()
                .any(|s| s.state == SignalState::New && s.center_hz.abs_diff(want) <= 6_000),
            "{news:?}"
        );
        assert!(
            news.iter().all(|s| s.center_hz.abs_diff(want) <= 15_000),
            "{news:?}"
        );
    }
}
