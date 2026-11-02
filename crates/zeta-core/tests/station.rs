//! The station record: set, get, clear, persisted across a reopen.

use std::path::Path;

use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{Op, OpError, OpResult};
use zeta_core::stage::{Backend, StageRegistry};
use zeta_core::station::{Antenna, AntennaKind, Station};
use zeta_core::types::Hz;

async fn open(dir: &Path) -> Node {
    let cfg = NodeConfig {
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: 64 << 20,
        scan_dwell_ms: 20,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory: None,
        modes_dir: None,
    };
    Node::open(cfg, StageRegistry::new(), vec![]).await.unwrap()
}

#[tokio::test]
async fn station_set_persists_and_get_reads_it_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let r = node
        .apply(Op::StationSet {
            lat: -27.38,
            lon: 153.12,
            alt_m: 40.0,
            region: "AU".into(),
            antennas: None,
        })
        .await
        .unwrap();
    let OpResult::Station(Some(s)) = r else {
        panic!("{r:?}")
    };
    assert_eq!(s.antennas.len(), 2);
    assert_eq!(s.antennas[0].kind, AntennaKind::Dipole);
    drop(node);
    let node = open(dir.path()).await;
    assert_eq!(node.station().unwrap().lat, -27.38);
    assert!(matches!(
        node.apply(Op::StationClear).await.unwrap(),
        OpResult::Station(None)
    ));
    assert!(matches!(
        node.apply(Op::StationGet).await.unwrap(),
        OpResult::Station(None)
    ));
    drop(node);
    assert!(open(dir.path()).await.station().is_none());
}

#[tokio::test]
async fn a_bad_antenna_is_refused_naming_the_field() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let a = Antenna {
        id: "y".into(),
        kind: AntennaKind::Yagi,
        min_hz: Hz(144_000_000),
        max_hz: Hz(100),
        height_m: 10.0,
        azimuth_deg: Some(400.0),
        elevation_deg: None,
    };
    let e = node
        .apply(Op::StationSet {
            lat: 0.0,
            lon: 0.0,
            alt_m: 0.0,
            region: "AU".into(),
            antennas: Some(vec![a]),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(e, OpError::Other { ref message } if message.starts_with("antennas[0].")),
        "{e:?}"
    );
    assert!(node.station().is_none());
    assert!(!dir.path().join("state/station.json").exists());
}

#[test]
fn antenna_for_takes_the_declared_one_else_the_default() {
    let mut s = Station {
        lat: 0.0,
        lon: 0.0,
        alt_m: 10.0,
        region: "AU".into(),
        antennas: Station::default_antennas(10.0),
    };
    s.antennas.push(Antenna {
        id: "beam".into(),
        kind: AntennaKind::Yagi,
        min_hz: Hz(430_000_000),
        max_hz: Hz(440_000_000),
        height_m: 12.0,
        azimuth_deg: Some(90.0),
        elevation_deg: None,
    });
    assert_eq!(s.antenna_for(Hz(435_000_000)).id, "beam");
    assert_eq!(s.antenna_for(Hz(404_025_000)).kind, AntennaKind::Discone);
    assert_eq!(s.antenna_for(Hz(7_100_000)).kind, AntennaKind::Dipole);
    s.antennas.clear();
    assert_eq!(s.antenna_for(Hz(7_100_000)).id, "dipole");
}

#[test]
fn duplicate_ids_are_refused() {
    let mut s = Station {
        lat: 0.0,
        lon: 0.0,
        alt_m: 0.0,
        region: "AU".into(),
        antennas: Station::default_antennas(0.0),
    };
    s.antennas[1].id = "dipole".into();
    assert!(s.validate().unwrap_err().starts_with("antennas[1].id"));
}
