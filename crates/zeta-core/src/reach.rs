//! The reach rule: how far a transmitter on a frequency can be heard from the station.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::station::{Antenna, AntennaKind};
use crate::types::Hz;

/// Search radius for ground-wave (up to `GROUND_MAX_HZ`) signals.
pub const REACH_GROUND_KM: f64 = 300.0;
/// Search radius for VHF (tropospheric) signals.
pub const REACH_TROPO_KM: f64 = 300.0;
/// Search radius for UHF signals, and for space bands.
pub const REACH_UHF_KM: f64 = 150.0;
/// Search radius for microwave signals.
pub const REACH_MICROWAVE_KM: f64 = 60.0;
/// Half-width of a directional antenna's main sector.
pub const SECTOR_DEG: f64 = 30.0;
/// An antenna pointed higher than this is listening to space.
pub const SPACE_ELEVATION_DEG: f64 = 20.0;
/// Top of the ground-wave class.
pub const GROUND_MAX_HZ: u64 = 2_000_000;
/// Top of the HF skywave class.
pub const SKYWAVE_MAX_HZ: u64 = 30_000_000;
/// Top of the VHF tropospheric class.
pub const TROPO_MAX_HZ: u64 = 300_000_000;
/// Top of the UHF class.
pub const UHF_MAX_HZ: u64 = 1_000_000_000;

/// How a band propagates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReachClass {
    Ground,
    Skywave,
    Tropo,
    Uhf,
    Microwave,
    Space,
}

/// The distance a signal on a frequency can come from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Reach {
    /// The search radius; `None` for skywave with no override (unbounded).
    pub km: Option<f64>,
    pub class: ReachClass,
    /// The antenna id the reach was computed for.
    pub antenna: String,
    pub note: Option<String>,
}

/// The reach of `center_hz` heard on `antenna`. `space_band` marks a band allocated to
/// space services; `override_km` replaces the table (and bounds skywave and space).
pub fn reach_for(
    center_hz: Hz,
    antenna: &Antenna,
    space_band: bool,
    override_km: Option<f64>,
) -> Reach {
    let hz = center_hz.0;
    let up = antenna
        .elevation_deg
        .is_some_and(|e| e > SPACE_ELEVATION_DEG);
    let (class, km, note) = if space_band || up {
        (
            ReachClass::Space,
            Some(REACH_UHF_KM),
            Some("the transmitter may be a spacecraft the register does not list".to_string()),
        )
    } else if hz <= GROUND_MAX_HZ {
        (ReachClass::Ground, Some(REACH_GROUND_KM), None)
    } else if hz <= SKYWAVE_MAX_HZ {
        (
            ReachClass::Skywave,
            None,
            Some("HF skywave: the transmitter may be abroad and outside the register".to_string()),
        )
    } else if hz <= TROPO_MAX_HZ {
        (ReachClass::Tropo, Some(REACH_TROPO_KM), None)
    } else if hz <= UHF_MAX_HZ {
        (ReachClass::Uhf, Some(REACH_UHF_KM), None)
    } else {
        (ReachClass::Microwave, Some(REACH_MICROWAVE_KM), None)
    };
    Reach {
        km: override_km.or(km),
        class,
        antenna: antenna.id.clone(),
        note,
    }
}

impl Reach {
    /// The reach toward `bearing_deg`: for a directional antenna double inside
    /// `SECTOR_DEG` of its azimuth and half outside; otherwise unchanged.
    pub fn km_toward(&self, bearing_deg: f64, antenna: &Antenna) -> Option<f64> {
        let km = self.km?;
        let directional = matches!(antenna.kind, AntennaKind::Yagi | AntennaKind::Dish);
        let Some(az) = antenna.azimuth_deg.filter(|_| directional) else {
            return Some(km);
        };
        let diff = ((bearing_deg - az + 180.0).rem_euclid(360.0) - 180.0).abs();
        Some(if diff <= SECTOR_DEG {
            2.0 * km
        } else {
            0.5 * km
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::station::Station;

    #[test]
    fn reach_follows_the_band_class() {
        let d = Station::default_antennas(10.0);
        let discone = &d[1];
        let dipole = &d[0];
        assert_eq!(
            reach_for(Hz(404_025_000), discone, false, None).km,
            Some(REACH_UHF_KM)
        );
        assert_eq!(
            reach_for(Hz(146_000_000), discone, false, None).km,
            Some(REACH_TROPO_KM)
        );
        assert_eq!(
            reach_for(Hz(1_090_000_000), discone, false, None).km,
            Some(REACH_MICROWAVE_KM)
        );
        let hf = reach_for(Hz(7_100_000), dipole, false, None);
        assert_eq!((hf.km, hf.class), (None, ReachClass::Skywave));
        assert!(hf.note.unwrap().contains("abroad"));
        assert_eq!(
            reach_for(Hz(1_000_000), dipole, false, None).class,
            ReachClass::Ground
        );
    }

    #[test]
    fn space_bands_and_pointed_up_antennas_are_space() {
        let discone = &Station::default_antennas(10.0)[1];
        let r = reach_for(Hz(137_500_000), discone, true, None);
        assert_eq!((r.class, r.km), (ReachClass::Space, Some(REACH_UHF_KM)));
        assert!(r.note.unwrap().contains("spacecraft"));
        let up = Antenna {
            elevation_deg: Some(45.0),
            ..discone.clone()
        };
        assert_eq!(
            reach_for(Hz(146_000_000), &up, false, None).class,
            ReachClass::Space
        );
    }

    #[test]
    fn an_override_replaces_the_table_and_caps_skywave() {
        let d = Station::default_antennas(10.0);
        assert_eq!(
            reach_for(Hz(404_025_000), &d[1], false, Some(20.0)).km,
            Some(20.0)
        );
        assert_eq!(
            reach_for(Hz(7_100_000), &d[0], false, Some(500.0)).km,
            Some(500.0)
        );
    }

    fn yagi() -> Antenna {
        Antenna {
            id: "y".into(),
            kind: AntennaKind::Yagi,
            min_hz: Hz(400_000_000),
            max_hz: Hz(470_000_000),
            height_m: 10.0,
            azimuth_deg: Some(90.0),
            elevation_deg: None,
        }
    }

    #[test]
    fn a_directional_antenna_doubles_in_its_sector_and_halves_outside() {
        let yagi = yagi();
        let r = reach_for(Hz(404_025_000), &yagi, false, None);
        assert_eq!(r.km_toward(100.0, &yagi), Some(2.0 * REACH_UHF_KM));
        assert_eq!(r.km_toward(300.0, &yagi), Some(0.5 * REACH_UHF_KM));
    }

    #[test]
    fn an_omni_antenna_is_unchanged_and_the_sector_wraps() {
        let d = Station::default_antennas(10.0);
        let r = reach_for(Hz(404_025_000), &d[1], false, None);
        assert_eq!(r.km_toward(10.0, &d[1]), Some(REACH_UHF_KM));
        let north = Antenna {
            azimuth_deg: Some(350.0),
            ..yagi()
        };
        let r = reach_for(Hz(404_025_000), &north, false, None);
        assert_eq!(r.km_toward(10.0, &north), Some(2.0 * REACH_UHF_KM));
    }

    #[test]
    fn a_band_no_antenna_covers_takes_the_default_for_its_range() {
        let s = Station {
            lat: 0.0,
            lon: 0.0,
            alt_m: 0.0,
            region: "AU".into(),
            antennas: vec![Antenna {
                id: "y".into(),
                kind: AntennaKind::Yagi,
                min_hz: Hz(430_000_000),
                max_hz: Hz(440_000_000),
                height_m: 10.0,
                azimuth_deg: Some(0.0),
                elevation_deg: None,
            }],
        };
        assert_eq!(s.antenna_for(Hz(146_000_000)).kind, AntennaKind::Discone);
    }
}
