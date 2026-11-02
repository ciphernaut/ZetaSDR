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
        bank_factory: None,
        modes_dir: None,
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
        tune_range: None,
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
        replace: vec![],
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
        replace: vec![],
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
        elastic: false,
        replace: vec![],
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
        elastic: false,
        replace: vec![],
    };
    let ensure = Op::SourceEnsure {
        source,
        center_hz: Hz(433_000_000),
        bandwidth_hz: Hz(200_000),
        replace: vec![],
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
async fn job_start_holds_the_gate_through_its_tune_and_lease() {
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
        elastic: false,
        replace: vec![],
    };
    // B: 2 MHz around A's band, asked while A's slow tune is under way. A holds the
    // source's gate from its tune through its lease, so B waits behind A (it cannot
    // slip in between and change the capture A's graph was expanded for) and then
    // conflicts with A's narrow lease.
    let wide = Tuning {
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(2_000_000),
    };
    let b = async {
        while port.tunes.load(std::sync::atomic::Ordering::SeqCst) < 1 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let g = JobKind::Passthrough
            .expand(&source, wide, wide.bandwidth_hz, None, None, None, &modes())
            .unwrap();
        node.job_start("passthrough", source.clone(), wide, g, None, false)
            .await
    };
    let (a, b) = tokio::join!(node.apply(a), b);
    let a = match a {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected Job, got {other:?}"),
    };
    assert!(
        matches!(b, Err(zeta_core::node::NodeError::Lease(_))),
        "{b:?}"
    );
    let narrow = Tuning {
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(12_500),
    };
    assert_eq!(port.current(), Some(narrow));
    let rec = node.job_list().into_iter().find(|r| r.id == a).unwrap();
    assert_eq!(rec.state, JobState::Running);
    // Expanded for the narrow capture and never relaunched.
    let out = rec.graph.nodes[0].params["out_rate_hz"].as_u64().unwrap();
    assert!(out <= 12_500, "{out}");
    assert_eq!(node.job_list().len(), 1);
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
    assert_eq!(vs.len(), 29, "one schema per Op variant");
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
    let mut reg = registry();
    reg.register(Arc::new(zeta_core::stream_in::StreamInStage::new()));
    for st in reg.describe().as_array().unwrap() {
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
    // A list of unit values (`centers_hz`), maybe null (`range_hz`).
    let is_array =
        |t: &Value| t == "array" || t.as_array().is_some_and(|a| a.contains(&"array".into()));
    if is_array(&s["type"]) {
        return unit_typed(&s["items"], defs);
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
    db: usize,
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
                    .any(|w| name.contains(w))
                        // Counts of stream restarts and read timeouts, not times.
                        && !["restarts", "timeouts"].contains(&name.as_str())
                        // A mode row's `time` names the event field holding a time.
                        && !(name == "time" && path.ends_with(".ModeRow"))
                        // `plan_start`'s `started` lists the channels it started.
                        && name != "started";
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
                    // `_db` (and a gain's own `db`) is decibels: a number, maybe null.
                    let db = name == "db" || name.ends_with("_db");
                    if db {
                        let number = s["type"] == "number"
                            || s["type"]
                                .as_array()
                                .is_some_and(|a| a.contains(&Value::from("number")));
                        assert!(number, "{path}.{name} is not a number: {s}");
                    }
                    seen.hz += name.ends_with("_hz") as usize;
                    seen.ns += name.ends_with("_ns") as usize;
                    seen.db += db as usize;
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
    assert!(seen.db > 0, "no _db field seen");
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
async fn channel_plans_lists_uhf_cb_au() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(cfg(dir.path()), registry(), vec![])
        .await
        .unwrap();
    let Ok(OpResult::ChannelPlans { plans }) = node.apply(Op::ChannelPlans {}).await else {
        panic!("expected ChannelPlans");
    };
    assert_eq!(plans.len(), 1);
    let p = &plans[0];
    assert_eq!((p.id.as_str(), p.region.as_str()), ("uhf_cb_au", "au"));
    assert_eq!((p.bandwidth_hz, p.adjacent_hz), (Hz(12_500), Hz(12_500)));
    assert_eq!(p.channels.len(), 80);
    let ch = |n| p.channels.iter().find(|c| c.number == n).unwrap();
    assert_eq!(ch(22).note.as_deref(), Some("data, no voice"));
    assert_eq!(ch(20).note, None);
    assert_eq!(ch(20).center_hz, Hz(476_900_000));
    assert!(
        p.channels
            .windows(2)
            .all(|w| w[0].center_hz.0 < w[1].center_hz.0)
    );
    // Wire shape: `note` is required and nullable.
    let v = serde_json::to_value(&plans).unwrap();
    assert!(v[0]["channels"][0].get("note").is_some_and(Value::is_null));
    let s = schema();
    let op = variants(&s["op"])
        .iter()
        .find(|v| v["properties"]["op"]["const"] == "channel_plans")
        .expect("channel_plans variant");
    assert_eq!(op["properties"].as_object().unwrap().len(), 1);
    let note = &s["result"]["$defs"]["PlanChannel"]["properties"]["note"];
    assert_eq!(note["type"], serde_json::json!(["string", "null"]));
    let req = s["result"]["$defs"]["PlanChannel"]["required"]
        .as_array()
        .unwrap();
    assert!(req.contains(&"note".into()));
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
        Ok(OpResult::Classification {
            labels: vec![],
            tried: vec![]
        })
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
        elastic: false,
        replace: vec![],
    };
    let job = match node.apply(start).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected Job, got {other:?}"),
    };
    let ensure = Op::SourceEnsure {
        source: source.clone(),
        center_hz: Hz(433_000_000),
        bandwidth_hz: Hz(200_000),
        replace: vec![],
    };
    match node.apply(ensure).await {
        Err(OpError::Conflict {
            source: s,
            holders,
            current,
            options,
        }) => {
            assert_eq!(s, source);
            assert_eq!(
                options,
                vec![
                    "stop one of the holders".to_string(),
                    "choose a band inside current".to_string(),
                    format!("retry with replace: [{}]", job.0),
                ]
            );
            assert_eq!(holders, vec![job]);
            assert_eq!(current, held);
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
        elastic: false,
        replace: vec![],
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
fn job_attach_schema_has_a_nullable_language() {
    let s = schema();
    let v = variants(&s["op"])
        .iter()
        .find(|v| v["properties"]["op"]["const"] == "job_attach")
        .expect("job_attach variant");
    let mut req: Vec<&str> = v["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    req.sort();
    assert_eq!(req, ["job", "kind", "language", "op"]);
    assert_eq!(
        v["properties"]["language"]["type"],
        serde_json::json!(["string", "null"])
    );
    let op: Op = serde_json::from_value(serde_json::json!({
        "op": "job_attach", "kind": "transcribe", "job": "j", "language": null
    }))
    .unwrap();
    assert_eq!(
        op,
        Op::JobAttach {
            kind: JobKind::Transcribe,
            job: JobId("j".into()),
            language: None
        }
    );
    // `language` is required, if only as null.
    let missing = serde_json::json!({"op": "job_attach", "kind": "transcribe", "job": "j"});
    assert!(serde_json::from_value::<Op>(missing).is_err());
    assert!(JobKind::Transcribe.attachable());
    assert!(!JobKind::WfmDemod.attachable());
}

#[test]
fn transcribe_expands_attached_to_stream_in_denoise_vad_transcribe() {
    let (job, stream) = (JobId("in".into()), StreamId("s".into()));
    let g = JobKind::Transcribe
        .expand_attached(&job, &stream, "de", false)
        .unwrap();
    let kinds: Vec<&str> = g.nodes.iter().map(|n| n.kind.as_str()).collect();
    assert_eq!(kinds, ["stream_in", "denoise", "vad", "transcribe"]);
    assert_eq!(
        g.nodes[0].params,
        serde_json::json!({"job": "in", "kind": "audio", "stream": "s"})
    );
    assert_eq!(g.nodes[1].params, serde_json::json!({}));
    assert_eq!(g.nodes[2].params, serde_json::json!({}));
    assert_eq!(g.nodes[3].params, serde_json::json!({"language": "de"}));
    assert_eq!(g.edges.len(), 3);
    for (i, e) in g.edges.iter().enumerate() {
        assert_eq!(e.kind, StreamKind::Audio);
        assert_eq!(e.from, g.nodes[i].id);
        assert_eq!(e.to, g.nodes[i + 1].id);
    }
    assert_eq!(
        JobKind::WfmDemod.expand_attached(&job, &stream, "en", false),
        Err(OpError::Other {
            message: "wfm_demod is not attachable".into()
        })
    );
    // A transcribe job is never expanded for the radio.
    let t = Tuning {
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(48_000),
    };
    assert_eq!(
        JobKind::Transcribe.expand(
            &SourceId("s".into()),
            t,
            Hz(48_000),
            None,
            None,
            None,
            &modes()
        ),
        Err(OpError::Other {
            message: "transcribe attaches to a job: use job_attach".into()
        })
    );
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
            &modes(),
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
                &modes(),
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

/// A watch's fft targets 1.5 kHz bins (a ~10 kHz sonde spans several), four times a
/// spectrum's resolution: 4096 bins at 6 MHz where a spectrum has 1024.
#[test]
fn watch_expansion_sizes_its_fft_finer_than_a_spectrum() {
    let size = |rate: u64| {
        let g = JobKind::Watch
            .expand(
                &SourceId("file0".into()),
                Tuning {
                    center_hz: Hz(403_000_000),
                    bandwidth_hz: Hz(rate),
                },
                Hz(rate),
                None,
                None,
                None,
                &modes(),
            )
            .unwrap();
        assert_eq!(g.nodes[0].kind, "fft");
        g.nodes[0].params["size"].as_u64().unwrap()
    };
    assert_eq!(size(6_000_000), 4096);
    assert_eq!(size(48_000), 1024);
    assert_eq!(size(20_000_000), 8192);
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
            &modes(),
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
        serde_json::json!({"deviation_hz": 5_000, "audio_rate_hz": 48_000, "squelch_db": null, "squelch": true,
            "audio_lowpass_hz": 3_400, "deemphasis_us": 530, "denoise": false})
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
            &modes(),
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
            .expand(
                &SourceId("file0".into()),
                tuning,
                Hz(0),
                None,
                None,
                None,
                &modes()
            )
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
        .expand(&src, tuning, Hz(2_048_000), None, None, None, &modes())
        .unwrap();
    assert_eq!(rates(&g), (51_200, AUDIO_RATE_HZ));
    let tuning = Tuning {
        center_hz: Hz(7_100_000),
        bandwidth_hz: Hz(10_000),
    };
    let g = JobKind::FmDemod
        .expand(&src, tuning, Hz(12_000), None, None, None, &modes())
        .unwrap();
    assert_eq!(rates(&g), (12_000, AUDIO_RATE_HZ));
    let g = JobKind::AprsDecode
        .expand(&src, tuning, Hz(12_000), None, None, None, &modes())
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
            .expand(
                &src,
                tuning,
                Hz(source_rate),
                bin_rate,
                None,
                None,
                &modes(),
            )
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
            &modes(),
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
            &modes(),
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
        .expand(
            &SourceId("kiwi0".into()),
            tuning,
            Hz(0),
            None,
            None,
            None,
            &modes(),
        )
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
            &modes(),
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
            &modes(),
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
        elastic: false,
        replace: vec![],
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
        match node.apply(start_with(kind.clone(), Some(5_000))).await {
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
fn demod_graphs_carry_the_squelch_params() {
    let tuning = Tuning {
        center_hz: Hz(100_005_000),
        bandwidth_hz: Hz(12_500),
    };
    let src = SourceId("file0".into());
    for (kind, demod) in [
        (JobKind::FmDemod, "nbfm_demod"),
        (JobKind::AmDemod, "am_demod"),
    ] {
        let g = kind
            .expand(&src, tuning, Hz(48_000), None, None, None, &modes())
            .unwrap();
        let n = g.nodes.iter().find(|n| n.kind == demod).expect(demod);
        assert_eq!(n.params["squelch_db"], serde_json::Value::Null, "{kind:?}");
        assert!(n.params.get("squelch_db").is_some(), "{kind:?}");
        assert_eq!(n.params["squelch"], true, "{kind:?}");
        // A resumed job expanded again keeps what `graph_put` set.
        let mut stored = g.clone();
        for m in stored.nodes.iter_mut().filter(|m| m.kind == demod) {
            m.params["squelch_db"] = (-70).into();
            m.params["squelch"] = false.into();
        }
        let again = kind
            .reexpand(&src, tuning, Hz(96_000), None, &stored, &modes())
            .unwrap();
        let n = again.nodes.iter().find(|n| n.kind == demod).expect(demod);
        assert_eq!(n.params["squelch_db"], -70, "{kind:?}");
        assert_eq!(n.params["squelch"], false, "{kind:?}");
        // A graph stored before the squelch (no such params) expands with the defaults.
        let mut old = g.clone();
        for m in old.nodes.iter_mut().filter(|m| m.kind == demod) {
            let o = m.params.as_object_mut().unwrap();
            o.remove("squelch_db");
            o.remove("squelch");
        }
        let again = kind
            .reexpand(&src, tuning, Hz(96_000), None, &old, &modes())
            .unwrap();
        let n = again.nodes.iter().find(|n| n.kind == demod).expect(demod);
        assert_eq!(n.params["squelch"], true, "{kind:?}");
    }
    // job_start carries no squelch field (the user's call: always on, automatic).
    let s = schema();
    let v = variants(&s["op"])
        .iter()
        .find(|v| v["properties"]["op"]["const"] == "job_start")
        .expect("job_start variant");
    assert!(v["properties"].get("squelch_db").is_none(), "{v}");
    assert!(v["properties"].get("squelch").is_none(), "{v}");
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
fn job_start_schema_requires_elastic_and_source_list_lists_it() {
    let s = schema();
    let v = variants(&s["op"])
        .iter()
        .find(|v| v["properties"]["op"]["const"] == "job_start")
        .expect("job_start variant");
    let req = v["required"].as_array().unwrap();
    assert!(req.contains(&"elastic".into()), "{v}");
    assert_eq!(v["properties"]["elastic"]["type"], "boolean", "{v}");
    let si = &s["result"]["$defs"]["SourceInfo"];
    assert!(
        si["required"]
            .as_array()
            .unwrap()
            .contains(&"elastic".into()),
        "{si}"
    );
    assert_eq!(si["properties"]["elastic"]["type"], "array", "{si}");
    let rec = &s["result"]["$defs"]["JobRecord"];
    assert!(
        rec["required"]
            .as_array()
            .unwrap()
            .contains(&"elastic".into()),
        "{rec}"
    );
}

#[test]
fn job_start_and_source_ensure_schema_require_replace() {
    let s = schema();
    for op in ["job_start", "source_ensure"] {
        let v = variants(&s["op"])
            .iter()
            .find(|v| v["properties"]["op"]["const"] == op)
            .expect("variant");
        let req = v["required"].as_array().unwrap();
        assert!(req.contains(&"replace".into()), "{op}: {v}");
        assert_eq!(v["properties"]["replace"]["type"], "array", "{op}: {v}");
    }
    let v = serde_json::json!({"op":"source_ensure","source":"s",
        "center_hz":100_000_000,"bandwidth_hz":48_000});
    let e = serde_json::from_value::<Op>(v).unwrap_err().to_string();
    assert!(e.contains("replace"), "{e}");
    let v = serde_json::json!({"op":"job_start","kind":"passthrough","source":"s",
        "center_hz":100_000_000,"bandwidth_hz":48_000,"deviation_hz":null,"sideband":null,
        "elastic":false});
    let e = serde_json::from_value::<Op>(v).unwrap_err().to_string();
    assert!(e.contains("replace"), "{e}");
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
    let v = serde_json::json!({"op":"job_start","kind":"passthrough","source":"s",
        "center_hz":100_000_000,"bandwidth_hz":48_000,"deviation_hz":null,"sideband":null});
    let e = serde_json::from_value::<Op>(v).unwrap_err().to_string();
    assert!(e.contains("elastic"), "{e}");
    let ok = serde_json::json!({"op":"job_start","kind":"passthrough","source":"s",
        "center_hz":100_000_000,"bandwidth_hz":48_000,"deviation_hz":null,"sideband":null,
        "elastic":false,"replace":[]});
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

/// A `SourceInfo` from a node older than `worker` (a federated source listed by
/// an older node) deserialises with `worker: None`; the schema still marks it
/// required, as every node of this version writes it.
#[tokio::test]
async fn source_info_without_worker_deserialises_with_none() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, file) = file_source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![file])
        .await
        .unwrap();
    let Ok(OpResult::Sources(v)) = node.apply(Op::SourceList {}).await else {
        panic!("expected sources");
    };
    let mut json = serde_json::to_value(&v[0]).unwrap();
    json.as_object_mut().unwrap().remove("worker").unwrap();
    let back: zeta_core::ops::SourceInfo = serde_json::from_value(json).unwrap();
    assert_eq!(back.worker, None);
    assert_eq!(back, v[0]);
    let si = &schema()["result"]["$defs"]["SourceInfo"];
    assert!(
        si["required"]
            .as_array()
            .unwrap()
            .contains(&"worker".into()),
        "{si}"
    );
}

/// `source_list` items carry `stats`: required, nullable (`null` for a source
/// that keeps no stream statistics, such as a recording), and when present an
/// object with every field required and `measured_rate_hz` nullable.
#[tokio::test]
async fn source_list_carries_nullable_stats() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, file) = file_source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![file])
        .await
        .unwrap();
    let Ok(OpResult::Sources(v)) = node.apply(Op::SourceList {}).await else {
        panic!("expected sources");
    };
    assert_eq!(v[0].stats, None);
    assert_eq!(v[0].worker, None);
    let json = serde_json::to_value(&v).unwrap();
    assert!(json[0].get("worker").is_some_and(Value::is_null), "{json}");
    let si = &schema()["result"]["$defs"]["SourceInfo"];
    assert!(
        si["required"]
            .as_array()
            .unwrap()
            .contains(&"worker".into()),
        "{si}"
    );
    let worker = &si["properties"]["worker"];
    let ty = worker["type"]
        .as_array()
        .unwrap_or_else(|| panic!("{worker}"));
    assert!(ty.contains(&Value::from("null")), "{worker}");
    assert!(ty.contains(&Value::from("object")), "{worker}");
    let wi = worker;
    let mut req: Vec<&str> = wi["required"]
        .as_array()
        .unwrap_or_else(|| panic!("{wi}"))
        .iter()
        .filter_map(Value::as_str)
        .collect();
    req.sort();
    assert_eq!(req, ["pid", "restarts", "since_ns"], "{wi}");
    let json = serde_json::to_value(&v).unwrap();
    assert!(json[0].get("stats").is_some_and(Value::is_null), "{json}");
    let si = &schema()["result"]["$defs"]["SourceInfo"];
    assert!(
        si["required"].as_array().unwrap().contains(&"stats".into()),
        "{si}"
    );
    let stats = &si["properties"]["stats"];
    let ty = stats["type"]
        .as_array()
        .unwrap_or_else(|| panic!("{stats}"));
    assert!(ty.contains(&Value::from("null")), "{stats}");
    assert!(ty.contains(&Value::from("object")), "{stats}");
    let mut req: Vec<&str> = stats["required"]
        .as_array()
        .unwrap_or_else(|| panic!("{stats}"))
        .iter()
        .filter_map(Value::as_str)
        .collect();
    req.sort();
    assert_eq!(
        req,
        ["measured_rate_hz", "overflows", "restarts", "timeouts"],
        "{stats}"
    );
    let rate = stats["properties"]["measured_rate_hz"]["type"]
        .as_array()
        .unwrap_or_else(|| panic!("{stats}"));
    assert!(rate.contains(&Value::from("null")), "{stats}");
}

/// `source_list` items carry `bank`: required, nullable, and when present an object
/// with `bin_hz` and `consumers` required and nothing else.
#[test]
fn source_list_schema_has_a_nullable_bank() {
    let si = &schema()["result"]["$defs"]["SourceInfo"];
    assert!(
        si["required"].as_array().unwrap().contains(&"bank".into()),
        "{si}"
    );
    let bank = &si["properties"]["bank"];
    let ty = bank["type"].as_array().unwrap_or_else(|| panic!("{bank}"));
    assert!(ty.contains(&Value::from("null")), "{bank}");
    assert!(ty.contains(&Value::from("object")), "{bank}");
    let mut req: Vec<&str> = bank["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    req.sort();
    assert_eq!(req, ["bin_hz", "consumers", "dropped"], "{bank}");
    assert_eq!(bank["additionalProperties"], Value::Bool(false), "{bank}");
}

/// `source_pin` requires `source`, `center_hz`, `bandwidth_hz` and `replace`;
/// `source_unpin` requires `source`; `source_list` items carry a nullable `pin`.
#[test]
fn source_pin_and_unpin_schema_require_every_field_and_source_list_has_a_nullable_pin() {
    let s = schema();
    let fields = |op: &str| {
        let v = variants(&s["op"])
            .iter()
            .find(|v| v["properties"]["op"]["const"] == op)
            .unwrap_or_else(|| panic!("{op} variant"))
            .clone();
        let mut req: Vec<String> = v["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect();
        req.sort();
        req
    };
    assert_eq!(
        fields("source_pin"),
        ["bandwidth_hz", "center_hz", "op", "replace", "source"]
    );
    assert_eq!(fields("source_unpin"), ["op", "source"]);
    let v = serde_json::json!({"op":"source_pin","source":"s",
        "center_hz":100_000_000,"bandwidth_hz":48_000});
    let e = serde_json::from_value::<Op>(v).unwrap_err().to_string();
    assert!(e.contains("replace"), "{e}");
    let v = serde_json::json!({"op":"source_unpin"});
    let e = serde_json::from_value::<Op>(v).unwrap_err().to_string();
    assert!(e.contains("source"), "{e}");
    let si = &s["result"]["$defs"]["SourceInfo"];
    assert!(
        si["required"].as_array().unwrap().contains(&"pin".into()),
        "{si}"
    );
    let ty = si["properties"]["pin"]["type"]
        .as_array()
        .unwrap_or_else(|| panic!("{si}"));
    assert!(ty.contains(&Value::from("null")), "{si}");
    assert!(ty.contains(&Value::from("object")), "{si}");
}

/// `source_set_gain` requires `source` and `gain_db` (a number or `null`);
/// `GainInfo`'s fields are all required, `db` nullable; `source_list` items carry
/// a nullable `gain`.
#[test]
fn source_set_gain_schema_requires_every_field_and_gain_info_is_nullable_db() {
    let s = schema();
    let v = variants(&s["op"])
        .iter()
        .find(|v| v["properties"]["op"]["const"] == "source_set_gain")
        .expect("source_set_gain variant")
        .clone();
    let mut req: Vec<&str> = v["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    req.sort();
    assert_eq!(req, ["gain_db", "op", "source"]);
    let ty = v["properties"]["gain_db"]["type"]
        .as_array()
        .unwrap_or_else(|| panic!("{v}"));
    assert!(ty.contains(&Value::from("number")), "{v}");
    assert!(ty.contains(&Value::from("null")), "{v}");
    let e = serde_json::from_value::<Op>(serde_json::json!({"op":"source_set_gain","source":"s"}))
        .unwrap_err()
        .to_string();
    assert!(e.contains("gain_db"), "{e}");
    let op: Op = serde_json::from_value(
        serde_json::json!({"op":"source_set_gain","source":"s","gain_db":null}),
    )
    .unwrap();
    assert_eq!(
        op,
        Op::SourceSetGain {
            source: SourceId("s".into()),
            gain_db: None
        }
    );
    let e = serde_json::from_value::<Op>(
        serde_json::json!({"op":"source_set_gain","source":"s","gain_db":1,"db":1}),
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("db"), "{e}");

    let gi = &s["result"]["$defs"]["GainInfo"];
    let mut req: Vec<&str> = gi["required"]
        .as_array()
        .unwrap_or_else(|| panic!("{gi}"))
        .iter()
        .filter_map(Value::as_str)
        .collect();
    req.sort();
    assert_eq!(
        req,
        ["automatic", "automatic_available", "db", "max_db", "min_db"]
    );
    assert_eq!(gi["additionalProperties"], Value::Bool(false), "{gi}");
    let ty = gi["properties"]["db"]["type"]
        .as_array()
        .unwrap_or_else(|| panic!("{gi}"));
    assert!(ty.contains(&Value::from("null")), "{gi}");
    let si = &s["result"]["$defs"]["SourceInfo"];
    assert!(
        si["required"].as_array().unwrap().contains(&"gain".into()),
        "{si}"
    );
    let ty = si["properties"]["gain"]["type"]
        .as_array()
        .unwrap_or_else(|| panic!("{si}"));
    assert!(ty.contains(&Value::from("null")), "{si}");
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
        "/result/$defs/SourceInfo/properties/pin",
        "/result/$defs/SourceInfo/properties/gain",
        "/result/$defs/GainInfo/properties/db",
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

/// `capture_rate_hz` is always written, so required and nullable like `input`; a record
/// written before it existed loads with `None`.
#[test]
fn job_record_capture_rate_is_required_nullable_and_defaults_to_none() {
    let rec = &schema()["result"]["$defs"]["JobRecord"];
    assert!(
        rec["required"]
            .as_array()
            .unwrap()
            .contains(&"capture_rate_hz".into()),
        "{rec}"
    );
    let ty = rec["properties"]["capture_rate_hz"]["type"]
        .as_array()
        .unwrap_or_else(|| panic!("{rec}"));
    assert!(ty.contains(&Value::from("integer")), "{rec}");
    assert!(ty.contains(&Value::from("null")), "{rec}");
    let v = serde_json::json!({
        "format": 1,
        "id": "01J00000000000000000000000",
        "kind": "passthrough",
        "graph": {"id": "g", "nodes": [], "edges": []},
        "source": "s",
        "tuning": {"center_hz": 1, "bandwidth_hz": 1},
        "state": {"status": "running"},
        "context": {},
        "created_ns": 0,
        "input": null
    });
    let r: zeta_core::job::JobRecord = serde_json::from_value(v).unwrap();
    assert_eq!(r.capture_rate_hz, None);
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
                &modes(),
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
            &modes(),
        )
        .unwrap();
    assert_eq!(g.nodes.len(), 2);
    assert_eq!(g.nodes[0].kind, "channel");
    assert_eq!(g.nodes[1].kind, "am_demod");
    assert_eq!(
        g.nodes[1].params,
        serde_json::json!({"audio_rate_hz": 48_000, "squelch_db": null, "squelch": true, "denoise": false})
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
                &modes(),
            )
            .unwrap();
        assert_eq!(g.nodes.len(), 2);
        assert_eq!(g.nodes[0].kind, "channel");
        assert_eq!(g.nodes[1].kind, "ssb_demod");
        assert_eq!(
            g.nodes[1].params,
            serde_json::json!({"sideband": name, "audio_rate_hz": AUDIO_RATE_HZ, "audio_bandwidth_hz": 2_800, "denoise": false})
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
        "deviation_hz":null,"sideband":"usb","elastic":false,"replace":[]}))
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
        .expand(&src, tuning, Hz(48_000), None, None, None, &modes())
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
        .expand(&src, tuning, Hz(240_000), None, None, None, &modes())
        .unwrap();
    let kinds: Vec<&str> = g.nodes.iter().map(|n| n.kind.as_str()).collect();
    assert_eq!(kinds, ["channel", "rds_decode"]);
    assert_eq!(g.nodes[1].params, serde_json::json!({}));
    assert_eq!(g.edges.len(), 1);
    assert_eq!(g.edges[0].kind, StreamKind::Iq);
    assert_eq!(g.edges[0].from, g.nodes[0].id);
    assert_eq!(g.edges[0].to, g.nodes[1].id);
}

/// `adsb_decode`'s program row: a channel at the narrowest exact divisor of the source
/// rate (the adaptor resamples to dump1090's 2.4 MHz) into `external_decode`.
#[test]
fn adsb_row_expands_to_channel_then_external_decode() {
    let src = SourceId("b210".into());
    assert_eq!(JobKind::AdsbDecode.name(), "adsb_decode");
    let modes = modes();
    // (source rate, band, the channel's out_rate_hz)
    for (rate, bw, want) in [
        (2_400_000u64, 2_400_000u64, Some(2_400_000u64)),
        (2_048_000, 2_000_000, Some(2_048_000)),
        (8_000_000, 2_400_000, None),
    ] {
        let tuning = Tuning {
            center_hz: Hz(1_090_000_000),
            bandwidth_hz: Hz(bw),
        };
        let g = JobKind::AdsbDecode
            .expand(&src, tuning, Hz(rate), None, None, None, &modes)
            .unwrap();
        let kinds: Vec<&str> = g.nodes.iter().map(|n| n.kind.as_str()).collect();
        assert_eq!(kinds, ["channel", "external_decode"]);
        let out = g.nodes[0].params["out_rate_hz"].as_u64().unwrap();
        assert_eq!(out, channel_rate_hz(rate, bw), "{rate} {bw}");
        if let Some(want) = want {
            assert_eq!(out, want, "{rate} {bw}");
        }
        assert_eq!(rate % out, 0, "{rate} {bw}: {out} divides the source rate");
        assert!(out >= bw, "{rate} {bw}: {out}");
        assert_eq!(
            g.nodes[0].params,
            serde_json::json!({"center_hz": 1_090_000_000, "bandwidth_hz": bw, "out_rate_hz": out})
        );
        assert_eq!(
            g.nodes[1].params,
            serde_json::json!({"mode": "adsb_decode"})
        );
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.edges[0].kind, StreamKind::Iq);
        assert_eq!(g.edges[0].from, g.nodes[0].id);
        assert_eq!(g.edges[0].to, g.nodes[1].id);
    }
}

#[test]
fn adsb_expand_refuses_narrow_band() {
    let src = SourceId("b210".into());
    let tuning = Tuning {
        center_hz: Hz(1_090_000_000),
        bandwidth_hz: Hz(1_000_000),
    };
    match JobKind::AdsbDecode.expand(&src, tuning, Hz(8_000_000), None, None, None, &modes()) {
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
            assert_eq!(max, 8_000_000);
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
        .expand(&src, tuning, Hz(240_000), None, None, None, &modes())
        .unwrap();
    let kinds: Vec<&str> = g.nodes.iter().map(|n| n.kind.as_str()).collect();
    assert_eq!(kinds, ["channel", "acars_decode"]);
    assert_eq!(g.nodes[1].params, serde_json::json!({}));
    assert_eq!(g.edges.len(), 1);
    assert_eq!(g.edges[0].kind, StreamKind::Iq);
    assert_eq!(g.edges[0].from, g.nodes[0].id);
    assert_eq!(g.edges[0].to, g.nodes[1].id);
}

/// An `fm_demod` at `center_hz` on a 1 024 000 Hz capture whose bank has 256 000 Hz bins.
fn cb_graph(center_hz: u64) -> zeta_core::graph::Graph {
    let tuning = Tuning {
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(12_500),
    };
    JobKind::FmDemod
        .expand(
            &SourceId("b210".into()),
            tuning,
            Hz(1_024_000),
            Some(256_000),
            None,
            None,
            &modes(),
        )
        .unwrap()
}

#[test]
fn plan_channels_carry_the_guard() {
    let g = cb_graph(476_887_500);
    assert_eq!(g.nodes[0].kind, "channel");
    assert_eq!(
        g.nodes[0].params,
        serde_json::json!({"center_hz": 476_887_500, "bandwidth_hz": 12_500, "out_rate_hz": 51_200, "wide_hz": 37_500})
    );
    assert_eq!(g.nodes[1].kind, "nbfm_demod");
    assert_eq!(
        g.nodes[1].params,
        serde_json::json!({
            "deviation_hz": 5_000, "audio_rate_hz": 48_000, "squelch_db": null, "squelch": true,
            "guard_hz": 12_500, "guard_db": 12, "channel_hz": 12_500,
            "audio_lowpass_hz": 3_400, "deemphasis_us": 530, "denoise": false,
        })
    );
    // Too narrow a channel rate for three raster steps: no guard.
    let narrow = JobKind::FmDemod
        .expand(
            &SourceId("b210".into()),
            Tuning {
                center_hz: Hz(476_887_500),
                bandwidth_hz: Hz(12_500),
            },
            Hz(25_000),
            None,
            None,
            None,
            &modes(),
        )
        .unwrap();
    assert!(narrow.nodes[0].params.get("wide_hz").is_none());
    assert!(narrow.nodes[1].params.get("guard_hz").is_none());
}

#[test]
fn off_plan_graphs_are_unchanged() {
    use zeta_core::graph::{Edge, Graph, GraphId, Node as GraphNode, NodeId, same_pipeline};
    // Plan 12's graph, literally, plus the voice shaping fm_demod carries since 2026-11-01.
    let (channel, demod) = (NodeId::new(), NodeId::new());
    let plan12 = Graph {
        id: GraphId::new(),
        nodes: vec![
            GraphNode {
                id: channel.clone(),
                kind: "channel".into(),
                params: serde_json::json!({"center_hz": 147_050_000, "bandwidth_hz": 12_500, "out_rate_hz": 51_200}),
            },
            GraphNode {
                id: demod.clone(),
                kind: "nbfm_demod".into(),
                params: serde_json::json!({"deviation_hz": 5_000, "audio_rate_hz": 48_000, "squelch_db": null, "squelch": true,
                    "audio_lowpass_hz": 3_400, "deemphasis_us": 530, "denoise": false}),
            },
        ],
        edges: vec![Edge {
            from: channel,
            to: demod,
            kind: zeta_core::types::StreamKind::Iq,
        }],
    };
    let g = cb_graph(147_050_000);
    assert!(same_pipeline(&g, &plan12), "{g:?}");
}

#[test]
fn reexpand_carries_the_guard_params() {
    let src = SourceId("b210".into());
    let tuning = Tuning {
        center_hz: Hz(476_887_500),
        bandwidth_hz: Hz(12_500),
    };
    let mut stored = cb_graph(476_887_500);
    for n in stored.nodes.iter_mut().filter(|n| n.kind == "nbfm_demod") {
        n.params["guard_hz"] = serde_json::Value::Null;
        n.params["guard_db"] = 20.into();
    }
    let again = JobKind::FmDemod
        .reexpand(&src, tuning, Hz(512_000), None, &stored, &modes())
        .unwrap();
    let demod = &again.nodes[1].params;
    assert!(
        demod.get("guard_hz").is_some_and(|v| v.is_null()),
        "{demod}"
    );
    assert_eq!(demod["guard_db"], 20, "{demod}");
    assert_eq!(demod["channel_hz"], 12_500, "{demod}");
    assert_eq!(again.nodes[0].params["wide_hz"], 37_500);
    // A graph stored before the guard (plan 12) gains it on re-expansion.
    let mut old = cb_graph(476_887_500);
    old.nodes[0]
        .params
        .as_object_mut()
        .unwrap()
        .remove("wide_hz");
    for k in ["guard_hz", "guard_db", "channel_hz"] {
        old.nodes[1].params.as_object_mut().unwrap().remove(k);
    }
    let again = JobKind::FmDemod
        .reexpand(&src, tuning, Hz(512_000), None, &old, &modes())
        .unwrap();
    assert_eq!(again.nodes[1].params["guard_hz"], 12_500);
    assert_eq!(again.nodes[0].params["wide_hz"], 37_500);
}

/// `cb_graph(476_900_000)` as an operator put it: squelch -70 dB and no guard.
fn operator_stored() -> zeta_core::graph::Graph {
    let mut stored = cb_graph(476_900_000);
    for n in stored.nodes.iter_mut().filter(|n| n.kind == "nbfm_demod") {
        n.params["squelch_db"] = (-70).into();
        n.params["guard_hz"] = serde_json::Value::Null;
    }
    stored
}

fn retune_expand(tuning: Tuning) -> zeta_core::graph::Graph {
    JobKind::FmDemod
        .reexpand_carrying(
            &SourceId("b210".into()),
            tuning,
            Hz(1_024_000),
            Some(256_000),
            &operator_stored(),
            &modes(),
            &zeta_core::ops::RETUNE_CARRIED_PARAMS,
        )
        .unwrap()
}

fn plan_tuning(center_hz: u64, bandwidth_hz: u64) -> Tuning {
    Tuning {
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(bandwidth_hz),
    }
}

#[test]
fn reexpand_carrying_squelch_only_rederives_the_guard_at_the_new_band() {
    let g = retune_expand(plan_tuning(476_887_500, 12_500));
    assert_eq!(
        g.nodes[0].params,
        serde_json::json!({"center_hz": 476_887_500, "bandwidth_hz": 12_500, "out_rate_hz": 51_200, "wide_hz": 37_500})
    );
    assert_eq!(
        g.nodes[1].params,
        serde_json::json!({
            "deviation_hz": 5_000, "audio_rate_hz": 48_000, "squelch_db": -70, "squelch": true,
            "guard_hz": 12_500, "guard_db": 12, "channel_hz": 12_500,
            "audio_lowpass_hz": 3_400, "deemphasis_us": 530, "denoise": false,
        })
    );
}

#[test]
fn reexpand_carrying_squelch_only_drops_the_guard_off_plan() {
    let g = retune_expand(plan_tuning(476_905_000, 12_500));
    assert!(g.nodes[0].params.get("wide_hz").is_none(), "{g:?}");
    for k in ["guard_hz", "guard_db", "channel_hz"] {
        assert!(g.nodes[1].params.get(k).is_none(), "{k}: {g:?}");
    }
    assert_eq!(g.nodes[1].params["squelch_db"], -70);
}

#[test]
fn a_width_preset_on_a_plan_channel_changes_channel_hz_and_wide_hz() {
    let g = retune_expand(plan_tuning(476_900_000, 25_000));
    assert_eq!(g.nodes[0].params["bandwidth_hz"], 25_000);
    assert_eq!(g.nodes[0].params["wide_hz"], 50_000);
    assert_eq!(g.nodes[1].params["channel_hz"], 25_000);
    assert_eq!(g.nodes[1].params["guard_hz"], 12_500);
}

/// The mode registry a node with the DSP stages loads: every embedded row ready.
fn modes() -> zeta_core::modes::ModeRegistry {
    let mut stages = StageRegistry::new();
    zeta_dsp::register_all(&mut stages);
    zeta_core::modes::ModeRegistry::load(&stages, None)
}

/// Plan 15's `aprs_decode`, `pocsag_decode`, `rds_decode` and `acars_decode` arms of
/// `JobKind::expand`, verbatim, at the channel rate `out` that `expand` picks.
fn legacy_expand(kind: &JobKind, tuning: Tuning, out: u64) -> zeta_core::graph::Graph {
    use zeta_core::graph::{Edge, Graph, GraphId, Node as GraphNode, NodeId};
    match kind {
        JobKind::AprsDecode => {
            let (channel, demod, decode) = (NodeId::new(), NodeId::new(), NodeId::new());
            Graph {
                id: GraphId::new(),
                nodes: vec![
                    GraphNode {
                        id: channel.clone(),
                        kind: "channel".into(),
                        params: serde_json::json!({
                            "center_hz": tuning.center_hz.0,
                            "bandwidth_hz": tuning.bandwidth_hz.0,
                            "out_rate_hz": out,
                        }),
                    },
                    GraphNode {
                        id: demod.clone(),
                        kind: "nbfm_demod".into(),
                        // The decoder reads every block, however weak: no squelch.
                        params: serde_json::json!({
                            "deviation_hz": 3_000,
                            "audio_rate_hz": AUDIO_RATE_HZ,
                            "squelch_db": null,
                            "squelch": false,
                        }),
                    },
                    GraphNode {
                        id: decode.clone(),
                        kind: "aprs_decode".into(),
                        params: serde_json::json!({}),
                    },
                ],
                edges: vec![
                    Edge {
                        from: channel,
                        to: demod.clone(),
                        kind: StreamKind::Iq,
                    },
                    Edge {
                        from: demod,
                        to: decode,
                        kind: StreamKind::Audio,
                    },
                ],
            }
        }
        JobKind::PocsagDecode => {
            let (channel, decode) = (NodeId::new(), NodeId::new());
            Graph {
                id: GraphId::new(),
                nodes: vec![
                    GraphNode {
                        id: channel.clone(),
                        kind: "channel".into(),
                        params: serde_json::json!({
                            "center_hz": tuning.center_hz.0,
                            "bandwidth_hz": tuning.bandwidth_hz.0,
                            "out_rate_hz": out,
                        }),
                    },
                    GraphNode {
                        id: decode.clone(),
                        kind: "pocsag_decode".into(),
                        params: serde_json::json!({}),
                    },
                ],
                edges: vec![Edge {
                    from: channel,
                    to: decode,
                    kind: StreamKind::Iq,
                }],
            }
        }
        JobKind::RdsDecode => {
            let (channel, decode) = (NodeId::new(), NodeId::new());
            Graph {
                id: GraphId::new(),
                nodes: vec![
                    GraphNode {
                        id: channel.clone(),
                        kind: "channel".into(),
                        params: serde_json::json!({
                            "center_hz": tuning.center_hz.0,
                            "bandwidth_hz": tuning.bandwidth_hz.0,
                            "out_rate_hz": out,
                        }),
                    },
                    GraphNode {
                        id: decode.clone(),
                        kind: "rds_decode".into(),
                        params: serde_json::json!({}),
                    },
                ],
                edges: vec![Edge {
                    from: channel,
                    to: decode,
                    kind: StreamKind::Iq,
                }],
            }
        }
        JobKind::AcarsDecode => {
            let (channel, decode) = (NodeId::new(), NodeId::new());
            Graph {
                id: GraphId::new(),
                nodes: vec![
                    GraphNode {
                        id: channel.clone(),
                        kind: "channel".into(),
                        params: serde_json::json!({
                            "center_hz": tuning.center_hz.0,
                            "bandwidth_hz": tuning.bandwidth_hz.0,
                            "out_rate_hz": out,
                        }),
                    },
                    GraphNode {
                        id: decode.clone(),
                        kind: "acars_decode".into(),
                        params: serde_json::json!({}),
                    },
                ],
                edges: vec![Edge {
                    from: channel,
                    to: decode,
                    kind: StreamKind::Iq,
                }],
            }
        }
        other => panic!("no legacy arm for {other:?}"),
    }
}

/// The four in-crate decoder kinds expand from their rows to exactly plan 15's graphs,
/// on a bank bin and on the source rate.
#[test]
fn decoder_rows_expand_to_todays_graphs() {
    let src = SourceId("b210".into());
    let modes = modes();
    let cases = [
        (144_800_000u64, 12_500u64, 2_048_000u64, Some(256_000u64)),
        (104_900_000, 200_000, 2_048_000, None),
    ];
    for kind in [
        JobKind::AprsDecode,
        JobKind::PocsagDecode,
        JobKind::RdsDecode,
        JobKind::AcarsDecode,
    ] {
        for (center, bw, rate, bin_rate) in cases {
            let tuning = Tuning {
                center_hz: Hz(center),
                bandwidth_hz: Hz(bw),
            };
            let out = match bin_rate {
                Some(b) if bw <= b / 2 => channel_rate_hz(b, bw),
                _ => channel_rate_hz(rate, bw),
            };
            let g = kind
                .expand(&src, tuning, Hz(rate), bin_rate, None, None, &modes)
                .unwrap();
            let want = legacy_expand(&kind, tuning, out);
            assert!(
                zeta_core::graph::same_pipeline(&g, &want),
                "{kind:?} {center} {bw}: {g:?}\nwant {want:?}"
            );
            // Fresh ids, as the arms gave: no template id leaks into the job's graph.
            for n in &g.nodes {
                assert!(!["channel", "demod", "decode"].contains(&n.id.0.as_str()));
            }
        }
    }
}

/// `job_start` of an id no row has names the ids the registry has, and tunes nothing.
#[tokio::test]
async fn an_unknown_mode_id_names_the_known_ids() {
    let dir = tempfile::tempdir().unwrap();
    let mut stages = registry();
    zeta_dsp::register_all(&mut stages);
    let port = Arc::new(WidePort::new("rtl0"));
    let node = Node::open(cfg(dir.path()), stages, vec![port.clone() as Arc<dyn Port>])
        .await
        .unwrap();
    let op: Op = serde_json::from_value(serde_json::json!({
        "op": "job_start", "kind": "flex_decode", "source": "rtl0",
        "center_hz": 929_612_500u64, "bandwidth_hz": 25_000, "deviation_hz": null,
        "sideband": null, "elastic": false, "replace": [],
    }))
    .unwrap();
    match node.apply(op).await {
        Err(OpError::Unknown {
            field,
            value,
            known,
        }) => {
            assert_eq!(field, "kind");
            assert_eq!(value, "flex_decode");
            assert!(known.iter().any(|k| k == "fm_demod"), "{known:?}");
            assert!(known.iter().any(|k| k == "pocsag_decode"), "{known:?}");
            assert!(!known.iter().any(|k| k == "transcribe"), "{known:?}");
        }
        other => panic!("expected unknown, got {other:?}"),
    }
    assert!(node.job_list().is_empty());
    assert_eq!(port.current(), None);
}

/// Built-in kinds keep their wire names; any other lowercase id is a mode; anything
/// else does not deserialise.
#[test]
fn job_kind_roundtrips_built_ins_and_mode_ids() {
    use zeta_core::modes::ModeId;
    for (wire, kind) in [
        ("passthrough", JobKind::Passthrough),
        ("fm_demod", JobKind::FmDemod),
        ("pocsag_decode", JobKind::PocsagDecode),
        ("adsb_decode", JobKind::AdsbDecode),
        ("transcribe", JobKind::Transcribe),
        ("flex_decode", JobKind::Mode(ModeId("flex_decode".into()))),
        ("m17", JobKind::Mode(ModeId("m17".into()))),
    ] {
        let v = serde_json::Value::from(wire);
        assert_eq!(serde_json::from_value::<JobKind>(v.clone()).unwrap(), kind);
        assert_eq!(serde_json::to_value(&kind).unwrap(), v);
        assert_eq!(kind.name(), wire);
        assert_eq!(JobKind::from_name(wire), Some(kind));
    }
    for bad in ["Nope", "", "9lives", "flex-decode", "a b"] {
        assert!(
            serde_json::from_value::<JobKind>(bad.into()).is_err(),
            "{bad:?}"
        );
        assert_eq!(JobKind::from_name(bad), None, "{bad:?}");
    }
    assert!(serde_json::from_value::<JobKind>(serde_json::json!(3)).is_err());
}

/// A recording as source `id`, tunable over `min_hz..=max_hz` (48 kHz wide at most).
fn ranged_file(dir: &Path, id: &str, min_hz: u64, max_hz: u64) -> Arc<dyn Port> {
    let meta = fixtures::write_tone(dir);
    let opts = FileOpts {
        realtime: false,
        block_samples: 4800,
        repeat: true,
        tune_range: Some((Hz(min_hz), Hz(max_hz))),
    };
    Arc::new(FileSource::open(SourceId(id.into()), &meta, opts).unwrap())
}

/// `source_free` on `center_hz`/`bandwidth_hz`: the free ids and the busy entries.
async fn source_free(
    node: &Node,
    center_hz: u64,
    bandwidth_hz: u64,
) -> (Vec<String>, Vec<zeta_core::ops::BusySource>) {
    match node
        .apply(Op::SourceFree {
            center_hz: Hz(center_hz),
            bandwidth_hz: Hz(bandwidth_hz),
        })
        .await
    {
        Ok(OpResult::SourceFree { free, busy }) => (free.into_iter().map(|s| s.0).collect(), busy),
        other => panic!("expected source_free, got {other:?}"),
    }
}

/// Start a job of `kind` on `source` at 403 MHz, 20 kHz wide.
async fn start_on(node: &Node, source: &str, kind: JobKind, elastic: bool) -> JobId {
    let op = Op::JobStart {
        kind,
        source: SourceId(source.into()),
        center_hz: Hz(403_000_000),
        bandwidth_hz: Hz(20_000),
        deviation_hz: None,
        sideband: None,
        elastic,
        replace: vec![],
    };
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected Job, got {other:?}"),
    }
}

#[tokio::test]
async fn source_free_lists_idle_sources_that_cover_the_band() {
    let dir = tempfile::tempdir().unwrap();
    let a = ranged_file(dir.path(), "sonde_a", 400_000_000, 406_000_000);
    let b = ranged_file(dir.path(), "sonde_b", 400_000_000, 406_000_000);
    let node = Node::open(cfg(dir.path()), registry(), vec![a, b])
        .await
        .unwrap();
    let (free, busy) = source_free(&node, 403_000_000, 20_000).await;
    assert_eq!(free, vec!["sonde_a", "sonde_b"]);
    assert!(busy.is_empty(), "{busy:?}");
    let job = start_on(&node, "sonde_a", JobKind::Passthrough, false).await;
    let (free, busy) = source_free(&node, 403_000_000, 20_000).await;
    assert_eq!(free, vec!["sonde_b"]);
    assert_eq!(busy.len(), 1, "{busy:?}");
    assert_eq!(busy[0].id.0, "sonde_a");
    assert_eq!(busy[0].holders, vec![job]);
    assert!(busy[0].elastic.is_empty());
    assert_eq!(busy[0].pin, None);
    assert_eq!(busy[0].fault, None);
}

/// Stands in for the dsp `fft` stage a `spectrum` job runs: idles until cancelled.
struct IdleFft(zeta_core::stage::StageSpec);

#[async_trait]
impl zeta_core::stage::Stage for IdleFft {
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
async fn an_elastic_job_does_not_make_a_source_busy() {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = registry();
    reg.register(Arc::new(IdleFft(zeta_core::stage::StageSpec {
        kind: "fft",
        inputs: vec![StreamKind::Iq],
        outputs: vec![StreamKind::Event],
        params_schema: schemars::json_schema!({}),
    })));
    let node = Node::open(
        cfg(dir.path()),
        reg,
        vec![Arc::new(WidePort::new("hackrf0"))],
    )
    .await
    .unwrap();
    start_on(&node, "hackrf0", JobKind::Spectrum, true).await;
    assert_eq!(node.lease_elastic(&SourceId("hackrf0".into())).len(), 1);
    let (free, busy) = source_free(&node, 403_000_000, 20_000).await;
    assert_eq!(free, vec!["hackrf0"]);
    assert!(busy.is_empty(), "{busy:?}");
}

#[tokio::test]
async fn a_pinned_or_faulted_source_is_busy() {
    let dir = tempfile::tempdir().unwrap();
    let pinned = ranged_file(dir.path(), "file_pinned", 400_000_000, 406_000_000);
    let faulted = Arc::new(WidePort::new("hackrf_faulted"));
    *faulted.fault.lock().unwrap() = Some("device gone".into());
    let idle = Arc::new(WidePort::new("hackrf_idle"));
    let node = Node::open(cfg(dir.path()), registry(), vec![pinned, faulted, idle])
        .await
        .unwrap();
    let op = Op::SourcePin {
        source: SourceId("file_pinned".into()),
        center_hz: Hz(401_000_000),
        bandwidth_hz: Hz(40_000),
        replace: vec![],
    };
    let Ok(OpResult::Tuning(_)) = node.apply(op).await else {
        panic!("pin refused");
    };
    let pin = node.pinned(&SourceId("file_pinned".into()));
    assert!(pin.is_some());
    let (free, busy) = source_free(&node, 403_000_000, 20_000).await;
    assert_eq!(free, vec!["hackrf_idle"]);
    let ids: Vec<&str> = busy.iter().map(|b| b.id.0.as_str()).collect();
    assert_eq!(ids, vec!["file_pinned", "hackrf_faulted"]);
    assert_eq!(busy[0].pin, pin);
    assert_eq!(busy[0].fault, None);
    assert!(busy[0].holders.is_empty());
    assert_eq!(busy[1].pin, None);
    assert_eq!(busy[1].fault.as_deref(), Some("device gone"));
    // Required, `pin` and `fault` nullable, in the result schema.
    let bs = &schema()["result"]["$defs"]["BusySource"];
    for field in ["id", "holders", "elastic", "pin", "fault"] {
        assert!(
            bs["required"].as_array().unwrap().contains(&field.into()),
            "{field}: {bs}"
        );
    }
    let json = serde_json::to_value(&busy).unwrap();
    assert!(
        json[0]["fault"].is_null() && json[1]["pin"].is_null(),
        "{json}"
    );
}

#[tokio::test]
async fn a_source_that_cannot_cover_the_band_is_in_neither_list() {
    let dir = tempfile::tempdir().unwrap();
    let file = ranged_file(dir.path(), "file0", 400_000_000, 406_000_000);
    let wide = Arc::new(WidePort::new("hackrf0"));
    let node = Node::open(cfg(dir.path()), registry(), vec![file, wide])
        .await
        .unwrap();
    // Outside the range, straddling either edge, and wider than the recording's 48 kHz.
    for (center_hz, bandwidth_hz) in [
        (145_000_000, 20_000),
        (400_005_000, 20_000),
        (405_995_000, 20_000),
        (403_000_000, 100_000),
    ] {
        let (free, busy) = source_free(&node, center_hz, bandwidth_hz).await;
        assert_eq!(free, vec!["hackrf0"], "{center_hz} {bandwidth_hz}");
        assert!(busy.is_empty(), "{center_hz} {bandwidth_hz}: {busy:?}");
    }
    // A band the range just holds is free.
    let (free, _) = source_free(&node, 400_010_000, 20_000).await;
    assert_eq!(free, vec!["file0", "hackrf0"]);
}

#[tokio::test]
async fn source_free_is_read_only() {
    let op = Op::SourceFree {
        center_hz: Hz(403_000_000),
        bandwidth_hz: Hz(20_000),
    };
    assert!(op.read_only());
    assert_eq!(op.name(), "source_free");
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(
        cfg(dir.path()),
        registry(),
        vec![Arc::new(WidePort::new("hackrf0"))],
    )
    .await
    .unwrap();
    assert!(matches!(
        node.apply(op).await,
        Ok(OpResult::SourceFree { .. })
    ));
    let log = std::fs::read_to_string(dir.path().join("state").join("ops.log")).unwrap();
    let line: Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
    assert_eq!(line["op"]["op"], "source_free", "{line}");
    assert_eq!(line["outcome"], "ok", "{line}");
    assert!(line.get("result").is_none(), "{line}");
}

/// A rigid `job_start` on an unheld source whose port cannot tune (a device that is not
/// there) is refused under the source's name, as a job that failed on the faulted source
/// reads; a range error on the same port keeps its corrective `field` and `suggested`.
#[cfg(feature = "test-fixtures")]
#[tokio::test]
async fn job_start_on_a_faulted_source_is_refused_naming_the_source() {
    let dir = tempfile::tempdir().unwrap();
    let meta = fixtures::write_tone(dir.path());
    let opts = FileOpts {
        realtime: false,
        block_samples: 4800,
        repeat: true,
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(SourceId("s".into()), &meta, opts).unwrap());
    let port = zeta_core::test_fixtures::FaultablePort::new(src);
    let node = Node::open(cfg(dir.path()), registry(), vec![port.clone()])
        .await
        .unwrap();
    port.set_fault(Some("device not found: x".into()));
    let start = |center_hz: u64| Op::JobStart {
        kind: JobKind::Passthrough,
        source: SourceId("s".into()),
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(48_000),
        deviation_hz: None,
        sideband: None,
        elastic: false,
        replace: vec![],
    };
    match node.apply(start(100_000_000)).await {
        Err(OpError::Other { message }) => {
            assert_eq!(message, "source s: io: device not found: x")
        }
        other => panic!("expected Other, got {other:?}"),
    }
    match node.apply(start(100_000)).await {
        Err(OpError::OutOfRange {
            field, suggested, ..
        }) => {
            assert_eq!(field, "center_hz");
            assert_eq!(suggested, Some(100_000_000));
        }
        other => panic!("expected OutOfRange, got {other:?}"),
    }
}

#[test]
fn demod_graphs_carry_denoise_and_the_voice_shaping() {
    let tuning = Tuning {
        center_hz: Hz(100_005_000),
        bandwidth_hz: Hz(12_500),
    };
    let src = SourceId("file0".into());
    for (kind, demod) in [
        (JobKind::FmDemod, "nbfm_demod"),
        (JobKind::AmDemod, "am_demod"),
        (JobKind::SsbDemod, "ssb_demod"),
    ] {
        let g = kind
            .expand(&src, tuning, Hz(48_000), None, None, None, &modes())
            .unwrap();
        let n = g.nodes.iter().find(|n| n.kind == demod).expect(demod);
        assert_eq!(n.params["denoise"], false, "{kind:?}");
        // What `graph_put` set survives a re-expansion and a retune.
        let mut stored = g.clone();
        for m in stored.nodes.iter_mut().filter(|m| m.kind == demod) {
            m.params["denoise"] = true.into();
            if demod == "nbfm_demod" {
                m.params["audio_lowpass_hz"] = serde_json::Value::Null;
                m.params["deemphasis_us"] = 750.into();
            }
        }
        for carried in [None, Some(&zeta_core::ops::RETUNE_CARRIED_PARAMS[..])] {
            let again = match carried {
                None => kind.reexpand(&src, tuning, Hz(96_000), None, &stored, &modes()),
                Some(c) => {
                    kind.reexpand_carrying(&src, tuning, Hz(96_000), None, &stored, &modes(), c)
                }
            }
            .unwrap();
            let n = again.nodes.iter().find(|n| n.kind == demod).expect(demod);
            assert_eq!(n.params["denoise"], true, "{kind:?} {carried:?}");
            if demod == "nbfm_demod" {
                assert_eq!(
                    n.params["audio_lowpass_hz"],
                    serde_json::Value::Null,
                    "{carried:?}"
                );
                assert_eq!(n.params["deemphasis_us"], 750, "{carried:?}");
            }
        }
    }
    // Only the NBFM voice demodulator shapes its audio; the WFM one has its own.
    let g = JobKind::WfmDemod
        .expand(&src, tuning, Hz(480_000), None, None, None, &modes())
        .unwrap();
    assert!(g.nodes.iter().all(|n| n.params.get("denoise").is_none()));
}

#[test]
fn an_attached_transcribe_skips_its_own_denoise_when_the_input_denoises() {
    let (job, stream) = (JobId("in".into()), StreamId("s".into()));
    let g = JobKind::Transcribe
        .expand_attached(&job, &stream, "en", true)
        .unwrap();
    let kinds: Vec<&str> = g.nodes.iter().map(|n| n.kind.as_str()).collect();
    assert_eq!(kinds, ["stream_in", "vad", "transcribe"]);
    assert_eq!(g.edges.len(), 2);
    let g = JobKind::Transcribe
        .expand_attached(&job, &stream, "en", false)
        .unwrap();
    let kinds: Vec<&str> = g.nodes.iter().map(|n| n.kind.as_str()).collect();
    assert_eq!(kinds, ["stream_in", "denoise", "vad", "transcribe"]);
}
