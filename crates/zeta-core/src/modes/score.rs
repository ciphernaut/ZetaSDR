//! Scoring the rows against a measured band (design 7.1): the order `classify` and
//! `identify` try the modes in.

use super::{LoadedMode, ModeRegistry, ModeState};

/// Score for the measured centre lying near one of the row's well-known centres
/// (granted when it lists none).
const CENTER_SCORE: f32 = 0.4;
/// Score for the measured bandwidth lying within half to 1.5 times the row's.
const BANDWIDTH_SCORE: f32 = 0.3;
/// Score for the layer-1 label being one the row presents as.
const MODULATION_SCORE: f32 = 0.3;

/// One row's score against a measured band.
#[derive(Clone, Debug)]
pub struct Scored<'a> {
    pub mode: &'a LoadedMode,
    pub score: f32,
    /// Whether the band passed to [`score_rows`] is wide enough for the row
    /// ([`super::Band::fits`]).
    pub fits_band: bool,
}

/// The `ready` and `unavailable` rows of `modes` (a broken row is never tried) scored
/// against a signal measured at `center_hz`, `bandwidth_hz` wide and labelled `layer1`:
/// 0.4 when `center_hz` lies within `band.bandwidth_hz` of one of `band.centers_hz` (or
/// the list is empty, "anywhere"); 0.3 when `bandwidth_hz` lies within 0.5-1.5 times
/// `band.bandwidth_hz`; 0.3 when `layer1` is in `modulation` (never for an empty list);
/// plus the best `catalogue(id)` among the row's `catalogue` ids, so a catalogue match
/// can lift a row above 1. Highest score first; equal scores by `family`, then `id`.
pub fn score_rows<'a>(
    modes: &'a ModeRegistry,
    center_hz: u64,
    bandwidth_hz: u64,
    layer1: &str,
    catalogue: &dyn Fn(&str) -> f32,
) -> Vec<Scored<'a>> {
    let mut scored: Vec<Scored> = modes
        .all()
        .iter()
        .filter(|m| matches!(m.state, ModeState::Ready | ModeState::Unavailable))
        .map(|m| {
            let band = &m.row.band;
            let center = band.centers_hz.is_empty()
                || band
                    .centers_hz
                    .iter()
                    .any(|&c| center_hz.abs_diff(c) <= band.bandwidth_hz);
            let bw = band.bandwidth_hz as f64;
            let width = (bw * 0.5..=bw * 1.5).contains(&(bandwidth_hz as f64));
            let modulation = m.row.modulation.iter().any(|l| l == layer1);
            let best = m
                .row
                .catalogue
                .iter()
                .map(|id| catalogue(id))
                .fold(0.0_f32, f32::max);
            let terms = [
                (center, CENTER_SCORE),
                (width, BANDWIDTH_SCORE),
                (modulation, MODULATION_SCORE),
            ];
            let score = terms
                .iter()
                .filter(|(hit, _)| *hit)
                .map(|(_, s)| s)
                .sum::<f32>()
                + best;
            Scored {
                mode: m,
                score,
                fits_band: band.fits(bandwidth_hz),
            }
        })
        .collect();
    scored.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.mode.row.family.cmp(&b.mode.row.family))
            .then(a.mode.row.id.cmp(&b.mode.row.id))
    });
    scored
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage::StageRegistry;

    fn ids(scored: &[Scored]) -> Vec<(String, f32, bool)> {
        scored
            .iter()
            .map(|s| (s.mode.row.id.0.clone(), s.score, s.fits_band))
            .collect()
    }

    /// The embedded rows, every one ready (the graph rows' stages are not registered
    /// here, so they load broken).
    fn registry() -> ModeRegistry {
        let mut modes = ModeRegistry::load(&StageRegistry::new(), None);
        for m in &mut modes.modes {
            m.state = ModeState::Ready;
        }
        modes
    }

    fn set_state(modes: &mut ModeRegistry, id: &str, state: ModeState) {
        for m in &mut modes.modes {
            if m.row.id.0 == id {
                m.state = state;
            }
        }
    }

    #[test]
    fn aprs_at_144_800_scores_highest_and_wide_rows_do_not_fit() {
        let modes = registry();
        let got = ids(&score_rows(&modes, 144_800_000, 12_500, "fm", &|_| 0.0));
        eprintln!("{got:?}");
        assert_eq!(got[0].0, "aprs_decode");
        assert!((got[0].1 - 1.0).abs() < 1e-6, "{got:?}");
        assert!(got[0].2, "{got:?}");
        // POCSAG: bandwidth and modulation, not the centre.
        let pocsag = got.iter().find(|g| g.0 == "pocsag_decode").unwrap();
        assert!((pocsag.1 - 0.6).abs() < 1e-6, "{got:?}");
        let rds = got.iter().find(|g| g.0 == "rds_decode").unwrap();
        assert!(!rds.2, "rds needs 150 kHz: {got:?}");
        let adsb = got.iter().find(|g| g.0 == "adsb_decode").unwrap();
        assert!(!adsb.2, "adsb needs 2 MHz: {got:?}");
        assert!(got.windows(2).all(|w| w[0].1 >= w[1].1), "{got:?}");
    }

    #[test]
    fn catalogue_scores_lift_a_row_and_broken_rows_are_not_scored() {
        let mut modes = registry();
        let cat = |id: &str| {
            if id == "radio-data-system-rds" {
                0.7
            } else {
                0.0
            }
        };
        let got = ids(&score_rows(&modes, 104_900_000, 150_000, "wfm", &cat));
        assert_eq!(got[0].0, "rds_decode", "{got:?}");
        // Centre granted (no list), bandwidth, modulation, plus the catalogue's 0.7.
        assert!((got[0].1 - 1.7).abs() < 1e-6, "{got:?}");
        assert!(got[0].2, "150 kHz meets rds's min_hz");
        set_state(&mut modes, "rds_decode", ModeState::Broken);
        set_state(&mut modes, "adsb_decode", ModeState::Unavailable);
        let got = ids(&score_rows(&modes, 104_900_000, 150_000, "wfm", &cat));
        assert!(got.iter().all(|g| g.0 != "rds_decode"), "{got:?}");
        assert!(got.iter().any(|g| g.0 == "adsb_decode"), "{got:?}");
    }

    #[test]
    fn equal_scores_order_by_family_then_id() {
        let modes = registry();
        // Nothing matches but rds's empty centre list.
        let got = ids(&score_rows(&modes, 1, 1, "noise", &|_| 0.0));
        let order: Vec<&str> = got.iter().map(|g| g.0.as_str()).collect();
        // rds at 0.4; then at 0 aviation (acars, adsb), pagers, amateur_digital.
        assert_eq!(
            order,
            [
                "rds_decode",
                "acars_decode",
                "adsb_decode",
                "pocsag_decode",
                "aprs_decode"
            ]
        );
    }
}
