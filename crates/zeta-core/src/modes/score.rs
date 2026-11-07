//! Scoring the rows against a measured band (design 7.1): the order `classify` and
//! `identify` try the modes in.

use super::{LoadedMode, ModeRegistry, ModeState};
use crate::knowledge::Allocation;

/// Score for the measured centre lying near one of the row's well-known centres or in
/// its `range_hz` (granted when it has neither).
const CENTER_SCORE: f32 = 0.4;
/// Score for the measured bandwidth lying within half to 1.5 times the row's.
const BANDWIDTH_SCORE: f32 = 0.3;
/// Score for the layer-1 label being one the row presents as.
const MODULATION_SCORE: f32 = 0.3;
/// Score for a licensed transmitter on the band whose emission fits the row
/// ([`allocation_term`]).
pub const ALLOCATION_SCORE: f32 = 0.2;
/// An assignment's bandwidth fits a row's within these factors of `band.bandwidth_hz`.
const ALLOCATION_WIDTH: (f64, f64) = (0.5, 1.5);
/// The layer-1 labels each emission designator's modulation letter is compatible with.
pub(crate) const DESIGNATOR_LABELS: &[(char, &[&str])] = &[
    ('F', &["fm", "fsk"]),
    ('G', &["fm", "fsk", "psk"]),
    ('D', &["psk"]),
    ('A', &["am", "ssb"]),
    ('H', &["am", "ssb"]),
    ('J', &["am", "ssb"]),
    ('R', &["am", "ssb"]),
    ('N', &["carrier"]),
];

/// The `tx` assignment of `allocs` that fits `row`, the first in their order (a
/// provider lists the nearest first): its decoded designator's `bandwidth_hz` (else the
/// assignment's own `bandwidth_hz`) within 0.5-1.5 times the row's `band.bandwidth_hz`,
/// and its modulation letter compatible with one of the row's `modulation` labels
/// (`F`, `G` with `fm`/`fsk`; `G`, `D` with `psk`; `A`, `H`, `J`, `R` with `am`/`ssb`;
/// `N` with `carrier`). An assignment without a designator passes the modulation test;
/// one without any bandwidth fits none. Plan rows and receivers never match.
pub fn allocation_match<'a>(row: &LoadedMode, allocs: &'a [Allocation]) -> Option<&'a Allocation> {
    let bw = row.row.band.bandwidth_hz as f64;
    let (lo, hi) = ALLOCATION_WIDTH;
    allocs.iter().find(|a| {
        if a.role.as_deref() != Some("tx") || a.kind.as_deref() == Some("plan") {
            return false;
        }
        let Some(width) = row_width(a) else {
            return false;
        };
        if !(bw * lo..=bw * hi).contains(&(width.0 as f64)) {
            return false;
        }
        let Some(e) = &a.emission_decoded else {
            return true;
        };
        designator_fits(e.modulation, &row.row.modulation)
    })
}

/// An assignment's width: its decoded designator's `bandwidth_hz`, else its own
/// `bandwidth_hz`; `None` without either.
pub(crate) fn row_width(a: &Allocation) -> Option<crate::types::Hz> {
    a.emission_decoded
        .as_ref()
        .map(|e| e.bandwidth_hz)
        .or(a.bandwidth_hz)
}

/// Whether an emission designator's modulation `letter` is compatible with one of the
/// layer-1 `labels` ([`DESIGNATOR_LABELS`]); a letter the table lacks fits none.
pub(crate) fn designator_fits(letter: char, labels: &[String]) -> bool {
    DESIGNATOR_LABELS
        .iter()
        .filter(|(c, _)| *c == letter)
        .flat_map(|(_, compatible)| compatible.iter())
        .any(|l| labels.iter().any(|m| m == l))
}

/// [`ALLOCATION_SCORE`] when [`allocation_match`] finds a `tx` assignment that fits
/// `row`, else 0.
pub fn allocation_term(row: &LoadedMode, allocs: &[Allocation]) -> f32 {
    match allocation_match(row, allocs) {
        Some(_) => ALLOCATION_SCORE,
        None => 0.0,
    }
}

/// One row's score against a measured band.
#[derive(Clone, Debug)]
pub struct Scored<'a> {
    pub mode: &'a LoadedMode,
    pub score: f32,
    /// Whether the band passed to [`score_rows`] is wide enough for the row
    /// ([`super::Band::fits`]).
    pub fits_band: bool,
    /// Whether the measured centre lies in the row's `band.range_hz` (true for a row
    /// without one); a row out of range scores nothing for the centre and never fits.
    pub in_range: bool,
}

/// The `ready` and `unavailable` rows of `modes` (a broken row is never tried) scored
/// against a signal measured at `center_hz`, `bandwidth_hz` wide and labelled `layer1`:
/// 0.4 when `center_hz` lies within `band.bandwidth_hz` of one of `band.centers_hz` or
/// inside `band.range_hz` (or the row has neither, "anywhere"; a row whose `range_hz`
/// and listed centres both exclude `center_hz` scores 0 for it, is not `in_range` and never `fits_band`); 0.3 when `bandwidth_hz`
/// lies within 0.5-1.5 times `band.bandwidth_hz`; 0.3 when `layer1` is in `modulation`
/// (never for an empty list); plus the best `catalogue(id)` among the row's `catalogue`
/// ids, so a catalogue match can lift a row above 1; plus `allocation(row)` (the
/// register's term, [`allocation_term`]). Highest score first; equal scores
/// by `identify.priority` (highest first), then `family`, then `id`.
pub fn score_rows<'a>(
    modes: &'a ModeRegistry,
    center_hz: u64,
    bandwidth_hz: u64,
    layer1: &str,
    catalogue: &dyn Fn(&str) -> f32,
    allocation: &dyn Fn(&LoadedMode) -> f32,
) -> Vec<Scored<'a>> {
    let mut scored: Vec<Scored> = modes
        .all()
        .iter()
        .filter(|m| matches!(m.state, ModeState::Ready | ModeState::Unavailable))
        .map(|m| {
            let band = &m.row.band;
            let in_range = band
                .range_hz
                .is_some_and(|[lo, hi]| (lo..=hi).contains(&center_hz));
            let near = band
                .centers_hz
                .iter()
                .any(|&c| center_hz.abs_diff(c) <= band.bandwidth_hz);
            // A row with a range and no listed centre near excludes a centre outside it.
            let in_range_or_none = band.range_hz.is_none() || in_range || near;
            let center = in_range_or_none
                && (in_range || near || (band.centers_hz.is_empty() && band.range_hz.is_none()));
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
                + best
                + allocation(m);
            Scored {
                mode: m,
                score,
                fits_band: in_range_or_none && band.fits(bandwidth_hz),
                in_range: in_range_or_none,
            }
        })
        .collect();
    scored.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(
                b.mode
                    .row
                    .identify
                    .priority
                    .cmp(&a.mode.row.identify.priority),
            )
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
        let got = ids(&score_rows(
            &modes,
            144_800_000,
            12_500,
            "fm",
            &|_| 0.0,
            &|_| 0.0,
        ));
        assert_eq!(got[0].0, "aprs_decode");
        assert!((got[0].1 - 1.0).abs() < 1e-6, "{got:?}");
        assert!(got[0].2, "{got:?}");
        // POCSAG: bandwidth and modulation, not the centre.
        let pocsag = got.iter().find(|g| g.0 == "pocsag_decode").unwrap();
        assert!((pocsag.1 - 0.6).abs() < 1e-6, "{got:?}");
        // FLEX's range starts at 151.8 MHz: 144.8 MHz earns it no centre credit, so it
        // neither ties APRS nor outranks POCSAG there.
        let flex = got.iter().find(|g| g.0 == "flex_decode").unwrap();
        assert!(
            flex.1 < got[0].1,
            "flex is out of range at 144.8 MHz: {got:?}"
        );
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
        let got = ids(&score_rows(
            &modes,
            104_900_000,
            150_000,
            "wfm",
            &cat,
            &|_| 0.0,
        ));
        assert_eq!(got[0].0, "rds_decode", "{got:?}");
        // Centre granted (no list), bandwidth, modulation, plus the catalogue's 0.7.
        assert!((got[0].1 - 1.7).abs() < 1e-6, "{got:?}");
        assert!(got[0].2, "150 kHz meets rds's min_hz");
        set_state(&mut modes, "rds_decode", ModeState::Broken);
        set_state(&mut modes, "adsb_decode", ModeState::Unavailable);
        let got = ids(&score_rows(
            &modes,
            104_900_000,
            150_000,
            "wfm",
            &cat,
            &|_| 0.0,
        ));
        assert!(got.iter().all(|g| g.0 != "rds_decode"), "{got:?}");
        assert!(got.iter().any(|g| g.0 == "adsb_decode"), "{got:?}");
    }

    /// A copy of `adsb_decode` (ready) under `id`, edited by `edit`.
    fn add_row(modes: &mut ModeRegistry, id: &str, edit: impl FnOnce(&mut super::super::ModeRow)) {
        let mut m = modes.get("adsb_decode").unwrap().clone();
        m.row.id = super::super::ModeId(id.into());
        edit(&mut m.row);
        modes.modes.push(m);
    }

    #[test]
    fn a_centre_inside_the_range_scores_the_centre_term() {
        let mut modes = registry();
        add_row(&mut modes, "sonde", |r| {
            r.band.centers_hz = vec![];
            r.band.range_hz = Some([400_050_000, 406_000_000]);
            r.band.bandwidth_hz = 10_000;
            r.modulation = vec![];
            r.catalogue = vec![];
        });
        let sonde = |center_hz| {
            score_rows(&modes, center_hz, 1, "noise", &|_| 0.0, &|_| 0.0)
                .iter()
                .find(|s| s.mode.row.id.0 == "sonde")
                .unwrap()
                .score
        };
        assert!((sonde(402_500_000) - CENTER_SCORE).abs() < 1e-6);
        assert!(
            (sonde(400_050_000) - CENTER_SCORE).abs() < 1e-6,
            "inclusive"
        );
        // An empty centre list with a range is not "anywhere".
        assert_eq!(sonde(410_000_000), 0.0);
        // A centre near a listed one still scores, range or not.
        add_row(&mut modes, "both", |r| {
            r.band.range_hz = Some([400_050_000, 406_000_000]);
        });
        let both = score_rows(&modes, 1_090_000_000, 1, "noise", &|_| 0.0, &|_| 0.0);
        let both = both.iter().find(|s| s.mode.row.id.0 == "both").unwrap();
        assert!((both.score - CENTER_SCORE).abs() < 1e-6, "{}", both.score);
    }

    #[test]
    fn priority_breaks_ties_first() {
        let mut modes = registry();
        // Equal score and family; the priority-1 row sorts first though its id is later.
        add_row(&mut modes, "zz_sonde", |r| r.identify.priority = 1);
        add_row(&mut modes, "aa_sonde", |r| r.identify.priority = 0);
        let got = ids(&score_rows(
            &modes,
            1_090_000_000,
            1,
            "noise",
            &|_| 0.0,
            &|_| 0.0,
        ));
        let order: Vec<&str> = got
            .iter()
            .filter(|g| g.0.ends_with("_sonde") || g.0 == "adsb_decode")
            .map(|g| g.0.as_str())
            .collect();
        assert_eq!(order, ["zz_sonde", "aa_sonde", "adsb_decode"], "{got:?}");
        // A negative priority sorts after the default.
        add_row(&mut modes, "a_low", |r| r.identify.priority = -1);
        let got = ids(&score_rows(
            &modes,
            1_090_000_000,
            1,
            "noise",
            &|_| 0.0,
            &|_| 0.0,
        ));
        assert_eq!(got.iter().rev().find(|g| g.1 > 0.0).unwrap().0, "a_low");
    }

    #[test]
    fn equal_scores_order_by_family_then_id() {
        let modes = registry();
        // Nothing matches but rds's empty centre list.
        let got = ids(&score_rows(&modes, 1, 1, "noise", &|_| 0.0, &|_| 0.0));
        let order: Vec<&str> = got.iter().map(|g| g.0.as_str()).collect();
        // rds at 0.4; then at 0 rs41 (priority 1), then aviation (acars, adsb, hfdl,
        // vdl2), maritime (ais), pagers (flex, pocsag), weather_science (dfm, m20), amateur_digital.
        assert_eq!(
            order,
            [
                "rds_decode",
                "rs41_decode",
                "acars_decode",
                "adsb_decode",
                "hfdl_decode",
                "vdl2_decode",
                "ais_decode",
                "flex_decode",
                "pocsag_decode",
                "dfm_decode",
                "m20_decode",
                "aprs_decode"
            ]
        );
    }

    /// A transmitter of `designator` (and `bandwidth_hz` when given) in role `role`.
    fn assignment(designator: Option<&str>, bandwidth_hz: Option<u64>, role: &str) -> Allocation {
        Allocation {
            kind: Some("assignment".into()),
            role: Some(role.into()),
            emission: designator.map(Into::into),
            emission_decoded: designator.and_then(crate::emission::parse_emission),
            bandwidth_hz: bandwidth_hz.map(crate::types::Hz),
            ..Allocation::default()
        }
    }

    #[test]
    fn a_fitting_transmitter_adds_the_allocation_term() {
        let modes = registry();
        let row = |id| modes.get(id).unwrap();
        let fm = [assignment(Some("16K0F1D"), None, "tx")];
        // POCSAG: 12.5 kHz, fm/fsk. ACARS: 10 kHz, am: 16 kHz is over 1.5 times.
        assert_eq!(allocation_term(row("pocsag_decode"), &fm), ALLOCATION_SCORE);
        assert_eq!(allocation_term(row("acars_decode"), &fm), 0.0);
        // The modulation letter must suit the row: A3E is AM, not FM.
        let am = [assignment(Some("10K0A3E"), None, "tx")];
        assert_eq!(allocation_term(row("pocsag_decode"), &am), 0.0);
        assert_eq!(allocation_term(row("acars_decode"), &am), ALLOCATION_SCORE);
        // No designator: the assignment's own bandwidth, and the modulation test passes.
        let bare = [assignment(None, Some(12_500), "tx")];
        assert_eq!(
            allocation_term(row("pocsag_decode"), &bare),
            ALLOCATION_SCORE
        );
        // Receivers, plan rows and a width-less assignment never score.
        assert_eq!(
            allocation_term(
                row("pocsag_decode"),
                &[assignment(Some("16K0F1D"), None, "rx")]
            ),
            0.0
        );
        let mut plan = assignment(Some("16K0F1D"), None, "tx");
        plan.kind = Some("plan".into());
        assert_eq!(allocation_term(row("pocsag_decode"), &[plan]), 0.0);
        assert_eq!(
            allocation_term(row("pocsag_decode"), &[assignment(None, None, "tx")]),
            0.0
        );
        // The term is added to the row's score.
        let with = score_rows(&modes, 148_637_500, 12_500, "fsk", &|_| 0.0, &|m| {
            allocation_term(m, &fm)
        });
        let pocsag = with
            .iter()
            .find(|s| s.mode.row.id.0 == "pocsag_decode")
            .unwrap();
        assert!(
            (pocsag.score - (1.0 + ALLOCATION_SCORE)).abs() < 1e-6,
            "{}",
            pocsag.score
        );
    }
}
