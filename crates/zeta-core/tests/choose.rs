//! The `choose` job (design 5.3): step 1 answers at once from the ring, as `classify`
//! does; step 2 writes the ring slice into the burst store and replays it through the
//! decoder rows that could prove otherwise; step 3 tries them on the live band where a
//! running capture holds it, or after growing a held one. Each test runs a node over a
//! synthesised capture played in real time into a 10 s ring.

mod common;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use num_complex::Complex;
use serde_json::Value;
use tokio::sync::broadcast;
use zeta_core::capture::{CaptureMeta, write_cf32};
use zeta_core::choice_policy::{ChoicePolicy, LeaseSetting};
use zeta_core::job::JobState;
use zeta_core::node::Node;
use zeta_core::ops::{Op, OpError, OpResult};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::StageRegistry;
use zeta_core::stream::{Frame, Payload};
use zeta_core::types::{Capabilities, Claim, Hz, JobId, SampleFormat, SourceId, Timestamp};
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
    node_opts(dir, iq, (center_hz, ring_seconds, trial_keep_bytes), None).await
}

/// [`node_with`] over a source that tunes like a radio over `tune_range` when given.
async fn node_opts(
    dir: &Path,
    iq: &[Complex<f32>],
    (center_hz, ring_seconds, trial_keep_bytes): (u64, f64, u64),
    tune_range: Option<(Hz, Hz)>,
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
        tune_range,
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
async fn a_slice_settled_at_step_two_carries_its_training_record() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &dmr_iq(), DMR_HZ, 10.0).await;
    let id = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 2})),
    )
    .await;
    let _ = ended(&node, &id).await;
    let t = trials(&node);
    assert_eq!(t.len(), 1, "{t:?}");
    let training = t[0].training.as_ref().expect("a training record");
    assert_eq!(training["settled"]["row"], "dmr_decode", "{training}");
    assert_eq!(training["settled"]["step"], 2, "{training}");
    assert_eq!(training["settled"]["settled"], "proved", "{training}");
    let sources = training["sources"].as_array().unwrap();
    assert!(
        sources
            .iter()
            .any(|s| s["source"] == "layer1" && s["guess"] == "fsk" && s["confidence"].is_number()),
        "{training}"
    );
    // The record is no pin: the slice is still an unpinned trial under its quota.
    assert!(!t[0].pinned, "{t:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn choose_on_voice_stays_fm_demod() {
    let dir = tempfile::tempdir().unwrap();
    // As wide as the digital voice rows, so step 2 has rows to try.
    let node = node_over(dir.path(), &voice_iq(), DMR_HZ, 10.0).await;
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
    // A best guess leaves its slice without a training record.
    let slices = trials(&node);
    assert_eq!(slices.len(), 1, "{slices:?}");
    assert_eq!(slices[0].training, None, "{slices:?}");
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

/// The newest sample time of the source's ring, once it holds `held_s` seconds.
async fn latest_once_held(node: &Node, held_s: f64) -> u64 {
    until("the ring holds enough", Duration::from_secs(20), || {
        node.ring(&rx())
            .is_some_and(|r| r.read().unwrap().held_s() >= held_s)
    })
    .await;
    node.ring(&rx())
        .unwrap()
        .read()
        .unwrap()
        .latest()
        .unwrap()
        .0
}

/// `{"up_to": 2}` over the span `from_ns..to_ns`.
fn over(from_ns: u64, to_ns: u64) -> Value {
    serde_json::json!({"up_to": 2, "span": {"start_ns": from_ns, "end_ns": to_ns}})
}

const SECOND_NS: u64 = 1_000_000_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_span_is_cut_to_its_newest_slice_for_both_steps() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &dmr_iq(), DMR_HZ, 10.0).await;
    let latest = latest_once_held(&node, 7.0).await;
    let (from, to) = (latest - 6 * SECOND_NS, latest - 3 * SECOND_NS);
    let id = start(&node, choose_op(DMR_HZ, 20_000, over(from, to))).await;
    let ctx = ended(&node, &id).await;
    assert_eq!(ctx["span"]["end_ns"], to, "the ask keeps the span: {ctx:#}");
    // Step 2 replayed the span's newest 2 s, the slice step 1 classified.
    let slice = ctx["slice"].as_str().expect("a trial slice");
    let t = node.bursts().get(slice).unwrap();
    assert_eq!((t.start_ns, t.end_ns), (to - 2 * SECOND_NS, to), "{t:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_span_the_ring_no_longer_holds_cannot_tell() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &dmr_iq(), DMR_HZ, 3.0).await;
    // A ring of 3 s that has run 6 s no longer holds its first 2 s.
    until("the ring runs 6 s", Duration::from_secs(20), || {
        node.ring(&rx())
            .and_then(|r| r.read().unwrap().latest())
            .is_some_and(|t| t.0 >= 6 * SECOND_NS)
    })
    .await;
    let id = start(&node, choose_op(DMR_HZ, 20_000, over(0, 2 * SECOND_NS))).await;
    let ctx = ended(&node, &id).await;
    let v = &ctx["verdict"];
    assert_eq!(v["settled"], "cannot_tell", "{v}");
    assert_eq!(v["reason"], "the ring no longer holds the span", "{v}");
    assert!(trials(&node).is_empty(), "{:?}", node.bursts().list());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_span_under_a_second_skips_step_two_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &dmr_iq(), DMR_HZ, 10.0).await;
    let latest = latest_once_held(&node, 3.0).await;
    let to = latest - SECOND_NS;
    let id = start(
        &node,
        choose_op(DMR_HZ, 20_000, over(to - SECOND_NS / 2, to)),
    )
    .await;
    let ctx = ended(&node, &id).await;
    let v = &ctx["verdict"];
    assert_eq!(v["step"], 1, "{v}");
    assert!(
        reasons(v).contains(&"not tried: the selection's span is under 1 s"),
        "{v}"
    );
    assert!(trials(&node).is_empty(), "{:?}", node.bursts().list());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_span_ending_before_it_starts_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &dmr_iq(), DMR_HZ, 10.0).await;
    let r = node
        .apply(choose_op(DMR_HZ, 20_000, over(2 * SECOND_NS, SECOND_NS)))
        .await;
    assert!(
        matches!(&r, Err(OpError::Other { message }) if message.contains("choose.span")),
        "{r:?}"
    );
    assert!(node.job_list().is_empty());
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
    assert!(reasons.contains(&RING_SHORT), "{v}");
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
async fn the_agent_transport_asks_as_the_agent_whatever_surface_it_names() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &dmr_iq(), DMR_HZ, 10.0).await;
    let op = || {
        choose_op(
            DMR_HZ,
            20_000,
            serde_json::json!({"up_to": 1, "surface": "watch"}),
        )
    };
    let id = match node.apply_from(op(), "agent").await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("choose: {r:?}"),
    };
    let ctx = ended(&node, &id).await;
    assert_eq!(ctx["surface"], "agent", "{ctx:#}");
    assert_eq!(
        ctx["surface_note"], "surface watch asked over the agent transport: asked as agent",
        "{ctx:#}"
    );
    // Any other caller names its own.
    let id = match node.apply_from(op(), "http").await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("choose: {r:?}"),
    };
    let ctx = ended(&node, &id).await;
    assert_eq!(ctx["surface"], "watch", "{ctx:#}");
    assert!(ctx.get("surface_note").is_none(), "{ctx:#}");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_step_two_that_ran_and_proved_nothing_is_in_steps_run() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &voice_iq(), DMR_HZ, 10.0).await;
    let id = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 2})),
    )
    .await;
    let ctx = ended(&node, &id).await;
    let v = &ctx["verdict"];
    assert_eq!(v["row"], "fm_demod", "{v}");
    assert_eq!(v["step"], 1, "{v}");
    assert_eq!(v["steps_run"], serde_json::json!([1, 2]), "{v}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_step_two_the_quota_refused_is_not_in_steps_run() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_with(dir.path(), &dmr_iq(), DMR_HZ, 10.0, SECOND_BYTES / 2).await;
    let id = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 2})),
    )
    .await;
    let ctx = ended(&node, &id).await;
    let v = &ctx["verdict"];
    assert_eq!(v["steps_run"], serde_json::json!([1]), "{v}");
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
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pinned_trial_slices_share_the_trial_quota() {
    // A quota of 3 s: the first choose pins a 2 s slice, so the second, asked while the
    // first's slice is pinned, gets the 1 s left; never more than the quota pinned.
    let dir = tempfile::tempdir().unwrap();
    let node = node_with(dir.path(), &voice_iq(), DMR_HZ, 10.0, SECOND_BYTES * 3).await;
    // The first climbs to step 3, its live trials keeping its slice pinned for seconds.
    start_fm(&node).await;
    let op = || choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 2}));
    let up_to_3 = choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 3}));
    let first = start(&node, up_to_3).await;
    until("the first slice is pinned", ENDS_WITHIN, || {
        trials(&node).iter().any(|r| r.pinned)
    })
    .await;
    let second = start(&node, op()).await;
    until("both slices are pinned", ENDS_WITHIN, || {
        let t = trials(&node);
        let pinned: u64 = t.iter().filter(|r| r.pinned).map(|r| r.bytes).sum();
        assert!(pinned <= SECOND_BYTES * 3, "over the quota: {t:?}");
        t.iter().filter(|r| r.pinned).count() == 2
    })
    .await;
    let ctx = ended(&node, &second).await;
    let _ = ended(&node, &first).await;
    let slice = ctx["slice"]
        .as_str()
        .expect("the second choose wrote a slice");
    let t = node.bursts().get(slice).unwrap();
    let held_ns = t.end_ns - t.start_ns;
    assert!((900_000_000..=1_000_000_000).contains(&held_ns), "{t:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slice_the_pinned_trials_leave_no_room_for_is_not_tried() {
    // A quota of 2.5 s: the first choose's 2 s slice leaves under 1 s for the second.
    let dir = tempfile::tempdir().unwrap();
    let node = node_with(dir.path(), &voice_iq(), DMR_HZ, 10.0, SECOND_BYTES * 5 / 2).await;
    // The first climbs to step 3, its live trials keeping its slice pinned for seconds.
    start_fm(&node).await;
    let op = || choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 2}));
    let up_to_3 = choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 3}));
    let first = start(&node, up_to_3).await;
    until("the first slice is pinned", ENDS_WITHIN, || {
        trials(&node).iter().any(|r| r.pinned)
    })
    .await;
    let second = start(&node, op()).await;
    let ctx = ended(&node, &second).await;
    assert!(
        state(&node, &first) == Some(JobState::Running),
        "the first choose ended before the second wrote"
    );
    assert!(ctx["slice"].is_null(), "{ctx:#}");
    assert!(
        reasons(&ctx["verdict"]).contains(&"not tried: the slice exceeds the trial quota"),
        "{ctx:#}"
    );
    let _ = ended(&node, &first).await;
    assert_eq!(trials(&node).len(), 1, "{:?}", trials(&node));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_slices_at_once_both_fit_or_one_is_not_tried() {
    // A quota of 2.5 s: two 2 s slices asked at once never both fit. Each choose writes
    // its slice or is not tried, and the pinned trial bytes never pass the quota.
    const QUOTA: u64 = SECOND_BYTES * 5 / 2;
    let dir = tempfile::tempdir().unwrap();
    let node = node_with(dir.path(), &dmr_iq(), DMR_HZ, 10.0, QUOTA).await;
    let store = node.bursts();
    let watching = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let peak = {
        let (store, watching) = (store.clone(), watching.clone());
        tokio::spawn(async move {
            let mut peak = 0;
            while watching.load(Ordering::Relaxed) {
                peak = peak.max(store.pinned_trial_bytes());
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            peak
        })
    };
    let op = || choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 2}));
    let (a, b) = tokio::join!(start(&node, op()), start(&node, op()));
    let ctxs = [ended(&node, &a).await, ended(&node, &b).await];
    watching.store(false, Ordering::Relaxed);
    let peak = peak.await.unwrap();
    assert!(peak <= QUOTA, "pinned {peak} bytes, over the quota {QUOTA}");
    for ctx in &ctxs {
        assert!(
            ctx["slice"].is_string()
                || reasons(&ctx["verdict"])
                    .contains(&"not tried: the slice exceeds the trial quota"),
            "{ctx:#}"
        );
    }
    assert!(
        ctxs.iter().any(|c| c["slice"].is_string()),
        "neither wrote: {ctxs:#?}"
    );
    // Every reservation was given back: the whole quota is free again.
    let whole = store.reserve_trial(QUOTA);
    assert!(whole.is_some(), "{:?}", store.list());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reservation_dropped_unwritten_frees_its_bytes() {
    // A quota of 2.5 s: a reservation of 2 s leaves under a second for a choose's slice.
    let dir = tempfile::tempdir().unwrap();
    let node = node_with(dir.path(), &dmr_iq(), DMR_HZ, 10.0, SECOND_BYTES * 5 / 2).await;
    let store = node.bursts();
    let held = store
        .reserve_trial(SECOND_BYTES * 2)
        .expect("2 s fit an empty quota");
    assert!(store.reserve_trial(SECOND_BYTES).is_none());
    let op = || choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 2}));
    let id = start(&node, op()).await;
    let ctx = ended(&node, &id).await;
    assert!(ctx["slice"].is_null(), "{ctx:#}");
    assert!(
        reasons(&ctx["verdict"]).contains(&"not tried: the slice exceeds the trial quota"),
        "{ctx:#}"
    );
    // Given back unwritten: the next choose writes its whole slice and proves DMR.
    drop(held);
    let id = start(&node, op()).await;
    let ctx = ended(&node, &id).await;
    assert_eq!(ctx["verdict"]["row"], "dmr_decode", "{ctx:#}");
    let slice = ctx["slice"].as_str().expect("a slice");
    let t = store.get(slice).unwrap();
    assert!(t.end_ns - t.start_ns >= 1_900_000_000, "{t:?}");
    assert!(store.reserve_trial(SECOND_BYTES * 5 / 2).is_some());
}

/// A 5 kHz FM voice on the DMR fixture's frequency: step 2 tries the digital voice rows
/// and none proves it.
fn voice_iq() -> Vec<Complex<f32>> {
    let mut iq = nbfm_modulate(&speech_like(RATE_HZ, 3.0), RATE_HZ, RATE_HZ, 5_000.0, 0.0);
    awgn(&mut iq, 25.0, 5);
    iq
}

/// The replay records in the job list (running or not).
fn replays(node: &Node) -> Vec<zeta_core::job::JobRecord> {
    node.job_list()
        .into_iter()
        .filter(|r| r.replay.is_some())
        .collect()
}

// The node's peak count of trials is a test hook.
#[cfg(feature = "test-fixtures")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_chooses_never_run_more_trials_than_the_policy_allows() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &voice_iq(), DMR_HZ, 10.0).await;
    let mut policy = ChoicePolicy::default();
    policy.budget.concurrent = 1;
    let r = node
        .apply(Op::ChoicePolicySet {
            policy: Some(policy),
            lease: None,
        })
        .await;
    assert!(matches!(r, Ok(OpResult::ChoicePolicy(_))), "{r:?}");
    let op = || choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 2}));
    let (a, b) = (start(&node, op()).await, start(&node, op()).await);
    let running = |id: &JobId| state(&node, id) == Some(JobState::Running);
    let deadline = tokio::time::Instant::now() + ENDS_WITHIN;
    while running(&a) || running(&b) {
        let replays: Vec<_> = replays(&node)
            .into_iter()
            .filter(|r| r.state == JobState::Running)
            .collect();
        assert!(replays.len() <= 1, "{replays:#?}");
        assert!(
            tokio::time::Instant::now() < deadline,
            "the chooses never ended"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Both tried their rows, one trial at a time between them.
    for id in [&a, &b] {
        let ctx = ended(&node, id).await;
        assert!(!ctx["trials"].as_array().unwrap().is_empty(), "{ctx:#}");
    }
    assert_eq!(node.test_trial_peak(), 1);
}

/// The default choice policy with `concurrent` trial slots and `trial_s` of trials.
#[cfg(feature = "test-fixtures")]
async fn trial_budget(node: &Node, concurrent: u32, trial_s: u32) {
    let mut policy = ChoicePolicy::default();
    policy.budget.concurrent = concurrent;
    policy.budget.trial_s = trial_s;
    let r = node
        .apply(Op::ChoicePolicySet {
            policy: Some(policy),
            lease: None,
        })
        .await;
    assert!(matches!(r, Ok(OpResult::ChoicePolicy(_))), "{r:?}");
}

/// How long a test hook's step-3 trial waits for a slot it is expected to get.
#[cfg(feature = "test-fixtures")]
const SLOT_WITHIN: Duration = Duration::from_secs(5);

/// Whether a replay runs now.
#[cfg(feature = "test-fixtures")]
fn a_replay_runs(node: &Node) -> bool {
    replays(node).iter().any(|r| r.state == JobState::Running)
}

/// The step-2 trials of a `choose` context that ran a replay.
#[cfg(feature = "test-fixtures")]
fn replayed(ctx: &Value) -> Vec<&Value> {
    ctx["trials"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|t| t["step"] == 2 && t["job"].is_string())
                .collect()
        })
        .unwrap_or_default()
}

// The step-3 slots a test holds stand for other chooses' dwells (a test hook).
#[cfg(feature = "test-fixtures")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replay_starts_while_live_trials_hold_every_slot_but_one() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &voice_iq(), DMR_HZ, 10.0).await;
    trial_budget(&node, 3, 30).await;
    // Two step-3 dwells hold two of the three slots; a third waits, as one slot is kept
    // for replays.
    let mut dwells = Vec::new();
    for _ in 0..2 {
        let slot = node.test_live_trial_slot(SLOT_WITHIN).await;
        dwells.push(slot.expect("a step-3 trial takes a free slot"));
    }
    let third = node.test_live_trial_slot(2 * ENDS_WITHIN);
    tokio::pin!(third);
    tokio::select! {
        _ = &mut third => panic!("a third step-3 trial took the replays' slot"),
        _ = tokio::time::sleep(Duration::from_millis(300)) => {}
    }
    let id = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 2})),
    )
    .await;
    // The replay starts while the dwells hold their slots and the third still waits.
    tokio::select! {
        _ = &mut third => panic!("a third step-3 trial took the replays' slot"),
        _ = until("a replay runs beside the dwells", ENDS_WITHIN, || a_replay_runs(&node)) => {}
    }
    let ctx = ended(&node, &id).await;
    assert!(!replayed(&ctx).is_empty(), "{ctx:#}");
    assert_eq!(node.test_live_trial_peak(), 2);
    assert!(node.test_trial_peak() <= 3, "{}", node.test_trial_peak());
    drop(dwells);
    // The slots given back, the third step-3 trial is served.
    assert!(
        third.await.is_some(),
        "the third step-3 trial never got a slot"
    );
}

#[cfg(feature = "test-fixtures")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_waiting_replay_is_served_before_a_waiting_live_trial() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &voice_iq(), DMR_HZ, 10.0).await;
    trial_budget(&node, 1, 30).await;
    // One step-3 dwell holds the only slot; a second step-3 trial waits for it first.
    let first = node.test_live_trial_slot(SLOT_WITHIN).await;
    let first = first.expect("a step-3 trial takes the free slot");
    let second = node.test_live_trial_slot(2 * ENDS_WITHIN);
    tokio::pin!(second);
    tokio::select! {
        _ = &mut second => panic!("a step-3 trial was served while the slot is held"),
        _ = tokio::time::sleep(Duration::from_millis(300)) => {}
    }
    let id = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 2})),
    )
    .await;
    // Then the choose's replay waits behind it too.
    tokio::select! {
        _ = &mut second => panic!("a step-3 trial was served while the slot is held"),
        _ = until("the choose's replay waits for a slot", ENDS_WITHIN, || {
            node.test_replays_waiting() == 1
        }) => {}
    }
    assert!(!a_replay_runs(&node));
    drop(first);
    // The slot given back goes to the replay, not the step-3 trial that waited longer.
    tokio::select! {
        _ = &mut second => panic!("the waiting step-3 trial was served before the replay"),
        _ = until("the replay runs", ENDS_WITHIN, || a_replay_runs(&node)) => {}
    }
    // The step-3 trial is served once no replay waits, and the choose still ends.
    let second = second.await;
    assert!(
        second.is_some(),
        "the waiting step-3 trial never got a slot"
    );
    drop(second);
    let ctx = ended(&node, &id).await;
    assert!(!replayed(&ctx).is_empty(), "{ctx:#}");
}

/// The running trial jobs on the live band a `choose` owns (its step 3).
#[cfg(feature = "test-fixtures")]
fn live_trial_jobs(node: &Node, owner: &JobId) -> Vec<zeta_core::job::JobRecord> {
    node.job_list()
        .into_iter()
        .filter(|r| {
            r.state == JobState::Running && r.owner.as_ref() == Some(owner) && r.replay.is_none()
        })
        .collect()
}

#[cfg(feature = "test-fixtures")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_one_slot_live_and_replay_share_it() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &voice_iq(), DMR_HZ, 10.0).await;
    trial_budget(&node, 1, 15).await;
    // A rigid job holds the capture, so step 3 may share it.
    let fm = start_fm(&node).await;
    let a = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 3})),
    )
    .await;
    until("the first choose dwells at step 3", ENDS_WITHIN, || {
        !live_trial_jobs(&node, &a).is_empty()
    })
    .await;
    // A second choose's replays share the one slot with the first's dwells.
    let b = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 2})),
    )
    .await;
    let running = |id: &JobId| state(&node, id) == Some(JobState::Running);
    let deadline = tokio::time::Instant::now() + ENDS_WITHIN;
    while running(&a) || running(&b) {
        let snapshot = node.job_list();
        let replaying = snapshot
            .iter()
            .filter(|r| r.replay.is_some() && r.state == JobState::Running)
            .count();
        let dwelling = snapshot
            .iter()
            .filter(|r| {
                r.state == JobState::Running && r.owner.as_ref() == Some(&a) && r.replay.is_none()
            })
            .count();
        assert!(replaying + dwelling <= 1, "{replaying} + {dwelling}");
        assert!(
            tokio::time::Instant::now() < deadline,
            "the chooses never ended"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let ctx_a = ended(&node, &a).await;
    assert!(!live_trials(&ctx_a).is_empty(), "{ctx_a:#}");
    let ctx_b = ended(&node, &b).await;
    assert!(!replayed(&ctx_b).is_empty(), "{ctx_b:#}");
    assert_eq!(node.test_trial_peak(), 1);
    assert_eq!(node.test_live_trial_peak(), 1);
    assert_eq!(state(&node, &fm), Some(JobState::Running));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_choose_leaves_no_replay_record() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &voice_iq(), DMR_HZ, 10.0).await;
    let id = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 2})),
    )
    .await;
    let ctx = ended(&node, &id).await;
    assert!(!ctx["trials"].as_array().unwrap().is_empty(), "{ctx:#}");
    // Its trials were its own: each goes once it ends.
    until("the trial replays are forgotten", ENDS_WITHIN, || {
        replays(&node).is_empty()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_a_choose_in_step_two_stops_its_replays_and_unpins_the_slice() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &voice_iq(), DMR_HZ, 10.0).await;
    let id = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 2})),
    )
    .await;
    until("a trial replay runs", ENDS_WITHIN, || {
        replays(&node).iter().any(|r| r.state == JobState::Running)
    })
    .await;
    let trial = replays(&node)
        .into_iter()
        .find(|r| r.state == JobState::Running)
        .unwrap();
    assert_eq!(trial.owner.as_ref(), Some(&id), "{trial:?}");
    // The choose's to stop, not the operator's.
    let refused = node
        .apply(Op::JobStop {
            job: trial.id.clone(),
        })
        .await;
    assert!(matches!(refused, Err(OpError::Other { .. })), "{refused:?}");
    assert!(trials(&node).iter().any(|r| r.pinned));
    match node.apply(Op::JobStop { job: id.clone() }).await {
        Ok(OpResult::Ok) => {}
        r => panic!("stop: {r:?}"),
    }
    until("the choose job stops", ENDS_WITHIN, || {
        state(&node, &id) == Some(JobState::Stopped)
    })
    .await;
    until("its replays stop and go", ENDS_WITHIN, || {
        replays(&node).is_empty()
    })
    .await;
    let t = trials(&node);
    assert_eq!(t.len(), 1, "{t:?}");
    assert!(!t[0].pinned, "{t:?}");
    assert_eq!(t[0].training, None, "{t:?}");
    // Stopped before a verdict.
    let ctx = node.job_context(&id).unwrap();
    assert!(ctx["choose"]["verdict"].is_null(), "{ctx:#}");
}

/// Step 2's note when the ring holds under a second once its oldest 100 ms are set aside.
const RING_SHORT: &str = "not tried: the ring holds under 1 s once its oldest 100 ms are set aside";
/// Step 2's note under a policy slice too short for it.
const POLICY_SLICE_SHORT: &str = "not tried: the policy's slice_s is under 1 s";
/// Step 3's note of a row no running capture holds.
const NOT_COVERED: &str = "not tried: no running capture covers it";
/// Step 3's note of a row whose trial gave way to a foreground request.
const GAVE_WAY: &str = "not tried: a foreground request took the capture";

/// The choice policy with `lease` and a slice of half a second, too short for step 2
/// (the ring holds 10 s): its trials, if any, are step 3's.
async fn live_only(node: &Node, lease: LeaseSetting) {
    let mut policy = ChoicePolicy {
        lease,
        ..ChoicePolicy::default()
    };
    policy.budget.slice_s = 0.5;
    let r = node
        .apply(Op::ChoicePolicySet {
            policy: Some(policy),
            lease: None,
        })
        .await;
    assert!(matches!(r, Ok(OpResult::ChoicePolicy(_))), "{r:?}");
}

/// The `not_tried` reasons of a choice.
fn reasons(choice: &Value) -> Vec<&str> {
    choice["not_tried"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["reason"].as_str())
        .collect()
}

/// The step-3 trials in a `choose` context.
fn live_trials(ctx: &Value) -> Vec<&Value> {
    ctx["trials"]
        .as_array()
        .map(|a| a.iter().filter(|t| t["step"] == 3).collect())
        .unwrap_or_default()
}

/// The running jobs of `kind`.
fn running(node: &Node, kind: &str) -> Vec<zeta_core::job::JobRecord> {
    node.job_list()
        .into_iter()
        .filter(|r| r.state == JobState::Running && r.kind == kind)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step_three_proves_on_the_live_band_under_share() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &dmr_iq(), DMR_HZ, 10.0).await;
    live_only(&node, LeaseSetting::Share).await;
    // A rigid job holds the capture, which holds the DMR band.
    let fm = start_fm(&node).await;
    let mut feed = feed(&node);
    let id = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 3})),
    )
    .await;
    let ctx = ended(&node, &id).await;
    let v = &ctx["verdict"];
    assert_eq!(v["row"], "dmr_decode", "{v}");
    assert_eq!(v["step"], 3, "{v}");
    assert_eq!(v["settled"], "proved", "{v}");
    assert_eq!(v["job"]["kind"], "dmr_decode", "{v}");
    assert_eq!(v["runner_up"]["row"], "fm_demod", "{v}");
    assert!(
        v["evidence"].as_array().unwrap().iter().any(|e| e
            .as_str()
            .is_some_and(|e| e.starts_with("decode dmr_decode: "))),
        "{v}"
    );
    // Step 2 had no slice to replay: nothing was written, so no record either (the
    // training record of a step-3 proof is unit-tested in node/choose.rs).
    assert!(reasons(v).contains(&POLICY_SLICE_SHORT), "{v}");
    assert!(trials(&node).is_empty(), "{:?}", node.bursts().list());
    let live = live_trials(&ctx);
    let proved: Vec<_> = live.iter().filter(|t| t["outcome"] == "proved").collect();
    assert_eq!(proved.len(), 1, "{ctx:#}");
    assert_eq!(proved[0]["row"], "dmr_decode", "{ctx:#}");
    let ev = events(&mut feed, &id);
    let protocols: Vec<&str> = ev.iter().filter_map(|e| e["protocol"].as_str()).collect();
    assert_eq!(protocols, ["choice", "choice", "verdict"], "{ev:#?}");
    assert_eq!(ev[1]["step"], 3, "{:#}", ev[1]);
    // The holder runs on, alone again once the trials are stopped.
    assert_eq!(state(&node, &fm), Some(JobState::Running));
    until("only the holder holds the source", ENDS_WITHIN, || {
        node.lease_holders(&rx())
            .iter()
            .map(|(j, _)| j)
            .collect::<Vec<_>>()
            == [&fm]
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_step_three_child_is_stopped_unless_started() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &dmr_iq(), DMR_HZ, 10.0).await;
    live_only(&node, LeaseSetting::Share).await;
    let fm = start_fm(&node).await;
    // Without `start`: every trial job is stopped, and goes with its run.
    let id = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 3})),
    )
    .await;
    let ctx = ended(&node, &id).await;
    assert_eq!(ctx["verdict"]["step"], 3, "{ctx:#}");
    let live = live_trials(&ctx);
    assert!(!live.is_empty(), "{ctx:#}");
    let jobs: Vec<JobId> = live
        .iter()
        .filter_map(|t| t["job"].as_str().map(|j| JobId(j.into())))
        .collect();
    until("the trial jobs stop and go", ENDS_WITHIN, || {
        node.job_list().iter().all(|r| !jobs.contains(&r.id))
    })
    .await;
    assert!(running(&node, "dmr_decode").is_empty());
    assert!(ctx["started"].is_null(), "{ctx:#}");
    // With `start`: the proved trial job is the verdict's job, kept running, and no
    // second one is started.
    let id = start(
        &node,
        choose_op(
            DMR_HZ,
            20_000,
            serde_json::json!({"up_to": 3, "start": true}),
        ),
    )
    .await;
    let ctx = ended(&node, &id).await;
    let v = &ctx["verdict"];
    assert_eq!(v["row"], "dmr_decode", "{v}");
    assert_eq!(v["step"], 3, "{v}");
    let proved = live_trials(&ctx)
        .into_iter()
        .find(|t| t["outcome"] == "proved")
        .expect("a proved trial");
    assert_eq!(ctx["started"], proved["job"], "{ctx:#}");
    let kept = running(&node, "dmr_decode");
    assert_eq!(kept.len(), 1, "{kept:?}");
    let k = &kept[0];
    assert_eq!(ctx["started"], k.id.0.as_str(), "{ctx:#}");
    // An ordinary job now, on the verdict's band.
    assert_eq!(k.owner, None, "{k:?}");
    assert_eq!(k.tuning.center_hz.0, v["job"]["center_hz"], "{k:?}");
    assert_eq!(k.tuning.bandwidth_hz.0, v["job"]["bandwidth_hz"], "{k:?}");
    match node.apply(Op::JobStop { job: k.id.clone() }).await {
        Ok(OpResult::Ok) => {}
        r => panic!("the kept job is the operator's to stop: {r:?}"),
    }
    assert_eq!(state(&node, &fm), Some(JobState::Running));
}

/// The frequency the [`Growing`] radio hears the DMR call on.
const CALL_HZ: u64 = DMR_HZ;
/// The rigid holder's band: its 48 kHz capture's lower edge is 4.5 kHz below the call,
/// inside the call's 12.5 kHz channel.
const HOLDER_HZ: u64 = CALL_HZ + 19_500;

/// A radio hearing one DMR call at [`CALL_HZ`]. It tunes to the centre asked, 48 kHz
/// wide for a band up to 48 kHz and 96 kHz above that (its widest), and delivers the
/// call where it falls in the capture, synthesised for the capture's rate and offset,
/// in real time in 100 ms blocks. It counts its tunes.
struct Growing {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tunes: AtomicUsize,
    tx: broadcast::Sender<IqBlock>,
}

impl Growing {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            id: rx(),
            caps: Capabilities {
                tune_min_hz: Claim::Asserted(Hz(1_000_000)),
                tune_max_hz: Claim::Asserted(Hz(6_000_000_000)),
                max_bandwidth_hz: Claim::Asserted(Hz(96_000)),
                formats: vec![SampleFormat::Cf32],
                channels: 1,
                rx: true,
                tx: false,
                remote: false,
                push: true,
                usable_fraction: Claim::Asserted(1.0),
                depths: vec![],
                sweep: None,
            },
            cur: Mutex::new(None),
            tunes: AtomicUsize::new(0),
            tx: broadcast::channel(64).0,
        })
    }

    fn tunes(&self) -> usize {
        self.tunes.load(Ordering::SeqCst)
    }

    /// Play the call into the current capture until the radio is dropped.
    fn start(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut clip: Option<(Tuning, Vec<Complex<f32>>)> = None;
            let (mut at, mut t_ns) = (0usize, 0u64);
            let mut tick = tokio::time::interval(Duration::from_millis(100));
            loop {
                tick.tick().await;
                let Some(me) = me.upgrade() else { return };
                let Some(cur) = *me.cur.lock().unwrap() else {
                    continue;
                };
                if clip.as_ref().is_none_or(|(t, _)| *t != cur) {
                    let offset = CALL_HZ as f64 - cur.center_hz.0 as f64;
                    clip = Some((cur, call_iq(cur.bandwidth_hz.0 as u32, offset)));
                    at = 0;
                }
                let (_, iq) = clip.as_ref().unwrap();
                let n = (cur.bandwidth_hz.0 / 10) as usize;
                let block: Vec<Complex<f32>> = (0..n).map(|i| iq[(at + i) % iq.len()]).collect();
                at = (at + n) % iq.len();
                let _ = me.tx.send(IqBlock {
                    t0: Timestamp(t_ns),
                    sample_rate_hz: cur.bandwidth_hz,
                    center_hz: cur.center_hz,
                    samples: block.into(),
                });
                t_ns += 100_000_000;
            }
        });
    }
}

#[async_trait]
impl Port for Growing {
    fn id(&self) -> &SourceId {
        &self.id
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        self.tunes.fetch_add(1, Ordering::SeqCst);
        if t.bandwidth_hz.0 > 96_000 {
            return Err(PortError::OutOfRange {
                field: "bandwidth_hz",
                value: t.bandwidth_hz.0,
                min: 1,
                max: 96_000,
                suggested: None,
            });
        }
        let rate = if t.bandwidth_hz.0 <= 48_000 {
            48_000
        } else {
            96_000
        };
        let got = Tuning {
            center_hz: t.center_hz,
            bandwidth_hz: Hz(rate),
        };
        *self.cur.lock().unwrap() = Some(got);
        Ok(got)
    }
    fn current(&self) -> Option<Tuning> {
        *self.cur.lock().unwrap()
    }
    fn subscribe(&self) -> IqReceiver {
        self.tx.subscribe()
    }
}

/// The DMR call of [`dmr_iq`] at `rate_hz`, `offset_hz` from the capture's centre.
fn call_iq(rate_hz: u32, offset_hz: f64) -> Vec<Complex<f32>> {
    let params = serde_json::json!({
        "family": "dmr", "color_code": 5, "src": 9990001, "dst": 9990, "superframes": 6,
        "offset_hz": offset_hz,
    });
    let (spec, imp) = zeta_dsp::synth::parse_params(params.as_object().unwrap().clone()).unwrap();
    let mut iq = zeta_dsp::synth::synthesise(&spec, &imp, rate_hz, CALL_HZ);
    awgn(&mut iq, 25.0, 13);
    iq
}

/// A node over a [`Growing`] radio held by a rigid `fm_demod` at [`HOLDER_HZ`], its
/// ring holding 2.5 s of that capture, and the choice policy [`live_only`] with `lease`.
async fn node_growing(dir: &Path, lease: LeaseSetting) -> (Node, Arc<Growing>, JobId) {
    node_growing_held(dir, lease, false).await
}

/// [`node_growing`], the holder an elastic `spectrum` when `elastic`.
async fn node_growing_held(
    dir: &Path,
    lease: LeaseSetting,
    elastic: bool,
) -> (Node, Arc<Growing>, JobId) {
    let radio = Growing::new();
    let port: Arc<dyn Port> = radio.clone();
    let mut reg = StageRegistry::new();
    zeta_dsp::register_all(&mut reg);
    let mut cfg = common::cfg(dir, true);
    cfg.ring_seconds = 10.0;
    let node = Node::open(cfg, reg, vec![port]).await.unwrap();
    for c in zeta_dsp::classifiers(node.modes_handle()) {
        node.add_classifier(c);
    }
    radio.start();
    live_only(&node, lease).await;
    let kind = if elastic { "spectrum" } else { "fm_demod" };
    let op = serde_json::from_value(serde_json::json!({
        "op": "job_start", "kind": kind, "source": "rx",
        "center_hz": HOLDER_HZ, "bandwidth_hz": 12_500,
        "deviation_hz": null, "sideband": null, "elastic": elastic, "replace": [],
    }))
    .unwrap();
    let holder = start(&node, op).await;
    assert_eq!(radio.current().unwrap().bandwidth_hz, Hz(48_000));
    until("the ring fills", Duration::from_secs(10), || {
        node.ring(&rx())
            .is_some_and(|r| r.read().unwrap().held_s() >= 2.5)
    })
    .await;
    (node, radio, holder)
}

/// A `choose` on the call at the capture's lower edge: the selection (as wide as the
/// digital voice rows' 12.5 kHz, so they could prove it) lies inside the capture, the
/// call's channel does not.
fn edge_op() -> Op {
    choose_op(CALL_HZ + 2_000, 13_000, serde_json::json!({"up_to": 3}))
}

/// The ops `ops.log` holds, by name.
fn logged_ops(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("state").join("ops.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| v["op"]["op"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn share_skips_a_band_no_capture_covers() {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio, holder) = node_growing(dir.path(), LeaseSetting::Share).await;
    let (tunes, capture) = (radio.tunes(), radio.current());
    let id = start(&node, edge_op()).await;
    let ctx = ended(&node, &id).await;
    let v = &ctx["verdict"];
    assert_eq!(v["row"], "fm_demod", "{v}");
    assert_eq!(v["settled"], "best_guess", "{v}");
    let could: Vec<&str> = v["could_prove"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(could.contains(&"dmr_decode"), "{v}");
    // Each row that could prove otherwise is not tried, and no trial job ran.
    for row in &could {
        let n = v["not_tried"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["row"] == *row && n["reason"] == NOT_COVERED);
        assert!(n.is_some(), "{row}: {v}");
    }
    assert!(live_trials(&ctx).is_empty(), "{ctx:#}");
    // Nothing was tuned or moved, and nothing but the choose was asked of the node.
    assert_eq!(radio.tunes(), tunes);
    assert_eq!(radio.current(), capture);
    let held = node.lease_holders(&rx());
    assert_eq!(held.iter().map(|(j, _)| j).collect::<Vec<_>>(), [&holder]);
    let ops = logged_ops(dir.path());
    assert_eq!(
        ops,
        ["choice_policy_set", "job_start", "job_start"],
        "{ops:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn widen_grows_a_held_capture_and_keeps_every_holder() {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio, holder) = node_growing(dir.path(), LeaseSetting::Widen).await;
    let held = node.lease_holders(&rx());
    let id = start(&node, edge_op()).await;
    let ctx = ended(&node, &id).await;
    let v = &ctx["verdict"];
    assert_eq!(v["row"], "dmr_decode", "{v}");
    assert_eq!(v["step"], 3, "{v}");
    assert_eq!(v["settled"], "proved", "{v}");
    // The capture grew once, to hold the call's channel beside the holder's band, and
    // the answer says so.
    let cur = radio.current().unwrap();
    assert_eq!(cur.bandwidth_hz, Hz(96_000), "{cur:?}");
    let job = &v["job"];
    let (lo, hi) = (cur.center_hz.0 - 48_000, cur.center_hz.0 + 48_000);
    let c = job["center_hz"].as_u64().unwrap();
    let w = job["bandwidth_hz"].as_u64().unwrap();
    assert!(lo <= c - w / 2 && c + w / 2 <= hi, "{job} in {cur:?}");
    let line = format!(
        "widened the capture to {}/{} Hz for a trial",
        cur.center_hz.0, cur.bandwidth_hz.0
    );
    let evidence: Vec<&str> = v["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(evidence.iter().filter(|e| **e == line).count(), 1, "{v}");
    assert!(
        evidence.last().unwrap().starts_with("decode dmr_decode: "),
        "{v}"
    );
    // Every holder keeps its band and runs on the grown capture; the trial jobs went.
    assert_eq!(state(&node, &holder), Some(JobState::Running));
    until(
        "only the holder holds the source, on its band",
        ENDS_WITHIN,
        || node.lease_holders(&rx()) == held,
    )
    .await;
    let rec = node
        .job_list()
        .into_iter()
        .find(|r| r.id == holder)
        .unwrap();
    assert_eq!(rec.tuning.center_hz, Hz(HOLDER_HZ), "{rec:?}");
}

/// The far band the tunable source is moved to, inside its range, outside the capture.
const FAR_HZ: u64 = 450_000_000;

/// A node over the FM voice fixture on a source that tunes over 400 to 500 MHz, held
/// only by an elastic `spectrum` (as the page's view holds it), with the choice policy
/// [`live_only`] under `share`; then a `choose` to step 3 on the voice, once one of its
/// live trial jobs runs. The node, the spectrum, the choose and that trial job.
async fn trial_beside_a_spectrum(dir: &Path) -> (Node, JobId, JobId, JobId) {
    let range = (Hz(400_000_000), Hz(500_000_000));
    let node = node_opts(dir, &voice_iq(), (DMR_HZ, 10.0, u64::MAX), Some(range)).await;
    live_only(&node, LeaseSetting::Share).await;
    let op = serde_json::from_value(serde_json::json!({
        "op": "job_start", "kind": "spectrum", "source": "rx",
        "center_hz": DMR_HZ, "bandwidth_hz": 48_000,
        "deviation_hz": null, "sideband": null, "elastic": true, "replace": [],
    }))
    .unwrap();
    let spectrum = start(&node, op).await;
    let id = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 3})),
    )
    .await;
    let mut child = None;
    until("a live trial job runs", ENDS_WITHIN, || {
        child = live_job(&node, &id);
        child.is_some()
    })
    .await;
    (node, spectrum, id, child.unwrap())
}

/// The running live trial job (not a replay) the `choose` `id` owns.
fn live_job(node: &Node, id: &JobId) -> Option<JobId> {
    node.job_list()
        .into_iter()
        .find(|r| {
            r.owner.as_ref() == Some(id) && r.replay.is_none() && r.state == JobState::Running
        })
        .map(|r| r.id)
}

/// The trial job `child` is stopped once its lease gave way, the `choose` `id` ends
/// with the row it was trying noted [`GAVE_WAY`], and the source is at [`FAR_HZ`].
async fn gave_way(node: &Node, id: &JobId, child: &JobId) {
    until("the trial job stops and goes", ENDS_WITHIN, || {
        node.job_list().iter().all(|r| &r.id != child)
    })
    .await;
    let ctx = ended(node, id).await;
    let v = &ctx["verdict"];
    assert!(reasons(v).contains(&GAVE_WAY), "{v}");
    let stopped = live_trials(&ctx)
        .into_iter()
        .find(|t| t["job"] == child.0.as_str())
        .expect("the trial is recorded");
    assert_eq!(stopped["outcome"], "stopped", "{ctx:#}");
    let cur = match node.apply(Op::SourceList {}).await {
        Ok(OpResult::Sources(s)) => s.into_iter().find(|s| s.id == rx()).unwrap().current,
        r => panic!("source_list: {r:?}"),
    };
    assert_eq!(cur.map(|c| c.center_hz), Some(Hz(FAR_HZ)), "{cur:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_trial_gives_way_to_the_views_retune() {
    let dir = tempfile::tempdir().unwrap();
    let (node, spectrum, id, child) = trial_beside_a_spectrum(dir.path()).await;
    // The page moves its view off the capture (`source_ensure`, its spectrum then
    // relaunched there): granted, though a trial runs on the old capture.
    let r = node
        .apply(Op::SourceEnsure {
            source: rx(),
            center_hz: Hz(FAR_HZ),
            bandwidth_hz: Hz(48_000),
            replace: vec![],
            widen: false,
        })
        .await;
    assert!(matches!(r, Ok(OpResult::Tuning(_))), "{r:?}");
    gave_way(&node, &id, &child).await;
    assert_eq!(state(&node, &spectrum), Some(JobState::Running));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_trial_gives_way_to_a_job_start_that_needs_a_retune() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _, id, child) = trial_beside_a_spectrum(dir.path()).await;
    let op = serde_json::from_value(serde_json::json!({
        "op": "job_start", "kind": "fm_demod", "source": "rx",
        "center_hz": FAR_HZ, "bandwidth_hz": 12_500,
        "deviation_hz": null, "sideband": null, "elastic": false, "replace": [],
    }))
    .unwrap();
    let fm = start(&node, op).await;
    gave_way(&node, &id, &child).await;
    assert_eq!(state(&node, &fm), Some(JobState::Running));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_a_choose_in_step_three_stops_its_trial() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &voice_iq(), DMR_HZ, 10.0).await;
    live_only(&node, LeaseSetting::Share).await;
    let fm = start_fm(&node).await;
    let id = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 3})),
    )
    .await;
    let mut child = None;
    until("a live trial job runs", ENDS_WITHIN, || {
        child = live_job(&node, &id);
        child.is_some()
    })
    .await;
    let child = child.unwrap();
    // The choose's to stop, not the operator's.
    let refused = node.apply(Op::JobStop { job: child.clone() }).await;
    assert!(matches!(refused, Err(OpError::Other { .. })), "{refused:?}");
    match node.apply(Op::JobStop { job: id.clone() }).await {
        Ok(OpResult::Ok) => {}
        r => panic!("stop: {r:?}"),
    }
    until("the choose job stops", ENDS_WITHIN, || {
        state(&node, &id) == Some(JobState::Stopped)
    })
    .await;
    until("its trial job stops and goes", ENDS_WITHIN, || {
        node.job_list().iter().all(|r| r.id != child)
    })
    .await;
    let ctx = node.job_context(&id).unwrap();
    assert!(ctx["choose"]["verdict"].is_null(), "{ctx:#}");
    until("only the holder holds the source", ENDS_WITHIN, || {
        node.lease_holders(&rx())
            .iter()
            .map(|(j, _)| j)
            .collect::<Vec<_>>()
            == [&fm]
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn widen_never_tunes_a_source_only_an_elastic_job_holds() {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio, spectrum) = node_growing_held(dir.path(), LeaseSetting::Widen, true).await;
    let (tunes, capture) = (radio.tunes(), radio.current());
    let id = start(&node, edge_op()).await;
    let ctx = ended(&node, &id).await;
    let v = &ctx["verdict"];
    assert_eq!(v["row"], "fm_demod", "{v}");
    assert!(reasons(v).contains(&NOT_COVERED), "{v}");
    assert!(live_trials(&ctx).is_empty(), "{ctx:#}");
    let evidence = v["evidence"].to_string();
    assert!(!evidence.contains("widened the capture"), "{v}");
    // Nothing rigid holds the radio, so nothing grew it.
    assert_eq!(radio.tunes(), tunes);
    assert_eq!(radio.current(), capture);
    assert_eq!(state(&node, &spectrum), Some(JobState::Running));
}

/// `job_list`'s ids, with or without `all`.
async fn listed(node: &Node, all: bool) -> Vec<JobId> {
    match node.apply(Op::JobList { all }).await {
        Ok(OpResult::Jobs(jobs)) => jobs.into_iter().map(|r| r.id).collect(),
        r => panic!("job_list: {r:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transient_choose_is_listed_only_with_all() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &dmr_iq(), DMR_HZ, 10.0).await;
    let id = start(
        &node,
        choose_op(
            DMR_HZ,
            20_000,
            serde_json::json!({"up_to": 3, "transient": true}),
        ),
    )
    .await;
    let plain = start(
        &node,
        choose_op(DMR_HZ, 20_000, serde_json::json!({"up_to": 3})),
    )
    .await;
    assert_eq!(state(&node, &id), Some(JobState::Running));
    let list = listed(&node, false).await;
    assert!(list.contains(&plain) && !list.contains(&id), "{list:?}");
    assert!(listed(&node, true).await.contains(&id));
    // A read by id is not filtered.
    assert!(node.job_context(&id).is_some());
    // It is still an applied operation.
    assert!(
        logged_ops(dir.path()).iter().any(|o| o == "job_start"),
        "{:?}",
        logged_ops(dir.path())
    );
    node.apply(Op::JobStop { job: id }).await.unwrap();
    node.apply(Op::JobStop { job: plain }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transient_choose_does_not_resume() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &dmr_iq(), DMR_HZ, 10.0).await;
    let id = start(
        &node,
        choose_op(
            DMR_HZ,
            20_000,
            serde_json::json!({"up_to": 3, "transient": true}),
        ),
    )
    .await;
    assert_eq!(state(&node, &id), Some(JobState::Running));
    let file = dir.path().join("state/jobs").join(format!("{}.json", id.0));
    assert!(file.exists());
    drop(node);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let node = node_over(dir.path(), &dmr_iq(), DMR_HZ, 10.0).await;
    assert!(node.job_list().iter().all(|r| r.id != id), "resumed");
    assert!(!file.exists(), "its record file is kept");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_transient_choose_leaves_no_history() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &dmr_iq(), DMR_HZ, 10.0).await;
    let id = start(
        &node,
        choose_op(
            DMR_HZ,
            20_000,
            serde_json::json!({"up_to": 1, "transient": true}),
        ),
    )
    .await;
    until("the record goes", ENDS_WITHIN, || {
        state(&node, &id).is_none()
    })
    .await;
    assert!(!listed(&node, true).await.contains(&id));
    assert!(
        !dir.path()
            .join("state/jobs")
            .join(format!("{}.json", id.0))
            .exists()
    );
}
