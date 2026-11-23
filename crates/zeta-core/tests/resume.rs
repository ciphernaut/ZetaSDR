//! Resume restores the capture: a source's running records launch widest first, a scan
//! first of all, and `job_start` records the capture rate its graph was expanded for.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use zeta_core::graph::{Graph, GraphId, Node as GraphNode, NodeId};
use zeta_core::job::{JOB_RECORD_FORMAT, JobRecord, JobState};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{AttachTo, JobKind, Op, OpResult};
use zeta_core::port::{
    IqBlock, IqReceiver, Port, PortError, SWEEP_ROWS_CAPACITY, SweepPlan, SweepRequest, SweepRow,
    SweepRows, SweepState, Tuning,
};
use zeta_core::stage::{
    Backend, PassthroughStage, Stage, StageContext, StageError, StageRegistry, StageSpec,
};
use zeta_core::stream::{Frame, StreamBus};
use zeta_core::types::{
    Capabilities, Claim, Hz, JobId, SampleFormat, SourceId, Span, StreamId, StreamKind, SweepCaps,
    Timestamp,
};
use zeta_source_file::{FileOpts, FileSource, fixtures};

mod common;

fn source_with(dir: &Path, repeat: bool) -> (Arc<FileSource>, Arc<dyn Port>) {
    let meta = fixtures::write_tone(dir);
    let opts = FileOpts {
        realtime: false,
        block_samples: 4800,
        repeat,
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(SourceId("file0".into()), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    (src, port)
}

fn cfg(dir: &Path) -> NodeConfig {
    NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
        owns_state_dir: true,
        bank_factory: None,
        modes_dir: None,
        handlers_dir: None,
    }
}

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    r.register(Arc::new(IdleFft(StageSpec {
        kind: "fft",
        inputs: vec![StreamKind::Iq],
        outputs: vec![StreamKind::Event],
        params_schema: schemars::json_schema!({}),
    })));
    r
}

fn on_disk(dir: &Path, id: &JobId) -> JobRecord {
    let p = dir
        .join("state")
        .join("jobs")
        .join(format!("{}.json", id.0));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// Stands in for the dsp `fft` stage a scan needs (core cannot depend on it): it
/// emits nothing and ends when cancelled.
struct IdleFft(StageSpec);

#[async_trait]
impl Stage for IdleFft {
    fn spec(&self) -> &StageSpec {
        &self.0
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    async fn run(
        &self,
        _params: serde_json::Value,
        _inputs: Vec<broadcast::Receiver<Frame>>,
        _outputs: Vec<StreamId>,
        _bus: Arc<StreamBus>,
        cancel: CancellationToken,
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        cancel.cancelled().await;
        Err(StageError::Cancelled)
    }
}

/// A radio that tunes to exactly the band asked for, so its capture is the band of
/// whichever job tuned it first and a wider job after a narrower one conflicts. One
/// made [`Radio::sweeping`] also sweeps for itself.
struct Radio {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tx: broadcast::Sender<IqBlock>,
    /// Every tune, in order.
    tunes: Mutex<Vec<Tuning>>,
    /// What its sweep is refused with, after `start_ms` (`None`: granted).
    refuse: Mutex<Option<String>>,
    start_ms: u64,
}

impl Radio {
    /// A radio that sweeps any span inside its tune range for itself, each sweep
    /// four steps of 1024 bins, a row every 5 ms, its start taking `start_ms`, or
    /// that refuses every sweep with `refuse`.
    fn sweeping(refuse: Option<&str>, start_ms: u64) -> Arc<Self> {
        let mut r = Arc::into_inner(Self::new()).expect("one owner");
        r.caps.sweep = Some(SweepCaps {
            min_hz: Hz(1_000_000),
            max_hz: Hz(6_000_000_000),
            bin_hz_min: Hz(1000),
            rows_per_s_max: 200,
            calibrated: false,
        });
        r.refuse = Mutex::new(refuse.map(Into::into));
        r.start_ms = start_ms;
        Arc::new(r)
    }

    fn tunes(&self) -> Vec<Tuning> {
        self.tunes.lock().unwrap().clone()
    }

    /// Refuse every sweep asked for from now on with `why` (`None`: grant them).
    fn set_refuse(&self, why: Option<&str>) {
        *self.refuse.lock().unwrap() = why.map(Into::into);
    }

    fn new() -> Arc<Self> {
        Arc::new(Self {
            id: SourceId("radio0".into()),
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
                usable_fraction: Claim::Asserted(1.0),
                depths: vec![],
                sweep: None,
            },
            cur: Mutex::new(None),
            tx: broadcast::channel(1).0,
            tunes: Mutex::new(Vec::new()),
            refuse: Mutex::new(None),
            start_ms: 0,
        })
    }
}

#[async_trait]
impl Port for Radio {
    fn id(&self) -> &SourceId {
        &self.id
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        self.tunes.lock().unwrap().push(t);
        *self.cur.lock().unwrap() = Some(t);
        Ok(t)
    }
    fn current(&self) -> Option<Tuning> {
        *self.cur.lock().unwrap()
    }
    fn subscribe(&self) -> IqReceiver {
        self.tx.subscribe()
    }
    async fn sweep(&self, req: SweepRequest) -> Result<SweepRows, PortError> {
        if self.caps.sweep.is_none() {
            return Err(PortError::Unsupported(
                "source does not sweep for itself".into(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(self.start_ms)).await;
        let refuse = self.refuse.lock().unwrap().clone();
        if let Some(why) = refuse {
            return Err(PortError::Unsupported(why));
        }
        let step_hz = req.span.bandwidth_hz.0 / 4;
        let lo = req.span.center_hz.0 - req.span.bandwidth_hz.0 / 2;
        let plan = SweepPlan {
            steps: 4,
            step_hz: Hz(step_hz),
            size: 1024,
            bin_hz: step_hz as f64 / 1024.0,
            dwell_ms: req.dwell_ms,
            shift_hz: Hz(0),
        };
        let (tx, rows) = tokio::sync::mpsc::channel(SWEEP_ROWS_CAPACITY);
        let (state_tx, state) = tokio::sync::watch::channel(SweepState::Running);
        let bin_hz = plan.bin_hz;
        tokio::spawn(async move {
            // Held open while the rows run.
            let _state_tx = state_tx;
            for n in 0u64.. {
                tokio::select! {
                    _ = tx.closed() => return,
                    _ = tokio::time::sleep(Duration::from_millis(5)) => {}
                }
                let step = (n % 4) as u32;
                let end_ns = (n + 1) * 5_000_000;
                let row = SweepRow {
                    sweep: n / 4,
                    step,
                    center_hz: Hz(lo + step_hz * u64::from(step) + step_hz / 2),
                    bin_hz,
                    db: vec![-100.0; 1024],
                    masked: vec![],
                    span: Span {
                        start_ns: Timestamp(end_ns - 5_000_000),
                        end_ns: Timestamp(end_ns),
                    },
                };
                if let Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) = tx.try_send(row) {
                    return;
                }
            }
        });
        Ok(SweepRows {
            plan,
            rows,
            state,
            control: None,
        })
    }
}

fn band(bandwidth_hz: u64) -> Tuning {
    Tuning {
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(bandwidth_hz),
    }
}

/// A `Running` record on `radio0`, as a node persists one.
fn running(id: &JobId, kind: &str, tuning: Tuning, created_ns: u64) -> JobRecord {
    let graph = match kind {
        "scan" => Graph {
            id: GraphId::new(),
            nodes: vec![],
            edges: vec![],
        },
        _ => Graph {
            id: GraphId::new(),
            nodes: vec![GraphNode {
                id: NodeId("p".into()),
                kind: "passthrough".into(),
                params: serde_json::json!({"kind": "iq"}),
            }],
            edges: vec![],
        },
    };
    JobRecord {
        format: JOB_RECORD_FORMAT,
        id: id.clone(),
        kind: kind.into(),
        graph,
        source: SourceId("radio0".into()),
        tuning,
        state: JobState::Running,
        context: serde_json::json!({}),
        created_ns: Timestamp(created_ns),
        input: None,
        capture_rate_hz: None,
        elastic: false,
        operator_graph: false,
        owner: None,
        audio_policy: None,
        stream: None,
        sinks: Vec::new(),
        handler: None,
        handler_named: false,
        needs: Vec::new(),
        group: None,
        replay: None,
    }
}

fn write_records(dir: &Path, recs: &[JobRecord]) {
    let jobs = dir.join("state").join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    for r in recs {
        std::fs::write(
            jobs.join(format!("{}.json", r.id.0)),
            serde_json::to_vec(r).unwrap(),
        )
        .unwrap();
    }
}

fn record(node: &Node, id: &JobId) -> JobRecord {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .expect("job listed")
}

#[tokio::test]
async fn resume_launches_the_widest_record_first_whatever_the_file_order() {
    // The same two file names in both cases, so whatever order the directory lists them
    // in, one case puts the narrow record first on disk; the narrow one is older in both.
    let (a, b) = (JobId::new(), JobId::new());
    for (narrow, wide) in [(&a, &b), (&b, &a)] {
        let dir = tempfile::tempdir().unwrap();
        write_records(
            dir.path(),
            &[
                running(narrow, "passthrough", band(9_600), 1),
                running(wide, "passthrough", band(48_000), 2),
            ],
        );
        let node = Node::open(cfg(dir.path()), registry(), vec![Radio::new()])
            .await
            .unwrap();
        for id in [narrow, wide] {
            assert_eq!(record(&node, id).state, JobState::Running, "{id:?}");
        }
        let sources = match node.apply(Op::SourceList {}).await.unwrap() {
            OpResult::Sources(v) => v,
            r => panic!("source_list: {r:?}"),
        };
        assert_eq!(sources[0].current.unwrap().bandwidth_hz, Hz(48_000));
    }
}

/// A stepped scan resumes after the rigid records, however wide or narrow, and gives
/// way to them: the demodulator resumes `Running`, and the scan resumes `Running` and
/// waiting for it (`job_context.scan.waiting` naming it), never tuning the radio, until
/// the demodulator stops.
#[tokio::test(flavor = "multi_thread")]
async fn a_scan_resumes_after_the_rigid_records_and_waits_for_them() {
    let (a, b) = (JobId::new(), JobId::new());
    for (demod, scan, width) in [(&a, &b, 40_000_000), (&b, &a, 40_000_000), (&a, &b, 40_000)] {
        let dir = tempfile::tempdir().unwrap();
        write_records(
            dir.path(),
            &[
                running(demod, "passthrough", band(48_000), 1),
                running(scan, "scan", band(width), 2),
            ],
        );
        let radio = Radio::new();
        let node = Node::open(cfg(dir.path()), registry(), vec![radio.clone()])
            .await
            .unwrap();
        assert_eq!(record(&node, demod).state, JobState::Running);
        assert_eq!(record(&node, scan).state, JobState::Running);
        let want = format!("source radio0 is held by passthrough {}", demod.0);
        until(
            "the scan waits for the demodulator",
            Duration::from_secs(10),
            || record(&node, scan).context["scan"]["waiting"] == want.as_str(),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(radio.tunes(), vec![band(48_000)], "the waiting scan tuned");
        node.job_stop(demod).await.unwrap();
        until("the scan runs", Duration::from_secs(10), || {
            record(&node, scan).context["scan"]["waiting"].is_null()
        })
        .await;
        node.job_stop(scan).await.unwrap();
    }
}

/// Narrow rigid jobs that shared one wide capture: resume tunes the source to the band
/// bounding them all, so every one fits again.
#[tokio::test]
async fn resume_restores_a_capture_shared_by_narrow_rigid_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let at = |center_hz: u64| Tuning {
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(48_000),
    };
    let ids = [JobId::new(), JobId::new(), JobId::new()];
    write_records(
        dir.path(),
        &[
            running(&ids[0], "passthrough", at(99_200_000), 1),
            running(&ids[1], "passthrough", at(100_000_000), 2),
            running(&ids[2], "passthrough", at(101_000_000), 3),
        ],
    );
    let node = Node::open(cfg(dir.path()), registry(), vec![Radio::new()])
        .await
        .unwrap();
    for id in &ids {
        assert_eq!(record(&node, id).state, JobState::Running, "{id:?}");
    }
    let sources = match node.apply(Op::SourceList {}).await.unwrap() {
        OpResult::Sources(v) => v,
        r => panic!("source_list: {r:?}"),
    };
    // 99_176_000 ..= 101_024_000 Hz.
    assert_eq!(
        sources[0].current,
        Some(Tuning {
            center_hz: Hz(100_100_000),
            bandwidth_hz: Hz(1_848_000),
        })
    );
    let mut holders = sources[0].holders.clone();
    holders.sort_by(|a, b| a.0.cmp(&b.0));
    let mut want = ids.to_vec();
    want.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(holders, want);
}

#[tokio::test]
async fn job_start_records_the_capture_rate() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source_with(dir.path(), true);
    let node = Node::open(cfg(dir.path()), registry(), vec![port.clone()])
        .await
        .unwrap();
    src.start();
    let op = Op::JobStart {
        kind: JobKind::Passthrough,
        source: SourceId("file0".into()),
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(9_600),
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        bits: None,
        method: None,
        bin_hz: None,
        audio_policy: None,
        dwell_scale: None,
        elastic: false,
        replace: vec![],
        handler: None,
        needs: vec![],
        beside: None,
    };
    let id = match node.apply(op).await.unwrap() {
        OpResult::Job(id) => id,
        r => panic!("job_start: {r:?}"),
    };
    let rate = port.current().unwrap().bandwidth_hz.0;
    assert_eq!(rate, 48_000);
    assert_eq!(on_disk(dir.path(), &id).capture_rate_hz, Some(rate));
}

/// A record resumed against a port that cannot tune (a device that is not there) fails
/// with the port's error under the source's name, as a job that failed on the faulted
/// source reads, so the node's recovery can tell it from any other failure.
#[cfg(feature = "test-fixtures")]
#[tokio::test]
async fn a_record_resumed_against_a_faulted_port_fails_naming_the_source() {
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
    port.set_fault(Some("device not found: x".into()));
    let id = JobId::new();
    let mut rec = running(&id, "passthrough", band(48_000), 1);
    rec.source = SourceId("s".into());
    write_records(dir.path(), &[rec]);
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    let want = JobState::Failed {
        reason: "source s: io: device not found: x".into(),
    };
    assert_eq!(record(&node, &id).state, want);
    assert_eq!(on_disk(dir.path(), &id).state, want);
}

/// A scan persisted before the scan carried a mask (an empty graph) gets the expansion
/// and its source's default mask on resume.
#[tokio::test]
async fn a_scan_record_with_an_empty_graph_resumes_with_the_default_mask() {
    let scan = JobId::new();
    let dir = tempfile::tempdir().unwrap();
    write_records(dir.path(), &[running(&scan, "scan", band(40_000_000), 1)]);
    let node = Node::open(cfg(dir.path()), registry(), vec![Radio::new()])
        .await
        .unwrap();
    assert_eq!(record(&node, &scan).state, JobState::Running);
    let g = &record(&node, &scan).graph;
    assert_eq!(g.nodes.len(), 1);
    assert_eq!(g.nodes[0].kind, "fft");
    assert_eq!(g.nodes[0].params["notch_hz"], 50_000);
    // The radio's whole capture is usable: no edges to mask.
    assert_eq!(g.nodes[0].params["edge_mask"], 0.0);
    let ctx = node.job_context(&scan).unwrap();
    assert_eq!(
        ctx["scan"]["mask"],
        serde_json::json!({"notch_hz": 50_000, "edge_mask": 0.0})
    );
    assert!(ctx["scan"]["alternate"].is_object(), "{ctx}");
    node.job_stop(&scan).await.unwrap();
}

/// A `Running` scan record that ran as the source's own sweep, as a node persists one
/// (its `fft` node as yet unexpanded, its `job_context.scan.method` `source`).
fn self_sweep_record(id: &JobId, created_ns: u64) -> JobRecord {
    let mut rec = running(id, "scan", band(40_000_000), created_ns);
    rec.context = serde_json::json!({"scan": {"method": "source", "steps": 4, "waiting": null}});
    rec
}

/// Wait up to `within` for a frame on job `id`'s event stream.
async fn a_row(node: &Node, id: &JobId, within: Duration) {
    let stream = node
        .bus()
        .list()
        .into_iter()
        .find(|s| s.kind == StreamKind::Event && s.provenance.job.as_ref() == Some(id))
        .expect("the scan has an event stream");
    let mut rx = node.bus().subscribe(&stream.id).unwrap();
    loop {
        match tokio::time::timeout(within, rx.recv()).await {
            Ok(Ok(_)) => return,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            other => panic!("no row: {other:?}"),
        }
    }
}

/// Poll `f` every 20 ms until it holds, panicking with `what` after `within`.
async fn until(what: &str, within: Duration, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A scan stored as `method: "source"` resumes as the source's own sweep, its start
/// slow (300 ms): rows come, the radio is never tuned and the scan holds no lease.
#[tokio::test(flavor = "multi_thread")]
async fn a_stored_self_sweep_resumes_as_one() {
    let dir = tempfile::tempdir().unwrap();
    let scan = JobId::new();
    write_records(dir.path(), &[self_sweep_record(&scan, 1)]);
    let radio = Radio::sweeping(None, 300);
    let node = Node::open(cfg(dir.path()), registry(), vec![radio.clone()])
        .await
        .unwrap();
    assert_eq!(record(&node, &scan).state, JobState::Running);
    let ctx = node.job_context(&scan).unwrap();
    assert_eq!(ctx["scan"]["method"], "source", "{ctx}");
    assert!(ctx["scan"]["waiting"].is_null(), "{ctx}");
    a_row(&node, &scan, Duration::from_secs(10)).await;
    assert!(radio.tunes().is_empty(), "{:?}", radio.tunes());
    assert!(node.lease_holders(&SourceId("radio0".into())).is_empty());
    node.job_stop(&scan).await.unwrap();
}

/// A scan stored as `method: "source"` whose source refuses its sweep on the new
/// node never steps: it runs, waiting with the reason, no tune of its own is made,
/// and the rigid jobs resumed beside it get their shared capture back and run.
#[tokio::test(flavor = "multi_thread")]
async fn a_stored_self_sweep_refused_at_resume_waits_and_never_steps() {
    let dir = tempfile::tempdir().unwrap();
    let scan = JobId::new();
    let at = |center_hz: u64| Tuning {
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(48_000),
    };
    let rigid = [JobId::new(), JobId::new()];
    write_records(
        dir.path(),
        &[
            self_sweep_record(&scan, 1),
            running(&rigid[0], "passthrough", at(99_200_000), 2),
            running(&rigid[1], "passthrough", at(101_000_000), 3),
        ],
    );
    let radio = Radio::sweeping(Some("a sweep is already running"), 0);
    let node = Node::open(cfg(dir.path()), registry(), vec![radio.clone()])
        .await
        .unwrap();
    assert_eq!(record(&node, &scan).state, JobState::Running);
    let ctx = node.job_context(&scan).unwrap();
    assert_eq!(ctx["scan"]["method"], "source", "{ctx}");
    assert_eq!(
        ctx["scan"]["waiting"], "a sweep is already running",
        "{ctx}"
    );
    for id in &rigid {
        assert_eq!(record(&node, id).state, JobState::Running, "{id:?}");
    }
    // The one tune is the rigid jobs' shared capture: 99_176_000 ..= 101_024_000 Hz.
    let capture = Tuning {
        center_hz: Hz(100_100_000),
        bandwidth_hz: Hz(1_848_000),
    };
    assert_eq!(radio.tunes(), vec![capture]);
    let mut holders = node.lease_holders(&SourceId("radio0".into()));
    holders.sort_by(|a, b| a.0.0.cmp(&b.0.0));
    let mut want = rigid.to_vec();
    want.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        holders.into_iter().map(|(j, _)| j).collect::<Vec<_>>(),
        want
    );
    // A retry a second in is refused again: still waiting, still no step.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    until("the scan still waits", Duration::from_secs(5), || {
        node.job_context(&scan).unwrap()["scan"]["waiting"] == "a sweep is already running"
    })
    .await;
    assert_eq!(record(&node, &scan).state, JobState::Running);
    assert_eq!(radio.tunes(), vec![capture]);
    node.job_stop(&scan).await.unwrap();
}

/// A scan stored with `method: "source"` in its `fft` node, its source refusing at
/// resume, waits as one stored without a method does (instead of failing as a first
/// start with it would): it runs, waiting with the reason, never tuned, and once the
/// source grants the sweep its rows come and `waiting` is `null`.
#[tokio::test(flavor = "multi_thread")]
async fn a_stored_explicit_source_scan_refused_at_resume_waits_then_runs() {
    let dir = tempfile::tempdir().unwrap();
    let scan = JobId::new();
    let mut rec = self_sweep_record(&scan, 1);
    rec.graph.nodes = vec![GraphNode {
        id: NodeId("fft".into()),
        kind: "fft".into(),
        params: serde_json::json!({"method": "source"}),
    }];
    write_records(dir.path(), &[rec]);
    let radio = Radio::sweeping(Some("a sweep is already running"), 0);
    let node = Node::open(cfg(dir.path()), registry(), vec![radio.clone()])
        .await
        .unwrap();
    assert_eq!(record(&node, &scan).state, JobState::Running);
    assert_eq!(
        record(&node, &scan).graph.nodes[0].params["method"],
        "source"
    );
    let ctx = node.job_context(&scan).unwrap();
    assert_eq!(ctx["scan"]["method"], "source", "{ctx}");
    assert_eq!(
        ctx["scan"]["waiting"], "a sweep is already running",
        "{ctx}"
    );
    assert!(ctx["scan"].get("steps").is_none(), "{ctx}");
    radio.set_refuse(None);
    until("the scan stops waiting", Duration::from_secs(20), || {
        node.job_context(&scan).unwrap()["scan"]["waiting"].is_null()
    })
    .await;
    a_row(&node, &scan, Duration::from_secs(10)).await;
    let ctx = node.job_context(&scan).unwrap();
    assert_eq!(ctx["scan"]["steps"], 4, "{ctx}");
    assert_eq!(record(&node, &scan).state, JobState::Running);
    assert!(radio.tunes().is_empty(), "{:?}", radio.tunes());
    assert!(node.lease_holders(&SourceId("radio0".into())).is_empty());
    node.job_stop(&scan).await.unwrap();
}

/// A stand-in decoder publishing one AX.25 packet per IQ block, and the real
/// `stream_in` and `packet_capture` a `packets` attachment runs.
fn packets_registry() -> StageRegistry {
    let mut r = registry();
    r.register(Arc::new(zeta_core::stream_in::StreamInStage::new()));
    r.register(Arc::new(
        zeta_core::packet_capture::PacketCaptureStage::new(),
    ));
    r.register(common::MapStage::new(
        "packet_src",
        StreamKind::Iq,
        &[StreamKind::Packet],
        |_| {
            vec![(
                0,
                zeta_core::stream::Payload::Packet {
                    link_type: zeta_core::stream::LinkType::Ax25Kiss,
                    data: Arc::from(vec![0u8, 0x82, 0xa0]),
                },
            )]
        },
    ));
    r
}

/// A `packets` job attached to `job`, on the graph `job_attach` expands (the stand-in
/// decoder's kind names no row, which the op would refuse as unreadable).
async fn attach_packets(node: &Node, job: &JobId) -> JobId {
    let stream = node.job_packet_stream(job).unwrap();
    let graph = JobKind::Packets
        .expand_attached(&AttachTo::new(job, &stream), "en")
        .unwrap();
    node.job_attach(JobKind::Packets.name(), job, None, graph)
        .await
        .unwrap()
}

/// `packets_list`, by its wire form.
async fn packets_list(node: &Node) -> Vec<serde_json::Value> {
    let op: Op = serde_json::from_value(serde_json::json!({"op": "packets_list"})).unwrap();
    let r = node.apply(op).await.expect("packets_list");
    let v = serde_json::to_value(r).unwrap();
    assert_eq!(v["result"], "packet_captures", "{v}");
    v["value"].as_array().unwrap().clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_packets_attachment_does_not_resume() {
    let dir = tempfile::tempdir().unwrap();
    let meta = fixtures::write_tone(dir.path());
    let opts = FileOpts {
        realtime: true,
        block_samples: 4800,
        repeat: true,
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(SourceId("file0".into()), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    let node = Node::open(cfg(dir.path()), packets_registry(), vec![port.clone()])
        .await
        .unwrap();
    // The file source plays only while subscribed: keep it playing between nodes.
    let _hold = port.subscribe();
    src.start();
    let input = node
        .job_start(
            "pkt",
            SourceId("file0".into()),
            band(48_000),
            Graph {
                id: GraphId::new(),
                nodes: vec![GraphNode {
                    id: NodeId("n".into()),
                    kind: "packet_src".into(),
                    params: serde_json::json!({}),
                }],
                edges: vec![],
            },
            None,
            false,
        )
        .await
        .unwrap();
    let att = attach_packets(&node, &input).await;
    until("a packet captured", Duration::from_secs(10), || {
        node.packets().list().first().is_some_and(|r| r.packets > 0)
    })
    .await;
    let capture = node.packets().list()[0].id.clone();
    drop(node);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let node = Node::open(cfg(dir.path()), packets_registry(), vec![port])
        .await
        .unwrap();
    assert_eq!(record(&node, &input).state, JobState::Running);
    let rec = record(&node, &att);
    assert_eq!(rec.state, JobState::Stopped);
    assert_eq!(
        rec.context["note"], "packet capture does not resume: attach again",
        "{}",
        rec.context
    );
    assert_eq!(on_disk(dir.path(), &att).state, JobState::Stopped);
    let listed = packets_list(&node).await;
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["id"], capture);
    assert_eq!(listed[0]["writing"], false);
    // A fresh attach starts a new capture.
    let again = attach_packets(&node, &input).await;
    assert_ne!(again, att);
    until("a fresh capture", Duration::from_secs(10), || {
        node.packets()
            .list()
            .iter()
            .any(|r| r.id != capture && r.packets > 0)
    })
    .await;
}

#[tokio::test]
async fn a_replay_found_running_is_stopped_with_a_note_and_not_resumed() {
    let dir = tempfile::tempdir().unwrap();
    let id = JobId::new();
    let rec = JobRecord {
        source: SourceId("node".into()),
        operator_graph: true,
        replay: Some("01CAPTURE".into()),
        context: serde_json::json!({"replay": {"capture": "01CAPTURE", "captures": ["01CAPTURE"]}}),
        ..running(&id, "passthrough", band(48_000), 1)
    };
    write_records(dir.path(), &[rec]);
    let (_src, port) = source_with(dir.path(), true);
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    let rec = record(&node, &id);
    assert_eq!(rec.state, JobState::Stopped);
    assert_eq!(
        rec.context["note"], "a replay does not resume: run it again",
        "{}",
        rec.context
    );
    // What the replay noted at its start is kept.
    assert_eq!(rec.context["replay"]["capture"], "01CAPTURE");
    assert_eq!(on_disk(dir.path(), &id).state, JobState::Stopped);
    assert!(
        node.bus()
            .list()
            .iter()
            .all(|s| s.provenance.job.as_ref() != Some(&id)),
        "no stream of the replay"
    );
}

// Plan 42: a relaunch keeps the job's handler, or decides it again.

use common::handlers::{self as h, ALT, MODE, OWN};

/// Open a node on `dir` with the `alt` handler file, alt's program missing when
/// `alt_missing`.
async fn handler_node(dir: &Path, alt_missing: bool) -> Node {
    let cfg = h::cfg(dir, false);
    let modes = h::registry(&h::stages(None), &cfg, alt_missing);
    let ports: Vec<Arc<dyn Port>> = vec![h::Radio::new()];
    Node::open_with_modes(cfg, h::stages(None), ports, Some(Arc::new(modes)))
        .await
        .unwrap()
}

/// The params of the job's `external_decode` node.
fn decode_params(rec: &JobRecord) -> serde_json::Value {
    rec.graph
        .nodes
        .iter()
        .find(|n| n.kind == "external_decode")
        .expect("an external_decode node")
        .params
        .clone()
}

#[tokio::test]
async fn a_chosen_handler_gone_at_restart_is_chosen_again_and_noted() {
    let dir = tempfile::tempdir().unwrap();
    let node = handler_node(dir.path(), false).await;
    let ranked = node
        .apply(Op::HandlerPolicySet {
            mode: Some(zeta_core::modes::ModeId(MODE.into())),
            order: vec![ALT.into()],
        })
        .await;
    assert!(ranked.is_ok(), "{ranked:?}");
    let id = h::start(&node, None, &[]).await;
    let rec = record(&node, &id);
    assert_eq!(
        (rec.handler.as_deref(), rec.handler_named),
        (Some(ALT), false)
    );
    drop(node);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let node = handler_node(dir.path(), true).await;
    let rec = record(&node, &id);
    assert_eq!(rec.state, JobState::Running, "{:?}", rec.state);
    assert_eq!(
        (rec.handler.as_deref(), rec.handler_named),
        (Some(OWN), false)
    );
    assert_eq!(
        rec.context["handler"],
        serde_json::json!({"was": ALT, "now": OWN, "why": h::ALT_MISSING})
    );
    assert_eq!(
        decode_params(&rec),
        serde_json::json!({"mode": MODE, "bandwidth_hz": 2_400_000})
    );
    let disk = on_disk(dir.path(), &id);
    assert_eq!(disk.handler.as_deref(), Some(OWN));
    assert_eq!(disk.context["handler"]["now"], OWN);
}

// Plan 43: a Send to group never holds one handler twice.

/// A job sent from (its handler chosen, the row's own) and a sibling beside it on alt,
/// on a node opened on `dir`; the two ids.
async fn sent_from(dir: &Path) -> (JobId, JobId) {
    let node = handler_node(dir, false).await;
    let first = h::start(&node, None, &[]).await;
    let rec = record(&node, &first);
    assert_eq!(
        (rec.handler.as_deref(), rec.handler_named),
        (Some(OWN), false)
    );
    let sibling = match h::start_beside(&node, &first, Some(ALT), &[]).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("a sibling beside: {r:?}"),
    };
    drop(node);
    tokio::time::sleep(Duration::from_millis(50)).await;
    (first, sibling)
}

/// The first job's own handler gone at restart: it is chosen again, but alt runs
/// beside it as its sibling, so it is passed over and the first fails saying so; the
/// sibling resumes on alt.
#[tokio::test]
async fn a_rechosen_member_never_takes_its_siblings_handler() {
    let dir = tempfile::tempdir().unwrap();
    let (first, sibling) = sent_from(dir.path()).await;

    let cfg = h::cfg(dir.path(), false);
    let mut modes = h::registry(&h::stages(None), &cfg, false);
    h::own_missing(&mut modes);
    let ports: Vec<Arc<dyn Port>> = vec![h::Radio::new()];
    let node = Node::open_with_modes(cfg, h::stages(None), ports, Some(Arc::new(modes)))
        .await
        .unwrap();
    common::until("the first is failed", Duration::from_secs(10), || {
        matches!(record(&node, &first).state, JobState::Failed { .. })
    })
    .await;
    let rec = record(&node, &first);
    let JobState::Failed { reason } = &rec.state else {
        unreachable!()
    };
    assert!(
        reason.contains(&format!("{ALT}: runs beside it as job {}", sibling.0)),
        "{reason}"
    );
    assert!(reason.contains(h::OWN_MISSING), "{reason}");
    let sib = record(&node, &sibling);
    assert_eq!(sib.state, JobState::Running, "{:?}", sib.state);
    assert_eq!(sib.handler.as_deref(), Some(ALT));
}

/// Plan 46: a relaunch whose sibling holds the row's own handler still ranks the rest
/// against the row's own outputs. `alt` (events and packets) is chosen by the mode's
/// ranking, its sibling runs the row's own (events); at restart alt is gone and the
/// node-wide ranking names `alt3`, which gives as much as the row's own (but less than
/// alt): it is chosen, not `alt2` by handler order.
#[tokio::test]
async fn a_relaunch_beside_the_rows_own_ranks_against_the_rows_own_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = || {
        h::write_handler(dir.path(), "alt2", false);
        h::write_handler(dir.path(), "alt3", false);
        h::cfg(dir.path(), true)
    };
    let open = |alt_missing: bool| {
        let cfg = cfg();
        let modes = h::registry(&h::stages(None), &cfg, alt_missing);
        let ports: Vec<Arc<dyn Port>> = vec![h::Radio::new()];
        Node::open_with_modes(cfg, h::stages(None), ports, Some(Arc::new(modes)))
    };
    let node = open(false).await.unwrap();
    for (mode, order) in [(Some(MODE), ALT), (None, "alt3")] {
        let ranked = node
            .apply(Op::HandlerPolicySet {
                mode: mode.map(|m| zeta_core::modes::ModeId(m.into())),
                order: vec![order.into()],
            })
            .await;
        assert!(ranked.is_ok(), "{ranked:?}");
    }
    let first = h::start(&node, None, &[]).await;
    assert_eq!(record(&node, &first).handler.as_deref(), Some(ALT));
    let sibling = match h::start_beside(&node, &first, Some(OWN), &[]).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("a sibling beside: {r:?}"),
    };
    drop(node);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let node = open(true).await.unwrap();
    let rec = record(&node, &first);
    assert_eq!(rec.state, JobState::Running, "{:?}", rec.state);
    assert_eq!(rec.handler.as_deref(), Some("alt3"));
    let sib = record(&node, &sibling);
    assert_eq!(sib.handler.as_deref(), Some(OWN));
}

/// With both handlers there at restart, each member resumes on its own handler and
/// stays in the group.
#[tokio::test]
async fn a_group_resumes_member_by_member() {
    let dir = tempfile::tempdir().unwrap();
    let (first, sibling) = sent_from(dir.path()).await;

    let node = handler_node(dir.path(), false).await;
    for (id, handler) in [(&first, OWN), (&sibling, ALT)] {
        let rec = record(&node, id);
        assert_eq!(rec.state, JobState::Running, "{id:?}: {:?}", rec.state);
        assert_eq!(rec.handler.as_deref(), Some(handler));
        assert_eq!(rec.group.as_ref(), Some(&first));
    }
}

#[tokio::test]
async fn a_named_handler_gone_at_restart_fails_the_job() {
    let dir = tempfile::tempdir().unwrap();
    let node = handler_node(dir.path(), false).await;
    let id = h::start(&node, Some(ALT), &[]).await;
    assert!(record(&node, &id).handler_named);
    drop(node);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let node = handler_node(dir.path(), true).await;
    let rec = record(&node, &id);
    assert_eq!(
        rec.state,
        JobState::Failed {
            reason: format!("handler {ALT} is unavailable: {}", h::ALT_MISSING),
        }
    );
    assert_eq!(rec.handler.as_deref(), Some(ALT));
    assert_eq!(on_disk(dir.path(), &id).state, rec.state);
}

/// A chosen handler gone at restart is chosen again for what the start needed: with
/// `needs: ["packet"]` and no other handler giving packets, the job fails naming why
/// each was passed over, rather than running without them.
#[tokio::test]
async fn a_rechosen_handler_still_satisfies_the_starts_needs() {
    let dir = tempfile::tempdir().unwrap();
    // `alt` gives packets, `alt2` does not; the row's own gives none.
    let cfg = || NodeConfig {
        handlers_dir: Some(h::write_handler(dir.path(), "alt2", false)),
        ..h::cfg(dir.path(), true)
    };
    let open = |alt_missing: bool| {
        let cfg = cfg();
        let modes = h::registry(&h::stages(None), &cfg, alt_missing);
        let ports: Vec<Arc<dyn Port>> = vec![h::Radio::new()];
        Node::open_with_modes(cfg, h::stages(None), ports, Some(Arc::new(modes)))
    };
    let node = open(false).await.unwrap();
    let ranked = node
        .apply(Op::HandlerPolicySet {
            mode: Some(zeta_core::modes::ModeId(MODE.into())),
            order: vec![ALT.into(), "alt2".into()],
        })
        .await;
    assert!(ranked.is_ok(), "{ranked:?}");
    let id = h::start(&node, None, &[StreamKind::Packet]).await;
    let rec = record(&node, &id);
    assert_eq!(
        (rec.handler.as_deref(), rec.handler_named),
        (Some(ALT), false)
    );
    assert_eq!(rec.needs, [StreamKind::Packet]);
    drop(node);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let node = open(true).await.unwrap();
    let rec = record(&node, &id);
    let JobState::Failed { reason } = &rec.state else {
        panic!("expected failed, got {:?}", rec.state)
    };
    assert!(reason.contains("alt2: outputs no packet"), "{reason}");
    assert!(
        reason.contains(&format!("{OWN}: outputs no packet")),
        "{reason}"
    );
    assert!(reason.contains(h::ALT_MISSING), "{reason}");
}

/// Plan 47: a job of a row with no decoder of its own, started through `alt`, whose
/// handler file is gone at restart: the node starts, and the job fails with the row's
/// reason (there is no decoder of the row's own to fall back on).
#[tokio::test]
async fn a_stored_job_of_a_row_whose_handler_is_gone_fails_with_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let open = || {
        let ports: Vec<Arc<dyn Port>> = vec![h::Radio::new()];
        // `nodec_cfg` rewrites the files: a node with the handler, then without it.
        Node::open(h::nodec_cfg(dir.path(), Some(true)), h::stages(None), ports)
    };
    let node = open().await.unwrap();
    let id = match node.apply(h::nodec_op(None)).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("nodec_decode start: {r:?}"),
    };
    assert_eq!(record(&node, &id).handler.as_deref(), Some(ALT));
    drop(node);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let ports: Vec<Arc<dyn Port>> = vec![h::Radio::new()];
    let node = Node::open(h::nodec_cfg(dir.path(), None), h::stages(None), ports)
        .await
        .expect("the node starts");
    let rec = record(&node, &id);
    assert_eq!(
        rec.state,
        JobState::Failed {
            reason: format!("mode {} is unavailable: {}", h::NODEC, h::NO_DECODER_REASON),
        }
    );
    assert_eq!(rec.handler.as_deref(), Some(ALT));
    assert_eq!(on_disk(dir.path(), &id).state, rec.state);
}
