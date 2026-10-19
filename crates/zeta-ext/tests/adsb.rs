//! adsb_decode: SBS parsing, the stage against a fake dump1090, and an oracle
//! run against the real one when the host has it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use num_complex::Complex;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::port::IqBlock;
use zeta_core::stage::{Stage, StageContext, StageError};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};
use zeta_ext::sbs::parse_line;
use zeta_ext::{ADSB_RATE_HZ, AdsbDecodeStage, find_dump1090};

const POSITION: &str = "MSG,3,1,1,4840D6,1,2026/10/20,12:00:00.000,2026/10/20,12:00:00.000,,38000,,,52.25720,3.91937,,,0,0,0,0";
const IDENT: &str =
    "MSG,1,1,1,4840D6,1,2026/10/20,12:00:00.000,2026/10/20,12:00:00.000,KLM1023 ,,,,,,,,,,,0";
const FAKE_IDENT: &str =
    "MSG,1,1,1,ABCDEF,1,2026/10/20,12:00:00.000,2026/10/20,12:00:00.000,ZETA1   ,,,,,,,,,,,0\r";

#[test]
fn parse_sbs_position_line() {
    let v = parse_line(POSITION).expect("an MSG line");
    assert_eq!(v["protocol"], "adsb");
    assert_eq!(v["icao"], "4840D6");
    assert_eq!(v["msg"], 3);
    assert_eq!(v["altitude_ft"], 38000);
    assert_eq!(v["lat"], 52.2572);
    assert_eq!(v["lon"], 3.91937);
    assert_eq!(v["callsign"], Value::Null);
    assert_eq!(v["ground_speed_kt"], Value::Null);
    assert_eq!(v["track_deg"], Value::Null);
    assert_eq!(v["vertical_rate_fpm"], Value::Null);
    assert_eq!(v["squawk"], Value::Null);
    assert_eq!(v["on_ground"], false);
}

#[test]
fn parse_sbs_identity_line() {
    let v = parse_line(IDENT).expect("an MSG line");
    assert_eq!(v["msg"], 1);
    assert_eq!(v["callsign"], "KLM1023");
    assert_eq!(v["lat"], Value::Null);
    assert_eq!(v["altitude_ft"], Value::Null);
}

#[test]
fn parse_sbs_other_fields_and_non_msg() {
    let v = parse_line(
        "MSG,4,1,1,a1b2c3,1,2026/10/20,12:00:00.000,2026/10/20,12:00:00.000,,,451.5,271.2,,,-640,,,,,-1\r",
    )
    .unwrap();
    assert_eq!(v["icao"], "A1B2C3");
    assert_eq!(v["ground_speed_kt"], 451.5);
    assert_eq!(v["track_deg"], 271.2);
    assert_eq!(v["vertical_rate_fpm"], -640);
    assert_eq!(v["on_ground"], true);
    let v = parse_line("MSG,6,1,1,A1B2C3,1,,,,,,,,,,,,7700,,,,").unwrap();
    assert_eq!(v["squawk"], "7700");
    assert_eq!(v["on_ground"], Value::Null);
    assert!(parse_line("STA,,1,1,4840D6,1,,,,,RM").is_none());
    assert!(parse_line("AIR,,1,1,4840D6,1,,,,,").is_none());
    assert!(parse_line("").is_none());
    assert!(parse_line("MSG,x,1,1,4840D6").is_none());
}

fn fake() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_dump1090")
}

fn prov() -> Provenance {
    Provenance {
        source: SourceId("s".into()),
        job: None,
        channel: None,
        span: Span {
            start_ns: Timestamp(5),
            end_ns: Timestamp(9),
        },
    }
}

fn iq_frame(iq: &[Complex<f32>], rate_hz: u64) -> Frame {
    Frame::new(
        StreamId::new(),
        prov(),
        Payload::Iq(IqBlock {
            t0: Timestamp(0),
            sample_rate_hz: Hz(rate_hz),
            center_hz: Hz(1_090_000_000),
            samples: iq.to_vec().into(),
        }),
    )
}

struct Running {
    tx: broadcast::Sender<Frame>,
    rx_out: broadcast::Receiver<Frame>,
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<Result<(), StageError>>,
}

fn start(stage: AdsbDecodeStage) -> Running {
    let bus = Arc::new(StreamBus::new(64));
    let out = bus.register(StreamKind::Event, prov());
    let rx_out = bus.subscribe(&out).unwrap();
    let (tx, rx_in) = broadcast::channel(64);
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    let handle = tokio::spawn(async move {
        stage
            .run(
                serde_json::json!({}),
                vec![rx_in],
                vec![out],
                bus,
                c,
                StageContext::new(SourceId("s".into())),
            )
            .await
    });
    Running {
        tx,
        rx_out,
        cancel,
        handle,
    }
}

/// The next event within `secs`, or `None`.
async fn next_event(rx: &mut broadcast::Receiver<Frame>, secs: u64) -> Option<Value> {
    match tokio::time::timeout(Duration::from_secs(secs), rx.recv()).await {
        Ok(Ok(f)) => match f.payload {
            Payload::Event(v) => Some(v),
            other => panic!("expected an event, got {:?}", other.kind()),
        },
        _ => None,
    }
}

async fn result_within(
    h: tokio::task::JoinHandle<Result<(), StageError>>,
    secs: u64,
) -> Result<(), StageError> {
    tokio::time::timeout(Duration::from_secs(secs), h)
        .await
        .expect("stage ended in time")
        .unwrap()
}

fn noise(n: usize) -> Vec<Complex<f32>> {
    (0..n)
        .map(|i| Complex::new(0.1 * ((i % 7) as f32 - 3.0) / 3.0, 0.05))
        .collect()
}

async fn round_trip(rate_hz: u64, samples_per_frame: usize) {
    let stage = AdsbDecodeStage::new(Some(fake())).with_env([("FAKE_SBS_LINE", FAKE_IDENT)]);
    let mut r = start(stage);
    let frame = noise(samples_per_frame);
    for _ in 0..4 {
        r.tx.send(iq_frame(&frame, rate_hz)).unwrap();
    }
    let v = next_event(&mut r.rx_out, 10).await.expect("an event");
    assert_eq!(v["protocol"], "adsb");
    assert_eq!(v["icao"], "ABCDEF");
    assert_eq!(v["callsign"], "ZETA1");
    drop(r.tx);
    assert!(matches!(result_within(r.handle, 10).await, Ok(())));
    assert!(next_event(&mut r.rx_out, 0).await.is_none());
}

#[tokio::test]
async fn fake_program_round_trip() {
    assert_eq!(ADSB_RATE_HZ, 2_400_000);
    round_trip(2_400_000, 4_800).await;
}

#[tokio::test]
async fn odd_input_rate_is_resampled() {
    round_trip(2_560_000, 4_800).await;
}

#[tokio::test]
async fn program_missing_fails_fast() {
    let r = start(AdsbDecodeStage::new(None));
    match result_within(r.handle, 1).await {
        Err(StageError::Failed(m)) => assert!(m.contains("ZETA_DUMP1090"), "{m}"),
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[tokio::test]
async fn program_without_sbs_port_fails() {
    let r = start(AdsbDecodeStage::new(Some("/usr/bin/true".into())));
    match result_within(r.handle, 10).await {
        Err(StageError::Failed(m)) => {
            assert!(m.contains("did not open its SBS port"), "{m}")
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[tokio::test]
async fn child_exit_fails_the_stage() {
    let stage = AdsbDecodeStage::new(Some(fake()))
        .with_env([("FAKE_SBS_LINE", FAKE_IDENT), ("FAKE_EXIT_AFTER", "1")]);
    let r = start(stage);
    let frame = noise(4_800);
    for _ in 0..4 {
        r.tx.send(iq_frame(&frame, 2_400_000)).unwrap();
    }
    match result_within(r.handle, 10).await {
        Err(StageError::Failed(m)) => {
            assert!(m.contains("exited"), "{m}");
            assert!(m.contains('3'), "{m}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    drop(r.tx);
}

#[tokio::test]
async fn cancel_kills_child() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let stage = AdsbDecodeStage::new(Some(fake())).with_env([
        ("FAKE_SBS_LINE", FAKE_IDENT),
        ("FAKE_PIDFILE", pidfile.to_str().unwrap()),
    ]);
    let mut r = start(stage);
    let frame = noise(4_800);
    for _ in 0..4 {
        r.tx.send(iq_frame(&frame, 2_400_000)).unwrap();
    }
    // The event proves the child is up and past its pidfile.
    next_event(&mut r.rx_out, 10).await.expect("an event");
    let pid = std::fs::read_to_string(&pidfile).unwrap();
    let alive = |pid: &str| {
        std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    assert!(alive(&pid), "child should be running before cancel");
    r.cancel.cancel();
    assert!(matches!(
        result_within(r.handle, 10).await,
        Err(StageError::Cancelled)
    ));
    assert!(!alive(&pid), "child {pid} survived cancel");
}

#[tokio::test]
async fn real_dump1090_decodes_identification() {
    let Some(program) = find_dump1090() else {
        eprintln!("skipped: no dump1090");
        return;
    };
    let mut r = start(AdsbDecodeStage::new(Some(program)));
    let iq = zeta_dsp::fixtures::modes_frame_iq(0x4840D6, "ZETA1", 50, 2_400_000);
    for chunk in iq.chunks(24_000) {
        r.tx.send(iq_frame(chunk, 2_400_000)).unwrap();
    }
    // dump1090 reads stdin in fixed-size buffers: keep feeding silence so the
    // last partial buffer is processed too.
    let silence = vec![Complex::new(0.0f32, 0.0); 24_000];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut found = None;
    while tokio::time::Instant::now() < deadline && found.is_none() {
        tokio::select! {
            got = r.rx_out.recv() => {
                let f = got.expect("event stream open");
                let Payload::Event(v) = f.payload else { panic!("expected an event") };
                if v["icao"] == "4840D6" && v["callsign"] == "ZETA1" {
                    found = Some(v);
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                let _ = r.tx.send(iq_frame(&silence, 2_400_000));
            }
        }
    }
    r.cancel.cancel();
    let res = result_within(r.handle, 10).await;
    let v = found.expect("dump1090 decoded the identification frame");
    assert_eq!(v["protocol"], "adsb");
    assert_eq!(v["msg"], 1);
    assert!(matches!(res, Err(StageError::Cancelled)), "{res:?}");
}
