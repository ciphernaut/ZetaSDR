//! The weights of the ranking terms: one table, embedded, which the operator's
//! `<modes_dir>/weights.json` replaces whole.

use serde::{Deserialize, Serialize};

/// The embedded table, `modes/weights.json` in the repo.
pub(crate) const EMBEDDED_WEIGHTS: &str = include_str!("../../../../modes/weights.json");

/// The file name of the operator's table in the modes directory.
pub(crate) const WEIGHTS_FILE: &str = "weights.json";

/// What each ranking term is worth. [`super::score::score_rows`] adds `center`,
/// `bandwidth`, `modulation` and `allocation`; the choice reads the rest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Weights {
    /// The measured centre lies near one of the row's centres or in its range.
    pub center: f32,
    /// The measured bandwidth lies within half to 1.5 times the row's.
    pub bandwidth: f32,
    /// The layer-1 label is one the row presents as.
    pub modulation: f32,
    /// A licensed transmitter on the band fits the row (divided by 1 + its rank).
    pub allocation: f32,
    /// The catalogue's match for the row.
    pub catalogue: f32,
    /// A learned signature's match for the row.
    pub learned: f32,
    /// The layer-3 classifier's vote for the row.
    pub layer3: f32,
    /// How far the best candidate must lead the runner-up to be called clear.
    pub runner_up_margin: f32,
    /// A factor from 0 to 1 the label's term (`modulation`) is multiplied by in the
    /// choice of a demodulator when every emission expected at the measured centre (the
    /// register's licences, an FM plan's channel) is incompatible with layer 1's label.
    /// Optional in a file: one without it takes [`CONTRADICTED`], the embedded value.
    #[serde(default = "contradicted")]
    pub contradicted: f32,
}

/// The embedded `contradicted`, and the value of a file that leaves it out. When the label
/// is contradicted, a demodulator row the expectation fits scores `center + bandwidth +
/// allocation` (0.9) at any width and rank, and a row serving the label at most `center +
/// bandwidth + contradicted * modulation`. For the expected row to lead clearly
/// (`runner_up_margin`), `allocation - contradicted * modulation >= runner_up_margin`:
/// `contradicted <= (0.2 - 0.15) / 0.3 = 1/6`. 0.15 leads by 0.155. When some expectation
/// agrees with the label nothing is adjusted, so the value does not reach that case.
///
/// The bound holds for the embedded weights. An operator's `weights.json` outside it (a
/// `contradicted` over `(allocation - runner_up_margin) / modulation` for its own
/// values) still loads as written, and nothing warns: the expected row then leads by less
/// than the margin, and once `contradicted * modulation` reaches `allocation` (for the
/// embedded others, `contradicted` of 2/3 or more) the label's own row stays first when
/// its width fits, so a contradicted label is no longer overturned.
pub(crate) const CONTRADICTED: f32 = 0.15;

fn contradicted() -> f32 {
    CONTRADICTED
}

impl Weights {
    /// Parse a weights file; every key but `contradicted` is required, none may be
    /// unknown, and `contradicted` lies from 0 to 1.
    pub fn parse(text: &str) -> Result<Weights, String> {
        let w: Weights = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if !(0.0..=1.0).contains(&w.contradicted) {
            return Err(format!(
                "contradicted is a factor from 0 to 1, not {}",
                w.contradicted
            ));
        }
        Ok(w)
    }
}

impl Default for Weights {
    /// The embedded table.
    fn default() -> Self {
        Weights::parse(EMBEDDED_WEIGHTS).expect("the embedded weights.json parses")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::{ModeRegistry, ModeState};
    use crate::stage::StageRegistry;

    /// An operator's file from before plan 49 (no `contradicted`), its margin 0.3.
    const BEFORE_PLAN_49: &str = r#"{"center":0.4,"bandwidth":0.3,"modulation":0.3,"allocation":0.2,
        "catalogue":0.25,"learned":0.3,"layer3":0.0,"runner_up_margin":0.3}"#;

    fn dir_with(text: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(WEIGHTS_FILE), text).unwrap();
        d
    }

    #[test]
    fn the_embedded_weights_are_the_old_constants() {
        let w = Weights::default();
        assert_eq!(
            (w.center, w.bandwidth, w.modulation, w.allocation),
            (0.4, 0.3, 0.3, 0.2)
        );
        assert_eq!(
            (w.catalogue, w.learned, w.layer3, w.runner_up_margin),
            (0.25, 0.3, 0.0, 0.15)
        );
        // Plan 49's key, and the value a file without it takes.
        assert_eq!(w.contradicted, 0.15);
        assert_eq!(w.contradicted, CONTRADICTED);
    }

    /// An operator's file from before plan 49 has no `contradicted`: it still loads, with
    /// the embedded value.
    #[test]
    fn a_weights_file_without_contradicted_still_loads() {
        let d = dir_with(BEFORE_PLAN_49);
        let reg = ModeRegistry::load(&StageRegistry::new(), Some(d.path()));
        assert!(
            reg.handler_files().iter().all(|f| f.id != "weights"),
            "{:?}",
            reg.handler_files()
        );
        assert_eq!(reg.weights().runner_up_margin, 0.3);
        assert_eq!(reg.weights().contradicted, Weights::default().contradicted);
    }

    /// `contradicted` is a factor from 0 to 1: a file outside that is a load error.
    #[test]
    fn a_contradicted_factor_outside_0_to_1_is_a_load_error() {
        let base = r#""center":0.4,"bandwidth":0.3,"modulation":0.3,"allocation":0.2,
                "catalogue":0.25,"learned":0.3,"layer3":0.0,"runner_up_margin":0.15"#;
        for (v, ok) in [
            ("0.0", true),
            ("1.0", true),
            ("1.5", false),
            ("-0.1", false),
        ] {
            let got = Weights::parse(&format!("{{{base},\"contradicted\":{v}}}"));
            assert_eq!(got.is_ok(), ok, "{v}: {got:?}");
            if let Err(e) = got {
                assert!(e.contains("contradicted"), "{e}");
            }
        }
    }

    #[test]
    fn an_operator_weights_file_overrides_one_value() {
        let d = dir_with(BEFORE_PLAN_49);
        let reg = ModeRegistry::load(&StageRegistry::new(), Some(d.path()));
        assert_eq!(reg.weights().runner_up_margin, 0.3);
        assert_eq!(reg.weights().center, 0.4);
        // The file is not listed as a row, nor as a broken file.
        assert!(reg.all().iter().all(|m| !m.file.ends_with(WEIGHTS_FILE)));
        assert!(reg.handler_files().iter().all(|f| f.id != "weights"));
    }

    #[test]
    fn a_weights_file_with_an_unknown_key_is_a_load_error() {
        let d = dir_with(
            r#"{"center":0.9,"bandwidth":0.3,"modulation":0.3,"allocation":0.2,
                "catalogue":0.25,"learned":0.3,"layer3":0.0,"runner_up_margin":0.3,"bogus":1}"#,
        );
        let reg = ModeRegistry::load(&StageRegistry::new(), Some(d.path()));
        assert_eq!(reg.weights(), &Weights::default());
        let f = reg
            .handler_files()
            .iter()
            .find(|f| f.id == "weights")
            .expect("listed");
        assert_eq!(f.state, ModeState::Broken);
        assert!(f.reason.as_deref().unwrap().contains("bogus"));
    }
}
