//! The choice (design 5.2): the one job a band's classification answers, with the
//! evidence for it, its runner-up and the decoder rows a later step could still prove.
//! Step 1 is what `classify` already holds: layer 1's measurement and the quick proofs.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::classify::{Classification, Label, Measurement, Outcome};
use crate::modes::score::{Scored, allocation_term, passed_over, score_demodulators, score_rows};
use crate::modes::weights::Weights;
use crate::modes::{CenterRule, Decoder, LoadedMode, ModeRegistry, WidthRule};
use crate::types::{Hz, Selection};

/// The widest `fsk` signal a demodulator row serves: under it an `fsk` label is as
/// likely a weak FM voice whose sidebands layer 1 did not see as a data signal.
pub const FSK_VOICE_MAX_HZ: u64 = 25_000;

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
    /// `decode <row>: <summary>`, `catalogue: <name>`.
    pub evidence: Vec<String>,
    #[schemars(required, schema_with = "crate::types::nullable::<Alternative>")]
    pub runner_up: Option<Alternative>,
    /// The ready decoder rows that fit the label, the band and the width, in score order:
    /// what a later step could still prove.
    pub could_prove: Vec<String>,
    pub not_tried: Vec<NotTried>,
    /// Why the choice cannot tell; `null` otherwise.
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub reason: Option<String>,
}

/// The catalogue entry a layer-2 catalogue label names (its evidence `catalogue <id>: ...`).
fn catalogue_entry(label: &Label) -> Option<&str> {
    if label.layer != 2 {
        return None;
    }
    label.evidence.strip_prefix("catalogue ")?.split(':').next()
}

/// The choice for the band `sel` of `c`, step 1: a quick proof wins; otherwise the
/// demodulator row that serves the layer-1 label best, sized by its width rule; otherwise
/// the best ready decoder row that fits; otherwise `cannot_tell`. Ranked by `weights`
/// (the catalogue only ranks, at `weights.catalogue`); a plan channel of `region` at the
/// measured centre gives the deviation, and the width when the measured one is distrusted.
pub fn choose_now(
    modes: &ModeRegistry,
    weights: &Weights,
    c: &Classification,
    sel: &Selection,
    region: &str,
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
    // The decoder rows a later step could prove: they fit the label, the band and the
    // width, and are not the row already proved.
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
    let (ready, unready): (Vec<&Scored>, Vec<&Scored>) = decoders
        .iter()
        .filter(|s| {
            s.in_range
                && s.fits_width
                && s.fits_label
                && s.mode.row.band.fits(sel.bandwidth_hz.0)
                && !passed_over(s.mode, None)
                && proved.is_none_or(|p| p.row.id != s.mode.row.id)
        })
        .partition(|s| s.mode.startable());
    // An `fsk` wider than a voice channel is data, not a weak FM voice.
    let fsk_data = label == "fsk" && bandwidth_hz > FSK_VOICE_MAX_HZ;
    let serving = score_demodulators(
        weights,
        modes,
        at.0,
        at.1,
        at.2,
        &catalogue_term,
        &allocation,
    );
    let (demods, unready_demods): (Vec<&Scored>, Vec<&Scored>) = serving
        .iter()
        .filter(|s| s.in_range && s.fits_label && !fsk_data)
        .partition(|s| s.mode.startable());
    let could_prove: Vec<String> = ready.iter().map(|s| s.mode.row.id.0.clone()).collect();
    let not_tried: Vec<NotTried> = unready
        .iter()
        .chain(&unready_demods)
        .map(|s| not_ready(s.mode))
        .collect();
    // An alternative's evidence: layer 1, its plan lines and the catalogue entries its
    // row lists.
    let alternative = |row: &LoadedMode| {
        let (job, lines) = sized(row, m, sel, region);
        let named = catalogue
            .iter()
            .filter(|(id, _)| row.row.catalogue.iter().any(|c| c == id))
            .map(|(_, l)| format!("catalogue: {}", l.name));
        Alternative {
            job,
            row: row.row.id.0.clone(),
            evidence: std::iter::once(layer1.clone())
                .chain(lines)
                .chain(named)
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
        let runner_up = ready
            .first()
            .filter(|d| top.score - d.score <= weights.runner_up_margin)
            .or_else(|| demods.get(1))
            .map(|d| alternative(d.mode));
        Ok((top.mode, Settled::BestGuess, vec![], runner_up))
    } else if let Some(top) = ready.first() {
        let runner_up = ready.get(1).map(|d| alternative(d.mode));
        Ok((top.mode, Settled::BestGuess, vec![], runner_up))
    } else {
        Err(format!("no row serves {label}"))
    };
    let mut evidence = vec![layer1.clone()];
    let (row, settled, decode, runner_up) = match decided {
        Ok(d) => d,
        Err(reason) => return cannot_tell(reason, evidence, could_prove, not_tried),
    };
    let (job, plan) = sized(row, m, sel, region);
    evidence.extend(decode);
    evidence.extend(plan);
    evidence.extend(
        catalogue
            .iter()
            .map(|(_, l)| format!("catalogue: {}", l.name)),
    );
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
    }
}

/// `<row> is not ready: <reason>`.
fn not_ready(row: &LoadedMode) -> NotTried {
    let id = &row.row.id.0;
    let why = row.reason.as_deref().unwrap_or("not ready");
    NotTried {
        row: Some(id.clone()),
        reason: format!("{id} is not ready: {why}"),
    }
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
        let choose = |modes: &ModeRegistry| choose_now(modes, modes.weights(), &c, &sel, "AU");
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
                reason: "vdl2_decode is not ready: vdl2: missing".into(),
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
        let choice = choose_now(&modes, modes.weights(), &c, &sel, "AU");
        assert_eq!(choice.reason.as_deref(), Some("no row serves carrier"));
        assert!(choice.job.is_none() && choice.row.is_none(), "{choice:?}");
        assert_eq!(choice.evidence, ["layer 1: carrier 50 Hz"]);
    }
}
