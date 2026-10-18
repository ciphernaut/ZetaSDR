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
use zeta_core::ops::{JobKind, Op, OpError, OpResult, schema};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::JobId;
use zeta_core::types::{
    Capabilities, Claim, Hz, SampleFormat, Selection, SourceId, Span, Timestamp,
};
use zeta_source_file::{FileOpts, FileSource, fixtures, read_meta};

/// A port tunable over 1 MHz..6 GHz that accepts any in-range request.
struct WidePort {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tx: tokio::sync::broadcast::Sender<IqBlock>,
    /// Sleep inside `tune`, to widen the window a concurrent call could race into.
    slow: bool,
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
}

fn cfg(dir: &Path) -> NodeConfig {
    NodeConfig {
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
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
    };
    let ensure = Op::SourceEnsure {
        source,
        center_hz: Hz(433_000_000),
        bandwidth_hz: Hz(200_000),
    };
    // `join!` polls `start` first, so the job is inside its slow tune when `ensure` runs.
    let (started, ensured) = tokio::join!(node.apply(start), node.apply(ensure));
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
    assert_eq!(vs.len(), 13, "one schema per Op variant");
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
                    assert!(
                        !time || name.ends_with("_ns"),
                        "{path}.{name} is a time field without _ns"
                    );
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
    let g = JobKind::Spectrum.expand(&SourceId("file0".into()), tuning);
    assert_eq!(g.nodes.len(), 1);
    assert!(g.edges.is_empty());
    assert_eq!(g.nodes[0].kind, "fft");
    assert_eq!(
        g.nodes[0].params,
        serde_json::json!({"size": 1024, "window": "hann"})
    );
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
    let g = JobKind::FmDemod.expand(&SourceId("file0".into()), tuning);
    assert_eq!(g.nodes.len(), 2);
    assert_eq!(g.nodes[0].kind, "channel");
    assert_eq!(
        g.nodes[0].params,
        serde_json::json!({"center_hz": 100_005_000, "bandwidth_hz": 12_500, "out_rate_hz": 48_000})
    );
    assert_eq!(g.nodes[1].kind, "nbfm_demod");
    assert_eq!(
        g.nodes[1].params,
        serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": 48_000})
    );
    assert_eq!(g.edges.len(), 1);
    assert_eq!(g.edges[0].from, g.nodes[0].id);
    assert_eq!(g.edges[0].to, g.nodes[1].id);
    assert_eq!(g.edges[0].kind, zeta_core::types::StreamKind::Iq);
}
