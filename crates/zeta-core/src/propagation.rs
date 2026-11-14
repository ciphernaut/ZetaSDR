//! A propagation estimate: how far a transmitter reaches an observer and about how strong
//! it should be there, from both ends' heights and the frequency. Every assumption is a
//! field of [`Propagation`]; the observer and the emitter are inputs, never constants.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Mean earth radius for the haversine distance.
const EARTH_RADIUS_KM: f64 = 6371.0088;
/// Distance floor for the free-space loss, so a co-located emitter stays finite.
const MIN_DISTANCE_KM: f64 = 0.01;

/// Every assumption of the estimate. `Default` gives the values noted on each field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
#[schemars(deny_unknown_fields, transform = crate::knowledge::require_all)]
pub struct Propagation {
    /// Effective earth radius factor; 4/3 is the standard atmosphere.
    pub k_factor: f64,
    /// Extra loss per km beyond the horizon: a placeholder between diffraction and
    /// troposcatter.
    pub beyond_horizon_db_per_km: f64,
    /// Loss off the beam when a directional device has no front-to-back ratio.
    pub off_beam_default_db: f64,
    /// How far a ground wave reaches, below `skywave_min_hz`.
    pub ground_wave_km: f64,
    /// Lowest frequency of the skywave band.
    pub skywave_min_hz: u64,
    /// Highest frequency of the skywave band.
    pub skywave_max_hz: u64,
    /// A level at or above this counts as in range: about the noise floor of a narrow
    /// channel.
    pub in_range_floor_dbm: f64,
    /// Factor on the furthest reach, for the search bound.
    pub search_margin: f64,
    /// Height assumed for a site before any row is known: a tall one.
    pub assumed_site_height_m: f64,
}

impl Default for Propagation {
    fn default() -> Self {
        Self {
            k_factor: 4.0 / 3.0,
            beyond_horizon_db_per_km: 0.5,
            off_beam_default_db: 15.0,
            ground_wave_km: 300.0,
            skywave_min_hz: 2_000_000,
            skywave_max_hz: 30_000_000,
            in_range_floor_dbm: -120.0,
            search_margin: 1.5,
            assumed_site_height_m: 1500.0,
        }
    }
}

impl Propagation {
    /// Whether these assumptions can drive the estimate: every number finite,
    /// `k_factor > 0`, `beyond_horizon_db_per_km >= 0`, `off_beam_default_db >= 0`,
    /// `ground_wave_km > 0`, `search_margin >= 1`, `assumed_site_height_m >= 0` and
    /// `skywave_min_hz <= skywave_max_hz`. The error starts with the field it names.
    pub fn validate(&self) -> Result<(), String> {
        /// `x`, the value of `field`, is finite and at least `min` (above it when `strict`).
        fn check(field: &str, x: f64, min: f64, strict: bool) -> Result<(), String> {
            if !x.is_finite() {
                return Err(format!("{field} must be a finite number, got {x}"));
            }
            match strict {
                true if x <= min => Err(format!("{field} must be greater than {min}, got {x}")),
                false if x < min => Err(format!("{field} must be at least {min}, got {x}")),
                _ => Ok(()),
            }
        }
        check("k_factor", self.k_factor, 0.0, true)?;
        check(
            "beyond_horizon_db_per_km",
            self.beyond_horizon_db_per_km,
            0.0,
            false,
        )?;
        check("off_beam_default_db", self.off_beam_default_db, 0.0, false)?;
        check("ground_wave_km", self.ground_wave_km, 0.0, true)?;
        check(
            "in_range_floor_dbm",
            self.in_range_floor_dbm,
            f64::MIN,
            false,
        )?;
        check("search_margin", self.search_margin, 1.0, false)?;
        check(
            "assumed_site_height_m",
            self.assumed_site_height_m,
            0.0,
            false,
        )?;
        // The skywave edges are whole Hz, always finite; only their order is checked.
        if self.skywave_min_hz > self.skywave_max_hz {
            return Err(format!(
                "skywave_min_hz must not exceed skywave_max_hz ({}), got {}",
                self.skywave_max_hz, self.skywave_min_hz
            ));
        }
        Ok(())
    }
}

/// Where the receiving antenna is.
#[derive(Clone, Debug, PartialEq)]
pub struct Observer {
    pub lat: f64,
    pub lon: f64,
    /// Ground height above sea level.
    pub alt_m: f64,
    /// Antenna height above that ground.
    pub antenna_height_m: f64,
}

/// A transmitting device, with whatever of its record is known.
#[derive(Clone, Debug, PartialEq)]
pub struct Emitter {
    pub lat: f64,
    pub lon: f64,
    pub freq_hz: u64,
    /// Ground height of the site above sea level.
    pub elevation_m: Option<f64>,
    /// Antenna height above that ground.
    pub height_m: Option<f64>,
    pub eirp_w: Option<f64>,
    pub power_w: Option<f64>,
    pub gain_dbi: Option<f64>,
    /// The direction the antenna points, degrees clockwise from north.
    pub azimuth_deg: Option<f64>,
    pub beamwidth_deg: Option<f64>,
    pub front_to_back_db: Option<f64>,
}

/// How the signal gets from the emitter to the observer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PathMode {
    LineOfSight,
    BeyondHorizon,
    GroundWave,
    Skywave,
}

/// Whether the signal should be heard at the observer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InRange {
    Yes,
    No,
    Possible,
}

/// The path between the two ends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Geometry {
    pub distance_km: f64,
    /// From the observer to the emitter.
    pub bearing_deg: f64,
    /// The angle between the emitter's azimuth and the direction to the observer.
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub off_beam_deg: Option<f64>,
    /// The radio horizon of the path; `None` outside the bands the horizon governs.
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub horizon_km: Option<f64>,
    pub beyond_horizon: bool,
}

/// What the estimate expects at the observer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Expected {
    pub mode: PathMode,
    /// The expected level, about; `None` when there is nothing to compute it from.
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub level_dbm: Option<f64>,
    pub in_range: InRange,
    /// Names of inputs that were missing: "eirp", "site_elevation", "antenna_height",
    /// "front_to_back", "level" (skywave).
    pub unknown: Vec<String>,
}

/// Great-circle distance in km.
pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let (dp, dl) = (p2 - p1, (lon2 - lon1).to_radians());
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_KM * a.sqrt().asin()
}

/// Initial bearing from the first point to the second, 0-360 clockwise from north.
pub fn bearing_deg(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dl = (lon2 - lon1).to_radians();
    let y = dl.sin() * p2.cos();
    let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
    y.atan2(x).to_degrees().rem_euclid(360.0)
}

/// The radio horizon between two antenna heights (metres above sea level), in km: 4.12
/// per root metre at a k-factor of 4/3. Negative heights count as 0.
pub fn horizon_km(h1_m: f64, h2_m: f64, k_factor: f64) -> f64 {
    3.57 * k_factor.sqrt() * (h1_m.max(0.0).sqrt() + h2_m.max(0.0).sqrt())
}

/// Free-space path loss in dB.
fn free_space_db(km: f64, freq_hz: u64) -> f64 {
    32.45 + 20.0 * km.max(MIN_DISTANCE_KM).log10() + 20.0 * (freq_hz.max(1) as f64 / 1e6).log10()
}

fn observer_height_m(o: &Observer) -> f64 {
    (o.alt_m + o.antenna_height_m).max(0.0)
}

/// The estimate for one emitter at one observer.
pub fn estimate(o: &Observer, e: &Emitter, p: &Propagation) -> (Geometry, Expected) {
    let mut unknown: Vec<String> = Vec::new();
    let mut miss = |name: &str| unknown.push(name.to_string());

    let distance_km = haversine_km(o.lat, o.lon, e.lat, e.lon);
    let bearing = bearing_deg(o.lat, o.lon, e.lat, e.lon);

    let elevation = match e.elevation_m {
        Some(v) => v,
        None => {
            miss("site_elevation");
            0.0
        }
    };
    let height = match e.height_m {
        Some(v) => v,
        None => {
            miss("antenna_height");
            0.0
        }
    };
    let site_height_m = (elevation + height).max(0.0);

    // Off the beam: the angle between the antenna's azimuth and the way to the observer.
    let to_observer = bearing_deg(e.lat, e.lon, o.lat, o.lon);
    let off_beam_deg = e
        .azimuth_deg
        .map(|az| ((to_observer - az + 180.0).rem_euclid(360.0) - 180.0).abs());

    let above_skywave = e.freq_hz > p.skywave_max_hz;
    let horizon =
        above_skywave.then(|| horizon_km(observer_height_m(o), site_height_m, p.k_factor));
    let beyond_horizon = horizon.is_some_and(|h| distance_km > h);

    let mode = if e.freq_hz < p.skywave_min_hz {
        PathMode::GroundWave
    } else if !above_skywave {
        PathMode::Skywave
    } else if beyond_horizon {
        PathMode::BeyondHorizon
    } else {
        PathMode::LineOfSight
    };

    // The radiated level, with the off-beam loss.
    // A zero or non-finite power is a hole in the row, not a level.
    let usable = |w: Option<f64>| w.filter(|w| w.is_finite() && *w > 0.0);
    let eirp_dbm = match (usable(e.eirp_w), usable(e.power_w)) {
        (Some(w), _) => Some(10.0 * (w * 1000.0).log10()),
        (None, Some(w)) => {
            miss("eirp");
            let gain = e.gain_dbi.filter(|g| g.is_finite()).unwrap_or(0.0);
            Some(10.0 * (w * 1000.0).log10() + gain)
        }
        (None, None) => {
            miss("eirp");
            None
        }
    };
    let off_beam_loss = match off_beam_deg {
        Some(off) if off > e.beamwidth_deg.unwrap_or(360.0) / 2.0 => match e.front_to_back_db {
            Some(db) => db,
            None => {
                miss("front_to_back");
                p.off_beam_default_db
            }
        },
        _ => 0.0,
    };

    let level_dbm = if mode == PathMode::Skywave {
        miss("level");
        None
    } else {
        eirp_dbm
            .map(|eirp| {
                let mut loss = free_space_db(distance_km, e.freq_hz) + off_beam_loss;
                if let (PathMode::BeyondHorizon, Some(h)) = (mode, horizon) {
                    loss += p.beyond_horizon_db_per_km * (distance_km - h);
                }
                eirp - loss
            })
            .filter(|l| l.is_finite())
    };

    let in_range = match mode {
        PathMode::Skywave => InRange::Possible,
        PathMode::GroundWave => yes_no(distance_km <= p.ground_wave_km),
        PathMode::LineOfSight => InRange::Yes,
        PathMode::BeyondHorizon => yes_no(level_dbm.is_some_and(|l| l >= p.in_range_floor_dbm)),
    };

    (
        Geometry {
            distance_km,
            bearing_deg: bearing,
            off_beam_deg,
            horizon_km: horizon,
            beyond_horizon,
        },
        Expected {
            mode,
            level_dbm,
            in_range,
            unknown,
        },
    )
}

fn yes_no(yes: bool) -> InRange {
    if yes { InRange::Yes } else { InRange::No }
}

/// The furthest any mode reaches for this observer; `None` when unbounded (skywave).
pub fn search_km(o: &Observer, freq_hz: u64, p: &Propagation) -> Option<f64> {
    if freq_hz < p.skywave_min_hz {
        Some(p.ground_wave_km * p.search_margin)
    } else if freq_hz <= p.skywave_max_hz {
        None
    } else {
        Some(
            horizon_km(observer_height_m(o), p.assumed_site_height_m, p.k_factor) * p.search_margin,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(alt_m: f64) -> Observer {
        Observer {
            lat: -27.3842,
            lon: 153.1175,
            alt_m,
            antenna_height_m: 5.0,
        }
    }

    /// A 450 MHz site `km_north` of the observer, on the ground, a 30 m mast, 25 W EIRP.
    fn site(km_north: f64) -> Emitter {
        Emitter {
            lat: -27.3842 + km_north / 111.195,
            lon: 153.1175,
            freq_hz: 450_000_000,
            elevation_m: Some(0.0),
            height_m: Some(30.0),
            eirp_w: Some(25.0),
            power_w: None,
            gain_dbi: None,
            azimuth_deg: None,
            beamwidth_deg: None,
            front_to_back_db: None,
        }
    }

    /// One change to the default assumptions.
    type Change = fn(&mut Propagation);

    #[test]
    fn validate_names_the_field_that_breaks_a_rule() {
        assert_eq!(Propagation::default().validate(), Ok(()));
        let bad = |edit: Change| {
            let mut p = Propagation::default();
            edit(&mut p);
            p.validate().unwrap_err()
        };
        let cases: [(Change, &str); 13] = [
            (|p| p.k_factor = 0.0, "k_factor"),
            (|p| p.k_factor = f64::NAN, "k_factor"),
            (
                |p| p.beyond_horizon_db_per_km = -0.1,
                "beyond_horizon_db_per_km",
            ),
            (|p| p.off_beam_default_db = -1.0, "off_beam_default_db"),
            (|p| p.ground_wave_km = 0.0, "ground_wave_km"),
            (|p| p.ground_wave_km = f64::INFINITY, "ground_wave_km"),
            (|p| p.search_margin = 0.99, "search_margin"),
            (|p| p.assumed_site_height_m = -1.0, "assumed_site_height_m"),
            (
                |p| p.in_range_floor_dbm = f64::NEG_INFINITY,
                "in_range_floor_dbm",
            ),
            (|p| p.in_range_floor_dbm = f64::NAN, "in_range_floor_dbm"),
            (
                |p| p.skywave_min_hz = p.skywave_max_hz + 1,
                "skywave_min_hz",
            ),
            (
                |p| p.beyond_horizon_db_per_km = f64::NAN,
                "beyond_horizon_db_per_km",
            ),
            (|p| p.search_margin = f64::INFINITY, "search_margin"),
        ];
        for (edit, field) in cases {
            let e = bad(edit);
            assert!(e.starts_with(&format!("{field} ")), "{field}: {e}");
        }
        // The edges of each rule are allowed.
        let edge = Propagation {
            beyond_horizon_db_per_km: 0.0,
            off_beam_default_db: 0.0,
            search_margin: 1.0,
            assumed_site_height_m: 0.0,
            skywave_min_hz: 5,
            skywave_max_hz: 5,
            ..Propagation::default()
        };
        assert_eq!(edge.validate(), Ok(()));
    }

    #[test]
    fn distance_and_bearing() {
        // The demo station (Brisbane Airport) to a synthetic site: about 9.5 km, a little
        // west of north.
        let d = haversine_km(-27.3842, 153.1175, -27.300, 153.100);
        assert!((d - 9.52).abs() < 0.05, "{d}");
        let b = bearing_deg(-27.3842, 153.1175, -27.300, 153.100);
        assert!((345.0..355.0).contains(&b), "{b}");
        assert!((bearing_deg(0.0, 0.0, 1.0, 0.0) - 0.0).abs() < 1e-9);
        assert!((bearing_deg(0.0, 0.0, 0.0, 1.0) - 90.0).abs() < 1e-9);
    }

    #[test]
    fn the_horizon_is_4_12_root_h_under_the_standard_atmosphere() {
        assert!((horizon_km(100.0, 0.0, 4.0 / 3.0) - 41.2).abs() < 0.1);
        assert!((horizon_km(9.0, 100.0, 4.0 / 3.0) - (12.36 + 41.2)).abs() < 0.2);
    }

    #[test]
    fn an_elevated_observer_sees_a_site_a_low_one_does_not() {
        let p = Propagation::default();
        let (g_low, e_low) = estimate(&obs(4.0), &site(120.0), &p);
        let (g_high, e_high) = estimate(&obs(3000.0), &site(120.0), &p);
        assert!(g_low.beyond_horizon && !g_high.beyond_horizon);
        assert_eq!(e_high.mode, PathMode::LineOfSight);
        assert!(e_high.level_dbm.unwrap() > e_low.level_dbm.unwrap());
    }

    #[test]
    fn free_space_level_inside_the_horizon() {
        // 25 W EIRP = 43.98 dBm; FSPL(10 km, 450 MHz) = 32.45 + 20 + 53.06 = 105.51 dB
        let (_, e) = estimate(&obs(4.0), &site(10.0), &Propagation::default());
        assert!((e.level_dbm.unwrap() - (43.98 - 105.51)).abs() < 0.2);
        assert_eq!(e.in_range, InRange::Yes);
    }

    #[test]
    fn a_directional_device_is_weaker_off_its_beam() {
        // the site is north of the observer, so the observer is on bearing 180 from it
        let on = Emitter {
            azimuth_deg: Some(180.0),
            beamwidth_deg: Some(30.0),
            front_to_back_db: Some(20.0),
            ..site(10.0)
        };
        let off = Emitter {
            azimuth_deg: Some(0.0),
            ..on.clone()
        };
        let p = Propagation::default();
        let (g_on, e_on) = estimate(&obs(4.0), &on, &p);
        let (g_off, e_off) = estimate(&obs(4.0), &off, &p);
        assert!(
            g_on.off_beam_deg.unwrap() < 1.0 && (g_off.off_beam_deg.unwrap() - 180.0).abs() < 1.0
        );
        assert!((e_on.level_dbm.unwrap() - e_off.level_dbm.unwrap() - 20.0).abs() < 0.01);
    }

    #[test]
    fn beyond_the_horizon_the_level_falls_per_km_and_ends_out_of_range() {
        let p = Propagation::default();
        let (g, e) = estimate(&obs(4.0), &site(400.0), &p);
        assert!(g.beyond_horizon);
        assert_eq!(e.mode, PathMode::BeyondHorizon);
        assert_eq!(e.in_range, InRange::No); // 0.5 dB/km over ~370 km past the horizon
    }

    #[test]
    fn a_skywave_band_has_no_level_and_is_only_possible() {
        let hf = Emitter {
            freq_hz: 8_000_000,
            ..site(2000.0)
        };
        let (_, e) = estimate(&obs(4.0), &hf, &Propagation::default());
        assert_eq!(
            (e.mode, e.level_dbm, e.in_range),
            (PathMode::Skywave, None, InRange::Possible)
        );
        assert!(e.unknown.contains(&"level".to_string()));
        assert_eq!(
            search_km(&obs(4.0), 8_000_000, &Propagation::default()),
            None
        );
    }

    #[test]
    fn below_the_skywave_band_the_path_is_ground_wave() {
        let mf = Emitter {
            freq_hz: 500_000,
            ..site(100.0)
        };
        let (_, e) = estimate(&obs(4.0), &mf, &Propagation::default());
        assert_eq!(e.mode, PathMode::GroundWave);
        assert_eq!(e.in_range, InRange::Yes); // inside ground_wave_km
        assert_eq!(
            search_km(&obs(4.0), 500_000, &Propagation::default()),
            Some(300.0 * 1.5)
        );
    }

    #[test]
    fn a_candidate_with_nothing_known_is_estimated_from_defaults_and_says_so() {
        let bare = Emitter {
            elevation_m: None,
            height_m: None,
            eirp_w: None,
            power_w: None,
            gain_dbi: None,
            azimuth_deg: Some(0.0),
            beamwidth_deg: None,
            front_to_back_db: None,
            ..site(10.0)
        };
        let (g, e) = estimate(&obs(4.0), &bare, &Propagation::default());
        assert!(g.distance_km.is_finite() && g.horizon_km.unwrap().is_finite());
        assert_eq!(e.level_dbm, None);
        for k in ["eirp", "site_elevation", "antenna_height"] {
            assert!(e.unknown.contains(&k.to_string()));
        }
    }

    #[test]
    fn a_zero_power_row_has_no_level_and_names_eirp() {
        let z = Emitter {
            eirp_w: Some(0.0),
            power_w: Some(0.0),
            ..site(10.0)
        };
        let (g, e) = estimate(&obs(4.0), &z, &Propagation::default());
        assert_eq!(e.level_dbm, None);
        assert!(e.unknown.contains(&"eirp".to_string()));
        assert!(g.distance_km.is_finite() && g.bearing_deg.is_finite());
        assert!(g.horizon_km.unwrap().is_finite());
    }

    #[test]
    fn a_zero_eirp_falls_back_to_power_and_gain() {
        let z = Emitter {
            eirp_w: Some(0.0),
            power_w: Some(25.0),
            gain_dbi: Some(0.0),
            ..site(10.0)
        };
        let (_, e) = estimate(&obs(4.0), &z, &Propagation::default());
        assert!((e.level_dbm.unwrap() - (43.98 - 105.51)).abs() < 0.2);
        assert!(e.unknown.contains(&"eirp".to_string()));
    }

    #[test]
    fn a_zero_frequency_does_not_produce_an_infinity() {
        let z = Emitter {
            freq_hz: 0,
            ..site(10.0)
        };
        let (_, e) = estimate(&obs(4.0), &z, &Propagation::default());
        assert!(e.level_dbm.unwrap().is_finite());
    }

    #[test]
    fn beyond_the_horizon_a_strong_signal_is_still_in_range() {
        let strong = Emitter {
            eirp_w: Some(1.0e6),
            ..site(60.0)
        };
        let (g, e) = estimate(&obs(4.0), &strong, &Propagation::default());
        assert!(g.beyond_horizon);
        assert!(e.level_dbm.unwrap() >= Propagation::default().in_range_floor_dbm);
        assert_eq!(e.in_range, InRange::Yes);
    }

    #[test]
    fn a_ground_wave_path_beyond_its_range_is_out_of_range() {
        let mf = Emitter {
            freq_hz: 500_000,
            ..site(400.0)
        };
        let (_, e) = estimate(&obs(4.0), &mf, &Propagation::default());
        assert_eq!((e.mode, e.in_range), (PathMode::GroundWave, InRange::No));
    }

    #[test]
    fn the_search_bound_grows_with_altitude_and_has_no_band_constant() {
        let p = Propagation::default();
        let low = search_km(&obs(4.0), 450_000_000, &p).unwrap();
        let high = search_km(&obs(10_000.0), 450_000_000, &p).unwrap();
        assert!(high > low * 2.0);
        // any frequency above skywave_max_hz
        assert_eq!(search_km(&obs(4.0), 40_000_000_000, &p), Some(low));
    }
}
