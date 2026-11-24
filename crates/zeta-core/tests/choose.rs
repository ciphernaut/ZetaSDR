//! The `choose` job (design 5.3): step 1 answers at once from the ring, as `classify`
//! does; step 2 writes the ring slice into the burst store and replays it through the
//! decoder rows that could prove otherwise. Each test runs a node over a synthesised
//! capture played in real time into a 10 s ring.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use num_complex::Complex;
use serde_json::Value;
use tokio::sync::broadcast;
use zeta_core::capture::{CaptureMeta, write_cf32};
use zeta_core::job::JobState;
use zeta_core::node::Node;
use zeta_core::ops::{Op, OpError, OpResult};
use zeta_core::port::Port;
use zeta_core::stage::StageRegistry;
use zeta_core::stream::{Frame, Payload};
use zeta_core::types::{Hz, JobId, SourceId, Timestamp};
use zeta_dsp::synth::{awgn, nbfm_modulate};
use zeta_source_file::{FileOpts, FileSource};

use common::until;

const RATE_HZ: u32 = 48_000;
/// The DMR fixture's centre, inside the catalogue's DMR range.
const DMR_HZ: u64 = 439_100_000;
/// How long a test waits for a `choose` job to end.
const ENDS_WITHIN: Duration = Duration::from_secs(40);

fn rx() -> SourceId {
    SourceId("rx".into())
}

/// Six superframes of a DMR call, about 2.2 s, under 25 dB of noise: layer 1 reads it as
/// FM, and only the DMR decoder proves it.
fn dmr_iq() -> Vec<Complex<f32>> {
    let params = serde_json::json!({
        "family": "dmr", "color_code": 5, "src": 9990001, "dst": 9990, "superframes": 6
    });
    let (spec, imp) = zeta_dsp::synth::parse_params(params.as_object().unwrap().clone()).unwrap();
    let mut iq = zeta_dsp::synth::synthesise(&spec, &imp, RATE_HZ, DMR_HZ);
    awgn(&mut iq, 25.0, 13);
    iq
}

/// One second-scale burst of three tones under a tremolo, then silence: voice enough
/// for layer 1 (as `tests/choice.rs` makes it).
fn speech_like(rate_hz: u32, seconds: f32) -> Vec<f32> {
    use std::f64::consts::PI;
    let len = (rate_hz as f64 * seconds as f64) as usize;
    let period = rate_hz as usize * 600 / 1000;
    let burst = rate_hz as usize * 300 / 1000;
    (0..len)
        .map(|n| {
            if n % period >= burst {
                return 0.0;
            }
            let t = n as f64 / rate_hz as f64;
            let tones: f64 = [220.0, 440.0, 1320.0]
                .iter()
                .map(|f| (2.0 * PI * f * t).sin())
                .sum::<f64>()
                / 3.0;
            let tremolo = 0.75 + 0.25 * (2.0 * PI * 4.0 * t).sin();
            (0.5 * tremolo * tones) as f32
        })
        .collect()
}

/// A node over `iq` centred on `center_hz`, looped in real time into a ring of
/// `ring_seconds`, with the DSP stages and classifiers, once the ring holds 2.5 s (or
/// nearly all of a shorter ring).
async fn node_over(dir: &Path, iq: &[Complex<f32>], center_hz: u64, ring_seconds: f64) -> Node {
    node_with(dir, iq, center_hz, ring_seconds, u64::MAX).await
}

/// [`node_over`] with a burst store whose trial quota is `trial_keep_bytes`.
async fn node_with(
    dir: &Path,
    iq: &[Complex<f32>],
    center_hz: u64,
    ring_seconds: f64,
    trial_keep_bytes: u64,
) -> Node {
    let meta = CaptureMeta {
        sample_rate_hz: Hz(RATE_HZ as u64),
        center_hz: Hz(center_hz),
        start: Timestamp(0),
        annotations: Vec::new(),
    };
    let meta = write_cf32(dir, "fixture", &meta, iq).expect("write fixture");
    let opts = FileOpts {
        realtime: true,
        block_samples: (RATE_HZ / 10) as usize,
        repeat: true,
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(rx(), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    let mut reg = StageRegistry::new();
    zeta_dsp::register_all(&mut reg);
    let mut cfg = common::cfg(dir, true);
    cfg.ring_seconds = ring_seconds;
    if trial_keep_bytes != u64::MAX {
        cfg.burst_policy.trial_keep_bytes = trial_keep_bytes;
    }
    let node = Node::open(cfg, reg, vec![port]).await.unwrap();
    for c in zeta_dsp::classifiers(node.modes_handle()) {
        node.add_classifier(c);
    }
    src.start();
    let want = (ring_seconds * 0.9).min(2.5);
    until("the ring fills", Duration::from_secs(10), || {
        node.ring(&rx())
            .is_some_and(|r| r.read().unwrap().held_s() >= want)
    })
    .await;
    node
}

/// A `job_start` of `choose` on `rx` at `center_hz`, `bandwidth_hz` wide, with `choose`.
fn choose_op(center_hz: u64, bandwidth_hz: u64, choose: Value) -> Op {
    serde_json::from_value(serde_json::json!({
        "op": "job_start", "kind": "choose", "source": "rx",
        "center_hz": center_hz, "bandwidth_hz": bandwidth_hz,
        "deviation_hz": null, "sideband": null, "elastic": false, "replace": [],
        "choose": choose,
    }))
    .expect("a job_start op")
}

/// The source's feed, subscribed before anything starts: the `choose` job's events are on it.
fn feed(node: &Node) -> broadcast::Receiver<Frame> {
    let bus = node.bus();
    let feed = bus.feed_of(&rx()).expect("the source's feed");
    bus.subscribe(&feed).expect("subscribe to the feed")
}

/// Start the `choose` job `op` asks for.
async fn start(node: &Node, op: Op) -> JobId {
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("choose start: {r:?}"),
    }
}

fn state(node: &Node, id: &JobId) -> Option<JobState> {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .map(|r| r.state)
}

/// Wait for the job to end `stopped`, and answer its `choose` context.
async fn ended(node: &Node, id: &JobId) -> Value {
    until("the choose job ends", ENDS_WITHIN, || {
        state(node, id).is_some_and(|s| s != JobState::Running)
    })
    .await;
    assert_eq!(state(node, id), Some(JobState::Stopped));
    let ctx = node.job_context(id).expect("the job's context");
    eprintln!("context {ctx:#}");
    ctx["choose"].clone()
}

/// The events the job `id` published on the feed so far, in order.
fn events(rx: &mut broadcast::Receiver<Frame>, id: &JobId) -> Vec<Value> {
    let mut out = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(f) => {
                if f.provenance.job.as_ref() == Some(id)
                    && let Payload::Event(v) = f.payload
                {
                    out.push(v);
                }
            }
            Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(_) => return out,
        }
    }
}

/// The trial captures in the burst store.
fn trials(node: &Node) -> Vec<zeta_core::bursts::CaptureRecord> {
    node.bursts()
        .list()
        .into_iter()
        .filter(|r| r.trial)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn choose_revises_fm_to_dmr_at_step_two() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &dmr_iq(), DMR_HZ, 10.0).await;
    let mut feed = feed(&node);
    let id = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 2})),
    )
    .await;
    let ctx = ended(&node, &id).await;
    let ev = events(&mut feed, &id);
    let protocols: Vec<&str> = ev.iter().filter_map(|e| e["protocol"].as_str()).collect();
    assert_eq!(protocols, ["choice", "choice", "verdict"], "{ev:#?}");
    // Step 1: layer 1 hears FM, and the DMR row could prove otherwise.
    let first = &ev[0];
    assert_eq!(first["step"], 1, "{first}");
    assert_eq!(first["choice"]["row"], "fm_demod", "{first}");
    assert_eq!(first["choice"]["settled"], "best_guess", "{first}");
    let could: Vec<&str> = first["choice"]["could_prove"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(could.contains(&"dmr_decode"), "{first}");
    // Step 2: the replay proves DMR.
    let second = &ev[1];
    assert_eq!(second["step"], 2, "{second}");
    let c = &second["choice"];
    assert_eq!(c["row"], "dmr_decode", "{c}");
    assert_eq!(c["step"], 2, "{c}");
    assert_eq!(c["settled"], "proved", "{c}");
    assert_eq!(c["job"]["kind"], "dmr_decode", "{c}");
    assert!(
        c["evidence"].as_array().unwrap().iter().any(|e| e
            .as_str()
            .is_some_and(|e| e.starts_with("decode dmr_decode: "))),
        "{c}"
    );
    // What step 1 answered is the runner-up.
    assert_eq!(c["runner_up"]["row"], "fm_demod", "{c}");
    // The verdict carries the final choice, and so does the context.
    assert_eq!(ev[2]["choice"], *c, "{:#}", ev[2]);
    assert_eq!(ctx["verdict"], *c, "{ctx:#}");
    assert_eq!(ctx["choice"], *c, "{ctx:#}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn choose_on_voice_stays_fm_demod() {
    let dir = tempfile::tempdir().unwrap();
    // A 5 kHz FM voice on the DMR fixture's frequency: as wide as the digital voice
    // rows, so step 2 has rows to try.
    let mut iq = nbfm_modulate(&speech_like(RATE_HZ, 3.0), RATE_HZ, RATE_HZ, 5_000.0, 0.0);
    awgn(&mut iq, 25.0, 5);
    let node = node_over(dir.path(), &iq, DMR_HZ, 10.0).await;
    let mut feed = feed(&node);
    let id = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 2})),
    )
    .await;
    let ctx = ended(&node, &id).await;
    let ev = events(&mut feed, &id);
    let protocols: Vec<&str> = ev.iter().filter_map(|e| e["protocol"].as_str()).collect();
    // No trial changed the answer: one choice, then the verdict.
    assert_eq!(protocols, ["choice", "verdict"], "{ev:#?}");
    let v = &ctx["verdict"];
    assert_eq!(v["row"], "fm_demod", "{v}");
    assert_eq!(v["settled"], "best_guess", "{v}");
    // Step 2 tried every row that could prove otherwise.
    let could: Vec<&str> = v["could_prove"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(!could.is_empty(), "{v}");
    let tried: Vec<&str> = ctx["trials"]
        .as_array()
        .expect("the trials are recorded")
        .iter()
        .filter_map(|t| t["row"].as_str())
        .collect();
    for row in &could {
        assert!(tried.contains(row), "{row} not tried: {ctx:#}");
    }
    for t in ctx["trials"].as_array().unwrap() {
        assert_ne!(t["outcome"], "proved", "{t}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn choose_up_to_one_never_writes_a_slice() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &dmr_iq(), DMR_HZ, 10.0).await;
    let id = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 1})),
    )
    .await;
    let ctx = ended(&node, &id).await;
    let v = &ctx["verdict"];
    assert_eq!(v["row"], "fm_demod", "{v}");
    assert_eq!(v["step"], 1, "{v}");
    assert!(trials(&node).is_empty(), "{:?}", node.bursts().list());
    assert!(
        node.bursts().list().is_empty(),
        "{:?}",
        node.bursts().list()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_short_ring_skips_step_two_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &dmr_iq(), DMR_HZ, 0.5).await;
    let id = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 2})),
    )
    .await;
    let ctx = ended(&node, &id).await;
    let v = &ctx["verdict"];
    assert_eq!(v["step"], 1, "{v}");
    let reasons: Vec<&str> = v["not_tried"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["reason"].as_str())
        .collect();
    assert!(
        reasons.contains(&"not tried: the ring holds under 1 s"),
        "{v}"
    );
    assert!(trials(&node).is_empty(), "{:?}", node.bursts().list());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_true_starts_the_verdict_and_ends() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &dmr_iq(), DMR_HZ, 10.0).await;
    let id = start(
        &node,
        choose_op(
            DMR_HZ,
            20_000,
            serde_json::json!({"up_to": 2, "start": true}),
        ),
    )
    .await;
    let ctx = ended(&node, &id).await;
    let job = &ctx["verdict"]["job"];
    assert_eq!(job["kind"], "dmr_decode", "{ctx:#}");
    let center = job["center_hz"].as_u64().unwrap();
    assert!(center.abs_diff(DMR_HZ) <= 2_000, "{job}");
    let running: Vec<_> = node
        .job_list()
        .into_iter()
        .filter(|r| r.state == JobState::Running && r.kind == "dmr_decode")
        .collect();
    assert_eq!(running.len(), 1, "{running:?}");
    let r = &running[0];
    assert_eq!(r.source, rx());
    assert_eq!(r.tuning.center_hz, Hz(center), "{r:?}");
    assert_eq!(r.replay, None, "{r:?}");
    // The context names the job it started.
    assert_eq!(ctx["started"], r.id.0.as_str(), "{ctx:#}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_trial_slice_is_unpinned_when_choose_ends() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &dmr_iq(), DMR_HZ, 10.0).await;
    let id = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 2})),
    )
    .await;
    // Pinned while its ladder runs.
    until("the trial slice is pinned", ENDS_WITHIN, || {
        trials(&node).iter().any(|r| r.pinned)
    })
    .await;
    let _ = ended(&node, &id).await;
    let t = trials(&node);
    assert_eq!(t.len(), 1, "{t:?}");
    assert!(!t[0].pinned, "{t:?}");
    // The slice is the job's band of the source.
    assert_eq!(t[0].source, rx());
    assert_eq!(t[0].center_hz, Hz(DMR_HZ));
    assert_eq!(t[0].bandwidth_hz, Hz(20_000));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn choose_holds_no_lease_and_stops_no_job() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &dmr_iq(), DMR_HZ, 10.0).await;
    let fm = start_fm(&node).await;
    let before = node.lease_holders(&rx());
    assert_eq!(
        before.iter().map(|(j, _)| j).collect::<Vec<_>>(),
        [&fm],
        "{before:?}"
    );
    let id = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 2})),
    )
    .await;
    // Never a holder while it runs.
    let deadline = tokio::time::Instant::now() + ENDS_WITHIN;
    while state(&node, &id) == Some(JobState::Running) {
        assert_eq!(node.lease_holders(&rx()), before);
        assert!(tokio::time::Instant::now() < deadline, "choose never ended");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let ctx = ended(&node, &id).await;
    assert_eq!(ctx["verdict"]["row"], "dmr_decode", "{ctx:#}");
    assert_eq!(node.lease_holders(&rx()), before);
    assert_eq!(state(&node, &fm), Some(JobState::Running));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_up_to_outside_one_to_three_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &dmr_iq(), DMR_HZ, 10.0).await;
    for up_to in [0, 4] {
        let r = node
            .apply(choose_op(
                DMR_HZ,
                20_000,
                serde_json::json!({"up_to": up_to}),
            ))
            .await;
        match r {
            Err(OpError::OutOfRange {
                field, min, max, ..
            }) => assert_eq!((field.as_str(), min, max), ("choose.up_to", 1, 3)),
            r => panic!("up_to {up_to}: {r:?}"),
        }
    }
    assert!(node.job_list().is_empty());
}

/// A rigid `fm_demod` on the DMR band.
async fn start_fm(node: &Node) -> JobId {
    let op = serde_json::from_value(serde_json::json!({
        "op": "job_start", "kind": "fm_demod", "source": "rx",
        "center_hz": DMR_HZ, "bandwidth_hz": 12_500,
        "deviation_hz": null, "sideband": null, "elastic": false, "replace": [],
    }))
    .unwrap();
    start(node, op).await
}

/// Bytes one second of the 48 kHz fixture takes as a `cf32` slice.
const SECOND_BYTES: u64 = RATE_HZ as u64 * 8;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slice_over_the_trial_quota_is_shortened_or_not_tried() {
    // A quota of 1.5 s: the 2 s slice is cut to its newest 1.5 s, and still proves DMR.
    let dir = tempfile::tempdir().unwrap();
    let node = node_with(dir.path(), &dmr_iq(), DMR_HZ, 10.0, SECOND_BYTES * 3 / 2).await;
    let id = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 2})),
    )
    .await;
    let ctx = ended(&node, &id).await;
    assert_eq!(ctx["verdict"]["row"], "dmr_decode", "{ctx:#}");
    let t = trials(&node);
    assert_eq!(t.len(), 1, "{t:?}");
    let held_ns = t[0].end_ns - t[0].start_ns;
    assert!((1_400_000_000..=1_500_000_000).contains(&held_ns), "{t:?}");
    drop(node);
    // A quota under 1 s: step 2 is not tried, and nothing is written.
    let dir = tempfile::tempdir().unwrap();
    let node = node_with(dir.path(), &dmr_iq(), DMR_HZ, 10.0, SECOND_BYTES / 2).await;
    let id = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 2})),
    )
    .await;
    let ctx = ended(&node, &id).await;
    let v = &ctx["verdict"];
    assert_eq!(v["row"], "fm_demod", "{v}");
    let reasons: Vec<&str> = v["not_tried"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["reason"].as_str())
        .collect();
    assert!(
        reasons.contains(&"not tried: the slice exceeds the trial quota"),
        "{v}"
    );
    assert!(trials(&node).is_empty(), "{:?}", node.bursts().list());
}
