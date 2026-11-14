//! The `acma` provider's register answer (`describe`) against the register stub: six
//! register queries over `execute_sql`, the data date from `GET /health`, ranked by the
//! propagation estimate. Every holder, site, licence and call sign here is made up; the
//! demo station is the only real position.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use zeta_core::knowledge::KnowledgeProvider;
use zeta_core::propagation::{InRange, Propagation};
use zeta_core::register::{
    REGISTER_MAX_CANDIDATES, RELATED_TOP, RegisterAnswer, RegisterQueries, RegisterState, Signal,
};
use zeta_core::station::Station;
use zeta_core::types::Hz;
use zeta_ext::acma::AcmaKnowledge;
use zeta_ext::mcp_client::McpClient;
use zeta_ext::testing::acma_stub::{self, Fixture, Mode, Stub};

/// The demo station, Brisbane Airport.
const LAT: f64 = -27.3842;
const LON: f64 = 153.1175;
/// The channel every site holds, inside the stub's plan segment (403-406 MHz).
const CHANNEL_HZ: u64 = 404_500_000;
const CHANNEL_BW: u64 = 12_500;
/// A receive frequency on the nearest site's licence, outside the signal's band.
const PAIRED_HZ: u64 = 409_500_000;

/// The latitude `km` north of the demo station.
fn north(km: f64) -> f64 {
    LAT + km / 111.195
}

fn station(alt_m: f64) -> Station {
    Station {
        lat: LAT,
        lon: LON,
        alt_m,
        region: "AU".into(),
        antennas: Station::default_antennas(alt_m),
    }
}

fn signal() -> Signal {
    Signal {
        center_hz: Hz(CHANNEL_HZ),
        bandwidth_hz: Hz(CHANNEL_BW),
    }
}

/// Four sites due north on one channel (5, 40, 120 and 600 km), the 40 km one with a
/// beam pointing away (north) and a receiver of another licence beside it, the 5 km
/// one's licence with a receive frequency too and its holder with another channel there,
/// a link from the 120 km site, one area-wide device, and the plan segment's footnotes
/// in both footnote tables (and one in neither).
fn fixture() -> Fixture {
    let f = Fixture::empty();
    f.sql(&format!(
        "INSERT INTO industry_cat VALUES (1, 'Utilities', 'Utilities');
         INSERT INTO client_type VALUES (1, 'Corporation');
         INSERT INTO licence_service VALUES (1, 'Land Mobile');
         INSERT INTO licence_subservice VALUES (1, 1, 'Land Mobile System');
         INSERT INTO class_of_station VALUES ('FB', 'Base station');
         INSERT INTO client (CLIENT_NO, LICENCEE, TRADING_NAME, CAT_ID, CLIENT_TYPE_ID) VALUES
           (1, 'Example Water Authority', 'Example Water', 1, 1),
           (2, 'Example Rail Operator', NULL, 1, 1),
           (3, 'Example Links Pty Ltd', NULL, 1, 1),
           (4, 'Example Upland Radio', NULL, 1, 1),
           (5, 'Example Mobile Fleet', NULL, 1, 1);
         INSERT INTO licence (LICENCE_NO, CLIENT_NO, SV_ID, SS_ID, DATE_ISSUED, DATE_OF_EXPIRY,
           STATUS) VALUES
           ('9000001/1', 1, 1, 1, '2020-01-01', '2030-01-01', 'Granted'),
           ('9000002/1', 2, 1, 1, NULL, NULL, 'Granted'),
           ('9000003/1', 3, 1, 1, NULL, NULL, 'Granted'),
           ('9000004/1', 4, 1, 1, NULL, NULL, 'Granted'),
           ('9000005/1', 2, 1, 1, NULL, NULL, 'Granted'),
           ('9000006/1', 1, 1, 1, NULL, NULL, 'Granted'),
           ('9000007/1', 5, 1, 1, NULL, NULL, 'Granted');
         INSERT INTO site (SITE_ID, LATITUDE, LONGITUDE, NAME, SITE_PRECISION, ELEVATION) VALUES
           ('9001', {n5}, {LON}, 'Example Hill', 'Within 10 m', 10),
           ('9040', {n40}, {LON}, 'Example Ridge', NULL, 100),
           ('9120', {n120}, {LON}, 'Example Range', NULL, 500),
           ('9600', {n600}, {LON}, 'Example Summit', NULL, 3000),
           ('9900', {n130}, {LON}, 'Example Far End', NULL, NULL);
         INSERT INTO antenna (ANTENNA_ID, GAIN, FRONT_TO_BACK, H_BEAMWIDTH) VALUES
           ('A40', 10, 30, 60);
         INSERT INTO device_details (LICENCE_NO, FREQUENCY, BANDWIDTH, EMISSION, DEVICE_TYPE,
           TRANSMITTER_POWER, TRANSMITTER_POWER_UNIT, SITE_ID, ANTENNA_ID, AZIMUTH, HEIGHT,
           CALL_SIGN, CLASS_OF_STATION_CODE, AREA_DESCRIPTION) VALUES
           ('9000001/1', {CHANNEL_HZ}, {CHANNEL_BW}, '11K0F3E  ', 'T', 5, 'W', '9001', NULL,
             NULL, 30, 'XX4EX', 'FB', NULL),
           ('9000001/1', {PAIRED_HZ}, {CHANNEL_BW}, '11K0F3E  ', 'R', NULL, NULL, '9001',
             NULL, NULL, 30, 'XX4EX', NULL, NULL),
           ('9000006/1', 404600000, {CHANNEL_BW}, '11K0F3E', 'T', 5, 'W', '9001', NULL, NULL,
             30, NULL, NULL, NULL),
           ('9000002/1', {CHANNEL_HZ}, {CHANNEL_BW}, '11K0F3E', 'T', 50, 'W', '9040', 'A40',
             0, 40, NULL, NULL, NULL),
           ('9000005/1', {CHANNEL_HZ}, {CHANNEL_BW}, '11K0F3E', 'R', NULL, NULL, '9040', NULL,
             NULL, 40, NULL, NULL, NULL),
           ('9000003/1', {CHANNEL_HZ}, {CHANNEL_BW}, '11K0F3E', 'T', 100, 'W', '9120', NULL,
             NULL, 50, NULL, NULL, NULL),
           ('9000004/1', {CHANNEL_HZ}, {CHANNEL_BW}, '11K0F3E', 'T', 100, 'W', '9600', NULL,
             NULL, 100, NULL, NULL, NULL),
           ('9000007/1', {CHANNEL_HZ}, {CHANNEL_BW}, '11K0F3E', 'T', 5, 'W', NULL, NULL, NULL,
             NULL, NULL, NULL, 'Example statewide area');
         INSERT INTO link_pairs VALUES (1, {n120}, {LON}, 'Example Range', {n130}, {LON},
           'Example Far End', '9120', '9900', {CHANNEL_HZ}, 'Fixed', 'Example Links Pty Ltd');
         UPDATE spectrum_allocations SET footnotes_json = '[\"AUS29\",\"5.901\",\"AUS999\"]';
         INSERT INTO spectrum_international_footnotes VALUES
           ('5.901', 'An example international footnote.', 1);",
        n5 = north(5.0),
        n40 = north(40.0),
        n120 = north(120.0),
        n130 = north(130.0),
        n600 = north(600.0),
    ));
    f
}

fn provider(url: &str, station: Option<Station>, override_km: Option<f64>) -> AcmaKnowledge {
    let client = McpClient::new(url, Duration::from_secs(10)).unwrap();
    AcmaKnowledge::new(
        client,
        override_km,
        Arc::new(move || station.clone()),
        Arc::new(RegisterQueries::builtin()),
    )
}

async fn describe(stub_fixture: Fixture, from: Option<Station>) -> (RegisterAnswer, Arc<Stub>) {
    let (url, stub) = acma_stub::serve_fixture(stub_fixture, Mode::Normal).await;
    let p = provider(&url, None, None);
    let a = p.describe("AU", &signal(), from.as_ref()).await.unwrap();
    (a, stub)
}

/// The last `candidates` statement the stub ran.
fn candidates_sql(stub: &Stub) -> String {
    stub.seen
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|q| q.contains("s.LATITUDE BETWEEN"))
        .cloned()
        .expect("a candidates query")
}

/// The box `[lat_min, lat_max, lon_min, lon_max]` in a `candidates` statement.
fn box_of(sql: &str) -> [f64; 4] {
    let pair = |key: &str| -> (f64, f64) {
        let rest = &sql[sql.find(key).unwrap() + key.len()..];
        let mut words = rest.split_whitespace();
        let a = words.next().unwrap().parse().unwrap();
        assert_eq!(words.next(), Some("AND"), "{sql}");
        let b = words.next().unwrap().parse().unwrap();
        (a, b)
    };
    let (a, b) = pair("s.LATITUDE BETWEEN ");
    let (c, d) = pair("s.LONGITUDE BETWEEN ");
    [a, b, c, d]
}

fn sites(a: &RegisterAnswer) -> Vec<&str> {
    a.candidates.iter().map(|c| c.site.id.as_str()).collect()
}

#[tokio::test]
async fn the_answer_holds_the_plan_the_ranked_candidates_and_the_data_date() {
    let (a, _) = describe(fixture(), Some(station(4.0))).await;
    assert_eq!(a.status.state, RegisterState::Ok, "{:?}", a.status);
    assert!(a.status.failed.is_empty() && a.status.reason.is_none());
    assert_eq!((a.provider.as_str(), a.region.as_str()), ("acma", "AU"));
    assert_eq!(a.as_of.as_deref(), Some("2026-01-01T00:00:00.000Z"));
    assert_eq!(a.assumptions, Propagation::default());

    assert_eq!(a.plan.len(), 1);
    let seg = &a.plan[0];
    assert_eq!((seg.min_hz, seg.max_hz), (Hz(403_000_000), Hz(406_000_000)));
    assert_eq!(seg.services.len(), 4);
    let mobile = seg.services.iter().find(|s| s.name == "MOBILE").unwrap();
    assert!(mobile.primary);
    assert_eq!(
        mobile.qualifier.as_deref(),
        Some("except aeronautical mobile (R)")
    );
    assert!(
        seg.services
            .iter()
            .any(|s| s.name == "RADIOLOCATION" && !s.primary)
    );
    let note = |r: &str| {
        seg.footnotes
            .iter()
            .find(|f| f.reference == r)
            .unwrap_or_else(|| panic!("{r}: {:?}", seg.footnotes))
            .text
            .clone()
    };
    assert_eq!(
        note("AUS29").as_deref(),
        Some("An example footnote for the plan segment.")
    );
    assert_eq!(
        note("5.901").as_deref(),
        Some("An example international footnote.")
    );
    assert_eq!(note("AUS999"), None);

    // The 600 km site is outside the search box from 4 m; the other three sites and the
    // receiver beside the 40 km one are candidates.
    assert_eq!(a.candidates.len(), 4, "{:?}", sites(&a));
    assert_eq!(a.matched, 4);
    assert!(!a.truncated);
    for c in &a.candidates {
        assert!(c.geometry.is_some() && c.expected.is_some(), "{c:?}");
    }
    // Strongest expected level first; the receiver, which has none, last.
    let levels: Vec<Option<f64>> = a
        .candidates
        .iter()
        .map(|c| c.expected.as_ref().unwrap().level_dbm)
        .collect();
    assert!(levels[..3].iter().all(Option::is_some), "{levels:?}");
    assert!(levels[..3].windows(2).all(|w| w[0] >= w[1]), "{levels:?}");
    assert_eq!(levels[3], None);
    assert_eq!(a.candidates[3].device.role.as_deref(), Some("rx"));
    // The beam pointing away costs the 40 km site its rank: the 120 km site is stronger.
    assert_eq!(sites(&a), ["9001", "9120", "9040", "9040"]);
    let ridge = &a.candidates[2];
    assert_eq!(ridge.geometry.as_ref().unwrap().off_beam_deg, Some(180.0));

    let top = &a.candidates[0];
    assert_eq!(top.device.center_hz, Hz(CHANNEL_HZ));
    assert_eq!(top.device.bandwidth_hz, Some(Hz(CHANNEL_BW)));
    assert_eq!(top.device.emission.as_deref(), Some("11K0F3E"));
    assert_eq!(
        top.device.emission_decoded.as_ref().unwrap().bandwidth_hz,
        Hz(11_000)
    );
    assert_eq!(top.device.role.as_deref(), Some("tx"));
    assert_eq!(top.device.power_w, Some(5.0));
    assert_eq!(top.device.height_m, Some(30.0));
    assert_eq!(top.device.callsign.as_deref(), Some("XX4EX"));
    assert_eq!(top.device.station_class.as_deref(), Some("Base station"));
    assert_eq!(top.site.name.as_deref(), Some("Example Hill"));
    assert_eq!(top.site.precision.as_deref(), Some("Within 10 m"));
    assert_eq!(top.site.elevation_m, Some(10.0));
    assert_eq!(top.licence.number, "9000001/1");
    assert_eq!(top.licence.service.as_deref(), Some("Land Mobile"));
    assert_eq!(top.licence.status.as_deref(), Some("Granted"));
    assert_eq!(top.licence.expires.as_deref(), Some("2030-01-01"));
    assert_eq!(top.holder.name.as_deref(), Some("Example Water Authority"));
    assert_eq!(top.holder.trading_name.as_deref(), Some("Example Water"));
    assert_eq!(top.holder.industry.as_deref(), Some("Utilities"));
    assert_eq!(top.holder.client_type.as_deref(), Some("Corporation"));
    let g = top.geometry.as_ref().unwrap();
    assert!((4.9..5.1).contains(&g.distance_km), "{g:?}");
    assert!(g.bearing_deg < 1.0 || g.bearing_deg > 359.0, "{g:?}");
    assert_eq!(top.expected.as_ref().unwrap().in_range, InRange::Yes);
}

#[tokio::test]
async fn the_600_km_site_is_fetched_only_when_the_observer_is_high_enough() {
    let (low, _) = describe(fixture(), Some(station(4.0))).await;
    assert!(!sites(&low).contains(&"9600"), "{:?}", sites(&low));
    let (high, _) = describe(fixture(), Some(station(9000.0))).await;
    let summit = high
        .candidates
        .iter()
        .find(|c| c.site.id == "9600")
        .unwrap_or_else(|| panic!("{:?}", sites(&high)));
    let g = summit.geometry.as_ref().unwrap();
    assert!((595.0..605.0).contains(&g.distance_km), "{g:?}");
    assert!(!g.beyond_horizon, "{g:?}");
    assert_eq!(summit.expected.as_ref().unwrap().in_range, InRange::Yes);
}

#[tokio::test]
async fn the_top_candidates_carry_what_they_belong_to() {
    let (a, _) = describe(fixture(), Some(station(4.0))).await;
    assert_eq!(a.status.state, RegisterState::Ok, "{:?}", a.status);
    for (i, c) in a.candidates.iter().enumerate() {
        assert_eq!(c.related.is_some(), i < RELATED_TOP, "{i}");
    }
    let hill = a.candidates[0].related.as_ref().unwrap();
    assert_eq!(hill.paired.len(), 1, "{:?}", hill.paired);
    assert_eq!(hill.paired[0].center_hz, Hz(PAIRED_HZ));
    assert_eq!(hill.paired[0].role.as_deref(), Some("rx"));
    assert_eq!(hill.paired[0].emission.as_deref(), Some("11K0F3E"));
    // The holder's other channels at the site: the receive frequency and another licence's.
    assert_eq!(hill.holder_channels, 2);
    let mut list: Vec<u64> = hill
        .holder_channel_list
        .iter()
        .map(|c| c.center_hz.0)
        .collect();
    list.sort();
    assert_eq!(list, [404_600_000, PAIRED_HZ]);
    assert_eq!(hill.far_end, None);

    let range = a
        .candidates
        .iter()
        .find(|c| c.site.id == "9120")
        .unwrap()
        .related
        .as_ref()
        .unwrap();
    let far = range.far_end.as_ref().unwrap();
    assert_eq!(far.site.id, "9900");
    assert_eq!(far.site.name.as_deref(), Some("Example Far End"));
    let d = far.distance_km.unwrap();
    assert!((9.5..10.5).contains(&d), "{d}");
    assert!(range.paired.is_empty());
    assert_eq!(range.holder_channels, 0);
}

#[tokio::test]
async fn area_devices_are_listed_without_geometry() {
    let (a, _) = describe(fixture(), Some(station(4.0))).await;
    assert_eq!(a.area_candidates.len(), 1);
    let area = &a.area_candidates[0];
    assert_eq!(area.area.as_deref(), Some("Example statewide area"));
    assert_eq!(area.licence.number, "9000007/1");
    assert_eq!(area.holder.name.as_deref(), Some("Example Mobile Fleet"));
    assert_eq!(area.device.role.as_deref(), Some("tx"));
    assert_eq!(area.device.power_w, Some(5.0));
    // No area device is a site candidate.
    assert!(a.candidates.iter().all(|c| c.licence.number != "9000007/1"));
}

#[tokio::test]
async fn duplicates_merge_before_the_cap_and_truncation_is_reported() {
    let f = Fixture::empty();
    f.sql("INSERT INTO client (CLIENT_NO, LICENCEE) VALUES (1, 'Example Fleet Operator')");
    for k in 0..60 {
        let lat = north(1.0 + k as f64 * 0.5);
        let insert = format!(
            "INSERT INTO site (SITE_ID, LATITUDE, LONGITUDE, NAME) VALUES ('{id}', {lat}, {LON},
               'Example Site {k}');
             INSERT INTO licence (LICENCE_NO, CLIENT_NO) VALUES ('L{k}', 1);",
            id = 7000 + k
        );
        f.sql(&insert);
        for _ in 0..2 {
            f.sql(&format!(
                "INSERT INTO device_details (LICENCE_NO, FREQUENCY, BANDWIDTH, EMISSION,
                   DEVICE_TYPE, TRANSMITTER_POWER, TRANSMITTER_POWER_UNIT, SITE_ID) VALUES
                   ('L{k}', {CHANNEL_HZ}, {CHANNEL_BW}, '11K0F3E', 'T', 1, 'W', '{id}')",
                id = 7000 + k
            ));
        }
    }
    let (a, _) = describe(f, Some(station(4.0))).await;
    assert_eq!(a.status.state, RegisterState::Ok, "{:?}", a.status);
    assert_eq!(a.matched, 120);
    assert_eq!(a.candidates.len(), REGISTER_MAX_CANDIDATES);
    assert!(a.truncated);
    let mut ids = sites(&a);
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), REGISTER_MAX_CANDIDATES, "merged, not cut first");
}

#[tokio::test]
async fn without_an_observer_candidates_have_no_geometry() {
    let f = fixture();
    // A device a little off the signal's centre, still overlapping it.
    f.sql(&format!(
        "INSERT INTO device_details (LICENCE_NO, FREQUENCY, BANDWIDTH, EMISSION, DEVICE_TYPE,
           TRANSMITTER_POWER, TRANSMITTER_POWER_UNIT, SITE_ID) VALUES
           ('9000006/1', {}, {CHANNEL_BW}, '11K0F3E', 'T', 5, 'W', '9001')",
        CHANNEL_HZ + 6_000
    ));
    let (a, stub) = describe(f, None).await;
    assert_eq!(a.status.state, RegisterState::Ok, "{:?}", a.status);
    // Every site on the channel, the 600 km one included.
    assert_eq!(a.candidates.len(), 6, "{:?}", sites(&a));
    assert!(
        a.candidates
            .iter()
            .all(|c| c.geometry.is_none() && c.expected.is_none())
    );
    let off: Vec<u64> = a
        .candidates
        .iter()
        .map(|c| c.device.center_hz.0.abs_diff(CHANNEL_HZ))
        .collect();
    assert!(off.windows(2).all(|w| w[0] <= w[1]), "{off:?}");
    assert_eq!(*off.last().unwrap(), 6_000);
    // No box: the whole globe.
    assert_eq!(box_of(&candidates_sql(&stub)), [-90.0, 90.0, -180.0, 180.0]);
}

#[tokio::test]
async fn a_failing_query_is_named_and_the_rest_stands() {
    let (url, stub) = acma_stub::serve_fixture(fixture(), Mode::Normal).await;
    *stub.fail_matching.lock().unwrap() = Some("spectrum_allocations".into());
    let p = provider(&url, None, None);
    let a = p
        .describe("AU", &signal(), Some(&station(4.0)))
        .await
        .unwrap();
    assert_eq!(a.status.state, RegisterState::Partial);
    let failed: Vec<&str> = a.status.failed.iter().map(|f| f.query.as_str()).collect();
    assert_eq!(failed, ["plan_span"]);
    assert!(!a.status.failed[0].reason.is_empty());
    assert!(a.plan.is_empty());
    assert_eq!(a.candidates.len(), 4);

    let f = fixture();
    f.drop_column("site", "ELEVATION");
    let (url, _) = acma_stub::serve_fixture(f, Mode::Normal).await;
    let a = provider(&url, None, None)
        .describe("AU", &signal(), Some(&station(4.0)))
        .await
        .unwrap();
    assert_eq!(a.status.state, RegisterState::Partial);
    let failed: Vec<&str> = a.status.failed.iter().map(|f| f.query.as_str()).collect();
    assert_eq!(failed, ["candidates"]);
    assert!(
        a.status.failed[0].reason.contains("ELEVATION"),
        "{:?}",
        a.status.failed
    );
    assert!(a.candidates.is_empty());
    assert_eq!(a.plan.len(), 1);
    assert_eq!(a.area_candidates.len(), 1);
}

#[tokio::test]
async fn a_service_that_does_not_answer_is_unreachable_not_empty() {
    for mode in [Mode::Slow, Mode::RpcError, Mode::Html] {
        let (url, _) = acma_stub::serve_fixture(fixture(), mode).await;
        let p = provider(&url, None, None).with_timeout(Duration::from_millis(300));
        let a = p
            .describe("AU", &signal(), Some(&station(4.0)))
            .await
            .expect("an answer, never None");
        assert_eq!(a.status.state, RegisterState::Unreachable, "{mode:?}");
        assert!(
            a.status.reason.as_deref().is_some_and(|r| !r.is_empty()),
            "{mode:?}: {:?}",
            a.status
        );
        assert!(a.candidates.is_empty() && a.plan.is_empty());
        assert_eq!(p.errors(), 1, "{mode:?}");
    }
}

#[tokio::test]
async fn another_region_is_not_asked() {
    let (url, stub) = acma_stub::serve_fixture(fixture(), Mode::Normal).await;
    let p = provider(&url, Some(station(4.0)), None);
    assert_eq!(p.regions(), ["AU"]);
    assert!(
        p.describe("NZ", &signal(), Some(&station(4.0)))
            .await
            .is_none()
    );
    assert_eq!(stub.calls.load(Ordering::SeqCst), 0);
    assert_eq!(stub.healths.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_whole_answer_fits_one_deadline() {
    let (url, _) = acma_stub::serve_fixture(fixture(), Mode::Slow).await;
    let p = provider(&url, None, None).with_timeout(Duration::from_millis(300));
    let t0 = Instant::now();
    let a = p
        .describe("AU", &signal(), Some(&station(4.0)))
        .await
        .unwrap();
    let took = t0.elapsed();
    assert!(took < Duration::from_millis(600), "{took:?}");
    assert_eq!(a.status.state, RegisterState::Unreachable);
}

#[tokio::test]
async fn a_station_near_a_pole_or_the_antimeridian_gets_a_finite_box() {
    for (lat, lon) in [(89.9, 0.0), (-89.9, 10.0), (0.0, 179.9), (10.0, -179.9)] {
        let (url, stub) = acma_stub::serve_fixture(fixture(), Mode::Normal).await;
        let mut at = station(4.0);
        (at.lat, at.lon) = (lat, lon);
        let a = provider(&url, None, None)
            .describe("AU", &signal(), Some(&at))
            .await
            .unwrap();
        assert_eq!(a.status.state, RegisterState::Ok, "{:?}", a.status);
        let b = box_of(&candidates_sql(&stub));
        assert!(b.iter().all(|x| x.is_finite()), "{b:?}");
        assert!(
            (-90.0..=90.0).contains(&b[0]) && (-90.0..=90.0).contains(&b[1]),
            "{b:?}"
        );
        assert!(
            (-180.0..=180.0).contains(&b[2]) && (-180.0..=180.0).contains(&b[3]),
            "{b:?}"
        );
        assert!(b[0] < b[1] && b[2] < b[3], "{b:?}");
    }
}

#[tokio::test]
async fn the_operator_reach_override_bounds_the_search() {
    let (url, stub) = acma_stub::serve_fixture(fixture(), Mode::Normal).await;
    let a = provider(&url, None, Some(10.0))
        .describe("AU", &signal(), Some(&station(4.0)))
        .await
        .unwrap();
    assert_eq!(sites(&a), ["9001"]);
    let b = box_of(&candidates_sql(&stub));
    assert!((b[1] - b[0] - 20.0 / 111.0).abs() < 1e-9, "{b:?}");
}

#[tokio::test]
async fn a_sites_channels_come_from_the_site_channels_query() {
    let (url, stub) = acma_stub::serve_fixture(fixture(), Mode::Normal).await;
    let p = provider(&url, None, None);
    let a = p.site("9001").await.expect("acma lists sites").unwrap();
    assert_eq!(a.provider, "acma");
    assert_eq!(a.site_id, "9001");
    assert_eq!(a.as_of.as_deref(), Some("2026-01-01T00:00:00.000Z"));
    let got: Vec<String> = a
        .channels
        .iter()
        .map(|c| {
            format!(
                "{} {} {} {}",
                c.channel.center_hz.0,
                c.channel.role.as_deref().unwrap_or("-"),
                c.licence_no.as_deref().unwrap_or("-"),
                c.licensee.as_deref().unwrap_or("-"),
            )
        })
        .collect();
    let water = "Example Water Authority";
    assert_eq!(
        got,
        [
            format!("{CHANNEL_HZ} tx 9000001/1 {water}"),
            format!("404600000 tx 9000006/1 {water}"),
            format!("{PAIRED_HZ} rx 9000001/1 {water}"),
        ]
    );
    let first = &a.channels[0].channel;
    assert_eq!(first.bandwidth_hz, Some(Hz(CHANNEL_BW)));
    assert_eq!(first.emission.as_deref(), Some("11K0F3E"));
    let seen = stub.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(seen[0].contains("SITE_ID = '9001'"), "{}", seen[0]);

    // A site the register does not hold has no channels; that is not a failure.
    let none = p.site("9999").await.unwrap().unwrap();
    assert!(none.channels.is_empty());
    assert_eq!(p.errors(), 0);
}

#[tokio::test]
async fn a_site_id_with_a_quote_is_refused_before_any_call() {
    let (url, stub) = acma_stub::serve_fixture(fixture(), Mode::Normal).await;
    let p = provider(&url, None, None);
    for bad in ["9001' OR 1=1 --", "a b", ""] {
        let e = p.site(bad).await.unwrap().unwrap_err();
        assert!(e.contains("site_id"), "{bad}: {e}");
    }
    assert_eq!(stub.queries.load(Ordering::SeqCst), 0);
    assert!(stub.seen.lock().unwrap().is_empty());
}
