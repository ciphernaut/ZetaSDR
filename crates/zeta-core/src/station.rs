//! The station: where the node is and what antennas it listens with. One record per
//! node, set once, persisted in `state_dir/station.json`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::node::NodeError;
use crate::types::Hz;

/// The kind of an antenna.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AntennaKind {
    Discone,
    Dipole,
    Skyloop,
    Vertical,
    Yagi,
    Dish,
    Other,
}

/// One antenna: its band, height and, for a directional one, where it points.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Antenna {
    pub id: String,
    pub kind: AntennaKind,
    pub min_hz: Hz,
    pub max_hz: Hz,
    pub height_m: f64,
    #[serde(deserialize_with = "crate::modes::nullable")]
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub azimuth_deg: Option<f64>,
    #[serde(deserialize_with = "crate::modes::nullable")]
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub elevation_deg: Option<f64>,
}

/// The node's position, region and antennas.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Station {
    pub lat: f64,
    pub lon: f64,
    pub alt_m: f64,
    pub region: String,
    pub antennas: Vec<Antenna>,
}

impl Station {
    /// A dipole for 0-30 MHz and a discone for 30 MHz-6 GHz, both 5 m up.
    pub fn default_antennas(_alt_m: f64) -> Vec<Antenna> {
        vec![
            Antenna {
                id: "dipole".into(),
                kind: AntennaKind::Dipole,
                min_hz: Hz(0),
                max_hz: Hz(30_000_000),
                height_m: 5.0,
                azimuth_deg: None,
                elevation_deg: None,
            },
            Antenna {
                id: "discone".into(),
                kind: AntennaKind::Discone,
                min_hz: Hz(30_000_000),
                max_hz: Hz(6_000_000_000),
                height_m: 5.0,
                azimuth_deg: None,
                elevation_deg: None,
            },
        ]
    }

    /// The declared antenna whose band holds `hz`, the narrowest band first (so a beam
    /// declared beside the default discone is the one chosen on its own band), ties to
    /// the first declared; else the default for that range.
    pub fn antenna_for(&self, hz: Hz) -> Antenna {
        self.antennas
            .iter()
            .filter(|a| a.min_hz.0 <= hz.0 && hz.0 <= a.max_hz.0)
            .min_by_key(|a| a.max_hz.0 - a.min_hz.0)
            .cloned()
            .unwrap_or_else(|| {
                let mut d = Self::default_antennas(self.alt_m);
                if hz.0 < 30_000_000 {
                    d.swap_remove(0)
                } else {
                    d.swap_remove(1)
                }
            })
    }

    /// Check the ranges; the error names the field (`lat: ...`, `antennas[0].min_hz: ...`).
    pub fn validate(&self) -> Result<(), String> {
        if !(-90.0..=90.0).contains(&self.lat) {
            return Err(format!("lat: {} outside -90..=90", self.lat));
        }
        if !(-180.0..=180.0).contains(&self.lon) {
            return Err(format!("lon: {} outside -180..=180", self.lon));
        }
        if !self.alt_m.is_finite() {
            return Err(format!("alt_m: {} is not a number", self.alt_m));
        }
        let mut ids = HashSet::new();
        for (i, a) in self.antennas.iter().enumerate() {
            if a.id.is_empty() {
                return Err(format!("antennas[{i}].id: empty"));
            }
            if !ids.insert(a.id.as_str()) {
                return Err(format!("antennas[{i}].id: duplicate id {}", a.id));
            }
            if a.min_hz.0 >= a.max_hz.0 {
                return Err(format!(
                    "antennas[{i}].min_hz: {} is not below max_hz {}",
                    a.min_hz.0, a.max_hz.0
                ));
            }
            if !a.height_m.is_finite() || a.height_m < 0.0 {
                return Err(format!(
                    "antennas[{i}].height_m: {} is negative",
                    a.height_m
                ));
            }
            if let Some(az) = a.azimuth_deg
                && !(0.0..360.0).contains(&az)
            {
                return Err(format!("antennas[{i}].azimuth_deg: {az} outside 0..360"));
            }
            if let Some(el) = a.elevation_deg
                && !(0.0..=90.0).contains(&el)
            {
                return Err(format!("antennas[{i}].elevation_deg: {el} outside 0..=90"));
            }
        }
        Ok(())
    }
}

fn path(state_dir: &Path) -> PathBuf {
    state_dir.join("station.json")
}

/// Read the station; a missing file is `None`, an unreadable one an error string.
pub(crate) fn load(state_dir: &Path) -> (Option<Station>, Option<String>) {
    match std::fs::read(path(state_dir)) {
        Ok(b) => match serde_json::from_slice(&b) {
            Ok(s) => (Some(s), None),
            Err(e) => (None, Some(format!("unreadable station.json: {e}"))),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, None),
        Err(e) => (None, Some(format!("station.json: {e}"))),
    }
}

/// Write the station atomically (a temp file renamed over it).
pub(crate) fn persist(state_dir: &Path, s: &Station) -> Result<(), NodeError> {
    let io = |e: &dyn std::fmt::Display| NodeError::Io(e.to_string());
    std::fs::create_dir_all(state_dir).map_err(|e| io(&e))?;
    let json = serde_json::to_vec_pretty(s).map_err(|e| io(&e))?;
    let tmp = state_dir.join("station.json.tmp");
    std::fs::write(&tmp, json).map_err(|e| io(&e))?;
    std::fs::rename(&tmp, path(state_dir)).map_err(|e| io(&e))
}

/// Remove the station file (absent is fine).
pub(crate) fn remove(state_dir: &Path) -> Result<(), NodeError> {
    match std::fs::remove_file(path(state_dir)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(NodeError::Io(e.to_string())),
    }
}
