//! Operation set: corrective errors, job round trip, IQ capture and schema strictness.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use zeta_core::job::JobState;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpError, OpResult, schema};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Capabilities, Claim, Hz, SampleFormat, Selection, SourceId, Span, Timestamp};
use zeta_source_file::{FileOpts, FileSource, fixtures, read_meta};

/// A port tunable over 1 MHz..6 GHz that accepts any in-range request.
struct WidePort {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tx: tokio::sync::broadcast::Sender<IqBlock>,
}

impl WidePort {
    fn new(id: &str) -> Self {
        Self {
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
    NodeConfig { state_dir: dir.join("state"), ring_seconds: 1.0, backends: vec![Backend::Cpu] }
}

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    r
}

fn file_source(dir: &Path) -> (Arc<FileSource>, Arc<dyn Port>) {
    let meta = fixtures::write_tone(dir);
    let opts = FileOpts { realtime: false, block_samples: 4800, repeat: true };
    let src = Arc::new(FileSource::open(SourceId("file0".into()), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    (src, port)
}

#[tokio::test]
async fn ensure_out_of_range_suggests_dropped_zeros() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(cfg(dir.path()), registry(), vec![Arc::new(WidePort::new("hackrf0"))]).await.unwrap();
    let op = Op::SourceEnsure { source: SourceId("hackrf0".into()), center_hz: Hz(101_500), bandwidth_hz: Hz(200_000) };
    match node.apply(op).await {
        Err(OpError::OutOfRange { field, value, min, max, suggested }) => {
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
    let ports: Vec<Arc<dyn Port>> = vec![Arc::new(WidePort::new("rtl0")), Arc::new(WidePort::new("hackrf0"))];
    let node = Node::open(cfg(dir.path()), registry(), ports).await.unwrap();
    let op = Op::SourceEnsure { source: SourceId("nope".into()), center_hz: Hz(100_000_000), bandwidth_hz: Hz(200_000) };
    match node.apply(op).await {
        Err(OpError::Unknown { field, value, known }) => {
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
    let node = Node::open(cfg(dir.path()), registry(), vec![port]).await.unwrap();
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
    assert_eq!(node.apply(Op::JobStop { job: id.clone() }).await, Ok(OpResult::Ok));
    match node.apply(Op::JobList {}).await {
        Ok(OpResult::Jobs(jobs)) => assert_eq!(jobs[0].state, JobState::Stopped),
        other => panic!("expected Jobs, got {other:?}"),
    }
    assert!(node.lease_holders(&SourceId("file0".into())).is_empty());
}

#[tokio::test]
async fn iq_read_writes_sigmf_pair() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = file_source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port]).await.unwrap();
    src.start();
    let source = SourceId("file0".into());
    let mut earliest = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(25)).await;
        earliest = node.ring(&source).and_then(|r| r.read().unwrap().earliest());
        if earliest.is_some() {
            break;
        }
    }
    let start = earliest.expect("ring has data").0;
    let selection = Selection {
        source,
        span: Span { start: Timestamp(start), end: Timestamp(start + 50_000_000) },
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(48_000),
    };
    let (rate, samples, path) = match node.apply(Op::IqRead { selection }).await {
        Ok(OpResult::Iq { sample_rate_hz, samples, path }) => (sample_rate_hz, samples, path),
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
    let data = std::fs::metadata(path.with_extension("sigmf-data")).unwrap();
    assert_eq!(data.len(), 2400 * 8);
}

fn variants(op: &Value) -> &Vec<Value> {
    op.get("oneOf").or_else(|| op.get("anyOf")).and_then(Value::as_array).expect("op schema has oneOf/anyOf")
}

#[test]
fn every_op_variant_denies_unknown_fields_and_requires_all() {
    let s = schema();
    let vs = variants(&s["op"]);
    assert_eq!(vs.len(), 11, "one schema per Op variant");
    for v in vs {
        assert_eq!(v["additionalProperties"], Value::Bool(false), "{v}");
        let mut props: Vec<&String> = v["properties"].as_object().expect("properties").keys().collect();
        let mut req: Vec<&str> = v["required"].as_array().expect("required").iter().map(|r| r.as_str().unwrap()).collect();
        props.sort();
        req.sort();
        assert_eq!(req, props, "{v}");
        assert!(req.contains(&"op"), "{v}");
    }
    let bad = serde_json::json!({"op": "job_list", "extra": 1});
    assert!(serde_json::from_value::<Op>(bad).is_err());
}

fn check_units(v: &Value, path: &str) {
    match v {
        Value::Object(m) => {
            if let Some(Value::Object(props)) = m.get("properties") {
                for name in props.keys() {
                    let unit = ["freq", "rate", "bandwidth", "center"].iter().any(|w| name.contains(w));
                    assert!(
                        !unit || name.ends_with("_hz") || name.ends_with("_baud"),
                        "{path}.{name} is a unit field without _hz/_baud"
                    );
                }
            }
            for (k, c) in m {
                check_units(c, &format!("{path}.{k}"));
            }
        }
        Value::Array(a) => a.iter().enumerate().for_each(|(i, c)| check_units(c, &format!("{path}[{i}]"))),
        _ => {}
    }
}

#[test]
fn unit_fields_end_in_hz_or_baud() {
    let s = schema();
    for key in ["op", "result", "error"] {
        assert!(s[key].is_object(), "schema has {key}");
    }
    check_units(&s, "schema");
}
