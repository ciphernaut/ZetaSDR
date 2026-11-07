//! The `acma` knowledge provider against a stub of the register service.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::{Value, json};
use zeta_core::knowledge::KnowledgeProvider;
use zeta_core::reach::ReachClass;
use zeta_core::station::Station;
use zeta_core::types::Hz;
use zeta_ext::acma::AcmaKnowledge;
use zeta_ext::mcp_client::McpClient;
use zeta_ext::testing::acma_stub::{self, Mode, Stub};

/// Four 404.025 MHz assignments shaped as the register answers them (2026-11-02), every row
/// synthetic: licensees, licences, sites and callsigns made up, the sites near the demo
/// station. The last column, `CALL_SIGN`, is one a service may not answer.
fn rows() -> Vec<Value> {
    vec![
        json!([
            404025000,
            25000,
            "16K0F2D  ",
            "T",
            1,
            "W",
            "9000001/1",
            "Example Rail Site A",
            -27.950,
            153.250,
            "Example Rail Operator",
            null
        ]),
        json!([
            404025000,
            25000,
            "16K0F2D  ",
            "R",
            null,
            "W",
            "9000001/1",
            "Example Rail Site B",
            -27.955,
            153.255,
            "Example Rail Operator",
            null
        ]),
        json!([
            404025000,
            25000,
            "16K0F1D  ",
            "T",
            1,
            "W",
            "9000002/1",
            "Example Site Hill Rd",
            -27.300,
            153.100,
            "Example Water Authority",
            "XX4EX"
        ]),
        json!([
            404025000,
            25000,
            "16K0F1D  ",
            "R",
            null,
            "W",
            "9000002/1",
            "Example Depot",
            -27.250,
            153.120,
            "Example Water Authority",
            "XX4EX"
        ]),
    ]
}

/// The register stub (`zeta_ext::testing::acma_stub`) answering [`rows`].
async fn serve(mode: Mode) -> (String, Arc<Stub>) {
    acma_stub::serve(rows(), mode).await
}

/// The demo station, Brisbane Airport.
fn demo_station() -> Option<Station> {
    Some(Station {
        lat: -27.3842,
        lon: 153.1175,
        alt_m: 0.0,
        region: "AU".into(),
        antennas: Station::default_antennas(0.0),
    })
}

fn provider(url: &str, station: Option<Station>) -> AcmaKnowledge {
    let client = McpClient::new(url, Duration::from_secs(10)).unwrap();
    AcmaKnowledge::new(client, None, Arc::new(move || station.clone()))
}

#[tokio::test]
async fn the_404_025_lookup_names_the_plan_and_the_four_assignments() {
    let (url, stub) = serve(Mode::Normal).await;
    let provider = provider(&url, demo_station());
    assert_eq!(provider.name(), "acma");
    assert!(provider.gives_assignments());
    let a = provider
        .allocations("AU", Hz(404_025_000), Hz(16_000))
        .await;
    let plan: Vec<_> = a
        .iter()
        .filter(|x| x.kind.as_deref() == Some("plan"))
        .collect();
    assert_eq!(plan.len(), 4);
    assert!(plan.iter().any(|x| x.service == "FIXED (primary)"));
    assert!(
        plan.iter()
            .any(|x| x.service == "METEOROLOGICAL AIDS (secondary)")
    );
    assert!(
        plan.iter()
            .all(|x| (x.min_hz, x.max_hz) == (Hz(403_000_000), Hz(406_000_000)))
    );
    let asg: Vec<_> = a
        .iter()
        .filter(|x| x.kind.as_deref() == Some("assignment"))
        .collect();
    assert_eq!(asg.len(), 4);
    // The plan rows come first.
    assert_eq!(a[0].kind.as_deref(), Some("plan"));
    let first = asg[0];
    assert_eq!(first.licensee.as_deref(), Some("Example Water Authority"));
    assert_eq!(first.role.as_deref(), Some("tx")); // 1 W, nearest tx
    assert_eq!(first.site.as_deref(), Some("Example Site Hill Rd"));
    assert_eq!(first.emission.as_deref(), Some("16K0F1D"));
    assert_eq!(
        first.emission_decoded.as_ref().unwrap().bandwidth_hz,
        Hz(16_000)
    );
    assert_eq!(first.power_w, Some(1.0));
    assert_eq!(first.service, "assignment");
    assert_eq!(first.licence_no.as_deref(), Some("9000002/1"));
    assert_eq!(
        (first.center_hz, first.bandwidth_hz),
        (Some(Hz(404_025_000)), Some(Hz(25_000)))
    );
    assert_eq!(
        (first.min_hz, first.max_hz),
        (Hz(404_012_500), Hz(404_037_500))
    );
    let d = first.distance_km.unwrap();
    assert!((9.0..10.0).contains(&d), "{d}");
    let b = first.bearing_deg.unwrap();
    assert!((270.0..360.0).contains(&b), "{b}"); // a little west of north
    assert_eq!(
        first.note.as_deref(),
        Some("Example Water Authority, Example Site Hill Rd, 10 km, 1 W, 16 kHz FM data")
    );
    assert_eq!(asg[1].role.as_deref(), Some("rx"));
    assert_eq!(asg[1].power_w, None);
    assert!(asg.windows(2).all(|w| w[0].distance_km <= w[1].distance_km));
    assert!(a.iter().all(|x| x.provider == "acma" && x.region == "AU"));
    // The query: the overlap, the joins, a bounding box of the 150 km reach.
    let sql = stub.sql.lock().unwrap().clone().unwrap();
    assert_eq!(sql["limit"], 200);
    let q = sql["sql"].as_str().unwrap();
    assert!(
        q.contains("FREQUENCY - COALESCE(d.BANDWIDTH, 0) / 2 <= 404033000"),
        "{q}"
    );
    assert!(
        q.contains("FREQUENCY + COALESCE(d.BANDWIDTH, 0) / 2 >= 404017000"),
        "{q}"
    );
    assert!(q.contains("LATITUDE BETWEEN"), "{q}");

    let r = provider
        .reach("AU", Hz(404_025_000), Hz(16_000))
        .await
        .unwrap();
    assert_eq!((r.class, r.km), (ReachClass::Uhf, Some(150.0)));
    assert_eq!(r.antenna, "discone");
    // The stub read the band from the query (its rows were cut by it, not passed whole).
    assert_eq!(stub.unbanded.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn without_a_station_the_list_is_by_frequency_distance_with_null_km() {
    let (url, stub) = serve(Mode::Normal).await;
    let provider = provider(&url, None);
    let a = provider
        .allocations("au", Hz(404_030_000), Hz(16_000))
        .await;
    let asg: Vec<_> = a
        .iter()
        .filter(|x| x.kind.as_deref() == Some("assignment"))
        .collect();
    assert_eq!(asg.len(), 4);
    assert!(
        asg.iter()
            .all(|x| x.distance_km.is_none() && x.bearing_deg.is_none())
    );
    // Equal frequency distance: transmitters first.
    assert_eq!(asg[0].role.as_deref(), Some("tx"));
    assert_eq!(asg[1].role.as_deref(), Some("tx"));
    assert_eq!(asg[2].role.as_deref(), Some("rx"));
    // No reach, so no bounding box.
    let q = stub.sql.lock().unwrap().clone().unwrap()["sql"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!q.contains("LATITUDE BETWEEN"), "{q}");
    assert!(asg[0].note.as_deref().is_some_and(|n| !n.contains(" km")));
    assert_eq!(
        provider.reach("AU", Hz(404_030_000), Hz(16_000)).await,
        None
    );
}

#[tokio::test]
async fn another_region_is_empty() {
    let (url, stub) = serve(Mode::Normal).await;
    let provider = provider(&url, demo_station());
    assert!(
        provider
            .allocations("US", Hz(404_025_000), Hz(16_000))
            .await
            .is_empty()
    );
    assert!(
        provider
            .signatures(Hz(404_025_000), Hz(16_000))
            .await
            .is_empty()
    );
    assert_eq!(
        provider.reach("US", Hz(404_025_000), Hz(16_000)).await,
        None
    );
    assert_eq!(stub.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn reach_reuses_the_plan_of_the_lookup_just_made() {
    let (url, stub) = serve(Mode::Normal).await;
    let provider = provider(&url, demo_station());
    provider
        .allocations("AU", Hz(404_025_000), Hz(16_000))
        .await;
    let r = provider.reach("AU", Hz(404_025_000), Hz(16_000)).await;
    assert_eq!(r.map(|r| r.class), Some(ReachClass::Uhf));
    assert_eq!(stub.plans.load(Ordering::SeqCst), 1);
    // Another centre has no plan answer yet: reach asks for it.
    provider.reach("AU", Hz(404_050_000), Hz(16_000)).await;
    assert_eq!(stub.plans.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_hung_service_costs_one_lookup_one_deadline() {
    let (url, stub) = serve(Mode::Slow).await;
    let deadline = Duration::from_millis(500);
    let provider = provider(&url, demo_station()).with_timeout(deadline);
    let t0 = std::time::Instant::now();
    assert!(
        provider
            .allocations("AU", Hz(404_025_000), Hz(16_000))
            .await
            .is_empty()
    );
    let r = provider.reach("AU", Hz(404_025_000), Hz(16_000)).await;
    let took = t0.elapsed();
    assert!(took < deadline + Duration::from_millis(300), "{took:?}");
    // The failed plan reads as no space band; the reach is the band's own.
    assert_eq!(r.map(|r| r.class), Some(ReachClass::Uhf));
    assert_eq!(stub.plans.load(Ordering::SeqCst), 1);
    assert_eq!(provider.errors(), 1);
}

#[tokio::test]
async fn a_service_error_a_bad_body_and_a_timeout_each_give_no_allocations() {
    for mode in [Mode::RpcError, Mode::Html, Mode::Slow] {
        let (url, _) = serve(mode).await;
        let provider = provider(&url, demo_station()).with_timeout(Duration::from_millis(500));
        let a = provider
            .allocations("AU", Hz(404_025_000), Hz(16_000))
            .await;
        assert!(a.is_empty());
        assert_eq!(provider.errors(), 1);
        assert_eq!(provider.logged(), 1);
        // A second failure inside the minute is counted but not logged again.
        provider
            .allocations("AU", Hz(404_025_000), Hz(16_000))
            .await;
        assert_eq!(provider.errors(), 2);
        assert_eq!(provider.logged(), 1);
    }
}

#[tokio::test]
async fn a_satellite_plan_sets_space_reach() {
    let (url, _) = serve(Mode::Satellite).await;
    let provider = provider(&url, demo_station());
    let r = provider
        .reach("AU", Hz(404_025_000), Hz(16_000))
        .await
        .unwrap();
    assert_eq!(r.class, ReachClass::Space);
    assert!(r.note.unwrap().contains("spacecraft"));
    let a = provider
        .allocations("AU", Hz(404_025_000), Hz(16_000))
        .await;
    assert!(a.iter().any(|x| x.service == "MOBILE-SATELLITE (primary)"));
}

#[tokio::test]
async fn a_device_without_a_bandwidth_is_kept() {
    let (url, stub) = serve(Mode::NullBandwidth).await;
    let provider = provider(&url, demo_station());
    let a = provider
        .allocations("AU", Hz(404_025_000), Hz(16_000))
        .await;
    // The query matches a NULL bandwidth as 0 Hz wide instead of dropping it ...
    let sql = stub.sql.lock().unwrap().clone().unwrap();
    let q = sql["sql"].as_str().unwrap();
    assert!(!q.contains(" d.BANDWIDTH / 2"), "{q}");
    assert!(q.contains("COALESCE(d.BANDWIDTH, 0) / 2"), "{q}");
    // ... and the row it answers becomes an assignment like the others.
    let asg: Vec<_> = a
        .iter()
        .filter(|x| x.kind.as_deref() == Some("assignment"))
        .collect();
    assert_eq!(asg.len(), 4);
    assert!(
        asg.iter()
            .any(|x| x.site.as_deref() == Some("Example Rail Site A")
                && x.center_hz == Some(Hz(404_025_000))),
        "{asg:?}"
    );
}

#[tokio::test]
async fn the_sql_names_call_sign_and_a_column_fills_it() {
    let (url, stub) = serve(Mode::Normal).await;
    let provider = provider(&url, demo_station());
    let a = provider
        .allocations("AU", Hz(404_025_000), Hz(16_000))
        .await;
    let asg: Vec<_> = a
        .iter()
        .filter(|x| x.kind.as_deref() == Some("assignment"))
        .collect();
    assert_eq!(asg.len(), 4);
    let tx = asg
        .iter()
        .find(|x| x.emission.as_deref() == Some("16K0F1D") && x.role.as_deref() == Some("tx"))
        .unwrap();
    assert_eq!(tx.callsign.as_deref(), Some("XX4EX"));
    let rail = asg
        .iter()
        .find(|x| x.site.as_deref() == Some("Example Rail Site A"))
        .unwrap();
    assert_eq!(rail.callsign, None);
    assert!(
        a.iter()
            .filter(|x| x.kind.as_deref() == Some("plan"))
            .all(|x| x.callsign.is_none())
    );
    let sql = stub.sql.lock().unwrap().clone().unwrap();
    let q = sql["sql"].as_str().unwrap();
    assert!(q.contains("d.CALL_SIGN AS CALL_SIGN"), "{q}");
}

#[tokio::test]
async fn a_stub_without_the_column_leaves_callsign_null() {
    let (url, _) = serve(Mode::NoCallSign).await;
    let without = provider(&url, demo_station())
        .allocations("AU", Hz(404_025_000), Hz(16_000))
        .await;
    let (url, _) = serve(Mode::Normal).await;
    let with = provider(&url, demo_station())
        .allocations("AU", Hz(404_025_000), Hz(16_000))
        .await;
    assert_eq!(without.len(), 8);
    assert!(without.iter().all(|x| x.callsign.is_none()));
    // Nothing else changes.
    let cleared: Vec<_> = with
        .into_iter()
        .map(|mut x| {
            x.callsign = None;
            x
        })
        .collect();
    assert_eq!(without, cleared);
}

#[tokio::test]
async fn allocations_from_another_station_cuts_and_measures_from_it() {
    let (url, stub) = serve(Mode::Normal).await;
    let provider = provider(&url, demo_station());
    // 100 km due north of the demo station.
    let mut north = demo_station().unwrap();
    north.lat += 100.0 / 111.195;
    let a = provider
        .allocations_from("AU", Hz(404_025_000), Hz(16_000), Some(&north))
        .await;
    let asg: Vec<_> = a
        .iter()
        .filter(|x| x.kind.as_deref() == Some("assignment"))
        .collect();
    // The rail sites, about 165 km from the north station, fall outside the 150 km
    // reach; the hill site 9 km from the demo station is about 91 km from it.
    assert!(
        !asg.is_empty()
            && asg
                .iter()
                .all(|x| x.licensee.as_deref() == Some("Example Water Authority")),
        "{asg:?}"
    );
    let hill = asg
        .iter()
        .find(|x| x.site.as_deref() == Some("Example Site Hill Rd"))
        .unwrap();
    let d = hill.distance_km.unwrap();
    assert!((90.0..92.0).contains(&d), "{d}");
    let b = hill.bearing_deg.unwrap();
    assert!((150.0..200.0).contains(&b), "{b}"); // about south
    // The query's box is around the north station.
    let q = stub.sql.lock().unwrap().clone().unwrap()["sql"]
        .as_str()
        .unwrap()
        .to_string();
    let (lo, hi) = (north.lat - 150.0 / 111.0, north.lat + 150.0 / 111.0);
    assert!(
        q.contains(&format!("s.LATITUDE BETWEEN {lo:.6} AND {hi:.6}")),
        "{q}"
    );
    let r = provider
        .reach_from("AU", Hz(404_025_000), Hz(16_000), Some(&north))
        .await
        .unwrap();
    assert_eq!(r.km, Some(150.0));

    // No vantage: no distances and no box, though the provider has a station.
    let a = provider
        .allocations_from("AU", Hz(404_025_000), Hz(16_000), None)
        .await;
    let asg: Vec<_> = a
        .iter()
        .filter(|x| x.kind.as_deref() == Some("assignment"))
        .collect();
    assert_eq!(asg.len(), 4);
    assert!(
        asg.iter()
            .all(|x| x.distance_km.is_none() && x.bearing_deg.is_none())
    );
    let q = stub.sql.lock().unwrap().clone().unwrap()["sql"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!q.contains("LATITUDE BETWEEN"), "{q}");
    assert_eq!(
        provider
            .reach_from("AU", Hz(404_025_000), Hz(16_000), None)
            .await,
        None
    );
}

#[tokio::test]
async fn a_register_without_the_call_sign_column_is_probed_once_and_selects_null() {
    for mode in [Mode::NoCallSignColumnRpc, Mode::NoCallSignColumnTool] {
        let (url, stub) = serve(mode).await;
        let provider = provider(&url, demo_station());
        for center in [404_025_000, 404_030_000] {
            let a = provider.allocations("AU", Hz(center), Hz(16_000)).await;
            let asg: Vec<_> = a
                .iter()
                .filter(|x| x.kind.as_deref() == Some("assignment"))
                .collect();
            assert_eq!(asg.len(), 4, "{a:?}");
            assert!(a.iter().all(|x| x.callsign.is_none()));
            let q = stub.sql.lock().unwrap().clone().unwrap()["sql"]
                .as_str()
                .unwrap()
                .to_string();
            assert!(q.contains("NULL AS CALL_SIGN"), "{q}");
        }
        // One rejected probe, then each lookup asks once: 1 + 2 queries.
        assert_eq!(stub.rejected.load(Ordering::SeqCst), 1);
        assert_eq!(stub.queries.load(Ordering::SeqCst), 3);
        assert_eq!(provider.errors(), 0);
    }
}

#[tokio::test]
async fn a_register_with_the_column_is_asked_once_per_lookup() {
    let (url, stub) = serve(Mode::Normal).await;
    let provider = provider(&url, demo_station());
    for _ in 0..2 {
        provider
            .allocations("AU", Hz(404_025_000), Hz(16_000))
            .await;
    }
    assert_eq!(stub.queries.load(Ordering::SeqCst), 2);
    assert_eq!(stub.rejected.load(Ordering::SeqCst), 0);
}
