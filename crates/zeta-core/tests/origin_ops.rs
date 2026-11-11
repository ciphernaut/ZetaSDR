//! `origin_lookup`: by job (the band and modulation read from the job, one `origin`
//! event on its event stream and its source's feed, the answer kept as
//! `job_context.origins`) or by band (returned only, read-only), over a synthetic
//! register of three rows on the tone band, seen from the demo station (Brisbane
//! Airport).

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Value, json};
use zeta_core::bursts::NewCapture;
use zeta_core::emission::parse_emission;
use zeta_core::knowledge::{Allocation, StaticKnowledge};
use zeta_core::node::{Agree, FromKind, Node, NodeConfig, OriginAnswer, job_modulation, summary};
use zeta_core::ops::{JobKind, Op, OpError, OpResult, schema};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::station::Station;
use zeta_core::stream::Payload;
use zeta_core::types::{
    Baud, Capabilities, Claim, Hz, JobId, PositionFrom, SampleFormat, SourceId, SourcePosition,
    StreamKind,
};
use zeta_source_file::{FileOpts, FileSource, fixtures};

/// The tone fixture's centre and rate.
const TONE_HZ: u64 = 100_000_000;
const RATE_HZ: u32 = 48_000;
const BAND_HZ: u64 = 12_500;
/// The bits chain's symbol rate.
const BAUD: u32 = 4_800;
/// A position near the demo station, not any real receiver's.
const KIWI: SourcePosition = SourcePosition {
    lat: -27.47,
    lon: 153.02,
    alt_m: Some(40.0),
    from: PositionFrom::Status,
};

/// Poll `f` until it gives `Some`, at most `within`.
async fn until<T>(within: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let end = Instant::now() + within;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= end {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A port tunable over 1 MHz..6 GHz, remote, with the position given.
struct RemotePort {
    id: SourceId,
    caps: Capabilities,
    position: Option<SourcePosition>,
    tx: tokio::sync::broadcast::Sender<IqBlock>,
}

impl RemotePort {
    fn port(id: &str, position: Option<SourcePosition>) -> Arc<dyn Port> {
        Arc::new(RemotePort {
            id: SourceId(id.into()),
            caps: Capabilities {
                tune_min_hz: Claim::Asserted(Hz(1_000_000)),
                tune_max_hz: Claim::Asserted(Hz(6_000_000_000)),
                max_bandwidth_hz: Claim::Asserted(Hz(2_000_000)),
                formats: vec![SampleFormat::Cf32],
                channels: 1,
                rx: true,
                tx: false,
                remote: true,
                push: false,
                usable_fraction: Claim::Asserted(1.0),
                depths: vec![],
            },
            position,
            tx: tokio::sync::broadcast::channel(1).0,
        })
    }
}

#[async_trait]
impl Port for RemotePort {
    fn id(&self) -> &SourceId {
        &self.id
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        Ok(t)
    }
    fn current(&self) -> Option<Tuning> {
        None
    }
    fn subscribe(&self) -> IqReceiver {
        self.tx.subscribe()
    }
    fn position(&self) -> Option<SourcePosition> {
        self.position
    }
}

fn demo_station() -> Station {
    Station {
        lat: -27.3842,
        lon: 153.1175,
        alt_m: 4.0,
        region: "AU".into(),
        antennas: Station::default_antennas(4.0),
    }
}

/// One synthetic `tx` row on the tone band.
fn row(licence: &str, licensee: &str, site: &str, emission: &str, center: u64) -> Allocation {
    Allocation {
        provider: "static".into(),
        region: "AU".into(),
        min_hz: Hz(center),
        max_hz: Hz(center),
        service: "Land Mobile".into(),
        note: None,
        kind: Some("assignment".into()),
        licensee: Some(licensee.into()),
        site: Some(site.into()),
        lat: Some(-27.35),
        lon: Some(153.08),
        distance_km: None,
        bearing_deg: None,
        power_w: None,
        center_hz: Some(Hz(center)),
        bandwidth_hz: None,
        licence_no: Some(licence.into()),
        callsign: None,
        role: Some("tx".into()),
        emission: Some(emission.into()),
        emission_decoded: parse_emission(emission),
        as_of: None,
    }
}

/// A register of three rows on the tone band.
fn register(dir: &Path) -> StaticKnowledge {
    let sigs = dir.join("signatures.json");
    std::fs::write(&sigs, "[]").unwrap();
    let allocs = dir.join("allocations.json");
    let mut third = row(
        "0000003",
        "Example Broadcasting",
        "Example Ridge",
        "16K0G1D",
        TONE_HZ - 2_000,
    );
    third.callsign = Some("XX4EX".into());
    let rows = vec![
        row(
            "0000001",
            "Example Water Authority",
            "Example Hill",
            "16K0F1D",
            TONE_HZ,
        ),
        row(
            "0000002",
            "Example Broadcasting",
            "Example Ridge",
            "8K50A3E",
            TONE_HZ + 1_000,
        ),
        third,
    ];
    std::fs::write(&allocs, serde_json::to_string(&rows).unwrap()).unwrap();
    StaticKnowledge::from_files(&sigs, Some(&allocs)).unwrap()
}

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    zeta_dsp::register_all(&mut r);
    r
}

/// A node over the looping tone file `file0`, the remote ports `kiwi` (with a
/// position) and `peer` (without), the demo station set, and the register when
/// `with_register`.
async fn node(dir: &Path, with_register: bool) -> Node {
    let cfg = NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: false,
        bank_factory: None,
        modes_dir: None,
    };
    let meta = fixtures::write_tone(dir);
    let opts = FileOpts {
        realtime: true,
        block_samples: 4800,
        repeat: true,
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(SourceId("file0".into()), &meta, opts).unwrap());
    let ports: Vec<Arc<dyn Port>> = vec![
        src.clone(),
        RemotePort::port("kiwi", Some(KIWI)),
        RemotePort::port("peer", None),
    ];
    let node = Node::open(cfg, registry(), ports).await.unwrap();
    src.start();
    node.set_station(demo_station()).await.unwrap();
    if with_register {
        node.add_knowledge(Arc::new(register(dir)));
    }
    node
}

async fn start(node: &Node, kind: JobKind, source: &str, center: u64) -> JobId {
    let op = Op::JobStart {
        kind,
        source: SourceId(source.into()),
        center_hz: Hz(center),
        bandwidth_hz: Hz(BAND_HZ),
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        elastic: false,
        replace: vec![],
    };
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("job_start: {other:?}"),
    }
}

fn by_job(job: &JobId) -> Op {
    Op::OriginLookup {
        job: Some(job.clone()),
        source: None,
        center_hz: None,
        bandwidth_hz: None,
        modulation: None,
    }
}

fn by_band(source: &str, center: u64, modulation: Option<&[&str]>) -> Op {
    Op::OriginLookup {
        job: None,
        source: Some(SourceId(source.into())),
        center_hz: Some(Hz(center)),
        bandwidth_hz: Some(Hz(BAND_HZ)),
        modulation: modulation.map(|m| m.iter().map(|s| s.to_string()).collect()),
    }
}

async fn lookup(node: &Node, op: Op) -> OriginAnswer {
    match node.apply(op).await {
        Ok(OpResult::Origins(a)) => a,
        other => panic!("origin_lookup: {other:?}"),
    }
}

/// The `origin` event payloads received so far on `rx`.
fn origin_frames(
    rx: &mut tokio::sync::broadcast::Receiver<zeta_core::stream::Frame>,
    seen: &mut Vec<Value>,
) {
    while let Ok(f) = rx.try_recv() {
        if let Payload::Event(v) = f.payload
            && v["protocol"] == "origin"
        {
            seen.push(v);
        }
    }
}

#[tokio::test]
async fn a_decoder_job_reads_its_rows_labels_and_publishes_once() {
    let dir = tempfile::tempdir().unwrap();
    let node = node(dir.path(), true).await;
    let id = start(&node, JobKind::AprsDecode, "file0", TONE_HZ).await;
    let out = until(Duration::from_secs(5), || {
        node.job_outputs(&id)
            .into_iter()
            .find(|(k, _)| *k == StreamKind::Event)
            .map(|(_, s)| s)
    })
    .await
    .expect("the decoder has an event output");
    let bus = node.bus();
    let mut rx = bus.subscribe(&out).unwrap();
    let feed = bus.feed_of(&SourceId("file0".into())).unwrap();
    let mut feed_rx = bus.subscribe(&feed).unwrap();

    let a = lookup(&node, by_job(&id)).await;
    let row = node
        .modes()
        .get("aprs_decode")
        .unwrap()
        .row
        .modulation
        .clone();
    assert_eq!(a.modulation, row);
    assert!(!a.modulation.is_empty());
    assert_eq!(a.source, SourceId("file0".into()));
    assert_eq!(
        (a.band.center_hz, a.band.bandwidth_hz),
        (Hz(TONE_HZ), Hz(BAND_HZ))
    );
    assert_eq!(a.origins.len(), 3);
    assert_eq!(a.from.as_ref().unwrap().kind, FromKind::Station);
    assert!(
        !Op::OriginLookup {
            job: Some(id.clone()),
            source: None,
            center_hz: None,
            bandwidth_hz: None,
            modulation: None
        }
        .read_only()
    );

    let want = serde_json::to_value(&a).unwrap();
    let ctx = until(Duration::from_secs(5), || {
        node.job_context(&id)
            .and_then(|c| c.get("origins").cloned())
    })
    .await
    .expect("job_context.origins");
    assert_eq!(ctx, want);

    let (mut on_job, mut on_feed) = (vec![], vec![]);
    until(Duration::from_secs(5), || {
        origin_frames(&mut rx, &mut on_job);
        origin_frames(&mut feed_rx, &mut on_feed);
        (!on_job.is_empty() && !on_feed.is_empty()).then_some(())
    })
    .await
    .expect("an origin frame on the job's stream and the feed");
    // No second frame follows.
    tokio::time::sleep(Duration::from_millis(200)).await;
    origin_frames(&mut rx, &mut on_job);
    origin_frames(&mut feed_rx, &mut on_feed);
    assert_eq!((on_job.len(), on_feed.len()), (1, 1));
    let e = &on_job[0];
    assert_eq!(e["summary"], summary(&a));
    assert_eq!(e["job"], id.0);
    assert_eq!(e["source"], "file0");
    for k in [
        "band",
        "modulation",
        "from",
        "from_note",
        "origins",
        "providers",
        "register_rows",
        "receivers",
        "plans",
        "reach",
        "stations",
    ] {
        assert_eq!(e[k], want[k], "{k}");
    }
    // No station tally yet: an empty one, and no site heard.
    assert_eq!(e["stations"], json!({}));
    let lit = e["origins"].as_array().unwrap();
    assert_eq!(lit.len(), 3);
    assert!(lit.iter().all(|o| o["heard"].is_null()), "{lit:?}");
    assert!(e["t_ns"].is_u64());
    assert_eq!(on_feed[0]["summary"], e["summary"]);
}

/// 200 ms of 2-FSK at [`BAUD`] on the tone band, captured from `file0`; its id.
fn write_fsk(node: &Node) -> String {
    let symbols: Vec<u8> = (0..960).map(|i| ((i * 7 + i / 3) % 2) as u8).collect();
    let iq = zeta_dsp::synth::gfsk_modulate(&symbols, 2, BAUD, 2_400.0, RATE_HZ);
    let start_ns = 1_760_000_000_000_000_000;
    node.bursts()
        .write(
            NewCapture {
                source: SourceId("file0".into()),
                set: JobId("set1".into()),
                center_hz: Hz(TONE_HZ),
                bandwidth_hz: Hz(20_000),
                sample_rate_hz: Hz(RATE_HZ as u64),
                start_ns,
                end_ns: start_ns + 200_000_000,
                features: json!({
                    "bandwidth_hz": 9_600, "center_hz": TONE_HZ, "symbol_rate_baud": BAUD,
                    "snr_db": 30.0, "burst": false, "duty": 1.0, "bursts": 1,
                    "burst_ms": 200.0, "period_ms": null, "order": 2,
                    "tone_spacing_hz": 4_800,
                }),
                signal: None,
                job: JobId("set1".into()),
                watch: None,
                allocations: json!([]),
            },
            &iq,
        )
        .unwrap()
        .id
}

#[tokio::test]
async fn fm_demod_reads_fm_and_a_bits_job_reads_its_chain_and_the_captures_band() {
    let dir = tempfile::tempdir().unwrap();
    let node = node(dir.path(), true).await;
    let id = start(&node, JobKind::FmDemod, "file0", TONE_HZ).await;
    let rec = node.job_list().into_iter().find(|r| r.id == id).unwrap();
    assert_eq!(job_modulation(&rec, &node.modes()), vec!["fm".to_string()]);
    let a = lookup(&node, by_job(&id)).await;
    assert_eq!(a.modulation, vec!["fm".to_string()]);
    // 16K0F1D fits fm; 8K50A3E (am) disagrees.
    let agree = |licence: &str| {
        a.origins
            .iter()
            .find(|o| o.allocation.licence_no.as_deref() == Some(licence))
            .unwrap()
            .agree
    };
    assert_eq!(agree("0000001"), Agree::Fits);
    assert_eq!(agree("0000002"), Agree::Disagrees);
    // An audio-only job still keeps the answer.
    until(Duration::from_secs(5), || {
        node.job_context(&id)
            .and_then(|c| c.get("origins").cloned())
    })
    .await
    .expect("job_context.origins");

    let capture = write_fsk(&node);
    let bits = match node
        .apply(Op::BitsStart {
            capture,
            modulation: Some(zeta_core::bits::Modulation::Fsk),
            symbol_rate_baud: Some(Baud(BAUD)),
            order: None,
        })
        .await
    {
        Ok(OpResult::Job(id)) => id,
        other => panic!("bits_start: {other:?}"),
    };
    let a = lookup(&node, by_job(&bits)).await;
    assert_eq!(a.modulation, vec!["fsk".to_string()]);
    // The capture's band, and its source for the vantage.
    assert_eq!(
        (a.band.center_hz, a.band.bandwidth_hz),
        (Hz(TONE_HZ), Hz(20_000))
    );
    assert_eq!(a.source, SourceId("file0".into()));
    assert_eq!(a.from.unwrap().kind, FromKind::Station);
}

#[tokio::test]
async fn a_mix_job_is_refused_invalid_naming_the_kind() {
    let dir = tempfile::tempdir().unwrap();
    let node = node(dir.path(), true).await;
    let mix = match node.apply(Op::MixStart { sources: None }).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("mix_start: {other:?}"),
    };
    match node.apply(by_job(&mix)).await {
        Err(OpError::Other { message }) => {
            assert_eq!(message, "origin_lookup: a mix job has no band")
        }
        other => panic!("expected the refusal, got {other:?}"),
    }
    match node.apply(by_job(&JobId("nope".into()))).await {
        Err(OpError::Unknown { field, value, .. }) => {
            assert_eq!((field.as_str(), value.as_str()), ("job", "nope"))
        }
        other => panic!("expected unknown job, got {other:?}"),
    }
}

#[tokio::test]
async fn the_band_form_returns_only_and_is_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let node = node(dir.path(), true).await;
    let streams = node.bus().list().len();
    let op = by_band("file0", TONE_HZ, None);
    assert!(op.read_only());
    let a = lookup(&node, op).await;
    assert!(a.modulation.is_empty());
    assert_eq!(a.origins.len(), 3);
    assert!(a.origins.iter().all(|o| o.agree == Agree::Unknown));
    // No stream registered, no job.
    assert_eq!(node.bus().list().len(), streams);
    assert!(node.job_list().is_empty());

    let a = lookup(&node, by_band("file0", TONE_HZ, Some(&["fm"]))).await;
    assert_eq!(a.modulation, vec!["fm".to_string()]);
    let fits = a
        .origins
        .iter()
        .find(|o| o.allocation.emission.as_deref() == Some("16K0F1D"))
        .unwrap();
    assert_eq!(fits.agree, Agree::Fits);
}

/// The band form belongs to no job, so it has no station tally: no site is heard and
/// `stations` is empty, written as `{}`, and each origin's `heard` as `null`.
#[tokio::test]
async fn the_band_form_carries_no_heard_or_stations() {
    let dir = tempfile::tempdir().unwrap();
    let node = node(dir.path(), true).await;
    let a = lookup(&node, by_band("file0", TONE_HZ, Some(&["fm"]))).await;
    assert_eq!(a.origins.len(), 3);
    assert!(a.stations.is_empty());
    assert!(a.origins.iter().all(|o| o.heard.is_none()));
    let v = serde_json::to_value(&a).unwrap();
    assert_eq!(v["stations"], json!({}));
    let lit = v["origins"].as_array().unwrap();
    assert!(
        lit.iter()
            .all(|o| o.as_object().unwrap().contains_key("heard") && o["heard"].is_null()),
        "{lit:?}"
    );
}

/// `classify` over the tone's ring with a register attached: the answer's modulation is
/// the measured layer-1 label and the register's licensee lights.
#[tokio::test]
async fn classify_on_a_tone_scores_origins_on_the_measured_label() {
    let dir = tempfile::tempdir().unwrap();
    let node = node(dir.path(), true).await;
    for c in zeta_dsp::classifiers(node.modes_handle()) {
        node.add_classifier(c);
    }
    let source = SourceId("file0".into());
    let mut earliest = None;
    for _ in 0..80 {
        earliest = node
            .ring(&source)
            .and_then(|r| r.read().unwrap().earliest());
        if earliest.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let start = earliest.expect("ring has data").0;
    let selection = zeta_core::types::Selection {
        source,
        span: zeta_core::types::Span {
            start_ns: zeta_core::types::Timestamp(start),
            end_ns: zeta_core::types::Timestamp(start + 50_000_000),
        },
        center_hz: Hz(TONE_HZ),
        bandwidth_hz: Hz(BAND_HZ),
    };
    let Ok(OpResult::Classification {
        labels, origins, ..
    }) = node.apply(Op::Classify { selection }).await
    else {
        panic!("expected Classification");
    };
    let l1 = labels.iter().find(|l| l.layer == 1).expect("layer 1");
    let origins = origins.expect("origins");
    assert_eq!(origins.modulation, vec![l1.name.clone()], "{labels:?}");
    assert!(
        origins
            .origins
            .iter()
            .any(|o| { o.allocation.licensee.as_deref() == Some("Example Water Authority") }),
        "{origins:?}"
    );
}

/// The band form refuses a source the node does not have, as other source ops do.
#[tokio::test]
async fn the_band_form_refuses_an_unknown_source() {
    let dir = tempfile::tempdir().unwrap();
    let node = node(dir.path(), true).await;
    let Err(OpError::Unknown {
        field,
        value,
        known,
    }) = node.apply(by_band("nowhere", TONE_HZ, None)).await
    else {
        panic!("expected unknown source");
    };
    assert_eq!(field, "source");
    assert_eq!(value, "nowhere");
    assert!(known.contains(&"file0".to_string()), "{known:?}");
}

/// The band form refuses a band of no width, as the job form refuses a job with none.
#[tokio::test]
async fn the_band_form_refuses_a_band_of_no_width() {
    let dir = tempfile::tempdir().unwrap();
    let node = node(dir.path(), true).await;
    let op = Op::OriginLookup {
        job: None,
        source: Some(SourceId("file0".into())),
        center_hz: Some(Hz(TONE_HZ)),
        bandwidth_hz: Some(Hz(0)),
        modulation: None,
    };
    match node.apply(op).await {
        Err(OpError::Other { message }) => {
            assert_eq!(message, "origin_lookup: a band of no width")
        }
        other => panic!("expected the refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn both_or_neither_form_is_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let node = node(dir.path(), true).await;
    let id = start(&node, JobKind::FmDemod, "file0", TONE_HZ).await;
    let want = "origin_lookup: give job, or source, center_hz and bandwidth_hz";
    let neither = Op::OriginLookup {
        job: None,
        source: None,
        center_hz: None,
        bandwidth_hz: None,
        modulation: None,
    };
    let mut both = by_band("file0", TONE_HZ, None);
    if let Op::OriginLookup { job, .. } = &mut both {
        *job = Some(id.clone());
    }
    let partial = Op::OriginLookup {
        job: None,
        source: Some(SourceId("file0".into())),
        center_hz: Some(Hz(TONE_HZ)),
        bandwidth_hz: None,
        modulation: None,
    };
    let job_and_centre = Op::OriginLookup {
        job: Some(id.clone()),
        source: None,
        center_hz: Some(Hz(TONE_HZ)),
        bandwidth_hz: None,
        modulation: None,
    };
    for op in [neither, both, partial, job_and_centre] {
        match node.apply(op.clone()).await {
            Err(OpError::Other { message }) => assert_eq!(message, want, "{op:?}"),
            other => panic!("{op:?}: expected the refusal, got {other:?}"),
        }
    }
    // Every field is required, if only as null.
    let missing = json!({"op": "origin_lookup", "job": null, "source": "file0",
        "center_hz": TONE_HZ, "bandwidth_hz": BAND_HZ});
    assert!(serde_json::from_value::<Op>(missing).is_err());
    let full = json!({"op": "origin_lookup", "job": null, "source": "file0",
        "center_hz": TONE_HZ, "bandwidth_hz": BAND_HZ, "modulation": null});
    assert_eq!(
        serde_json::from_value::<Op>(full).unwrap(),
        by_band("file0", TONE_HZ, None)
    );
}

#[tokio::test]
async fn a_remote_source_without_a_position_has_from_null_and_the_note() {
    let dir = tempfile::tempdir().unwrap();
    let node = node(dir.path(), true).await;
    let id = start(&node, JobKind::Passthrough, "peer", TONE_HZ).await;
    let a = lookup(&node, by_job(&id)).await;
    assert_eq!(a.source, SourceId("peer".into()));
    assert_eq!(a.from, None);
    assert_eq!(
        a.from_note.as_deref(),
        Some("remote source without a position")
    );
    assert_eq!(a.origins.len(), 3);
}

#[tokio::test]
async fn a_remote_source_with_a_position_answers_from_source() {
    let dir = tempfile::tempdir().unwrap();
    let node = node(dir.path(), true).await;
    let id = start(&node, JobKind::Passthrough, "kiwi", TONE_HZ).await;
    let a = lookup(&node, by_job(&id)).await;
    let from = a.from.expect("a vantage");
    assert_eq!(
        (from.lat, from.lon, from.kind, from.source),
        (
            KIWI.lat,
            KIWI.lon,
            FromKind::Source,
            Some(SourceId("kiwi".into()))
        )
    );
    assert_eq!(a.from_note, None);
}

#[tokio::test]
async fn with_no_provider_providers_and_origins_are_empty_and_the_summary_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let node = node(dir.path(), false).await;
    let a = lookup(&node, by_band("file0", TONE_HZ, Some(&["fm"]))).await;
    // Only the node's own accepted signatures, which give no allocations.
    assert_eq!(a.providers, vec!["learned".to_string()]);
    assert!(a.origins.is_empty());
    assert_eq!(a.register_rows, None);
    assert_eq!(summary(&a), "no register provider");
}

#[test]
fn schema_lists_origin_lookup_and_the_origins_result() {
    let s = schema();
    let ops = s["op"]["oneOf"]
        .as_array()
        .or_else(|| s["op"]["anyOf"].as_array())
        .unwrap();
    let v = ops
        .iter()
        .find(|v| v["properties"]["op"]["const"] == "origin_lookup")
        .expect("origin_lookup variant");
    let mut req: Vec<&str> = v["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    req.sort();
    assert_eq!(
        req,
        [
            "bandwidth_hz",
            "center_hz",
            "job",
            "modulation",
            "op",
            "source"
        ]
    );
    assert_eq!(v["additionalProperties"], Value::Bool(false));
    let results = serde_json::to_string(&s["result"]).unwrap();
    assert!(results.contains("\"origins\""), "the origins result");
    let r = OpResult::Origins(OriginAnswer {
        origins: vec![],
        band: zeta_core::ops::SignalBand {
            center_hz: Hz(TONE_HZ),
            bandwidth_hz: Hz(BAND_HZ),
        },
        source: SourceId("file0".into()),
        modulation: vec![],
        symbol_rate_baud: None,
        from: None,
        from_note: Some("no station set".into()),
        providers: vec![],
        register_rows: None,
        receivers: 0,
        plans: 0,
        reach: None,
        t_ns: zeta_core::types::Timestamp(0),
        stations: Default::default(),
    });
    assert_eq!(serde_json::to_value(&r).unwrap()["result"], "origins");
}
