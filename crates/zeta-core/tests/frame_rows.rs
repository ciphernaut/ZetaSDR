//! Frame rows (`decoder.kind: frame`): a row naming a demodulator, a framing and a parser
//! expands to `channel → resample → demod → clock_recover → slicer → frame_decode`,
//! decodes as a job, proves in `classify` through the `frame` quick attempt and in
//! `identify`. Run over a 48 kHz file source of the `framing` synthesiser family.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::{Value, json};
use zeta_core::capture::{CaptureMeta, write_cf32};
use zeta_core::classify::Outcome;
use zeta_core::job::JobState;
use zeta_core::modes::{Decoder, DecoderKind, ModeRegistry, parse, template, validate};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpResult};
use zeta_core::port::{Port, Tuning};
use zeta_core::stage::{Backend, StageRegistry};
use zeta_core::stream::Payload;
use zeta_core::types::{Hz, JobId, Selection, SourceId, Span, StreamKind, Timestamp};
use zeta_source_file::{FileOpts, FileSource};

const ROW_ID: &str = "fsk_test_frames";
const CENTER_HZ: u64 = 433_920_000;
const BANDWIDTH_HZ: u64 = 12_500;
const RATE_HZ: u32 = 48_000;
const BAUD: u32 = 4_800;
/// The byte length of a frame: `length.fixed_bits` 272 over 8.
const FRAME_BYTES: u64 = 34;

/// Each node runs a real-time file source: the node tests run one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The row's framing: a 16-bit sync, a fixed 272-bit frame, a CRC-16/CCITT.
fn framing() -> Value {
    json!({"sync": {"hex": "2dd4", "bits": 16}, "length": {"fixed_bits": 272}, "crc": "crc16-ccitt"})
}

/// The frame row of these tests.
fn row_json() -> Value {
    json!({
        "id": ROW_ID,
        "name": "FSK test frames",
        "family": "general",
        "band": {"bandwidth_hz": BANDWIDTH_HZ, "centers_hz": [CENTER_HZ]},
        "modulation": ["fsk", "fm"],
        "label": "FSK frames",
        "protocol": "frame",
        "decoder": {
            "kind": "frame",
            "demod": {"modulation": "fsk", "symbol_rate_baud": BAUD, "order": 2, "tone_spacing_hz": null},
            "framing": framing(),
            "parser": "opaque"
        },
        "event": {
            "schema": {"type": "object", "properties": {
                "protocol": {"type": "string"},
                "sync": {"type": "string"},
                "length_bytes": {"type": "integer"},
                "bytes": {"type": "string"},
                "crc_ok": {"type": "boolean"},
                "payload": {"type": "string"}
            }},
            "summary": "frame {length_bytes} B"
        },
        "identify": {"proof": "field", "field": "crc_ok", "min_events": 1, "dwell_ms": 10000,
            "min_span_ms": 1000, "quick": "frame"},
        "catalogue": []
    })
}

/// [`row_json`] with `edit` applied.
fn edited(edit: impl FnOnce(&mut Value)) -> String {
    let mut v = row_json();
    edit(&mut v);
    v.to_string()
}

fn stages() -> StageRegistry {
    let mut r = StageRegistry::new();
    zeta_dsp::register_all(&mut r);
    r
}

/// The `framing` family's capture (Task 3's spec: 8 frames of 32 pseudo-random bytes at
/// 4800 Bd fsk, 2400 Hz deviation, 20 dB), sampled at 48 kHz and labelled 433.92 MHz,
/// written once.
fn capture() -> &'static Path {
    static F: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    &F.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let params = json!({"family": "framing", "modulation": "fsk", "symbol_rate_baud": BAUD,
            "deviation_hz": 2400.0, "frames": 8, "body_bytes": 32, "seed": 7,
            "framing": framing(), "repeat": 1, "snr_db": 20.0});
        let (spec, imp) =
            zeta_dsp::synth::parse_params(params.as_object().unwrap().clone()).unwrap();
        let iq = zeta_dsp::synth::synthesise(&spec, &imp, RATE_HZ, CENTER_HZ);
        let meta = CaptureMeta {
            sample_rate_hz: Hz(RATE_HZ as u64),
            center_hz: Hz(CENTER_HZ),
            start: Timestamp(0),
            annotations: Vec::new(),
        };
        let path = write_cf32(dir.path(), "frames", &meta, &iq).unwrap();
        (dir, path)
    })
    .1
}

fn rx() -> SourceId {
    SourceId("rx".into())
}

/// A node over a looping real-time file source of [`capture`], the row file in
/// `modes_dir`, with zeta-dsp's classifiers (as `tests/modes.rs`'s `cfg`, with a ring
/// long enough for a 1 s span).
async fn open(dir: &Path, modes_dir: &Path) -> Node {
    std::fs::create_dir_all(modes_dir).unwrap();
    std::fs::write(
        modes_dir.join(format!("{ROW_ID}.json")),
        row_json().to_string(),
    )
    .unwrap();
    let cfg = NodeConfig {
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        ring_seconds: 4.0,
        ring_budget_bytes: 64 << 20,
        scan_dwell_ms: 20,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory: None,
        modes_dir: Some(modes_dir.to_path_buf()),
    };
    let opts = FileOpts {
        realtime: true,
        block_samples: 2_400,
        repeat: true,
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(rx(), capture(), opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    let node = Node::open(cfg, stages(), vec![port]).await.unwrap();
    for c in zeta_dsp::classifiers(node.modes()) {
        node.add_classifier(c);
    }
    src.start();
    node
}

/// Poll `f` every 20 ms until it holds, panicking with `what` after `within`.
async fn until(what: &str, within: Duration, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn state(node: &Node, id: &JobId) -> JobState {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .expect("job listed")
        .state
}

async fn start(node: &Node, kind: JobKind) -> JobId {
    let op = Op::JobStart {
        kind,
        source: rx(),
        center_hz: Hz(CENTER_HZ),
        bandwidth_hz: Hz(BANDWIDTH_HZ),
        deviation_hz: None,
        sideband: None,
        elastic: false,
        replace: vec![],
    };
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("job_start: {r:?}"),
    }
}

#[test]
fn a_frame_row_parses_validates_and_lists_its_kind() {
    let row = parse(&row_json().to_string()).unwrap();
    validate(&row, &stages()).unwrap();
    assert_eq!(row.decoder.kind(), DecoderKind::Frame);
    let Decoder::Frame(block) = &row.decoder else {
        panic!("a frame row");
    };
    assert_eq!(block.framing, framing());
    assert_eq!(block.parser, "opaque");
    // `parser` defaults to `opaque`; `order` to 2 and `tone_spacing_hz` to null.
    let text = edited(|v| {
        v["decoder"].as_object_mut().unwrap().remove("parser");
        let demod = v["decoder"]["demod"].as_object_mut().unwrap();
        demod.remove("order");
        demod.remove("tone_spacing_hz");
    });
    let again = parse(&text).unwrap();
    assert_eq!(again, row);
    // The listing reads `frame`.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("frames.json"), row_json().to_string()).unwrap();
    let reg = ModeRegistry::load(&stages(), Some(dir.path()));
    let m = reg.get(ROW_ID).expect("listed");
    assert_eq!(m.reason, None);
    let info = zeta_core::ops::ModeInfo::from(m);
    let v = serde_json::to_value(&info).unwrap();
    assert_eq!(v["decoder_kind"], "frame", "{v}");
    assert_eq!(v["state"], "ready", "{v}");
}

#[test]
fn a_bad_framing_or_parser_names_the_field() {
    let check = |text: String| validate(&parse(&text).unwrap(), &stages()).unwrap_err();
    let e = check(edited(|v| {
        v["decoder"]["framing"]["sync"]["bits"] = 70.into()
    }));
    assert_eq!(e.field, "decoder.framing.sync.bits", "{e}");
    let e = check(edited(|v| v["decoder"]["parser"] = "modbus_rtu".into()));
    assert_eq!(e.field, "decoder.parser", "{e}");
    assert!(e.error.contains("modbus_rtu"), "{e}");
    // `quick: frame` belongs to a frame row only.
    let (_, pocsag) = ModeRegistry::embedded()
        .into_iter()
        .find(|(f, _)| f.ends_with("pocsag_decode.json"))
        .unwrap();
    let mut v: Value = serde_json::from_str(pocsag).unwrap();
    v["identify"]["quick"] = "frame".into();
    let e = check(v.to_string());
    assert_eq!(
        e.to_string(),
        "identify.quick: frame needs decoder.kind frame"
    );
}

#[test]
fn a_frame_row_expands_to_the_chain() {
    let row = parse(&row_json().to_string()).unwrap();
    let tuning = Tuning {
        center_hz: Hz(CENTER_HZ),
        bandwidth_hz: Hz(BANDWIDTH_HZ),
    };
    template::check_band(&row, tuning, RATE_HZ as u64).unwrap();
    let g = template::expand(&row, tuning, RATE_HZ as u64).unwrap();
    let kinds: Vec<&str> = g.nodes.iter().map(|n| n.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "channel",
            "resample",
            "fsk_demod",
            "clock_recover",
            "slicer",
            "frame_decode"
        ]
    );
    assert_eq!(
        g.nodes[0].params,
        json!({"center_hz": CENTER_HZ, "bandwidth_hz": BANDWIDTH_HZ, "out_rate_hz": RATE_HZ})
    );
    assert_eq!(g.nodes[1].params["out_rate_hz"], 38_400);
    assert_eq!(g.nodes[5].params, framing());
    let edges: Vec<StreamKind> = g.edges.iter().map(|e| e.kind).collect();
    use StreamKind::{Audio, Event, Iq};
    assert_eq!(edges, [Iq, Iq, Audio, Audio, Event]);
    for (e, w) in g.edges.iter().zip(g.nodes.windows(2)) {
        assert_eq!((&e.from, &e.to), (&w[0].id, &w[1].id));
    }
    stages().spec("frame_decode").expect("registered");
    zeta_core::graph::validate(&g, &stages()).unwrap();
    // A band under the row's width is refused, as for an IQ program row.
    let narrow = Tuning {
        center_hz: Hz(CENTER_HZ),
        bandwidth_hz: Hz(6_000),
    };
    assert!(template::check_band(&row, narrow, RATE_HZ as u64).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_frame_row_job_decodes_crc_clean_frames() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), &dir.path().join("modes")).await;
    let id = start(&node, JobKind::from_name(ROW_ID).unwrap()).await;
    // The job's event outputs in graph order: `clock_recover`'s timing, then the frames.
    let events = || -> Vec<_> {
        node.job_outputs(&id)
            .into_iter()
            .filter(|(k, _)| *k == StreamKind::Event)
            .map(|(_, s)| s)
            .collect()
    };
    until("the job's event outputs", Duration::from_secs(5), || {
        !events().is_empty()
    })
    .await;
    // `frame_decode` forwards the slicer's `bits` events beside its frames.
    let stream = events().pop().expect("the job's frame output");
    let mut rx = node.bus().subscribe(&stream).unwrap();
    let first = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match rx.recv().await {
                Ok(f) => {
                    if let Payload::Event(v) = f.payload
                        && v["protocol"] != "bits"
                    {
                        return v;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(e) => panic!("stream closed: {e}"),
            }
        }
    })
    .await
    .expect("a frame within 10 s");
    assert_eq!(first["protocol"], "frame", "{first}");
    assert_eq!(first["crc_ok"], true, "{first}");
    assert_eq!(first["length_bytes"], FRAME_BYTES, "{first}");
    assert_eq!(first["summary"], "frame 34 B", "{first}");
    assert_eq!(state(&node, &id), JobState::Running);
    node.job_stop(&id).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn classify_proves_a_frame_row_through_the_quick_attempt() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), &dir.path().join("modes")).await;
    until("the ring", Duration::from_secs(10), || {
        node.ring(&rx()).is_some()
    })
    .await;
    let ring = || node.ring(&rx()).unwrap();
    let span = || {
        let r = ring();
        let r = r.read().unwrap();
        r.earliest().zip(r.latest())
    };
    until("1.2 s in the ring", Duration::from_secs(10), || {
        span().is_some_and(|(a, b)| b.0 - a.0 >= 1_200_000_000)
    })
    .await;
    let (start, _) = span().unwrap();
    let selection = Selection {
        source: rx(),
        span: Span {
            start_ns: start,
            end_ns: Timestamp(start.0 + 1_000_000_000),
        },
        center_hz: Hz(CENTER_HZ),
        bandwidth_hz: Hz(BANDWIDTH_HZ),
    };
    let Ok(OpResult::Classification { labels, tried, .. }) =
        node.apply(Op::Classify { selection }).await
    else {
        panic!("expected a classification");
    };
    let ours = tried
        .iter()
        .find(|t| t.mode == ROW_ID)
        .unwrap_or_else(|| panic!("{ROW_ID} tried: {tried:?}"));
    assert_eq!(ours.outcome, Outcome::Proved, "{tried:?}");
    let label = labels
        .iter()
        .find(|l| l.layer == 2 && l.name == "FSK frames")
        .unwrap_or_else(|| panic!("a layer-2 FSK frames label: {labels:?}"));
    assert!(
        label
            .evidence
            .starts_with(&format!("decode {ROW_ID}: frame 34 B, crc16-ccitt ok (")),
        "{label:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identify_proves_a_frame_row() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), &dir.path().join("modes")).await;
    let id = start(&node, JobKind::Identify).await;
    until("identify ends", Duration::from_secs(30), || {
        state(&node, &id) != JobState::Running
    })
    .await;
    let ctx = node.job_context(&id).unwrap();
    let verdict = &ctx["identify"]["verdict"];
    assert_eq!(verdict["mode"], ROW_ID, "{ctx}");
    assert_eq!(verdict["label"], "FSK frames", "{ctx}");
    for r in node.job_list() {
        if r.state == JobState::Running {
            node.job_stop(&r.id).await.unwrap();
        }
    }
}
