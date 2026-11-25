//! The choice (design 5.2): the one job a band's classification answers, with the
//! evidence for it, its runner-up and the decoder rows a later step could still prove.
//! Step 1 is what `classify` already holds: layer 1's measurement and the quick proofs.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::classify::{Classification, Label, Measurement, Outcome};
use crate::modes::score::{
    Expected, Scored, allocation_term, expected_fits, expected_labels, passed_over, rank_scored,
    score_demodulators, score_rows,
};
use crate::modes::weights::Weights;
use crate::modes::{CenterRule, Decoder, LoadedMode, ModeRegistry, Role, WidthRule};
use crate::port::Tuning;
use crate::types::{Hz, Selection};

/// How far the choice is settled: `proved` by a decode passing its row's own rule,
/// `best_guess` from the measurement and the rows, or `cannot_tell`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Settled {
    Proved,
    BestGuess,
    CannotTell,
}

impl Settled {
    /// Its name as serialised.
    pub fn name(self) -> &'static str {
        match self {
            Settled::Proved => "proved",
            Settled::BestGuess => "best_guess",
            Settled::CannotTell => "cannot_tell",
        }
    }
}

/// A job as `job_start` takes it: the kind (a row's id), its band and its parameters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ChoiceJob {
    pub kind: String,
    pub center_hz: Hz,
    pub bandwidth_hz: Hz,
    /// The kind's parameters (`deviation_hz`, `sideband`, ...); `{}` for a decoder row.
    pub params: serde_json::Map<String, Value>,
}

/// The job the choice would answer next: its row and the evidence for it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Alternative {
    pub job: ChoiceJob,
    pub row: String,
    pub evidence: Vec<String>,
}

/// A row the choice did not try, and why.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = crate::knowledge::require_all)]
pub struct NotTried {
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub row: Option<String>,
    pub reason: String,
}

/// The one ranked answer for a band.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = crate::knowledge::require_all)]
pub struct Choice {
    /// The job to start; `null` when the choice cannot tell.
    #[schemars(required, schema_with = "crate::types::nullable::<ChoiceJob>")]
    pub job: Option<ChoiceJob>,
    /// The row the job is of; `null` with no job.
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub row: Option<String>,
    /// The ladder step that settled it: 1 for what `classify` holds.
    pub step: u8,
    pub settled: Settled,
    /// `<source>: <line>`: `layer 1: <label> <width> Hz`, `plan: <id> deviation <d>`,
    /// `register: <designator> expected here (layer 1 read <label>)` or `plan: <id> is
    /// an FM plan (layer 1 read <label>)` (a demodulator the label does not serve, brought
    /// by the emission expected there), `decode <row>: <summary>`, `catalogue: <name>`.
    pub evidence: Vec<String>,
    #[schemars(required, schema_with = "crate::types::nullable::<Alternative>")]
    pub runner_up: Option<Alternative>,
    /// The ready decoder rows that fit the label (any label, for `unknown`) and the
    /// width, and whose own band the capture holds, in score order: what a later step
    /// could still prove.
    pub could_prove: Vec<String>,
    pub not_tried: Vec<NotTried>,
    /// Why the choice cannot tell; `null` otherwise.
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub reason: Option<String>,
    /// The ladder steps of which at least one trial started: `[1]` for what `classify`
    /// holds, then `[1, 2]`, `[1, 3]` or `[1, 2, 3]`. `step` stays the step that settled
    /// the answer; a step skipped whole has its reason in `not_tried`.
    #[serde(default)]
    pub steps_run: Vec<u8>,
}

/// The catalogue entry a layer-2 catalogue label names (its evidence `catalogue <id>: ...`).
pub(crate) fn catalogue_entry(label: &Label) -> Option<&str> {
    if label.layer != 2 {
        return None;
    }
    label.evidence.strip_prefix("catalogue ")?.split(':').next()
}

/// The choice for the band `sel` of `c`, step 1: a quick proof wins; otherwise the
/// demodulator row that serves the layer-1 label best, sized by its width rule; otherwise
/// the best ready decoder row that fits; otherwise `cannot_tell`. A decoder row fits the
/// label it presents as, except for `unknown`, which any row of a fitting width is listed
/// for (in `could_prove`, to be tried, never a best guess). Ranked by `weights`
/// (the catalogue only ranks, at `weights.catalogue`); a plan channel of `region` at the
/// measured centre gives the deviation, and the width when the measured one is distrusted.
///
/// The emission expected at the measured centre ([`expected_labels`]: the register's
/// licences there, an FM plan's channel) weighs against the label (spec 49.2): a
/// demodulator row that serves an expected emission is a candidate though it does not
/// serve the label ([`expected_fits`]), sized as for the expected label, with the
/// expectation's line in its evidence. Only an expectation some demodulator row fits at
/// the measured width counts. When every such expectation is incompatible with the label
/// (the label is contradicted), each demodulator row an expectation fits scores the width
/// term and `weights.allocation` (a licence at rank 0; a plan's channel the same, never
/// twice), and each row that serves the label scores its label term at
/// `weights.contradicted` and no licence term from elsewhere in the band. It ranks, never
/// proves: the answer stays a best guess, and the decoder rows are ranked as before.
///
/// A decoder row is tried at its own band ([`job_for`]), not the box's: a box narrower
/// than the row still lists it. With `capture` (the ring's centre and rate) a ready
/// decoder row whose band reaches past the capture is not tried ([`not_held`]); `None`
/// tests no capture.
pub fn choose_now(
    modes: &ModeRegistry,
    weights: &Weights,
    c: &Classification,
    sel: &Selection,
    region: &str,
    capture: Option<(Hz, Hz)>,
) -> Choice {
    let Some(m) = c.measurement.as_ref() else {
        return cannot_tell(
            "layer 1 did not measure the band".into(),
            vec![],
            vec![],
            vec![],
        );
    };
    let (label, f) = (m.label.name.as_str(), &m.features);
    let (center_hz, bandwidth_hz) = (f.center_hz.0, f.bandwidth_hz.0);
    let layer1 = format!("layer 1: {label} {bandwidth_hz} Hz");
    let catalogue: Vec<(&str, &Label)> = c
        .labels
        .iter()
        .filter_map(|l| catalogue_entry(l).map(|id| (id, l)))
        .collect();
    let catalogue_term = |id: &str| {
        let best = catalogue
            .iter()
            .filter(|(e, _)| *e == id)
            .map(|(_, l)| l.confidence)
            .fold(0.0_f32, f32::max);
        weights.catalogue * best
    };
    let allocation = |row: &LoadedMode| allocation_term(row, &c.allocations, weights);
    let proved = c
        .tried
        .iter()
        .find(|t| t.outcome == Outcome::Proved)
        .and_then(|t| modes.get(&t.mode));
    // The decoder rows a later step could prove: they fit the label (except for
    // `unknown`, below) and the width, and are not the row already proved.
    let at = (center_hz, bandwidth_hz, label);
    let decoders = score_rows(
        weights,
        modes,
        at.0,
        at.1,
        at.2,
        &catalogue_term,
        &allocation,
    );
    // For `unknown` alone the label term is waived: a row is listed to be tried when its
    // width window holds the occupied width, `power99_hz` or the selection's width.
    let unknown = label == "unknown";
    let widths = [
        Some(bandwidth_hz),
        f.power99_hz.map(|w| w.0),
        Some(sel.bandwidth_hz.0),
    ];
    let width_fits = |s: &Scored| {
        let bw = s.mode.row.band.bandwidth_hz as f64;
        s.fits_width
            || (unknown
                && widths
                    .iter()
                    .flatten()
                    .any(|&w| (bw * 0.5..=bw * 1.5).contains(&(w as f64))))
    };
    let (ready, unready): (Vec<&Scored>, Vec<&Scored>) = decoders
        .iter()
        .filter(|s| {
            s.in_range
                && width_fits(s)
                && (s.fits_label || unknown)
                && !passed_over(s.mode, None)
                && proved.is_none_or(|p| p.row.id != s.mode.row.id)
        })
        .partition(|s| s.mode.startable());
    // A ready row is tried at its own band: one the capture does not hold is not tried.
    let held = |s: &&Scored| {
        capture.is_none_or(|cap| capture_holds(cap, job_band(&job_for(s.mode, m, sel, region))))
    };
    let (ready, outside): (Vec<&Scored>, Vec<&Scored>) = ready.into_iter().partition(held);
    // An `fsk` wider than a row's `voice_max_hz` is data, not a weak FM voice: the row
    // (`fm_demod`'s 25 kHz) does not serve it.
    let fsk_data = |row: &LoadedMode| {
        label == "fsk"
            && row
                .row
                .band
                .width_rule
                .as_ref()
                .and_then(|w| w.voice_max_hz)
                .is_some_and(|max| bandwidth_hz > max)
    };
    let mut serving = score_demodulators(
        weights,
        modes,
        at.0,
        at.1,
        at.2,
        &catalogue_term,
        &allocation,
    );
    // What the register and the plans expect at the centre: only an expectation some
    // demodulator row fits at the measured width speaks for or against the label.
    let mut expected = expected_labels(&c.allocations, center_hz, region);
    expected.retain(|x| {
        serving
            .iter()
            .any(|s| expected_fits(s.mode, x, bandwidth_hz))
    });
    let contradicted = !expected.is_empty() && expected.iter().all(|(_, l)| !l.contains(&label));
    if contradicted {
        for s in &mut serving {
            let own = allocation(s.mode);
            if expected
                .iter()
                .any(|x| expected_fits(s.mode, x, bandwidth_hz))
            {
                // The expected width fits the row, and a misread's measured width is what
                // is in doubt: the width term counts. The expectation holding the centre
                // counts as a licence at rank 0 (a plan's too), never twice.
                if !s.fits_width {
                    s.score += weights.bandwidth;
                }
                s.score += (weights.allocation - own).max(0.0);
            } else if s.fits_label {
                // A licence elsewhere in the band does not speak for the label against
                // the ones that hold the centre.
                s.score -= own;
            }
            if s.fits_label {
                s.score -= weights.modulation * (1.0 - weights.contradicted);
            }
        }
        rank_scored(&mut serving);
    }
    // The expectations that bring a demodulator row the label does not serve.
    let brought = |row: &LoadedMode| -> Vec<&(Expected, Vec<&'static str>)> {
        if row.role() != Role::Demodulator || row.row.modulation.iter().any(|l| l == label) {
            return vec![];
        }
        expected
            .iter()
            .filter(|x| expected_fits(row, x, bandwidth_hz))
            .collect()
    };
    let (demods, unready_demods): (Vec<&Scored>, Vec<&Scored>) = serving
        .iter()
        .filter(|s| {
            s.in_range && (s.fits_label || !brought(s.mode).is_empty()) && !fsk_data(s.mode)
        })
        .partition(|s| s.mode.startable());
    // The rows a best guess may name: those that fit the label. A row listed for `unknown`
    // alone is tried, never started on this evidence.
    let guessable: Vec<&Scored> = ready.iter().copied().filter(|s| s.fits_label).collect();
    let could_prove: Vec<String> = ready.iter().map(|s| s.mode.row.id.0.clone()).collect();
    let not_tried: Vec<NotTried> = unready
        .iter()
        .chain(&unready_demods)
        .map(|s| not_ready(s.mode))
        .chain(
            outside
                .iter()
                .map(|s| not_held(&job_for(s.mode, m, sel, region))),
        )
        .collect();
    // The catalogue's names for the entries `row` lists: the catalogue is evidence for
    // a row only through them.
    let named = |row: &LoadedMode| -> Vec<String> {
        catalogue
            .iter()
            .filter(|(id, _)| row.row.catalogue.iter().any(|c| c == id))
            .map(|(_, l)| format!("catalogue: {}", l.name))
            .collect()
    };
    // A row's job and its evidence lines: a demodulator the expectation brought is sized
    // as for the expected label it serves (an FM channel's width, not a carrier's), and
    // says what was expected.
    let job_lines = |row: &LoadedMode| -> (ChoiceJob, Vec<String>) {
        let by = brought(row);
        let as_label = by
            .iter()
            .flat_map(|(_, labels)| labels.iter())
            .find(|l| row.row.modulation.iter().any(|m| m == *l));
        let Some(as_label) = as_label else {
            return sized(row, m, sel, region);
        };
        let mut relabelled = m.clone();
        relabelled.label.name = (*as_label).to_string();
        let (job, mut lines) = sized(row, &relabelled, sel, region);
        lines.extend(by.iter().map(|(x, _)| x.line(label)));
        (job, lines)
    };
    // An alternative's evidence: layer 1, its plan lines and its row's catalogue names.
    let alternative = |row: &LoadedMode| {
        let (job, lines) = job_lines(row);
        Alternative {
            job,
            row: row.row.id.0.clone(),
            evidence: std::iter::once(layer1.clone())
                .chain(lines)
                .chain(named(row))
                .collect(),
        }
    };
    let decided = if let Some(row) = proved {
        let decode = format!("decode {}: ", row.row.id.0);
        let summary: Vec<String> = c
            .labels
            .iter()
            .filter(|l| l.evidence.starts_with(&decode))
            .map(|l| l.evidence.clone())
            .collect();
        let runner_up = demods.first().map(|d| alternative(d.mode));
        Ok((row, Settled::Proved, summary, runner_up))
    } else if label == "noise" {
        Err("the band holds noise only".to_string())
    } else if let Some(top) = demods.first() {
        let runner_up = guessable
            .first()
            .filter(|d| top.score - d.score <= weights.runner_up_margin)
            .or_else(|| demods.get(1))
            .map(|d| alternative(d.mode));
        Ok((top.mode, Settled::BestGuess, vec![], runner_up))
    } else if let Some(top) = guessable.first() {
        let runner_up = guessable.get(1).map(|d| alternative(d.mode));
        Ok((top.mode, Settled::BestGuess, vec![], runner_up))
    } else if outside.iter().any(|s| s.fits_label) {
        // A row serves it, but its band reaches past the capture.
        Err(format!("no row the capture holds serves {label}"))
    } else {
        Err(format!("no row serves {label}"))
    };
    let mut evidence = vec![layer1.clone()];
    let (row, settled, decode, runner_up) = match decided {
        Ok(d) => d,
        Err(reason) => return cannot_tell(reason, evidence, could_prove, not_tried),
    };
    let (job, plan) = job_lines(row);
    evidence.extend(decode);
    evidence.extend(plan);
    evidence.extend(named(row));
    Choice {
        job: Some(job),
        row: Some(row.row.id.0.clone()),
        step: 1,
        settled,
        evidence,
        runner_up,
        could_prove,
        not_tried,
        reason: None,
        steps_run: vec![1],
    }
}

/// A choice with no job, for `reason`.
fn cannot_tell(
    reason: String,
    evidence: Vec<String>,
    could_prove: Vec<String>,
    not_tried: Vec<NotTried>,
) -> Choice {
    Choice {
        job: None,
        row: None,
        step: 1,
        settled: Settled::CannotTell,
        evidence,
        runner_up: None,
        could_prove,
        not_tried,
        reason: Some(reason),
        steps_run: vec![1],
    }
}

/// `not tried: <row> is not ready: <reason>`.
fn not_ready(row: &LoadedMode) -> NotTried {
    let id = &row.row.id.0;
    let why = row.reason.as_deref().unwrap_or("not ready");
    NotTried {
        row: Some(id.clone()),
        reason: format!("not tried: {id} is not ready: {why}"),
    }
}

/// How [`not_held`]'s reason starts.
pub const NOT_HELD_LEAD: &str = "not tried: the capture does not hold ";

/// `not tried: the capture does not hold <row>'s <width> Hz here`, for a decoder row
/// whose own band `job` reaches past the capture.
fn not_held(job: &ChoiceJob) -> NotTried {
    NotTried {
        row: Some(job.kind.clone()),
        reason: format!(
            "{NOT_HELD_LEAD}{}'s {} Hz here",
            job.kind, job.bandwidth_hz.0
        ),
    }
}

/// A job's band.
pub(crate) fn job_band(job: &ChoiceJob) -> Tuning {
    Tuning {
        center_hz: job.center_hz,
        bandwidth_hz: job.bandwidth_hz,
    }
}

/// Whether `band` lies inside a capture centred `center` holding `rate` Hz, touching an
/// edge counting as inside (the band cutter's own test, [`crate::cut::BandCutter`]):
/// worked doubled, so an odd width is exact.
pub(crate) fn capture_holds((center, rate): (Hz, Hz), band: Tuning) -> bool {
    let (c, w) = (
        i128::from(band.center_hz.0),
        i128::from(band.bandwidth_hz.0),
    );
    let (cc, cw) = (i128::from(center.0), i128::from(rate.0));
    2 * c - w >= 2 * cc - cw && 2 * c + w <= 2 * cc + cw
}

/// The smallest band holding every band of `bands`; `None` for none. Its centre is
/// whole Hz, so an odd span is widened by a hertz rather than cut short.
pub(crate) fn union_band(bands: impl IntoIterator<Item = Tuning>) -> Option<Tuning> {
    // Doubled edges: a band (c, w) spans [2c - w, 2c + w].
    let (lo, hi) = bands
        .into_iter()
        .fold(None, |acc: Option<(i128, i128)>, b| {
            let (c, w) = (i128::from(b.center_hz.0), i128::from(b.bandwidth_hz.0));
            let (l, h) = (2 * c - w, 2 * c + w);
            Some(acc.map_or((l, h), |(lo, hi)| (lo.min(l), hi.max(h))))
        })?;
    let c = (lo + hi).div_euclid(4);
    let w = (hi - 2 * c).max(2 * c - lo);
    Some(Tuning {
        center_hz: Hz(u64::try_from(c).ok()?),
        bandwidth_hz: Hz(u64::try_from(w).ok()?),
    })
}

/// `band`, a union of bands the capture `(centre, rate)` holds, brought inside the
/// capture: its whole-Hz centre can leave it up to 1.5 Hz past an edge ([`union_band`]).
/// It is cut back to the capture and centred again on whichever whole hertz keeps it
/// inside, still holding every band it held there; failing both, the capture itself.
pub(crate) fn inside_capture(band: Tuning, capture: (Hz, Hz)) -> Tuning {
    if capture_holds(capture, band) {
        return band;
    }
    let whole = Tuning {
        center_hz: capture.0,
        bandwidth_hz: capture.1,
    };
    // Doubled edges, as in `union_band`.
    let edges = |t: Tuning| {
        let (c, w) = (i128::from(t.center_hz.0), i128::from(t.bandwidth_hz.0));
        (2 * c - w, 2 * c + w)
    };
    let ((lo, hi), (clo, chi)) = (edges(band), edges(whole));
    let (lo, hi) = (lo.max(clo), hi.min(chi));
    let mid = lo + hi;
    [mid.div_euclid(4), (mid + 3).div_euclid(4)]
        .into_iter()
        .filter_map(|c| {
            let w = (hi - 2 * c).max(2 * c - lo);
            Some(Tuning {
                center_hz: Hz(u64::try_from(c).ok()?),
                bandwidth_hz: Hz(u64::try_from(w).ok()?),
            })
        })
        .find(|t| lo <= hi && capture_holds(capture, *t))
        .unwrap_or(whole)
}

/// The band step 2's slice is cut to: the smallest band holding every `could_prove`
/// row's own band ([`job_for`] for the measurement `m` in `sel`); `None` when no row
/// is listed (or none is in `modes`).
pub(crate) fn trial_band(
    modes: &ModeRegistry,
    choice: &Choice,
    m: &Measurement,
    sel: &Selection,
    region: &str,
) -> Option<Tuning> {
    union_band(trial_bands(modes, choice, m, sel, region))
}

/// Each `could_prove` row's own band ([`job_for`]), in order; rows not in `modes` left out.
pub(crate) fn trial_bands(
    modes: &ModeRegistry,
    choice: &Choice,
    m: &Measurement,
    sel: &Selection,
    region: &str,
) -> Vec<Tuning> {
    choice
        .could_prove
        .iter()
        .filter_map(|row| modes.get(row))
        .map(|row| job_band(&job_for(row, m, sel, region)))
        .collect()
}

/// The job of `row` for the band `m` measured in `sel` (design 5.1): a demodulator row
/// sized and centred by its width rule, with its parameters (the deviation of a plan
/// channel of `region` at the centre for `fm_demod`, the measured sideband for
/// `ssb_demod`); a decoder row at its own width, on the measured centre (on the
/// selection's for an `as_asked` row).
pub fn job_for(row: &LoadedMode, m: &Measurement, sel: &Selection, region: &str) -> ChoiceJob {
    sized(row, m, sel, region).0
}

/// [`job_for`], with the evidence lines a plan channel added.
fn sized(
    row: &LoadedMode,
    m: &Measurement,
    sel: &Selection,
    region: &str,
) -> (ChoiceJob, Vec<String>) {
    let f = &m.features;
    let band = &row.row.band;
    let builtin = match &row.row.decoder {
        Some(Decoder::Builtin(b)) => Some(b),
        _ => None,
    };
    let (center_hz, bandwidth_hz) = match &band.width_rule {
        Some(rule) => {
            let center = match rule.center {
                CenterRule::Measured => f.center_hz,
                CenterRule::Dial => f.ssb.map_or(f.center_hz, |s| s.dial_hz),
            };
            (center, Hz(rule_width(rule, m, sel, center, region)))
        }
        None => {
            let center = match row.row.identify.as_asked {
                true => sel.center_hz,
                false => f.center_hz,
            };
            (center, Hz(band.bandwidth_hz.max(band.min_hz.unwrap_or(0))))
        }
    };
    let mut params = builtin.map(|b| b.params.clone()).unwrap_or_default();
    let mut lines = Vec::new();
    match builtin.map(|b| b.builtin.as_str()) {
        Some("fm_demod") => {
            let plan = crate::plans::plan_channel_at(center_hz.0, region);
            if let Some((plan, d)) = plan.and_then(|(p, _)| p.deviation_hz.map(|d| (p, d))) {
                params.insert("deviation_hz".into(), d.into());
                lines.push(format!("plan: {} deviation {d}", plan.id));
            }
        }
        Some("ssb_demod") => {
            if let Some(s) = f.ssb {
                let side = serde_json::to_value(s.sideband).expect("a sideband serialises");
                params.insert("sideband".into(), side);
            }
        }
        _ => {}
    }
    let job = ChoiceJob {
        kind: row.row.id.0.clone(),
        center_hz,
        bandwidth_hz,
        params,
    };
    (job, lines)
}

/// The width `rule` gives the measurement `m` centred at `center_hz`: `fixed_hz`; else the
/// occupied width times `scale`, no narrower than `floor_hz`. An occupied width under the
/// floor is not trusted for a label in `distrust_below_floor`: the width of the plan
/// channel of `region` at the centre, else `power99_hz` when it reaches the floor, else
/// the selection's width, never under the floor.
fn rule_width(
    rule: &WidthRule,
    m: &Measurement,
    sel: &Selection,
    center_hz: Hz,
    region: &str,
) -> u64 {
    if let Some(fixed) = rule.fixed_hz {
        return fixed;
    }
    let occupied = m.features.bandwidth_hz.0;
    let floor = rule.floor_hz.unwrap_or(0);
    if occupied < floor && rule.distrust_below_floor.contains(&m.label.name) {
        let plan = crate::plans::plan_channel_at(center_hz.0, region).map(|(p, _)| p.bandwidth_hz);
        let power99 = m.features.power99_hz.map(|w| w.0).filter(|&w| w >= floor);
        return plan.or(power99).unwrap_or(sel.bandwidth_hz.0).max(floor);
    }
    let scaled = (occupied as f64 * rule.scale.unwrap_or(1.0)).round() as u64;
    scaled.max(floor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::FeatureRecord;
    use crate::stage::StageRegistry;
    use crate::types::{SourceId, Span, Timestamp};

    /// A UHF CB channel centre (`uhf_cb_au` ch 10).
    const CB_HZ: u64 = 476_650_000;
    /// Well away from any plan channel.
    const OFF_PLAN_HZ: u64 = 433_920_000;

    fn measured(
        label: &str,
        center_hz: u64,
        bandwidth_hz: u64,
        power99_hz: Option<u64>,
    ) -> Measurement {
        Measurement {
            features: FeatureRecord {
                bandwidth_hz: Hz(bandwidth_hz),
                center_hz: Hz(center_hz),
                symbol_rate_baud: None,
                snr_db: 20.0,
                burst: false,
                duty: None,
                bursts: None,
                burst_ms: None,
                period_ms: None,
                order: None,
                tone_spacing_hz: None,
                symbol_rate_line: None,
                power99_hz: power99_hz.map(Hz),
                audio_rms_hz: None,
                ssb: None,
            },
            label: Label {
                layer: 1,
                name: label.into(),
                confidence: 0.6,
                evidence: String::new(),
            },
        }
    }

    fn box_at(center_hz: u64, bandwidth_hz: u64) -> Selection {
        Selection {
            source: SourceId("rx".into()),
            span: Span {
                start_ns: Timestamp(0),
                end_ns: Timestamp(1),
            },
            center_hz: Hz(center_hz),
            bandwidth_hz: Hz(bandwidth_hz),
        }
    }

    fn width(row: &str, m: &Measurement, sel: &Selection) -> u64 {
        let modes = ModeRegistry::load(&StageRegistry::new(), None);
        job_for(modes.get(row).unwrap(), m, sel, "AU")
            .bandwidth_hz
            .0
    }

    fn band(center_hz: u64, bandwidth_hz: u64) -> Tuning {
        Tuning {
            center_hz: Hz(center_hz),
            bandwidth_hz: Hz(bandwidth_hz),
        }
    }

    /// The union of bands holds each whole, at a whole-Hz centre; none is no band.
    #[test]
    fn the_trial_band_is_the_smallest_holding_every_band() {
        assert_eq!(union_band([]), None);
        assert_eq!(union_band([band(1_000, 100)]), Some(band(1_000, 100)));
        // 950-1050 and 1000-1200: 950-1200.
        assert_eq!(
            union_band([band(1_000, 100), band(1_100, 200)]),
            Some(band(1_075, 250))
        );
        // An odd span (0-3 Hz) cannot be centred on a whole hertz: widened by one.
        let u = union_band([band(1, 2), band(2, 2)]).unwrap();
        assert_eq!(u, band(1, 4));
        for b in [band(1, 2), band(2, 2)] {
            assert!(
                capture_holds((u.center_hz, u.bandwidth_hz), b),
                "{u:?} {b:?}"
            );
        }
    }

    /// A union whose whole-Hz centre pushes it half a hertz past the capture's lower edge
    /// is brought back inside, still holding every band it was made of; one the capture
    /// already holds is kept as it is.
    #[test]
    fn a_union_rounded_past_the_capture_is_brought_inside() {
        let cap = (Hz(1_000_000), Hz(48_000));
        // One band on the lower edge, one whose odd width leaves the union's centre a
        // quarter hertz off a whole one.
        let (a, b) = (band(982_250, 12_500), band(982_501, 12_503));
        assert!(capture_holds(cap, a) && capture_holds(cap, b));
        let u = union_band([a, b]).unwrap();
        assert!(!capture_holds(cap, u), "{u:?}");
        let inside = inside_capture(u, cap);
        assert!(capture_holds(cap, inside), "{inside:?}");
        for b in [a, b] {
            assert!(
                capture_holds((inside.center_hz, inside.bandwidth_hz), b),
                "{inside:?} {b:?}"
            );
        }
        assert_eq!(inside_capture(a, cap), a);
    }

    /// When the only decoder that would serve a label is one the capture does not hold,
    /// the reason says the capture is short, not that no row serves it.
    #[test]
    fn a_decoder_only_label_whose_rows_the_capture_does_not_hold_says_so() {
        const VDL2_HZ: u64 = 136_975_000;
        let mut modes = ModeRegistry::load(&StageRegistry::new(), None);
        modes.set_program("vdl2_decode", Some("/usr/bin/vdl2".into()), None);
        let c = Classification {
            measurement: Some(measured("psk", VDL2_HZ, 18_000, None)),
            ..Classification::default()
        };
        let sel = box_at(VDL2_HZ, 20_000);
        // The signal 4 kHz inside the capture's upper edge: the row's 25 kHz is not held.
        let cap = Some((Hz(VDL2_HZ - 20_000), Hz(48_000)));
        let choice = choose_now(&modes, modes.weights(), &c, &sel, "AU", cap);
        assert_eq!(choice.settled, Settled::CannotTell, "{choice:?}");
        assert_eq!(
            choice.reason.as_deref(),
            Some("no row the capture holds serves psk"),
            "{choice:?}"
        );
        assert!(
            choice
                .not_tried
                .iter()
                .any(|n| n.row.as_deref() == Some("vdl2_decode")
                    && n.reason
                        == "not tried: the capture does not hold vdl2_decode's 25000 Hz here"),
            "{choice:?}"
        );
        // Held, it is the best guess as before.
        let held = choose_now(&modes, modes.weights(), &c, &sel, "AU", None);
        assert_eq!(held.row.as_deref(), Some("vdl2_decode"), "{held:?}");
    }

    /// A band touching the capture's edge is held; one hertz past it is not.
    #[test]
    fn a_capture_holds_a_band_up_to_its_edges() {
        let cap = (Hz(1_000_000), Hz(48_000));
        assert!(capture_holds(cap, band(1_000_000, 48_000)));
        assert!(capture_holds(cap, band(1_024_000 - 6_250, 12_500)));
        assert!(!capture_holds(cap, band(1_024_000 - 6_249, 12_500)));
        assert!(!capture_holds(cap, band(976_000 + 6_249, 12_500)));
        assert!(!capture_holds(cap, band(1_000_000, 48_001)));
    }

    #[test]
    fn a_trusted_width_is_scaled_and_floored() {
        let sel = box_at(OFF_PLAN_HZ, 50_000);
        assert_eq!(
            width("fm_demod", &measured("fm", OFF_PLAN_HZ, 8_000, None), &sel),
            10_000
        );
        assert_eq!(
            width("am_demod", &measured("am", OFF_PLAN_HZ, 2_000, None), &sel),
            5_000
        );
        let wide = box_at(OFF_PLAN_HZ, 240_000);
        assert_eq!(
            width(
                "wfm_demod",
                &measured("wfm", OFF_PLAN_HZ, 100_000, None),
                &wide
            ),
            150_000
        );
        assert_eq!(
            width(
                "ssb_demod",
                &measured("ssb", OFF_PLAN_HZ, 2_200, None),
                &sel
            ),
            6_000
        );
    }

    #[test]
    fn a_distrusted_width_takes_the_plan_channel_then_power99_then_the_box() {
        let carrier_line = |center, p99| measured("fm", center, 900, p99);
        // On a plan channel: the channel's width.
        assert_eq!(
            width(
                "fm_demod",
                &carrier_line(CB_HZ + 700, None),
                &box_at(CB_HZ, 50_000)
            ),
            12_500
        );
        // Off any plan: the 99 % width when it reaches the floor, else the box's.
        let sel = box_at(OFF_PLAN_HZ, 20_000);
        assert_eq!(
            width("fm_demod", &carrier_line(OFF_PLAN_HZ, Some(7_000)), &sel),
            7_000
        );
        assert_eq!(
            width("fm_demod", &carrier_line(OFF_PLAN_HZ, Some(3_000)), &sel),
            20_000
        );
        // Never under the floor; an AM width under it is trusted, and floored.
        assert_eq!(
            width(
                "fm_demod",
                &carrier_line(OFF_PLAN_HZ, None),
                &box_at(OFF_PLAN_HZ, 3_000)
            ),
            5_000
        );
        assert_eq!(
            width("am_demod", &measured("am", CB_HZ, 900, None), &sel),
            5_000
        );
    }

    #[test]
    fn with_no_demodulator_the_label_takes_a_ready_decoder_or_names_the_unready_one() {
        // A VDL2-shaped psk signal on the airband: no demodulator row serves `psk`.
        const VDL2_HZ: u64 = 136_975_000;
        let mut modes = ModeRegistry::load(&StageRegistry::new(), None);
        let c = Classification {
            measurement: Some(measured("psk", VDL2_HZ, 18_000, None)),
            ..Classification::default()
        };
        let sel = box_at(VDL2_HZ, 25_000);
        let choose =
            |modes: &ModeRegistry| choose_now(modes, modes.weights(), &c, &sel, "AU", None);
        modes.set_program("vdl2_decode", None, Some("vdl2: missing".into()));
        let onair = Some("onair-rx: missing".into());
        modes.set_handler_program("vdl2_decode", "onair", None, onair);
        let unready = choose(&modes);
        assert_eq!(unready.settled, Settled::CannotTell, "{unready:?}");
        assert_eq!(unready.reason.as_deref(), Some("no row serves psk"));
        assert_eq!(
            unready.not_tried,
            [NotTried {
                row: Some("vdl2_decode".into()),
                reason: "not tried: vdl2_decode is not ready: vdl2: missing".into(),
            }]
        );
        modes.set_program("vdl2_decode", Some("/usr/bin/vdl2".into()), None);
        let ready = choose(&modes);
        assert_eq!(ready.settled, Settled::BestGuess, "{ready:?}");
        let job = ready.job.as_ref().expect("a job");
        assert_eq!(job.kind, "vdl2_decode");
        // A decoder row runs at its own width, on the measured centre.
        assert_eq!((job.center_hz, job.bandwidth_hz), (Hz(VDL2_HZ), Hz(25_000)));
        assert_eq!(ready.could_prove, ["vdl2_decode"]);
    }

    #[test]
    fn a_label_nothing_serves_cannot_tell() {
        let modes = ModeRegistry::load(&StageRegistry::new(), None);
        let c = Classification {
            measurement: Some(measured("carrier", OFF_PLAN_HZ, 50, None)),
            ..Classification::default()
        };
        let sel = box_at(OFF_PLAN_HZ, 12_500);
        let choice = choose_now(&modes, modes.weights(), &c, &sel, "AU", None);
        assert_eq!(choice.reason.as_deref(), Some("no row serves carrier"));
        assert!(choice.job.is_none() && choice.row.is_none(), "{choice:?}");
        assert_eq!(choice.evidence, ["layer 1: carrier 50 Hz"]);
    }

    type Edit = fn(&mut Value);

    /// A registry of the embedded rows and, from an operator directory, a copy of each
    /// `(embedded row, id, edit)`, every copy loaded ready.
    fn with_rows(rows: &[(&str, &str, Edit)]) -> ModeRegistry {
        let dir = tempfile::tempdir().unwrap();
        let embedded = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modes");
        for (base, id, edit) in rows {
            let text = std::fs::read_to_string(embedded.join(format!("{base}.json"))).unwrap();
            let mut row: Value = serde_json::from_str(&text).expect("an embedded row parses");
            row["id"] = (*id).into();
            edit(&mut row);
            std::fs::write(dir.path().join(format!("{id}.json")), row.to_string()).unwrap();
        }
        let modes = ModeRegistry::load(&StageRegistry::new(), Some(dir.path()));
        for (_, id, _) in rows {
            let m = modes.get(id).expect("the copy loads");
            assert!(m.startable(), "{id}: {:?} {:?}", m.state, m.reason);
        }
        modes
    }

    /// `choose_now` over a band measured `label`, `bandwidth_hz` wide at `center_hz`, in a
    /// box `box_hz` wide.
    fn choose(
        modes: &ModeRegistry,
        label: &str,
        center_hz: u64,
        bandwidth_hz: u64,
        box_hz: u64,
    ) -> Choice {
        let c = Classification {
            measurement: Some(measured(label, center_hz, bandwidth_hz, None)),
            ..Classification::default()
        };
        choose_now(
            modes,
            modes.weights(),
            &c,
            &box_at(center_hz, box_hz),
            "AU",
            None,
        )
    }

    /// A radiosonde decoder row (fm and fsk, 12500 Hz) moved to 433-435 MHz.
    fn near_decoder(row: &mut Value) {
        row["band"]["range_hz"] = serde_json::json!([433_000_000, 435_000_000]);
    }

    /// [`near_decoder`] listing a well-known centre away from [`OFF_PLAN_HZ`]: it scores
    /// nothing for the centre there.
    fn off_centre_decoder(row: &mut Value) {
        near_decoder(row);
        row["band"]["centers_hz"] = serde_json::json!([434_900_000]);
    }

    fn unchanged(_: &mut Value) {}

    #[test]
    fn a_decoder_within_the_margin_is_the_runner_up() {
        let modes = with_rows(&[("rs41_decode", "near_decode", near_decoder)]);
        let choice = choose(&modes, "fm", OFF_PLAN_HZ, 10_000, 25_000);
        assert_eq!(choice.row.as_deref(), Some("fm_demod"), "{choice:?}");
        assert_eq!(choice.could_prove, ["near_decode"]);
        // Both score 1.0: within `runner_up_margin` (0.15).
        let runner_up = choice.runner_up.as_ref().expect("a runner-up");
        assert_eq!(runner_up.row, "near_decode");
        assert_eq!(runner_up.job.bandwidth_hz, Hz(12_500));
    }

    #[test]
    fn a_decoder_outside_the_margin_gives_way_to_the_next_demodulator() {
        let modes = with_rows(&[
            ("rs41_decode", "near_decode", off_centre_decoder),
            ("fm_demod", "fm_narrow", unchanged),
        ]);
        let choice = choose(&modes, "fm", OFF_PLAN_HZ, 10_000, 25_000);
        assert_eq!(choice.row.as_deref(), Some("fm_demod"), "{choice:?}");
        // 0.6 against the demodulator's 1.0: a row that could prove, not the runner-up.
        assert_eq!(choice.could_prove, ["near_decode"]);
        let runner_up = choice.runner_up.as_ref().expect("a runner-up");
        assert_eq!(runner_up.row, "fm_narrow", "{choice:?}");
    }

    #[test]
    fn with_no_demodulator_the_runner_up_is_the_next_decoder() {
        const VDL2_HZ: u64 = 136_975_000;
        let modes = with_rows(&[("vdl2_decode", "vdl2_copy", unchanged)]);
        let choice = choose(&modes, "psk", VDL2_HZ, 18_000, 25_000);
        assert_eq!(choice.settled, Settled::BestGuess, "{choice:?}");
        assert_eq!(choice.could_prove, ["vdl2_copy", "vdl2_decode"]);
        assert_eq!(choice.row.as_deref(), Some("vdl2_copy"));
        let runner_up = choice.runner_up.as_ref().expect("a runner-up");
        assert_eq!(runner_up.row, "vdl2_decode");
    }

    /// A demodulator row serving fsk that is not `fm_demod`: the wide-fsk rule leaves it be.
    fn fsk_wideband_demod(row: &mut Value) {
        row["modulation"] = serde_json::json!(["fsk"]);
    }

    /// [`near_decoder`] 50 kHz wide, for a wide data signal.
    fn wide_decoder(row: &mut Value) {
        near_decoder(row);
        row["band"]["bandwidth_hz"] = 50_000.into();
    }

    #[test]
    fn a_wide_fsk_is_not_served_by_fm_demod() {
        let embedded = ModeRegistry::load(&StageRegistry::new(), None);
        // At a voice channel's width an fsk label may be a weak FM voice.
        let narrow = choose(&embedded, "fsk", OFF_PLAN_HZ, 12_000, 100_000);
        assert_eq!(narrow.row.as_deref(), Some("fm_demod"), "{narrow:?}");
        // Over 25 kHz it is data: nothing embedded serves it here.
        let wide = choose(&embedded, "fsk", OFF_PLAN_HZ, 40_000, 100_000);
        assert_eq!(
            wide.reason.as_deref(),
            Some("no row serves fsk"),
            "{wide:?}"
        );
        // A decoder that fits takes it, unproved.
        let decoders = with_rows(&[("rs41_decode", "wide_decode", wide_decoder)]);
        let decoded = choose(&decoders, "fsk", OFF_PLAN_HZ, 40_000, 100_000);
        assert_eq!(decoded.row.as_deref(), Some("wide_decode"), "{decoded:?}");
        assert_eq!(decoded.settled, Settled::BestGuess);
        // The rule is `fm_demod`'s: another demodulator row serving fsk still serves it.
        let both = with_rows(&[
            ("rs41_decode", "wide_decode", wide_decoder),
            ("wfm_demod", "fsk_wideband", fsk_wideband_demod),
        ]);
        let demod = choose(&both, "fsk", OFF_PLAN_HZ, 40_000, 100_000);
        assert_eq!(demod.row.as_deref(), Some("fsk_wideband"), "{demod:?}");
    }

    /// The fsk cap is the row's data: the embedded `fm_demod` carries 25 kHz, and a
    /// variant of it with no `voice_max_hz` serves a wide fsk as well.
    #[test]
    fn the_fsk_voice_cap_is_the_rows_width_rule() {
        let embedded = ModeRegistry::load(&StageRegistry::new(), None);
        let rule = embedded
            .get("fm_demod")
            .unwrap()
            .row
            .band
            .width_rule
            .clone();
        assert_eq!(rule.and_then(|r| r.voice_max_hz), Some(25_000));
        let uncapped = |row: &mut Value| {
            row["band"]["width_rule"]["voice_max_hz"] = Value::Null;
        };
        let modes = with_rows(&[("fm_demod", "fm_any_fsk", uncapped)]);
        let wide = choose(&modes, "fsk", OFF_PLAN_HZ, 40_000, 100_000);
        assert_eq!(wide.row.as_deref(), Some("fm_any_fsk"), "{wide:?}");
    }
}
