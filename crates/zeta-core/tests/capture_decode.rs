//! `capture_decode`: a stored capture's whole set replayed into a decoder mode's graph, as
//! a leaseless job marked `replay`, its events on the node-wide `replay` feed with the
//! recording's times and nowhere a radio's are.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use zeta_core::bursts::NewCapture;
use zeta_core::job::{JobRecord, JobState};
use zeta_core::node::{Node, REPLAY_SOURCE};
use zeta_core::ops::{Op, OpError, OpResult, SourceInfo};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stream::{Frame, Payload};
use zeta_core::types::{Capabilities, Claim, Hz, JobId, SampleFormat, SourceId};

mod common;
use common::handlers::{self as h, ALT, MODE, OWN, RADIO};
use common::until;

const APRS: &str = "aprs_decode";
const APRS_COPY: &str = "aprs_copy";
const APRS_CENTER_HZ: u64 = 144_800_000;
const APRS_BAND_HZ: u64 = 12_500;
const APRS_RATE_HZ: u32 = 48_000;
const ADSB_CENTER_HZ: u64 = 1_090_000_000;
const ADSB_RATE_HZ: u32 = 2_400_000;
/// A fixed past instant the first capture of a set starts at.
const T0_NS: u64 = 1_760_000_000_000_000_000;
/// Between the starts of two captures of a set.
const GAP_NS: u64 = 2_000_000_000;

/// A node on the plan 42 handler fixtures (`adsb_decode` with `dump1090` and `alt`, the
/// `external_decode` stand-in, every DSP stage) and its radio.
async fn open(dir: &Path) -> (Node, Arc<h::Radio>) {
    let radio = h::Radio::new();
    let ports: Vec<Arc<dyn Port>> = vec![radio.clone()];
    let node = Node::open(h::cfg(dir, false), h::stages(None), ports)
        .await
        .unwrap();
    (node, radio)
}

/// `iq` written as one capture of set `set`, recorded on [`RADIO`] from `start_ns` for
/// as long as it lasts at `rate_hz`; its id.
fn write_capture(
    node: &Node,
    set: &str,
    (center_hz, bandwidth_hz, rate_hz): (u64, u64, u32),
    start_ns: u64,
    iq: &[num_complex::Complex<f32>],
) -> String {
    let end_ns = start_ns + iq.len() as u64 * 1_000_000_000 / rate_hz as u64;
    node.bursts()
        .write(
            NewCapture {
                source: SourceId(RADIO.into()),
                set: JobId(set.into()),
                center_hz: Hz(center_hz),
                bandwidth_hz: Hz(bandwidth_hz),
                sample_rate_hz: Hz(rate_hz as u64),
                start_ns,
                end_ns,
                features: json!({}),
                signal: None,
                job: JobId(set.into()),
                watch: None,
                allocations: json!([]),
            },
            iq,
        )
        .unwrap()
        .id
}

/// A set of `n` captures, each one synthesised APRS packet at 144.8 MHz, 12.5 kHz wide
/// at 48 kHz, starting [`GAP_NS`] apart from [`T0_NS`]; their ids by start time.
fn write_aprs(node: &Node, n: usize) -> Vec<String> {
    (0..n)
        .map(|i| {
            let iq = zeta_dsp::synth::aprs_packet_iq(
                "N0CALL",
                "APZETA",
                &format!(">replay {i}"),
                APRS_RATE_HZ,
                0.0,
            );
            write_capture(
                node,
                "aprs",
                (APRS_CENTER_HZ, APRS_BAND_HZ, APRS_RATE_HZ),
                T0_NS + i as u64 * GAP_NS,
                &iq,
            )
        })
        .collect()
}

/// A set of `n` captures of 500 ms of silence at 1090 MHz, 2.4 MHz wide at 2.4 MHz,
/// which `adsb_decode`'s handlers take; their ids by start time.
fn write_adsb(node: &Node, n: usize) -> Vec<String> {
    write_adsb_set(node, "adsb", n)
}

/// [`write_adsb`] as the set `set`.
fn write_adsb_set(node: &Node, set: &str, n: usize) -> Vec<String> {
    let iq = vec![num_complex::Complex::new(0.0f32, 0.0); ADSB_RATE_HZ as usize / 2];
    (0..n)
        .map(|i| {
            write_capture(
                node,
                set,
                (ADSB_CENTER_HZ, ADSB_RATE_HZ as u64, ADSB_RATE_HZ),
                T0_NS + i as u64 * GAP_NS,
                &iq,
            )
        })
        .collect()
}

async fn decode(
    node: &Node,
    capture: &str,
    kind: &str,
    handler: Option<&str>,
    packets: bool,
) -> Result<OpResult, OpError> {
    node.apply(Op::CaptureDecode {
        capture: capture.into(),
        kind: kind.into(),
        handler: handler.map(String::from),
        packets,
    })
    .await
}

/// The job a start answered.
fn started(r: Result<OpResult, OpError>) -> JobId {
    match r {
        Ok(OpResult::Job(id)) => id,
        r => panic!("a job: {r:?}"),
    }
}

/// Subscribed to the node's `replay` feed.
fn replay_feed(node: &Node) -> broadcast::Receiver<Frame> {
    let bus = node.bus();
    let feed = bus
        .feed_of(&SourceId(REPLAY_SOURCE.into()))
        .expect("the node registers the replay feed when it opens");
    bus.subscribe(&feed).unwrap()
}

fn event_of(f: &Frame) -> Option<&Value> {
    match &f.payload {
        Payload::Event(v) => Some(v),
        _ => None,
    }
}

/// The next decoded event (one carrying both `replay` and `handler`; `capture_in`'s
/// `segment` events have no `handler`) on `rx`, within `within`.
async fn envelope(rx: &mut broadcast::Receiver<Frame>, within: Duration) -> Frame {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let f = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .expect("a decoded event within the time")
            .expect("the feed stays open");
        let Some(ev) = event_of(&f) else { continue };
        if ev["protocol"] != "segment" && ev.get("replay").is_some() && ev.get("handler").is_some()
        {
            return f;
        }
    }
}

fn record(node: &Node, id: &JobId) -> JobRecord {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .expect("job listed")
}

async fn until_stopped(node: &Node, id: &JobId, within: Duration) {
    until("the replay ends stopped", within, || {
        record(node, id).state == JobState::Stopped
    })
    .await;
}

fn t_ns(f: &Frame) -> u64 {
    event_of(f).unwrap()["t_ns"].as_u64().expect("t_ns")
}

#[tokio::test]
async fn a_capture_replays_into_its_mode_and_the_events_carry_the_recording() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let ids = write_aprs(&node, 1);
    let mut feed = replay_feed(&node);
    let id = started(decode(&node, &ids[0], APRS, None, false).await);
    let f = envelope(&mut feed, Duration::from_secs(15)).await;
    let ev = event_of(&f).unwrap();
    assert_eq!(ev["replay"], ids[0].as_str(), "{ev}");
    assert_eq!(ev["handler"], "builtin", "{ev}");
    assert_eq!(ev["src"], "N0CALL", "{ev}");
    assert!(ev["summary"].is_string(), "{ev}");
    let cap = node.bursts().get(&ids[0]).unwrap();
    assert!(
        (cap.start_ns..=cap.end_ns).contains(&t_ns(&f)),
        "{} outside {}..={}",
        t_ns(&f),
        cap.start_ns,
        cap.end_ns
    );
    assert_eq!(f.provenance.source, SourceId(REPLAY_SOURCE.into()));
    assert_eq!(f.provenance.job, Some(id.clone()));
    let rec = record(&node, &id);
    assert_eq!(rec.kind, APRS);
    assert_eq!(rec.source, SourceId("node".into()));
    assert_eq!(rec.replay.as_deref(), Some(ids[0].as_str()));
    assert!(rec.operator_graph);
    assert_eq!(
        rec.tuning,
        Tuning {
            center_hz: Hz(APRS_CENTER_HZ),
            bandwidth_hz: Hz(APRS_BAND_HZ),
        }
    );
    until_stopped(&node, &id, Duration::from_secs(15)).await;
}

#[tokio::test]
async fn the_whole_set_is_replayed_whichever_capture_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let ids = write_aprs(&node, 3);
    let mut feed = replay_feed(&node);
    let id = started(decode(&node, &ids[1], APRS, None, false).await);
    let mut times = Vec::new();
    for _ in 0..3 {
        let f = envelope(&mut feed, Duration::from_secs(15)).await;
        assert_eq!(event_of(&f).unwrap()["replay"], ids[1].as_str());
        times.push(t_ns(&f));
    }
    assert!(times.is_sorted(), "{times:?}");
    for (cap, t) in ids.iter().zip(&times) {
        let cap = node.bursts().get(cap).unwrap();
        assert!((cap.start_ns..=cap.end_ns).contains(t), "{t} in {cap:?}");
    }
    let rec = record(&node, &id);
    let replay = &rec.context["replay"];
    assert_eq!(replay["captures"], json!(ids), "{replay}");
    assert_eq!(replay["capture"], ids[1].as_str(), "{replay}");
    assert_eq!(replay["source"], RADIO, "{replay}");
    let first = node.bursts().get(&ids[0]).unwrap();
    let last = node.bursts().get(&ids[2]).unwrap();
    assert_eq!(replay["start_ns"], first.start_ns, "{replay}");
    assert_eq!(replay["end_ns"], last.end_ns, "{replay}");
    until_stopped(&node, &id, Duration::from_secs(15)).await;
}

/// What `source_list` says of the radio: its band, holders and elastic jobs.
async fn radio_state(node: &Node) -> (Option<Tuning>, Vec<JobId>, Vec<JobId>) {
    let Ok(OpResult::Sources(list)) = node.apply(Op::SourceList {}).await else {
        panic!("source_list")
    };
    let s: SourceInfo = list
        .into_iter()
        .find(|s| s.id.0 == RADIO)
        .expect("the radio is listed");
    (s.current, s.holders, s.elastic)
}

#[tokio::test]
async fn a_replay_holds_no_lease_and_tunes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio) = open(dir.path()).await;
    let ids = write_aprs(&node, 1);
    let before = (radio.tunes(), radio_state(&node).await);
    let mut feed = replay_feed(&node);
    let id = started(decode(&node, &ids[0], APRS, None, false).await);
    envelope(&mut feed, Duration::from_secs(15)).await;
    assert_eq!(record(&node, &id).state, JobState::Running);
    assert_eq!((radio.tunes(), radio_state(&node).await), before);
    until_stopped(&node, &id, Duration::from_secs(15)).await;
    assert_eq!((radio.tunes(), radio_state(&node).await), before);
}

#[tokio::test]
async fn a_replay_stays_out_of_the_radios_feed_and_the_event_log() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let ids = write_aprs(&node, 1);
    let bus = node.bus();
    let radio_feed = bus.feed_of(&SourceId(RADIO.into())).unwrap();
    let mut on_radio = bus.subscribe(&radio_feed).unwrap();
    let mut feed = replay_feed(&node);
    let id = started(decode(&node, &ids[0], APRS, None, false).await);
    envelope(&mut feed, Duration::from_secs(15)).await;
    until_stopped(&node, &id, Duration::from_secs(15)).await;
    // The radio's feed heard nothing of the job.
    loop {
        match on_radio.try_recv() {
            Ok(f) => assert_ne!(f.provenance.job, Some(id.clone()), "{f:?}"),
            Err(broadcast::error::TryRecvError::Empty) => break,
            Err(e) => panic!("radio feed: {e}"),
        }
    }
    // And `events.log` (the record of what was received) has no line of it.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let log =
        std::fs::read_to_string(dir.path().join("state").join("events.log")).unwrap_or_default();
    for line in log.lines() {
        let l: Value = serde_json::from_str(line).unwrap();
        assert_ne!(l["job"], id.0.as_str(), "{line}");
        assert!(l["event"].get("replay").is_none(), "{line}");
    }
}

#[tokio::test]
async fn an_unknown_capture_is_refused_with_the_known_ones() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let ids = write_aprs(&node, 1);
    let r = decode(&node, "01NOSUCHCAPTURE", APRS, None, false).await;
    assert_eq!(
        r,
        Err(OpError::Unknown {
            field: "capture".into(),
            value: "01NOSUCHCAPTURE".into(),
            known: ids,
        })
    );
    assert!(node.job_list().is_empty());
}

#[tokio::test]
async fn a_kind_that_is_no_mode_row_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let ids = write_aprs(&node, 1);
    for kind in ["bits", "fm_demod"] {
        let r = decode(&node, &ids[0], kind, None, false).await;
        assert_eq!(
            r,
            Err(OpError::Other {
                message: format!("{kind} has no handlers"),
            })
        );
    }
    assert!(node.job_list().is_empty());
}

#[tokio::test]
async fn a_mode_the_recording_cannot_hold_is_refused_before_anything_starts() {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio) = open(dir.path()).await;
    let ids = write_aprs(&node, 1);
    // 12.5 kHz recorded, under the 2 MHz ADS-B's handlers need.
    let r = decode(&node, &ids[0], MODE, None, false).await;
    match r {
        Err(OpError::OutOfRange {
            ref field,
            value,
            min,
            ..
        }) => assert_eq!(
            (field.as_str(), value, min),
            ("bandwidth_hz", APRS_BAND_HZ, 2_000_000)
        ),
        r => panic!("the expansion's band error: {r:?}"),
    }
    assert!(node.job_list().is_empty());
    assert_eq!(radio.tunes(), 0);
}

#[tokio::test]
async fn a_named_handler_runs_the_replay() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let ids = write_adsb(&node, 1);
    let id = started(decode(&node, &ids[0], MODE, Some(ALT), false).await);
    let rec = record(&node, &id);
    assert_eq!(
        (rec.handler.as_deref(), rec.handler_named),
        (Some(ALT), true)
    );
    assert_eq!(rec.kind, MODE);
    assert_eq!(rec.replay.as_deref(), Some(ids[0].as_str()));
    let decoder = rec
        .graph
        .nodes
        .iter()
        .find(|n| n.kind == "external_decode")
        .expect("an external_decode node");
    assert_eq!(decoder.params["handler"], ALT);
    let root = rec
        .graph
        .nodes
        .iter()
        .find(|n| n.kind == "capture_in")
        .expect("a capture_in node");
    assert_eq!(
        root.params,
        json!({"captures": ids, "loop": false, "realtime": true, "lead_ms": 0})
    );
    node.job_stop(&id).await.unwrap();
    // The policy's pick, when none is named.
    let id = started(decode(&node, &ids[0], MODE, None, false).await);
    let rec = record(&node, &id);
    assert_eq!(
        (rec.handler.as_deref(), rec.handler_named),
        (Some(OWN), false)
    );
    node.job_stop(&id).await.unwrap();
}

#[tokio::test]
async fn a_second_replay_of_the_same_capture_mode_and_handler_is_a_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let ids = write_adsb(&node, 3);
    let first = started(decode(&node, &ids[0], MODE, None, false).await);
    let r = decode(&node, &ids[0], MODE, Some(OWN), false).await;
    assert_eq!(
        r,
        Err(OpError::Duplicate {
            job: first.clone(),
            message: format!(
                "capture {} is already replaying as {MODE} via {OWN}: job {}",
                ids[0], first.0
            ),
        })
    );
    // Another capture of the set is the same recording: a duplicate too, named as asked.
    let r = decode(&node, &ids[1], MODE, None, false).await;
    assert_eq!(
        r,
        Err(OpError::Duplicate {
            job: first.clone(),
            message: format!(
                "capture {} is already replaying as {MODE} via {OWN}: job {}",
                ids[1], first.0
            ),
        })
    );
    assert_eq!(record(&node, &first).state, JobState::Running);
    let other = started(decode(&node, &ids[0], MODE, Some(ALT), false).await);
    assert_eq!(node.job_list().len(), 2);
    node.job_stop(&first).await.unwrap();
    node.job_stop(&other).await.unwrap();
    // Once the first has ended, the same replay starts again.
    let again = started(decode(&node, &ids[0], MODE, None, false).await);
    node.job_stop(&again).await.unwrap();
}

#[tokio::test]
async fn a_replay_cannot_be_sent_from() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let ids = write_adsb(&node, 3);
    let id = started(decode(&node, &ids[0], MODE, Some(OWN), false).await);
    let rec = record(&node, &id);
    assert_eq!(rec.state, JobState::Running);
    // Asked as Send to asks: the replay's own kind, source and band.
    let op = json!({
        "op": "job_start", "kind": MODE, "source": rec.source.0,
        "center_hz": rec.tuning.center_hz.0, "bandwidth_hz": rec.tuning.bandwidth_hz.0,
        "deviation_hz": null, "sideband": null, "elastic": false, "replace": [],
        "handler": ALT, "needs": [], "beside": id.0,
    });
    assert_eq!(
        node.apply(serde_json::from_value(op).unwrap()).await,
        Err(OpError::Other {
            message: format!("job {} cannot be sent from: it is a replay", id.0),
        })
    );
    assert_eq!(node.job_list().len(), 1, "nothing started");
    assert_eq!(record(&node, &id).group, None, "no group written");
    node.job_stop(&id).await.unwrap();
}

#[tokio::test]
async fn a_finished_replay_notes_what_it_decoded() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let ids = write_aprs(&node, 3);
    let id = started(decode(&node, &ids[0], APRS, None, false).await);
    until_stopped(&node, &id, Duration::from_secs(20)).await;
    let rec = record(&node, &id);
    let replay = &rec.context["replay"];
    assert_eq!(replay["events"], 3, "{}", rec.context);
    // The rest of what the replay noted at its start is kept.
    assert_eq!(replay["captures"], json!(ids), "{replay}");
    assert_eq!(replay["source"], RADIO, "{replay}");
    assert_eq!(
        rec.context["note"],
        format!("replayed 3 capture(s) from {RADIO}: 3 event(s)"),
        "{}",
        rec.context
    );
}

#[tokio::test]
async fn a_replay_that_decodes_nothing_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let mut iq = vec![num_complex::Complex::new(0.0f32, 0.0); APRS_RATE_HZ as usize];
    // A second of unit-power noise.
    zeta_dsp::synth::awgn(&mut iq, 0.0, 7);
    let cap = write_capture(
        &node,
        "noise",
        (APRS_CENTER_HZ, APRS_BAND_HZ, APRS_RATE_HZ),
        T0_NS,
        &iq,
    );
    let id = started(decode(&node, &cap, APRS, None, false).await);
    until_stopped(&node, &id, Duration::from_secs(20)).await;
    let rec = record(&node, &id);
    assert_eq!(rec.state, JobState::Stopped);
    assert_eq!(rec.context["replay"]["events"], 0, "{}", rec.context);
    assert_eq!(
        rec.context["note"],
        format!("replayed 1 capture(s) from {RADIO}: 0 event(s)"),
        "{}",
        rec.context
    );
}

#[tokio::test]
async fn a_handler_that_never_exits_is_given_eight_seconds_and_the_job_still_stops() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    // 500 ms of recording, into the stand-in decoder that ends only when cancelled.
    let ids = write_adsb(&node, 1);
    let cap = node.bursts().get(&ids[0]).unwrap();
    let length = Duration::from_nanos(cap.end_ns - cap.start_ns);
    let mut feed = replay_feed(&node);
    let id = started(decode(&node, &ids[0], MODE, None, false).await);
    // `capture_in` ends its pause after the last block once the recording has played
    // from its `segment` event.
    let played = loop {
        let f = tokio::time::timeout(Duration::from_secs(10), feed.recv())
            .await
            .expect("the segment event in time")
            .unwrap();
        if event_of(&f).is_some_and(|ev| ev["protocol"] == "segment") {
            break tokio::time::Instant::now();
        }
    };
    let pause = Duration::from_millis(zeta_dsp::capture_in::LOOP_PAUSE_MS_DEFAULT);
    until_stopped(&node, &id, Duration::from_secs(20)).await;
    let after_end = played.elapsed().saturating_sub(length + pause);
    assert!(
        (Duration::from_millis(2500)..Duration::from_secs(10)).contains(&after_end),
        "stopped {after_end:?} after the recording ended"
    );
    let rec = record(&node, &id);
    assert_eq!(rec.state, JobState::Stopped);
    assert!(
        rec.context["note"]
            .as_str()
            .is_some_and(|n| n.starts_with("replayed 1 capture(s) from ")),
        "{}",
        rec.context
    );
}

#[tokio::test]
async fn two_replays_of_one_capture_keep_one_finished_record_per_handler() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let a = write_adsb_set(&node, "a", 1);
    let b = write_adsb_set(&node, "b", 1);
    for (cap, handler) in [
        (&a[0], None),
        (&a[0], None),
        (&a[0], Some(ALT)),
        (&b[0], None),
    ] {
        let id = started(decode(&node, cap, MODE, handler, false).await);
        node.job_stop(&id).await.unwrap();
    }
    let finished = |cap: &str| -> Vec<Option<String>> {
        let mut hs: Vec<_> = node
            .job_list()
            .into_iter()
            .filter(|r| r.replay.as_deref() == Some(cap) && r.state == JobState::Stopped)
            .map(|r| r.handler)
            .collect();
        hs.sort();
        hs
    };
    // The second run of `a` by its own handler took the first's place; `alt`'s run and
    // the run of another recording on the same band each keep theirs.
    assert_eq!(
        finished(&a[0]),
        [Some(ALT.to_string()), Some(OWN.to_string())]
    );
    assert_eq!(finished(&b[0]), [Some(OWN.to_string())]);
}

/// A port of id `id`, never fed.
struct Named {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tx: broadcast::Sender<IqBlock>,
}

impl Named {
    fn new(id: &str) -> Arc<Self> {
        Arc::new(Self {
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
                usable_fraction: Claim::Asserted(1.0),
                depths: vec![],
                sweep: None,
            },
            cur: Mutex::new(None),
            tx: broadcast::channel(1).0,
        })
    }
}

#[async_trait]
impl Port for Named {
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

#[tokio::test]
async fn a_source_may_not_be_named_replay() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    for id in ["replay", "node"] {
        let e = node.add_port(Named::new(id)).unwrap_err();
        assert_eq!(
            e.to_string(),
            format!("source id \"{id}\" is reserved for the node's own streams"),
        );
        assert!(node.ports().iter().all(|(s, _)| s.0 != id));
    }
    // The replay feed is still the node's own.
    assert!(
        node.bus()
            .feed_of(&SourceId(REPLAY_SOURCE.into()))
            .is_some()
    );
}

/// An accepted signature on the APRS capture's band, put in the store directly (as
/// `tests/proposals.rs` makes one through a classifier).
fn accepted_signature(node: &Node) -> zeta_core::signatures::SignatureRecord {
    use zeta_core::signatures::NewSignature;
    let features = serde_json::from_value(json!({
        "bandwidth_hz": APRS_BAND_HZ, "center_hz": APRS_CENTER_HZ, "symbol_rate_baud": 1_200,
        "snr_db": 20.0, "burst": true, "duty": 0.5, "bursts": 2, "burst_ms": 50.0,
        "period_ms": 100.0, "order": 2, "tone_spacing_hz": 1_000, "symbol_rate_line": null,
    }))
    .unwrap();
    let new = NewSignature {
        name: None,
        features,
        modulation: "fsk".into(),
        framing: None,
        crc_frames: 0,
        evidence: "test".into(),
        source: SourceId(RADIO.into()),
        station: None,
        captures: Vec::new(),
        jobs: Vec::new(),
        allocation: None,
        proposed_by: "test".into(),
    };
    let sig = node.signatures().propose(new, T0_NS).unwrap();
    node.signatures()
        .accept(&sig.id, None, "test", T0_NS)
        .unwrap()
}
/// The embedded APRS row as a second mode row, `aprs_copy`: a mode id (the built-in
/// `aprs_decode` kind has no row watch), written to `dir`'s modes directory for [`open`].
fn write_aprs_copy(dir: &Path) {
    let (_, text) = zeta_core::modes::ModeRegistry::embedded()
        .into_iter()
        .find(|(f, _)| f.ends_with("aprs_decode.json"))
        .expect("embedded aprs_decode");
    let mut row: Value = serde_json::from_str(text).unwrap();
    row["id"] = APRS_COPY.into();
    let modes = dir.join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    std::fs::write(modes.join(format!("{APRS_COPY}.json")), row.to_string()).unwrap();
}

#[tokio::test]
async fn a_replay_supersedes_no_signature() {
    let dir = tempfile::tempdir().unwrap();
    write_aprs_copy(dir.path());
    let (node, _radio) = open(dir.path()).await;
    let sig = accepted_signature(&node);
    let before = node.signatures().list(None);
    let ids = write_aprs(&node, 2);
    let mut feed = replay_feed(&node);
    let id = started(decode(&node, &ids[0], APRS_COPY, None, false).await);
    // A decoded frame proves the row, which a live job's would let supersede.
    let f = envelope(&mut feed, Duration::from_secs(15)).await;
    assert_eq!(event_of(&f).unwrap()["fcs_ok"], true);
    until_stopped(&node, &id, Duration::from_secs(20)).await;
    let after = node.signatures().list(None);
    assert_eq!(after, before, "{after:?}");
    assert_eq!(after[0].id, sig.id);
}

const FRAME_ROW: &str = "fsk_frames";
const FRAME_CENTER_HZ: u64 = 433_920_000;
const FRAME_RATE_HZ: u32 = 48_000;

/// The framing of the frame row below: a 16-bit sync, a fixed 272-bit frame, a CRC-16.
fn framing() -> Value {
    json!({"sync": {"hex": "2dd4", "bits": 16}, "length": {"fixed_bits": 272}, "crc": "crc16-ccitt"})
}

/// A frame row (`decoder.kind: frame`, so it keeps a session) whose subject counts every
/// CRC-clean frame as an anonymous station, written to `dir`'s modes directory for [`open`].
fn write_frame_row(dir: &Path) {
    let row = json!({
        "id": FRAME_ROW, "name": "FSK test frames", "family": "general",
        "band": {"bandwidth_hz": APRS_BAND_HZ, "centers_hz": [FRAME_CENTER_HZ]},
        "modulation": ["fsk", "fm"], "label": "FSK frames", "protocol": "frame",
        "decoder": {
            "kind": "frame",
            "demod": {"modulation": "fsk", "symbol_rate_baud": 4_800, "order": 2, "tone_spacing_hz": null},
            "framing": framing(),
            "parser": "opaque"
        },
        "event": {
            "schema": {"type": "object", "properties": {
                "protocol": {"type": "string"}, "sync": {"type": "string"},
                "length_bytes": {"type": "integer"},
                "crc_ok": {"type": "boolean"}
            }},
            "summary": "frame {length_bytes} B"
        },
        "subject": {
            "station": {"when": {"crc_ok": true}, "key": null, "addressed": null},
            "key": "sync", "name": "frame", "waterfall": "summary",
            "columns": [{"field": "length_bytes", "label": "Bytes", "unit": null}]
        },
        "identify": {"proof": "field", "field": "crc_ok", "min_events": 1, "dwell_ms": 10000,
            "min_span_ms": 1000, "quick": "frame"},
        "catalogue": []
    });
    let modes = dir.join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    std::fs::write(modes.join(format!("{FRAME_ROW}.json")), row.to_string()).unwrap();
}

#[tokio::test]
async fn a_replay_keeps_no_session_and_tallies_no_station() {
    let dir = tempfile::tempdir().unwrap();
    write_frame_row(dir.path());
    let (node, _radio) = open(dir.path()).await;
    // Eight frames of the row's framing, as `tests/frame_rows.rs` synthesises them.
    let params = json!({"family": "framing", "modulation": "fsk", "symbol_rate_baud": 4_800,
        "deviation_hz": 2400.0, "frames": 8, "body_bytes": 32, "seed": 7,
        "framing": framing(), "repeat": 1, "snr_db": 20.0});
    let (spec, imp) = zeta_dsp::synth::parse_params(params.as_object().unwrap().clone()).unwrap();
    let iq = zeta_dsp::synth::synthesise(&spec, &imp, FRAME_RATE_HZ, FRAME_CENTER_HZ);
    let cap = write_capture(
        &node,
        "frames",
        (FRAME_CENTER_HZ, APRS_BAND_HZ, FRAME_RATE_HZ),
        T0_NS,
        &iq,
    );
    let before = node.sessions().len();
    let mut feed = replay_feed(&node);
    let id = started(decode(&node, &cap, FRAME_ROW, None, false).await);
    // The replay decodes: a frame the row counts as a station's and proves.
    let f = envelope(&mut feed, Duration::from_secs(20)).await;
    assert_eq!(event_of(&f).unwrap()["crc_ok"], true);
    until_stopped(&node, &id, Duration::from_secs(30)).await;
    assert_eq!(node.sessions().len(), before, "a replay keeps a session");
    let rec = record(&node, &id);
    assert!(
        rec.context.get("stations").is_none(),
        "a replay tallies a station: {}",
        rec.context
    );
}

#[tokio::test]
async fn graph_put_of_a_replay_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let ids = write_aprs(&node, 1);
    let id = started(decode(&node, &ids[0], APRS, None, false).await);
    let graph = record(&node, &id).graph;
    let r = node
        .apply(Op::GraphPut {
            job: id.clone(),
            graph,
        })
        .await;
    match r {
        Err(OpError::Other { message: m }) => assert_eq!(m, "a replay's graph is its mode's"),
        r => panic!("refused: {r:?}"),
    }
    node.job_stop(&id).await.ok();
}

#[tokio::test]
async fn a_replay_with_packets_captures_every_frame() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let ids = write_aprs(&node, 3);
    let id = started(decode(&node, &ids[0], APRS, None, true).await);
    until_stopped(&node, &id, Duration::from_secs(30)).await;
    until("the packet capture closes", Duration::from_secs(10), || {
        let l = node.packets().list();
        l.len() == 1 && !l[0].writing && l[0].packets == 3
    })
    .await;
    let list = node.packets().list();
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0].job, id);
    assert_eq!(list[0].packets, 3);
    let bytes = std::fs::read(&list[0].path).unwrap();
    let whole = zeta_core::pcapng::whole_blocks(&bytes);
    let (_, packets) = zeta_core::pcapng::read(&bytes[..whole]).unwrap();
    assert_eq!(packets.len(), 3);
    let spans: Vec<_> = ids.iter().map(|c| node.bursts().get(c).unwrap()).collect();
    for p in &packets {
        assert!(
            spans
                .iter()
                .any(|c| (c.start_ns..=c.end_ns).contains(&p.t_ns)),
            "{} outside every capture",
            p.t_ns
        );
    }
}

#[tokio::test]
async fn packets_on_a_mode_that_gives_none_is_refused_and_nothing_starts() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let ids = write_adsb(&node, 1);
    let Err(e) = decode(&node, &ids[0], MODE, None, true).await else {
        panic!("refused")
    };
    let OpError::NoHandler { passed, .. } = &e else {
        panic!("no handler: {e:?}")
    };
    assert!(
        passed.iter().all(|p| p.why.contains("outputs no packet")),
        "{passed:?}"
    );
    assert!(!passed.is_empty(), "{passed:?}");
    assert!(node.job_list().is_empty());
    assert!(node.packets().list().is_empty());
}

#[tokio::test]
async fn a_replay_without_packets_has_no_lead() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let ids = write_aprs(&node, 1);
    let mut feed = replay_feed(&node);
    let id = started(decode(&node, &ids[0], APRS, None, false).await);
    let params = &record(&node, &id).graph.nodes[0].params;
    assert_eq!(params["lead_ms"], 0, "{params}");
    // capture_in's first frame (a segment event) comes at once.
    tokio::time::timeout(Duration::from_millis(200), feed.recv())
        .await
        .expect("a frame within 200 ms")
        .unwrap();
    until_stopped(&node, &id, Duration::from_secs(15)).await;
}
