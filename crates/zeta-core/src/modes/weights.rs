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
}

impl Weights {
    /// Parse a weights file; every key is required and none may be unknown.
    pub fn parse(text: &str) -> Result<Weights, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
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
    }

    #[test]
    fn an_operator_weights_file_overrides_one_value() {
        let d = dir_with(
            r#"{"center":0.4,"bandwidth":0.3,"modulation":0.3,"allocation":0.2,
                "catalogue":0.25,"learned":0.3,"layer3":0.0,"runner_up_margin":0.3}"#,
        );
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
