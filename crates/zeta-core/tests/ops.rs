//! Operation set: corrective errors, job round trip, IQ capture and schema strictness.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use zeta_core::classify::StubClassifier;
use zeta_core::job::JobState;
use zeta_core::knowledge::{Allocation, KnowledgeProvider, Signature};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{
    AUDIO_RATE_HZ, JobKind, Op, OpError, OpResult, Sideband, channel_rate_hz, schema,
};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{
    Capabilities, Claim, Hz, SampleFormat, Selection, SourceId, Span, Timestamp,
};
use zeta_core::types::{JobId, StreamId, StreamKind};
use zeta_source_file::{FileOpts, FileSource, fixtures, read_meta};

/// A port tunable over 1 MHz..6 GHz that accepts any in-range request.
struct WidePort {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tx: tokio::sync::broadcast::Sender<IqBlock>,
    /// Sleep inside `tune`, to widen the window a concurrent call could race into.
    slow: bool,
    /// `tune` calls so far, counted on entry.
    tunes: std::sync::atomic::AtomicUsize,
    /// What `fault()` reports.
    fault: Mutex<Option<String>>,
}

impl WidePort {
    fn slow(id: &str) -> Self {
        Self {
            slow: true,
            ..Self::new(id)
        }
    }

    fn new(id: &str) -> Self {
        Self {
            slow: false,
            tunes: std::sync::atomic::AtomicUsize::new(0),
            fault: Mutex::new(None),
            id: SourceId(id.into()),
            caps: Capabilities {
                tune_min_hz: Claim::Asserted(Hz(1_000_000)),
                tune_max_hz: Claim::Asserted(Hz(6_000_000_000)),
                max_bandwidth_hz: Claim::Asserted(Hz(20_000_000)),
                formats: vec![SampleFormat::Cf32],
                channels: 1,
                rx: true,
                tx: false,
                remote: false,
                push: false,
            },
            cur: Mutex::new(None),
            tx: tokio::sync::broadcast::channel(1).0,
        }
    }
}

#[async_trait]
impl Port for WidePort {
    fn id(&self) -> &SourceId {
        &self.id
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        self.tunes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.slow {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        *self.cur.lock().unwrap() = Some(t);
        Ok(t)
    }
    fn current(&self) -> Option<Tuning> {
        *self.cur.lock().unwrap()
    }
    fn subscribe(&self) -> IqReceiver {
        self.tx.subscribe()
    }
    fn fault(&self) -> Option<String> {
        self.fault.lock().unwrap().clone()
    }
}

fn cfg(dir: &Path) -> NodeConfig {
    NodeConfig {
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
    }
}

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    r
}

fn file_source(dir: &Path) -> (Arc<FileSource>, Arc<dyn Port>) {
    let meta = fixtures::write_tone(dir);
    let opts = FileOpts {
        realtime: false,
        block_samples: 4800,
        repeat: true,
    };
    let src = Arc::new(FileSource::open(SourceId("file0".into()), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    (src, port)
}

#[tokio::test]
async fn ensure_out_of_range_suggests_dropped_zeros() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(
        cfg(dir.path()),
        registry(),
        vec![Arc::new(WidePort::new("hackrf0"))],
    )
    .await
    .unwrap();
    let op = Op::SourceEnsure {
        source: SourceId("hackrf0".into()),
        center_hz: Hz(101_500),
        bandwidth_hz: Hz(200_000),
    };
    match node.apply(op).await {
        Err(OpError::OutOfRange {
            field,
            value,
            min,
            max,
            suggested,
        }) => {
            assert_eq!(field, "center_hz");
            assert_eq!((value, min, max), (101_500, 1_000_000, 6_000_000_000));
            assert_eq!(suggested, Some(101_500_000));
        }
        other => panic!("expected OutOfRange, got {other:?}"),
    }
}

#[tokio::test]
async fn ensure_unknown_source_lists_known() {
    let dir = tempfile::tempdir().unwrap();
    let ports: Vec<Arc<dyn Port>> = vec![
        Arc::new(WidePort::new("rtl0")),
        Arc::new(WidePort::new("hackrf0")),
    ];
    let node = Node::open(cfg(dir.path()), registry(), ports)
        .await
        .unwrap();
    let op = Op::SourceEnsure {
        source: SourceId("nope".into()),
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(200_000),
    };
    match node.apply(op).await {
        Err(OpError::Unknown {
            field,
            value,
            known,
        }) => {
            assert_eq!(field, "source");
            assert_eq!(value, "nope");
            assert_eq!(known, vec!["hackrf0".to_string(), "rtl0".to_string()]);
        }
        other => panic!("expected Unknown, got {other:?}"),
    }
}

#[tokio::test]
async fn job_start_passthrough_then_list_then_stop() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = file_source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let op = Op::JobStart {
        kind: JobKind::Passthrough,
        source: SourceId("file0".into()),
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(48_000),
        deviation_hz: None,
        sideband: None,
    };
    let id = match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected Job, got {other:?}"),
    };
    match node.apply(Op::JobList {}).await {
        Ok(OpResult::Jobs(jobs)) => {
            assert_eq!(jobs.len(), 1);
            assert_eq!(jobs[0].id, id);
            assert_eq!(jobs[0].kind, "passthrough");
            assert_eq!(jobs[0].state, JobState::Running);
        }
        other => panic!("expected Jobs, got {other:?}"),
    }
    assert_eq!(
        node.apply(Op::JobStop { job: id.clone() }).await,
        Ok(OpResult::Ok)
    );
    match node.apply(Op::JobList {}).await {
        Ok(OpResult::Jobs(jobs)) => assert_eq!(jobs[0].state, JobState::Stopped),
        other => panic!("expected Jobs, got {other:?}"),
    }
    assert!(node.lease_holders(&SourceId("file0".into())).is_empty());
}

/// Wait for the source's ring to fill, then return a 50 ms selection from its start.
async fn selection(node: &Node, center_hz: u64, bandwidth_hz: u64) -> Selection {
    let source = SourceId("file0".into());
    let mut earliest = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(25)).await;
        earliest = node
            .ring(&source)
            .and_then(|r| r.read().unwrap().earliest());
        if earliest.is_some() {
            break;
        }
    }
    let start = earliest.expect("ring has data").0;
    Selection {
        source,
        span: Span {
            start_ns: Timestamp(start),
            end_ns: Timestamp(start + 50_000_000),
        },
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(bandwidth_hz),
    }
}

#[tokio::test]
async fn iq_read_writes_sigmf_pair() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = file_source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let selection = selection(&node, 100_000_000, 48_000).await;
    let start = selection.span.start_ns;
    let (rate, samples, path) = match node.apply(Op::IqRead { selection }).await {
        Ok(OpResult::Iq {
            sample_rate_hz,
            samples,
            path,
            ..
        }) => (sample_rate_hz, samples, path),
        other => panic!("expected Iq, got {other:?}"),
    };
    assert_eq!(rate, Hz(48_000));
    assert_eq!(samples, 2400);
    assert!(path.starts_with(dir.path().join("state").join("captures")));
    let meta_path = path.with_extension("sigmf-meta");
    assert!(meta_path.exists());
    let meta = read_meta(&meta_path).unwrap();
    assert_eq!(meta.datatype, "cf32_le");
    assert_eq!(meta.sample_rate_hz, Hz(48_000));
    assert_eq!(meta.center_hz, Hz(100_000_000));
    assert_eq!(meta.start, start);
    let data = std::fs::metadata(path.with_extension("sigmf-data")).unwrap();
    assert_eq!(data.len(), 2400 * 8);
}

#[tokio::test]
async fn iq_read_labels_capture_centre_not_selection_centre() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = file_source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    // A 10 kHz band 10 kHz above the ring's 100 MHz centre.
    let selection = selection(&node, 100_010_000, 10_000).await;
    let (center, bandwidth, path) = match node.apply(Op::IqRead { selection }).await {
        Ok(OpResult::Iq {
            center_hz,
            bandwidth_hz,
            path,
            ..
        }) => (center_hz, bandwidth_hz, path),
        other => panic!("expected Iq, got {other:?}"),
    };
    assert_eq!((center, bandwidth), (Hz(100_000_000), Hz(48_000)));
    assert_eq!(read_meta(&path).unwrap().center_hz, Hz(100_000_000));
    let meta: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let a = &meta["annotations"][0];
    assert_eq!(a["core:label"], "selection");
    assert_eq!(a["core:sample_start"], 0);
    assert_eq!(a["core:sample_count"], 2400);
    assert_eq!(a["core:freq_lower_edge"], 100_005_000);
    assert_eq!(a["core:freq_upper_edge"], 100_015_000);
}

#[tokio::test]
async fn source_ensure_during_job_start_does_not_retune() {
    let dir = tempfile::tempdir().unwrap();
    let port = Arc::new(WidePort::slow("hackrf0"));
    let node = Node::open(cfg(dir.path()), registry(), vec![port.clone()])
        .await
        .unwrap();
    let source = SourceId("hackrf0".into());
    let job_tuning = Tuning {
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(200_000),
    };
    let start = Op::JobStart {
        kind: JobKind::Passthrough,
        source: source.clone(),
        center_hz: job_tuning.center_hz,
        bandwidth_hz: job_tuning.bandwidth_hz,
        deviation_hz: None,
        sideband: None,
    };
    let ensure = Op::SourceEnsure {
        source,
        center_hz: Hz(433_000_000),
        bandwidth_hz: Hz(200_000),
    };
    // `job_start` tunes twice: once (unheld) to learn the source rate, then
    // under its lease. Send `ensure` once the second, slow tune has begun, so
    // it lands inside the job's lease-and-tune.
    let ensured = async {
        while port.tunes.load(std::sync::atomic::Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        node.apply(ensure).await
    };
    let (started, ensured) = tokio::join!(node.apply(start), ensured);
    let job: JobId = match started {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected Job, got {other:?}"),
    };
    match ensured {
        Err(OpError::Conflict { holders, .. }) => assert_eq!(holders, vec![job]),
        Ok(OpResult::Tuning(t)) => assert_eq!(t, job_tuning),
        other => panic!("expected Conflict or the job's tuning, got {other:?}"),
    }
    assert_eq!(port.current(), Some(job_tuning));
}

/// A stage that runs until cancelled, registered under another stage's kind
/// so graphs naming DSP stages validate in core tests.
struct IdleStage(zeta_core::stage::StageSpec);

impl IdleStage {
    fn new(kind: &'static str, input: StreamKind, output: StreamKind) -> Self {
        Self(zeta_core::stage::StageSpec {
            kind,
            inputs: vec![input],
            outputs: vec![output],
            params_schema: schemars::json_schema!({}),
        })
    }
}

#[async_trait]
impl zeta_core::stage::Stage for IdleStage {
    fn spec(&self) -> &zeta_core::stage::StageSpec {
        &self.0
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    async fn run(
        &self,
        _params: Value,
        _inputs: Vec<tokio::sync::broadcast::Receiver<zeta_core::stream::Frame>>,
        _outputs: Vec<StreamId>,
        _bus: Arc<zeta_core::stream::StreamBus>,
        cancel: tokio_util::sync::CancellationToken,
        _ctx: zeta_core::stage::StageContext,
    ) -> Result<(), zeta_core::stage::StageError> {
        cancel.cancelled().await;
        Err(zeta_core::stage::StageError::Cancelled)
    }
}

#[tokio::test]
async fn job_start_reexpands_when_lease_rate_differs() {
    let dir = tempfile::tempdir().unwrap();
    let port = Arc::new(WidePort::slow("hackrf0"));
    let mut reg = registry();
    reg.register(Arc::new(IdleStage::new(
        "channel",
        StreamKind::Iq,
        StreamKind::Iq,
    )));
    reg.register(Arc::new(IdleStage::new(
        "nbfm_demod",
        StreamKind::Iq,
        StreamKind::Audio,
    )));
    let node = Node::open(cfg(dir.path()), reg, vec![port.clone()])
        .await
        .unwrap();
    let source = SourceId("hackrf0".into());
    // A: narrow; WidePort grants the requested band, so its pre-tune rate is 12.5 kHz.
    let a = Op::JobStart {
        kind: JobKind::FmDemod,
        source: source.clone(),
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(12_500),
        deviation_hz: None,
        sideband: None,
    };
    // B: 2 MHz around A's band, leased while A's slow pre-tune holds the gate,
    // so B's lease is queued ahead of A's and A's lease shares B's 2 MS/s capture.
    let wide = Tuning {
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(2_000_000),
    };
    let b = async {
        while port.tunes.load(std::sync::atomic::Ordering::SeqCst) < 1 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let g = JobKind::Passthrough
            .expand(&source, wide, wide.bandwidth_hz, None, None, None)
            .unwrap();
        node.job_start("passthrough", source.clone(), wide, g).await
    };
    let (a, b) = tokio::join!(node.apply(a), b);
    let a = match a {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected Job, got {other:?}"),
    };
    b.unwrap();
    assert_eq!(port.current(), Some(wide));
    let rec = node.job_list().into_iter().find(|r| r.id == a).unwrap();
    assert_eq!(rec.state, JobState::Running);
    let out = rec.graph.nodes[0].params["out_rate_hz"].as_u64().unwrap();
    assert_eq!(out, 50_000);
    assert_eq!(2_000_000 % out, 0);
    assert_eq!(rec.graph.nodes[1].params["audio_rate_hz"], 48_000);
    // Relaunched under the same id: no stray stopped record.
    assert_eq!(node.job_list().len(), 2);
}

fn variants(op: &Value) -> &Vec<Value> {
    op.get("oneOf")
        .or_else(|| op.get("anyOf"))
        .and_then(Value::as_array)
        .expect("op schema has oneOf/anyOf")
}

#[test]
fn every_op_variant_denies_unknown_fields_and_requires_all() {
    let s = schema();
    let vs = variants(&s["op"]);
    assert_eq!(vs.len(), 14, "one schema per Op variant");
    for v in vs {
        assert_eq!(v["additionalProperties"], Value::Bool(false), "{v}");
        let mut props: Vec<&String> = v["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .collect();
        let mut req: Vec<&str> = v["required"]
            .as_array()
            .expect("required")
            .iter()
            .map(|r| r.as_str().unwrap())
            .collect();
        props.sort();
        req.sort();
        assert_eq!(req, props, "{v}");
        assert!(req.contains(&"op"), "{v}");
    }
    let bad = serde_json::json!({"op": "job_list", "extra": 1});
    assert!(serde_json::from_value::<Op>(bad).is_err());
}

/// Every object schema with properties, anywhere under `v` (including `$defs`),
/// forbids extra fields and requires every property.
fn check_strict(v: &Value, path: &str) {
    match v {
        Value::Object(m) => {
            if let Some(Value::Object(props)) = m.get("properties") {
                assert_eq!(
                    m.get("additionalProperties"),
                    Some(&Value::Bool(false)),
                    "{path} allows extra fields"
                );
                let mut want: Vec<&str> = props.keys().map(String::as_str).collect();
                let mut req: Vec<&str> = m
                    .get("required")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                want.sort();
                req.sort();
                assert_eq!(req, want, "{path} does not require every property");
            }
            for (k, c) in m {
                check_strict(c, &format!("{path}.{k}"));
            }
        }
        Value::Array(a) => a
            .iter()
            .enumerate()
            .for_each(|(i, c)| check_strict(c, &format!("{path}[{i}]"))),
        _ => {}
    }
}

#[test]
fn nested_schemas_are_strict() {
    let s = schema();
    for key in ["op", "result", "error"] {
        check_strict(&s[key], key);
    }
    // Stage params, as `node.schema()["stages"]` publishes them.
    for st in registry().describe().as_array().unwrap() {
        check_strict(&st["params_schema"], &format!("stages.{}", st["kind"]));
    }
    // stream_tail: provenance fields on every frame, and the missed count.
    let fs = &s["result"]["$defs"]["FrameSummary"];
    for f in ["source", "job", "channel"] {
        assert!(fs["properties"].get(f).is_some(), "FrameSummary.{f}: {fs}");
    }
    let frames = variants(&s["result"])
        .iter()
        .find(|v| v["properties"]["result"]["const"] == "frames")
        .expect("frames result variant");
    let value = &frames["properties"]["value"];
    for f in ["frames", "missed"] {
        assert!(value["properties"].get(f).is_some(), "frames.{f}: {value}");
    }
}

/// Whether a unit field's schema `s` is an integer or a `$ref` to a unit type.
/// `Option<T>` (`anyOf` with `null`) and `Claim<T>` (a def whose variants each
/// hold one unit value under `asserted` / `measured`) are unwrapped.
fn unit_typed(s: &Value, defs: &Value) -> bool {
    let is_int =
        |t: &Value| t == "integer" || t.as_array().is_some_and(|a| a.contains(&"integer".into()));
    if is_int(&s["type"]) {
        return true;
    }
    if let Some(alts) = s["anyOf"].as_array() {
        return alts
            .iter()
            .filter(|a| a["type"] != "null")
            .all(|a| unit_typed(a, defs));
    }
    match s["$ref"].as_str().and_then(|r| r.rsplit('/').next()) {
        Some("Hz" | "Baud" | "Timestamp") => true,
        Some(n) if n.starts_with("Claim") => defs[n]["oneOf"].as_array().is_some_and(|vs| {
            vs.iter().all(|v| {
                let props = v["properties"].as_object();
                props.is_some_and(|p| p.len() == 1 && p.values().all(|x| unit_typed(x, defs)))
            })
        }),
        _ => false,
    }
}

#[derive(Default)]
struct UnitsSeen {
    hz: usize,
    ns: usize,
}

fn check_units(v: &Value, defs: &Value, path: &str, seen: &mut UnitsSeen) {
    match v {
        Value::Object(m) => {
            if let Some(Value::Object(props)) = m.get("properties") {
                for (name, s) in props {
                    let unit = ["freq", "rate", "bandwidth", "center"]
                        .iter()
                        .any(|w| name.contains(w));
                    assert!(
                        !unit || name.ends_with("_hz") || name.ends_with("_baud"),
                        "{path}.{name} is a unit field without _hz/_baud"
                    );
                    let time = [
                        "start",
                        "end",
                        "created",
                        "requested",
                        "earliest",
                        "latest",
                        "time",
                    ]
                    .iter()
                    .any(|w| name.contains(w));
                    // `_ns` is an instant; `_ms` a duration (a timeout), plain integer.
                    assert!(
                        !time || name.ends_with("_ns") || name.ends_with("_ms"),
                        "{path}.{name} is a time field without _ns/_ms"
                    );
                    if name.ends_with("_ms") {
                        assert_eq!(s["type"], "integer", "{path}.{name} is not an integer");
                    }
                    if ["_hz", "_ns", "_baud"].iter().any(|u| name.ends_with(u)) {
                        assert!(unit_typed(s, defs), "{path}.{name} is not a unit type: {s}");
                    }
                    seen.hz += name.ends_with("_hz") as usize;
                    seen.ns += name.ends_with("_ns") as usize;
                }
            }
            for (k, c) in m {
                check_units(c, defs, &format!("{path}.{k}"), seen);
            }
        }
        Value::Array(a) => a
            .iter()
            .enumerate()
            .for_each(|(i, c)| check_units(c, defs, &format!("{path}[{i}]"), seen)),
        _ => {}
    }
}

#[test]
fn unit_fields_carry_their_unit() {
    let s = schema();
    let mut seen = UnitsSeen::default();
    for key in ["op", "result", "error"] {
        assert!(s[key].is_object(), "schema has {key}");
        check_units(&s[key], &s[key]["$defs"], key, &mut seen);
    }
    assert!(seen.hz > 0, "no _hz field seen");
    assert!(seen.ns > 0, "no _ns field seen");
}

struct TestProvider;

#[async_trait]
impl KnowledgeProvider for TestProvider {
    fn name(&self) -> &str {
        "test"
    }
    async fn allocations(
        &self,
        region: &str,
        _center_hz: Hz,
        _bandwidth_hz: Hz,
    ) -> Vec<Allocation> {
        vec![Allocation {
            provider: "test".into(),
            region: region.into(),
            min_hz: Hz(144_000_000),
            max_hz: Hz(148_000_000),
            service: "amateur".into(),
            note: None,
        }]
    }
    async fn signatures(&self, _center_hz: Hz, _bandwidth_hz: Hz) -> Vec<Signature> {
        vec![]
    }
}

#[tokio::test]
async fn knowledge_lookup_op_returns_allocations_from_provider() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(cfg(dir.path()), registry(), vec![])
        .await
        .unwrap();
    node.add_knowledge(Arc::new(TestProvider));
    let op = Op::KnowledgeLookup {
        region: "AU".into(),
        center_hz: Hz(144_800_000),
        bandwidth_hz: Hz(25_000),
    };
    match node.apply(op).await {
        Ok(OpResult::Knowledge {
            allocations,
            signatures,
        }) => {
            assert_eq!(allocations.len(), 1);
            assert_eq!(allocations[0].provider, "test");
            assert_eq!(allocations[0].region, "AU");
            assert!(signatures.is_empty());
        }
        other => panic!("expected Knowledge, got {other:?}"),
    }
}

#[tokio::test]
async fn classify_op_runs_stub_and_returns_empty_labels() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = file_source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    node.add_classifier(Arc::new(StubClassifier));
    src.start();
    let selection = selection(&node, 100_000_000, 48_000).await;
    assert_eq!(
        node.apply(Op::Classify { selection }).await,
        Ok(OpResult::Labels(vec![]))
    );
}

#[tokio::test]
async fn iq_read_rejects_band_outside_ring() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = file_source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let selection = selection(&node, 433_000_000, 12_500).await;
    match node.apply(Op::IqRead { selection }).await {
        Err(OpError::OutOfRange {
            field,
            value,
            min,
            max,
            ..
        }) => {
            assert_eq!(field, "selection.center_hz");
            assert_eq!(value, 433_000_000);
            // 100 MHz ring at 48 kHz: centres keeping a 12.5 kHz band inside it.
            assert_eq!((min, max), (99_982_250, 100_017_750));
        }
        other => panic!("expected OutOfRange, got {other:?}"),
    }
}

#[tokio::test]
async fn iq_read_rejects_zero_bandwidth() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = file_source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let selection = selection(&node, 100_000_000, 0).await;
    match node.apply(Op::IqRead { selection }).await {
        Err(OpError::OutOfRange {
            field,
            value,
            min,
            max,
            ..
        }) => {
            assert_eq!(field, "selection.bandwidth_hz");
            assert_eq!((value, min, max), (0, 1, 48_000));
        }
        other => panic!("expected OutOfRange, got {other:?}"),
    }
}

#[tokio::test]
async fn conflict_reports_current_tuning_and_real_options() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(
        cfg(dir.path()),
        registry(),
        vec![Arc::new(WidePort::new("hackrf0"))],
    )
    .await
    .unwrap();
    let source = SourceId("hackrf0".into());
    let held = Tuning {
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(200_000),
    };
    let start = Op::JobStart {
        kind: JobKind::Passthrough,
        source: source.clone(),
        center_hz: held.center_hz,
        bandwidth_hz: held.bandwidth_hz,
        deviation_hz: None,
        sideband: None,
    };
    let job = match node.apply(start).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected Job, got {other:?}"),
    };
    let ensure = Op::SourceEnsure {
        source: source.clone(),
        center_hz: Hz(433_000_000),
        bandwidth_hz: Hz(200_000),
    };
    match node.apply(ensure).await {
        Err(OpError::Conflict {
            source: s,
            holders,
            current,
            options,
        }) => {
            assert_eq!(s, source);
            assert_eq!(holders, vec![job]);
            assert_eq!(current, held);
            assert_eq!(
                options,
                vec!["stop one of the holders", "choose a band inside current"]
            );
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[tokio::test]
async fn schema_op_lists_registered_stages() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(cfg(dir.path()), registry(), vec![])
        .await
        .unwrap();
    let v = match node.apply(Op::Schema {}).await {
        Ok(OpResult::Schema(v)) => v,
        other => panic!("expected Schema, got {other:?}"),
    };
    for key in ["op", "result", "error"] {
        assert_eq!(v[key], schema()[key], "{key}");
    }
    let stages = v["stages"].as_array().expect("stages array");
    assert_eq!(stages.len(), 1);
    let p = &stages[0];
    assert_eq!(p["kind"], "passthrough");
    assert!(p["inputs"].as_array().unwrap().contains(&"iq".into()));
    assert!(p["outputs"].as_array().unwrap().contains(&"iq".into()));
    assert_eq!(p["params_schema"]["additionalProperties"], false);
    assert!(p["params_schema"]["properties"]["kind"].is_object());
}

#[tokio::test]
async fn reversed_span_is_bad_span_even_before_ring_data() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, port) = file_source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    let selection = Selection {
        source: SourceId("file0".into()),
        span: Span {
            start_ns: Timestamp(2),
            end_ns: Timestamp(1),
        },
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(48_000),
    };
    assert_eq!(
        node.apply(Op::Classify { selection }).await,
        Err(OpError::Ring {
            detail: zeta_core::ring::RingError::BadSpan {
                start_ns: Timestamp(2),
                end_ns: Timestamp(1)
            }
        })
    );
}

#[tokio::test]
async fn graph_put_rejects_unknown_stage_and_leaves_job_running() {
    use zeta_core::graph::{Graph, GraphError, GraphId, Node as GraphNode, NodeId};
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = file_source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let start = Op::JobStart {
        kind: JobKind::Passthrough,
        source: SourceId("file0".into()),
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(48_000),
        deviation_hz: None,
        sideband: None,
    };
    let job = match node.apply(start).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected Job, got {other:?}"),
    };
    let before = node.job_list()[0].graph.clone();
    let graph = Graph {
        id: GraphId::new(),
        nodes: vec![GraphNode {
            id: NodeId("x".into()),
            kind: "nope".into(),
            params: serde_json::json!({}),
        }],
        edges: vec![],
    };
    match node
        .apply(Op::GraphPut {
            job: job.clone(),
            graph,
        })
        .await
    {
        Err(OpError::Graph {
            detail: GraphError::UnknownStage { kind, known, .. },
        }) => {
            assert_eq!(kind, "nope");
            assert_eq!(known, vec!["passthrough".to_string()]);
        }
        other => panic!("expected UnknownStage, got {other:?}"),
    }
    let rec = &node.job_list()[0];
    assert_eq!(rec.id, job);
    assert_eq!(rec.state, JobState::Running);
    assert_eq!(rec.graph, before);
    assert_eq!(node.lease_holders(&SourceId("file0".into())).len(), 1);
}

#[test]
fn spectrum_kind_expands_to_single_fft_node() {
    assert_eq!(JobKind::Spectrum.name(), "spectrum");
    let tuning = Tuning {
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(48_000),
    };
    let g = JobKind::Spectrum
        .expand(
            &SourceId("file0".into()),
            tuning,
            Hz(48_000),
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(g.nodes.len(), 1);
    assert!(g.edges.is_empty());
    assert_eq!(g.nodes[0].kind, "fft");
    assert_eq!(
        g.nodes[0].params,
        serde_json::json!({"size": 1024, "window": "hann", "rows_per_s": 20})
    );
}

#[test]
fn spectrum_expansion_picks_size_from_the_band() {
    let size = |bw: u64| {
        let g = JobKind::Spectrum
            .expand(
                &SourceId("file0".into()),
                Tuning {
                    center_hz: Hz(100_000_000),
                    bandwidth_hz: Hz(bw),
                },
                Hz(bw),
                None,
                None,
                None,
            )
            .unwrap();
        assert_eq!(g.nodes[0].params["rows_per_s"], 20);
        assert_eq!(g.nodes[0].params["window"], "hann");
        g.nodes[0].params["size"].as_u64().unwrap()
    };
    assert_eq!(size(48_000), 1024);
    assert_eq!(size(24_000_000), 4096);
    assert_eq!(size(50_000_000), 8192);
}

#[test]
fn fm_demod_kind_expands_to_channel_then_nbfm() {
    assert_eq!(JobKind::FmDemod.name(), "fm_demod");
    assert_eq!(
        serde_json::to_value(JobKind::FmDemod).unwrap(),
        serde_json::json!("fm_demod")
    );
    let tuning = Tuning {
        center_hz: Hz(100_005_000),
        bandwidth_hz: Hz(12_500),
    };
    let g = JobKind::FmDemod
        .expand(
            &SourceId("file0".into()),
            tuning,
            Hz(48_000),
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(g.nodes.len(), 2);
    assert_eq!(g.nodes[0].kind, "channel");
    assert_eq!(
        g.nodes[0].params,
        serde_json::json!({"center_hz": 100_005_000, "bandwidth_hz": 12_500, "out_rate_hz": 48_000})
    );
    assert_eq!(g.nodes[1].kind, "nbfm_demod");
    assert_eq!(
        g.nodes[1].params,
        serde_json::json!({"deviation_hz": 5_000, "audio_rate_hz": 48_000})
    );
    assert_eq!(g.edges.len(), 1);
    assert_eq!(g.edges[0].from, g.nodes[0].id);
    assert_eq!(g.edges[0].to, g.nodes[1].id);
    assert_eq!(g.edges[0].kind, zeta_core::types::StreamKind::Iq);
}

#[test]
fn measure_kind_expands_to_channel_then_measure() {
    assert_eq!(JobKind::Measure.name(), "measure");
    assert_eq!(
        serde_json::to_value(JobKind::Measure).unwrap(),
        serde_json::json!("measure")
    );
    let tuning = Tuning {
        center_hz: Hz(144_812_000),
        bandwidth_hz: Hz(12_500),
    };
    let g = JobKind::Measure
        .expand(
            &SourceId("file0".into()),
            tuning,
            Hz(2_048_000),
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(g.nodes.len(), 2);
    assert_eq!(g.nodes[0].kind, "channel");
    assert_eq!(
        g.nodes[0].params,
        serde_json::json!({"center_hz": 144_812_000, "bandwidth_hz": 12_500, "out_rate_hz": 51_200})
    );
    assert_eq!(g.nodes[1].kind, "measure");
    assert_eq!(
        g.nodes[1].params,
        serde_json::json!({"bandwidth_hz": 12_500})
    );
    assert_eq!(g.edges.len(), 1);
    assert_eq!(g.edges[0].from, g.nodes[0].id);
    assert_eq!(g.edges[0].to, g.nodes[1].id);
    assert_eq!(g.edges[0].kind, zeta_core::types::StreamKind::Iq);
    // Channelised, so an unknown source rate is refused.
    assert!(
        JobKind::Measure
            .expand(&SourceId("file0".into()), tuning, Hz(0), None, None, None)
            .is_err()
    );
}

#[test]
fn channel_rate_table() {
    for (rate, bw, want) in [
        (48_000, 12_500, 48_000),
        (12_000, 10_000, 12_000),
        (1_000_000, 12_500, 50_000),
        (2_048_000, 12_500, 51_200),
        (2_400_000, 12_500, 48_000),
        (8_000_000, 12_500, 50_000),
        (10_000_000, 12_500, 50_000),
        (20_000_000, 200_000, 200_000),
        (12_007, 5_000, 12_007),
    ] {
        assert_eq!(channel_rate_hz(rate, bw), want, "{rate} {bw}");
    }
}

#[test]
fn expand_uses_source_rate_for_channel_and_audio() {
    let rates = |g: &zeta_core::graph::Graph| {
        assert_eq!(g.nodes[0].kind, "channel");
        assert_eq!(g.nodes[1].kind, "nbfm_demod");
        (
            g.nodes[0].params["out_rate_hz"].as_u64().unwrap(),
            g.nodes[1].params["audio_rate_hz"].as_u64().unwrap(),
        )
    };
    let src = SourceId("rtl0".into());
    let tuning = Tuning {
        center_hz: Hz(144_800_000),
        bandwidth_hz: Hz(12_500),
    };
    let g = JobKind::FmDemod
        .expand(&src, tuning, Hz(2_048_000), None, None, None)
        .unwrap();
    assert_eq!(rates(&g), (51_200, AUDIO_RATE_HZ));
    let tuning = Tuning {
        center_hz: Hz(7_100_000),
        bandwidth_hz: Hz(10_000),
    };
    let g = JobKind::FmDemod
        .expand(&src, tuning, Hz(12_000), None, None, None)
        .unwrap();
    assert_eq!(rates(&g), (12_000, AUDIO_RATE_HZ));
    let g = JobKind::AprsDecode
        .expand(&src, tuning, Hz(12_000), None, None, None)
        .unwrap();
    assert_eq!(rates(&g), (12_000, AUDIO_RATE_HZ));
    assert_eq!(g.nodes[1].params["deviation_hz"], 3_000);
}

#[test]
fn bank_served_channel_rate_divides_the_bin_rate() {
    let src = SourceId("b210".into());
    let out = |source_rate: u64, bin_rate: Option<u64>, bw: u64| {
        let tuning = Tuning {
            center_hz: Hz(144_800_000),
            bandwidth_hz: Hz(bw),
        };
        let g = JobKind::FmDemod
            .expand(&src, tuning, Hz(source_rate), bin_rate, None, None)
            .unwrap();
        g.nodes[0].params["out_rate_hz"].as_u64().unwrap()
    };
    // 2.4 Msps, 8 bins: bin rate 600 kHz. Served by a bin, the channel rate
    // divides the bin rate (50 kHz), not the source rate (48 kHz).
    assert_eq!(out(2_400_000, None, 12_500), 48_000);
    assert_eq!(out(2_400_000, Some(600_000), 12_500), 50_000);
    assert!(600_000 % out(2_400_000, Some(600_000), 12_500) == 0);
    // 8 Msps, 8 bins: bin 1 MHz, bin rate 2 MHz.
    assert_eq!(out(8_000_000, Some(2_000_000), 200_000), 200_000);
    assert_eq!(out(8_000_000, Some(2_000_000), 1_000_000), 1_000_000);
    // Wider than a bin: the source rate decides, as without a bank.
    assert_eq!(
        out(8_000_000, Some(2_000_000), 1_500_000),
        out(8_000_000, None, 1_500_000)
    );
    assert_eq!(out(8_000_000, Some(2_000_000), 1_500_000), 1_600_000);
}

#[test]
fn fm_demod_expansion_outputs_48k_audio() {
    assert_eq!(AUDIO_RATE_HZ, 48_000);
    let tuning = Tuning {
        center_hz: Hz(98_100_000),
        bandwidth_hz: Hz(200_000),
    };
    let g = JobKind::FmDemod
        .expand(
            &SourceId("rtl0".into()),
            tuning,
            Hz(1_000_000),
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(g.nodes[0].kind, "channel");
    assert_eq!(g.nodes[0].params["out_rate_hz"], 200_000);
    assert_eq!(g.nodes[1].kind, "nbfm_demod");
    assert_eq!(g.nodes[1].params["audio_rate_hz"], 48_000);
}

#[test]
fn expand_rejects_band_wider_than_channel_rate() {
    let tuning = Tuning {
        center_hz: Hz(7_100_000),
        bandwidth_hz: Hz(20_000),
    };
    let err = JobKind::FmDemod
        .expand(
            &SourceId("kiwi0".into()),
            tuning,
            Hz(12_000),
            None,
            None,
            None,
        )
        .unwrap_err();
    assert_eq!(
        err,
        OpError::OutOfRange {
            field: "bandwidth_hz".into(),
            value: 20_000,
            min: 1,
            max: 12_000,
            suggested: None,
        }
    );
}

#[test]
fn expand_rejects_unknown_source_rate() {
    let tuning = Tuning {
        center_hz: Hz(7_100_000),
        bandwidth_hz: Hz(10_000),
    };
    let err = JobKind::AprsDecode
        .expand(&SourceId("kiwi0".into()), tuning, Hz(0), None, None, None)
        .unwrap_err();
    assert_eq!(
        err,
        OpError::Other {
            message: "source rate unknown: tune returned bandwidth_hz 0".into(),
        }
    );
}

#[tokio::test]
async fn stream_tail_unknown_stream_lists_known() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(cfg(dir.path()), registry(), vec![])
        .await
        .unwrap();
    let span = Span {
        start_ns: Timestamp(0),
        end_ns: Timestamp(1),
    };
    let prov = |s: &str| zeta_core::types::Provenance {
        source: SourceId(s.into()),
        job: None,
        channel: None,
        span: span.clone(),
    };
    let a = node.bus().register(StreamKind::Event, prov("a"));
    let b = node.bus().register(StreamKind::Audio, prov("b"));
    // The node's own `ops` stream is always known too.
    let mut want = vec![a.0, b.0, node.ops_stream().0];
    want.sort();
    let op = Op::StreamTail {
        stream: StreamId("nope".into()),
        max_frames: 1,
        timeout_ms: 10,
    };
    match node.apply(op).await {
        Err(OpError::Unknown {
            field,
            value,
            known,
        }) => {
            assert_eq!(field, "stream");
            assert_eq!(value, "nope");
            assert_eq!(known, want);
        }
        other => panic!("expected Unknown, got {other:?}"),
    }
}

fn tail_prov() -> zeta_core::types::Provenance {
    zeta_core::types::Provenance {
        source: SourceId("s".into()),
        job: Some(JobId("j".into())),
        channel: Some(zeta_core::types::ChannelId("c".into())),
        span: Span {
            start_ns: Timestamp(10),
            end_ns: Timestamp(20),
        },
    }
}

#[tokio::test]
async fn stream_tail_times_out_with_empty_when_no_frames() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(cfg(dir.path()), registry(), vec![])
        .await
        .unwrap();
    let id = node.bus().register(StreamKind::Text, tail_prov());
    let t0 = std::time::Instant::now();
    let r = node
        .apply(Op::StreamTail {
            stream: id,
            max_frames: 3,
            timeout_ms: 100,
        })
        .await;
    let took = t0.elapsed();
    assert_eq!(
        r,
        Ok(OpResult::Frames {
            frames: vec![],
            missed: 0
        })
    );
    assert!(took >= Duration::from_millis(100), "{took:?}");
    assert!(took < Duration::from_millis(300), "{took:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_tail_returns_collected_when_stream_closes() {
    use zeta_core::stream::{Frame, Payload};
    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(
        Node::open(cfg(dir.path()), registry(), vec![])
            .await
            .unwrap(),
    );
    let bus = node.bus();
    let id = bus.register(StreamKind::Text, tail_prov());
    let (n, sid) = (node.clone(), id.clone());
    let t0 = std::time::Instant::now();
    let tail = tokio::spawn(async move {
        n.apply(Op::StreamTail {
            stream: sid,
            max_frames: 10,
            timeout_ms: 5000,
        })
        .await
    });
    // Publish only once the tail has subscribed.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while bus.receiver_count(&id).unwrap_or(0) < 1 {
        assert!(
            std::time::Instant::now() < deadline,
            "tail never subscribed"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    for t in ["one", "two"] {
        bus.publish(Frame::new(id.clone(), tail_prov(), Payload::Text(t.into())));
    }
    bus.unregister(&id);
    let (frames, missed) = match tail.await.unwrap() {
        Ok(OpResult::Frames { frames, missed }) => (frames, missed),
        other => panic!("expected Frames, got {other:?}"),
    };
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "closed stream must end the tail"
    );
    assert_eq!(missed, 0);
    assert_eq!(frames.len(), 2);
    for (f, t) in frames.iter().zip(["one", "two"]) {
        assert_eq!(f.stream, id);
        assert_eq!(f.source, SourceId("s".into()));
        assert_eq!(f.job, Some(JobId("j".into())));
        assert_eq!(f.channel, Some(zeta_core::types::ChannelId("c".into())));
        assert_eq!(f.kind, StreamKind::Text);
        assert_eq!((f.start_ns, f.end_ns), (Timestamp(10), Timestamp(20)));
        assert_eq!(f.payload, Value::String(t.into()));
    }
}

#[tokio::test]
async fn stream_tail_counts_missed_frames() {
    use zeta_core::stream::{Frame, Payload};
    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(
        Node::open(cfg(dir.path()), registry(), vec![])
            .await
            .unwrap(),
    );
    let bus = node.bus();
    let id = bus.register(StreamKind::Text, tail_prov());
    let (n, sid) = (node.clone(), id.clone());
    let tail = tokio::spawn(async move {
        n.apply(Op::StreamTail {
            stream: sid,
            max_frames: 1000,
            timeout_ms: 5000,
        })
        .await
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while bus.receiver_count(&id).unwrap_or(0) < 1 {
        assert!(
            std::time::Instant::now() < deadline,
            "tail never subscribed"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // Single-threaded runtime: the tail cannot drain while these are
    // published, so it falls more than the bus capacity behind.
    const SENT: u64 = 600;
    for i in 0..SENT {
        bus.publish(Frame::new(
            id.clone(),
            tail_prov(),
            Payload::Text(i.to_string()),
        ));
    }
    bus.unregister(&id);
    let (frames, missed) = match tail.await.unwrap() {
        Ok(OpResult::Frames { frames, missed }) => (frames, missed),
        other => panic!("expected Frames, got {other:?}"),
    };
    assert!(missed > 0, "missed {missed}");
    assert_eq!(frames.len() as u64 + missed, SENT);
    assert_eq!(frames.last().unwrap().payload, Value::String("599".into()));
}

#[tokio::test]
async fn stream_tail_validates_limits() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(cfg(dir.path()), registry(), vec![])
        .await
        .unwrap();
    let id = node.bus().register(StreamKind::Text, tail_prov());
    for (max_frames, timeout_ms, field, value, max) in [
        (0, 10, "max_frames", 0, 1000),
        (1001, 10, "max_frames", 1001, 1000),
        (1, 0, "timeout_ms", 0, 10_000),
        (1, 10_001, "timeout_ms", 10_001, 10_000),
    ] {
        let r = node
            .apply(Op::StreamTail {
                stream: id.clone(),
                max_frames,
                timeout_ms,
            })
            .await;
        assert_eq!(
            r,
            Err(OpError::OutOfRange {
                field: field.into(),
                value,
                min: 1,
                max,
                suggested: None,
            })
        );
    }
}

fn fm_tuning() -> Tuning {
    Tuning {
        center_hz: Hz(100_005_000),
        bandwidth_hz: Hz(12_500),
    }
}

#[test]
fn fm_demod_default_deviation_is_5000() {
    let g = JobKind::FmDemod
        .expand(
            &SourceId("file0".into()),
            fm_tuning(),
            Hz(48_000),
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(g.nodes[1].params["deviation_hz"], 5_000);
    assert_eq!(zeta_core::ops::FM_DEVIATION_DEFAULT_HZ, 5_000);
}

#[test]
fn deviation_param_reaches_nbfm_node() {
    let g = JobKind::FmDemod
        .expand(
            &SourceId("file0".into()),
            fm_tuning(),
            Hz(48_000),
            None,
            Some(Hz(2_500)),
            None,
        )
        .unwrap();
    assert_eq!(g.nodes[1].params["deviation_hz"], 2_500);
}

fn start_with(kind: JobKind, deviation_hz: Option<u64>) -> Op {
    Op::JobStart {
        kind,
        source: SourceId("hackrf0".into()),
        center_hz: Hz(100_005_000),
        bandwidth_hz: Hz(12_500),
        deviation_hz,
        sideband: None,
    }
}

#[tokio::test]
async fn deviation_out_of_range_names_field() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(
        cfg(dir.path()),
        registry(),
        vec![Arc::new(WidePort::new("hackrf0"))],
    )
    .await
    .unwrap();
    // 5 Hz is below 100; the x1000 suggestion (5000 Hz) is in range.
    match node.apply(start_with(JobKind::FmDemod, Some(5))).await {
        Err(OpError::OutOfRange {
            field,
            value,
            min,
            max,
            suggested,
        }) => {
            assert_eq!(field, "deviation_hz");
            assert_eq!((value, min, max), (5, 100, 75_000));
            assert_eq!(suggested, Some(5_000));
        }
        other => panic!("expected OutOfRange, got {other:?}"),
    }
    assert!(matches!(
        node.apply(start_with(JobKind::FmDemod, Some(75_001))).await,
        Err(OpError::OutOfRange { .. })
    ));
}

#[tokio::test]
async fn deviation_on_other_kind_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(
        cfg(dir.path()),
        registry(),
        vec![Arc::new(WidePort::new("hackrf0"))],
    )
    .await
    .unwrap();
    for kind in [JobKind::Measure, JobKind::AprsDecode] {
        match node.apply(start_with(kind, Some(5_000))).await {
            Err(OpError::Other { message }) => {
                assert!(
                    message.starts_with("deviation_hz applies only"),
                    "{message}"
                );
                assert!(message.contains(kind.name()), "{message}");
            }
            other => panic!("expected Other, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn am_demod_rejects_deviation_hz() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(
        cfg(dir.path()),
        registry(),
        vec![Arc::new(WidePort::new("hackrf0"))],
    )
    .await
    .unwrap();
    match node.apply(start_with(JobKind::AmDemod, Some(5_000))).await {
        Err(OpError::Other { message }) => assert_eq!(
            message,
            "deviation_hz applies only to kinds fm_demod and wfm_demod; pass null (kind is am_demod)"
        ),
        other => panic!("expected Other, got {other:?}"),
    }
}

#[test]
fn job_start_schema_requires_nullable_deviation() {
    let s = schema();
    let v = variants(&s["op"])
        .iter()
        .find(|v| v["properties"]["op"]["const"] == "job_start")
        .expect("job_start variant");
    let req = v["required"].as_array().unwrap();
    assert!(req.contains(&"deviation_hz".into()), "{v}");
    let ty = &v["properties"]["deviation_hz"]["type"];
    assert_eq!(ty, &serde_json::json!(["integer", "null"]), "{v}");
}

#[test]
fn job_start_without_deviation_key_is_rejected() {
    let v = serde_json::json!({"op":"job_start","kind":"passthrough","source":"s",
        "center_hz":100_000_000,"bandwidth_hz":48_000});
    let e = serde_json::from_value::<Op>(v).unwrap_err().to_string();
    assert!(e.contains("deviation_hz"), "{e}");
    let v = serde_json::json!({"op":"job_start","kind":"passthrough","source":"s",
        "center_hz":100_000_000,"bandwidth_hz":48_000,"deviation_hz":null});
    let e = serde_json::from_value::<Op>(v).unwrap_err().to_string();
    assert!(e.contains("sideband"), "{e}");
    let ok = serde_json::json!({"op":"job_start","kind":"passthrough","source":"s",
        "center_hz":100_000_000,"bandwidth_hz":48_000,"deviation_hz":null,"sideband":null});
    assert!(serde_json::from_value::<Op>(ok).is_ok());
}

#[tokio::test]
async fn source_list_reports_fault() {
    let dir = tempfile::tempdir().unwrap();
    let wide = Arc::new(WidePort::new("hackrf0"));
    *wide.fault.lock().unwrap() = Some("device gone".into());
    let (_src, file) = file_source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![wide, file])
        .await
        .unwrap();
    let Ok(OpResult::Sources(v)) = node.apply(Op::SourceList {}).await else {
        panic!("expected sources");
    };
    let fault = |id: &str| v.iter().find(|s| s.id.0 == id).unwrap().fault.clone();
    assert_eq!(fault("hackrf0"), Some("device gone".into()));
    assert_eq!(fault("file0"), None);
    let json = serde_json::to_value(&v).unwrap();
    assert!(
        json.as_array()
            .unwrap()
            .iter()
            .any(|s| s["fault"].is_null())
    );
    // Required and nullable in the result schema.
    let si = &schema()["result"]["$defs"]["SourceInfo"];
    assert!(
        si["required"].as_array().unwrap().contains(&"fault".into()),
        "{si}"
    );
    assert_eq!(
        si["properties"]["fault"]["type"],
        serde_json::json!(["string", "null"]),
        "{si}"
    );
}

/// Fields that serialize `null` say so in their schema: `"null"` is in the
/// `type` of each.
#[test]
fn nullable_fields_have_null_in_schema_type() {
    let s = schema();
    let paths = [
        "/result/$defs/Allocation/properties/note",
        "/result/$defs/Provenance/properties/job",
        "/result/$defs/Provenance/properties/channel",
        "/result/$defs/FrameSummary/properties/job",
        "/result/$defs/FrameSummary/properties/channel",
        "/result/$defs/SourceInfo/properties/current",
        "/result/$defs/SourceInfo/properties/fault",
        "/error/oneOf/0/properties/suggested",
    ];
    for p in paths {
        let f = s.pointer(p).unwrap_or_else(|| panic!("{p} missing"));
        let ty = f["type"].as_array().unwrap_or_else(|| panic!("{p}: {f}"));
        assert!(ty.contains(&Value::from("null")), "{p}: {f}");
    }
    assert_eq!(
        s.pointer("/error/oneOf/0/properties/error/const"),
        Some(&Value::from("out_of_range")),
        "oneOf/0 is OutOfRange"
    );
}

#[test]
fn wfm_expansion_is_channel_then_wfm_demod() {
    assert_eq!(JobKind::WfmDemod.name(), "wfm_demod");
    assert_eq!(
        serde_json::to_value(JobKind::WfmDemod).unwrap(),
        serde_json::json!("wfm_demod")
    );
    let tuning = Tuning {
        center_hz: Hz(104_900_000),
        bandwidth_hz: Hz(200_000),
    };
    for (deviation, want) in [(None, 75_000), (Some(Hz(50_000)), 50_000)] {
        let g = JobKind::WfmDemod
            .expand(
                &SourceId("b210".into()),
                tuning,
                Hz(1_000_000),
                None,
                deviation,
                None,
            )
            .unwrap();
        assert_eq!(g.nodes.len(), 2);
        assert_eq!(g.nodes[0].kind, "channel");
        assert_eq!(
            g.nodes[0].params,
            serde_json::json!({"center_hz": 104_900_000, "bandwidth_hz": 200_000, "out_rate_hz": 200_000})
        );
        assert_eq!(g.nodes[1].kind, "wfm_demod");
        assert_eq!(
            g.nodes[1].params,
            serde_json::json!({"deviation_hz": want, "audio_rate_hz": 48_000, "deemphasis_us": 50})
        );
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.edges[0].from, g.nodes[0].id);
        assert_eq!(g.edges[0].to, g.nodes[1].id);
        assert_eq!(g.edges[0].kind, StreamKind::Iq);
    }
}

#[test]
fn am_expansion_is_channel_then_am_demod() {
    assert_eq!(JobKind::AmDemod.name(), "am_demod");
    assert_eq!(
        serde_json::to_value(JobKind::AmDemod).unwrap(),
        serde_json::json!("am_demod")
    );
    let tuning = Tuning {
        center_hz: Hz(121_500_000),
        bandwidth_hz: Hz(10_000),
    };
    let g = JobKind::AmDemod
        .expand(
            &SourceId("b210".into()),
            tuning,
            Hz(48_000),
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(g.nodes.len(), 2);
    assert_eq!(g.nodes[0].kind, "channel");
    assert_eq!(g.nodes[1].kind, "am_demod");
    assert_eq!(
        g.nodes[1].params,
        serde_json::json!({"audio_rate_hz": 48_000})
    );
    assert_eq!(g.edges.len(), 1);
    assert_eq!(g.edges[0].from, g.nodes[0].id);
    assert_eq!(g.edges[0].to, g.nodes[1].id);
    assert_eq!(g.edges[0].kind, StreamKind::Iq);
}

#[test]
fn ssb_expansion_defaults_to_usb() {
    assert_eq!(JobKind::SsbDemod.name(), "ssb_demod");
    assert_eq!(
        serde_json::to_value(JobKind::SsbDemod).unwrap(),
        serde_json::json!("ssb_demod")
    );
    let tuning = Tuning {
        center_hz: Hz(14_200_000),
        bandwidth_hz: Hz(3_000),
    };
    for (sideband, name) in [
        (None, "usb"),
        (Some(Sideband::Usb), "usb"),
        (Some(Sideband::Lsb), "lsb"),
    ] {
        let g = JobKind::SsbDemod
            .expand(
                &SourceId("kiwi".into()),
                tuning,
                Hz(48_000),
                None,
                None,
                sideband,
            )
            .unwrap();
        assert_eq!(g.nodes.len(), 2);
        assert_eq!(g.nodes[0].kind, "channel");
        assert_eq!(g.nodes[1].kind, "ssb_demod");
        assert_eq!(
            g.nodes[1].params,
            serde_json::json!({"sideband": name, "audio_rate_hz": AUDIO_RATE_HZ, "audio_bandwidth_hz": 2_800})
        );
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.edges[0].from, g.nodes[0].id);
        assert_eq!(g.edges[0].to, g.nodes[1].id);
        assert_eq!(g.edges[0].kind, StreamKind::Iq);
    }
}

#[tokio::test]
async fn job_start_rejects_sideband_on_other_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(
        cfg(dir.path()),
        registry(),
        vec![Arc::new(WidePort::new("hackrf0"))],
    )
    .await
    .unwrap();
    let op: Op = serde_json::from_value(serde_json::json!({"op":"job_start","kind":"fm_demod",
        "source":"hackrf0","center_hz":100_005_000,"bandwidth_hz":12_500,
        "deviation_hz":null,"sideband":"usb"}))
    .unwrap();
    match node.apply(op).await {
        Err(OpError::Other { message }) => assert_eq!(
            message,
            "sideband applies only to kind ssb_demod; pass null (kind is fm_demod)"
        ),
        other => panic!("expected Other, got {other:?}"),
    }
    // deviation_hz stays null-only for ssb_demod.
    match node.apply(start_with(JobKind::SsbDemod, Some(5_000))).await {
        Err(OpError::Other { message }) => assert_eq!(
            message,
            "deviation_hz applies only to kinds fm_demod and wfm_demod; pass null (kind is ssb_demod)"
        ),
        other => panic!("expected Other, got {other:?}"),
    }
}

#[test]
fn job_start_schema_has_nullable_sideband() {
    let s = schema();
    let v = variants(&s["op"])
        .iter()
        .find(|v| v["properties"]["op"]["const"] == "job_start")
        .expect("job_start variant");
    let req = v["required"].as_array().unwrap();
    assert!(req.contains(&"sideband".into()), "{v}");
    let p = &v["properties"]["sideband"];
    assert_eq!(p["type"], serde_json::json!(["string", "null"]), "{v}");
    assert_eq!(p["enum"], serde_json::json!(["usb", "lsb", null]), "{v}");
    assert_eq!(
        serde_json::to_value(Sideband::Lsb).unwrap(),
        serde_json::json!("lsb")
    );
}

#[test]
fn pocsag_decode_expands_channel_to_decoder() {
    let src = SourceId("b210".into());
    let tuning = Tuning {
        center_hz: Hz(152_000_000),
        bandwidth_hz: Hz(12_500),
    };
    assert_eq!(JobKind::PocsagDecode.name(), "pocsag_decode");
    let g = JobKind::PocsagDecode
        .expand(&src, tuning, Hz(48_000), None, None, None)
        .unwrap();
    let kinds: Vec<&str> = g.nodes.iter().map(|n| n.kind.as_str()).collect();
    assert_eq!(kinds, ["channel", "pocsag_decode"]);
    assert_eq!(g.nodes[1].params, serde_json::json!({}));
    assert_eq!(g.edges.len(), 1);
    assert_eq!(g.edges[0].kind, StreamKind::Iq);
    assert_eq!(g.edges[0].from, g.nodes[0].id);
    assert_eq!(g.edges[0].to, g.nodes[1].id);
}

#[test]
fn rds_decode_expands_channel_to_decoder() {
    let src = SourceId("b210".into());
    let tuning = Tuning {
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(200_000),
    };
    assert_eq!(JobKind::RdsDecode.name(), "rds_decode");
    let g = JobKind::RdsDecode
        .expand(&src, tuning, Hz(240_000), None, None, None)
        .unwrap();
    let kinds: Vec<&str> = g.nodes.iter().map(|n| n.kind.as_str()).collect();
    assert_eq!(kinds, ["channel", "rds_decode"]);
    assert_eq!(g.nodes[1].params, serde_json::json!({}));
    assert_eq!(g.edges.len(), 1);
    assert_eq!(g.edges[0].kind, StreamKind::Iq);
    assert_eq!(g.edges[0].from, g.nodes[0].id);
    assert_eq!(g.edges[0].to, g.nodes[1].id);
}

#[test]
fn adsb_decode_expands_channel_to_decoder() {
    let src = SourceId("b210".into());
    let tuning = Tuning {
        center_hz: Hz(1_090_000_000),
        bandwidth_hz: Hz(2_400_000),
    };
    assert_eq!(JobKind::AdsbDecode.name(), "adsb_decode");
    let g = JobKind::AdsbDecode
        .expand(&src, tuning, Hz(2_400_000), None, None, None)
        .unwrap();
    let kinds: Vec<&str> = g.nodes.iter().map(|n| n.kind.as_str()).collect();
    assert_eq!(kinds, ["channel", "adsb_decode"]);
    assert_eq!(g.nodes[0].params["out_rate_hz"], 2_400_000);
    assert_eq!(g.nodes[1].params, serde_json::json!({}));
    assert_eq!(g.edges.len(), 1);
    assert_eq!(g.edges[0].kind, StreamKind::Iq);
    assert_eq!(g.edges[0].from, g.nodes[0].id);
    assert_eq!(g.edges[0].to, g.nodes[1].id);
}

#[test]
fn adsb_expand_refuses_narrow_band() {
    let src = SourceId("b210".into());
    let tuning = Tuning {
        center_hz: Hz(1_090_000_000),
        bandwidth_hz: Hz(1_000_000),
    };
    match JobKind::AdsbDecode.expand(&src, tuning, Hz(8_000_000), None, None, None) {
        Err(OpError::OutOfRange {
            field,
            value,
            min,
            max,
            suggested,
        }) => {
            assert_eq!(field, "bandwidth_hz");
            assert_eq!(value, 1_000_000);
            assert_eq!(min, 2_000_000);
            assert_eq!(max, 1_000_000);
            assert_eq!(suggested, Some(2_400_000));
        }
        other => panic!("expected OutOfRange, got {other:?}"),
    }
}

#[test]
fn acars_decode_expands_channel_to_decoder() {
    let src = SourceId("b210".into());
    let tuning = Tuning {
        center_hz: Hz(131_550_000),
        bandwidth_hz: Hz(10_000),
    };
    assert_eq!(JobKind::AcarsDecode.name(), "acars_decode");
    let g = JobKind::AcarsDecode
        .expand(&src, tuning, Hz(240_000), None, None, None)
        .unwrap();
    let kinds: Vec<&str> = g.nodes.iter().map(|n| n.kind.as_str()).collect();
    assert_eq!(kinds, ["channel", "acars_decode"]);
    assert_eq!(g.nodes[1].params, serde_json::json!({}));
    assert_eq!(g.edges.len(), 1);
    assert_eq!(g.edges[0].kind, StreamKind::Iq);
    assert_eq!(g.edges[0].from, g.nodes[0].id);
    assert_eq!(g.edges[0].to, g.nodes[1].id);
}
