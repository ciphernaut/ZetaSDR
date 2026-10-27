//! external_decode: a program row's program run as a stage, against the fake programs
//! `fake_lines` (stdout or TCP output, stdin or fifo input) and `fake_dump1090`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use num_complex::Complex;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::modes::{ModeRegistry, ModeState};
use zeta_core::port::IqBlock;
use zeta_core::stage::{Stage, StageContext, StageError, StageRegistry};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};
use zeta_ext::{ExternalDecodeStage, ModesHandle};

fn fake(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(name)
}

/// A program row `id` running `program` with `args`, its input `format` at `rate_hz`
/// via `input_via`, its output via `output_via` read with `parser`.
#[allow(clippy::too_many_arguments)]
fn row(
    id: &str,
    program: &Path,
    args: &[&str],
    format: &str,
    rate_hz: u64,
    input_via: &str,
    output_via: &str,
    parser: &str,
    summary: &str,
    properties: Value,
) -> Value {
    let port = match output_via {
        "tcp" => json!("$port"),
        _ => Value::Null,
    };
    json!({
        "id": id,
        "name": format!("{id} test row"),
        "family": "general",
        "band": {"bandwidth_hz": 12500, "centers_hz": [], "min_hz": null},
        "modulation": [],
        "label": id.to_uppercase(),
        "protocol": id,
        "decoder": {
            "kind": "program",
            "programs": [program.display().to_string()],
            "args": args,
            "input": {"stream": "iq", "format": format, "rate_hz": rate_hz, "via": input_via},
            "output": {"via": output_via, "port": port, "parser": parser},
            "install": {"debian": "fake-dec", "url": "https://example.org/fake-dec"}
        },
        "event": {"schema": {"type": "object", "properties": properties}, "summary": summary},
        "identify": {"proof": "event", "min_events": 1, "dwell_ms": 1000, "min_span_ms": 1000, "quick": null},
        "catalogue": []
    })
}

fn json_row(id: &str, format: &str, via: &str, args: &[&str]) -> Value {
    row(
        id,
        &fake("fake_lines"),
        args,
        format,
        48_000,
        via,
        "stdout",
        "json_lines",
        "{text} #{n}",
        json!({"text": {"type": "string"}, "n": {"type": "integer"}}),
    )
}

/// The registry of the rows in a temp operator dir, validated against stages that
/// include `external_decode`; every test row must load ready.
fn registry(rows: &[Value]) -> ModeRegistry {
    let dir = tempfile::tempdir().unwrap();
    for r in rows {
        let id = r["id"].as_str().unwrap();
        std::fs::write(dir.path().join(format!("{id}.json")), r.to_string()).unwrap();
    }
    let mut stages = StageRegistry::new();
    zeta_dsp::register_all(&mut stages);
    stages.register(Arc::new(ExternalDecodeStage::deferred(
        ModesHandle::default(),
    )));
    let reg = ModeRegistry::load(&stages, Some(dir.path()));
    for r in rows {
        let m = reg.get(r["id"].as_str().unwrap()).unwrap();
        assert_eq!(m.state, ModeState::Ready, "{:?}", m.reason);
    }
    reg
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
            center_hz: Hz(144_800_000),
            samples: iq.to_vec().into(),
        }),
    )
}

struct Running {
    tx: broadcast::Sender<Frame>,
    rx_out: broadcast::Receiver<Frame>,
    context: Arc<Mutex<Value>>,
    handle: tokio::task::JoinHandle<Result<(), StageError>>,
}

fn start(stage: ExternalDecodeStage, mode: &str) -> Running {
    let bus = Arc::new(StreamBus::new(64));
    let out = bus.register(StreamKind::Event, prov());
    let rx_out = bus.subscribe(&out).unwrap();
    let (tx, rx_in) = broadcast::channel(64);
    let context = Arc::new(Mutex::new(json!({})));
    let c = context.clone();
    let ctx = StageContext::new(SourceId("s".into())).with_note(Arc::new(move |v: Value| {
        let mut cx = c.lock().unwrap();
        for (k, x) in v.as_object().unwrap() {
            cx[k] = x.clone();
        }
    }));
    let params = json!({ "mode": mode });
    let handle = tokio::spawn(async move {
        stage
            .run(
                params,
                vec![rx_in],
                vec![out],
                bus,
                CancellationToken::new(),
                ctx,
            )
            .await
    });
    Running {
        tx,
        rx_out,
        context,
        handle,
    }
}

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

fn tone(n: usize) -> Vec<Complex<f32>> {
    vec![Complex::new(0.5, -0.5); n]
}

fn send(tx: &broadcast::Sender<Frame>, frames: usize, samples: usize, rate_hz: u64) {
    for _ in 0..frames {
        tx.send(iq_frame(&tone(samples), rate_hz)).unwrap();
    }
}

#[tokio::test]
async fn json_lines_rows_publish_enveloped_events() {
    let reg = registry(&[json_row("fake_json", "cs16", "stdin", &[])]);
    let stage = ExternalDecodeStage::new(Arc::new(reg))
        .with_env([("FAKE_LINES", r#"{"text":"hello","n":7}"#)]);
    let mut r = start(stage, "fake_json");
    send(&r.tx, 4, 480, 48_000);
    let v = next_event(&mut r.rx_out, 10).await.expect("an event");
    assert_eq!(v["protocol"], "fake_json", "{v}");
    assert_eq!(v["summary"], "hello #7", "{v}");
    assert_eq!(v["t_ns"], 5, "{v}");
    assert_eq!(v["text"], "hello");
    assert_eq!(v["n"], 7);
    drop(r.tx);
    assert!(matches!(result_within(r.handle, 10).await, Ok(())));
    assert!(next_event(&mut r.rx_out, 0).await.is_none());
}

#[tokio::test]
async fn unparsable_lines_are_counted_not_fatal() {
    let reg = registry(&[json_row("fake_json", "cs16", "stdin", &[])]);
    let stage = ExternalDecodeStage::new(Arc::new(reg))
        .with_env([("FAKE_LINES", "this is not json\n{\"text\":\"ok\",\"n\":1}")]);
    let mut r = start(stage, "fake_json");
    send(&r.tx, 4, 480, 48_000);
    let v = next_event(&mut r.rx_out, 10).await.expect("an event");
    assert_eq!(v["summary"], "ok #1");
    drop(r.tx);
    assert!(matches!(result_within(r.handle, 10).await, Ok(())));
    assert!(next_event(&mut r.rx_out, 0).await.is_none());
    let cx = r.context.lock().unwrap().clone();
    assert_eq!(cx["external"]["unparsed"], 1, "{cx}");
}

#[tokio::test]
async fn tcp_output_rows_connect_and_parse() {
    let sbs = row(
        "fake_adsb",
        &fake("fake_dump1090"),
        &["--net-sbs-port", "$port"],
        "cs16",
        2_400_000,
        "stdin",
        "tcp",
        "sbs",
        "{icao} {callsign?}",
        json!({"icao": {"type": "string"}, "callsign": {"type": ["string", "null"]}}),
    );
    let reg = registry(&[sbs]);
    let stage = ExternalDecodeStage::new(Arc::new(reg)).with_env([(
        "FAKE_SBS_LINE",
        "MSG,1,1,1,ABCDEF,1,2026/10/20,12:00:00.000,2026/10/20,12:00:00.000,ZETA1   ,,,,,,,,,,,0\r",
    )]);
    let mut r = start(stage, "fake_adsb");
    send(&r.tx, 4, 4_800, 2_400_000);
    let v = next_event(&mut r.rx_out, 10).await.expect("an event");
    assert_eq!(v["protocol"], "fake_adsb");
    assert_eq!(v["icao"], "ABCDEF");
    assert_eq!(v["callsign"], "ZETA1");
    assert_eq!(v["summary"], "ABCDEF ZETA1");
    drop(r.tx);
    assert!(matches!(result_within(r.handle, 10).await, Ok(())));
}

#[tokio::test]
async fn child_exit_fails_the_stage_naming_the_mode() {
    let reg = registry(&[json_row("fake_json", "cs16", "stdin", &[])]);
    let stage = ExternalDecodeStage::new(Arc::new(reg)).with_env([
        ("FAKE_LINES", r#"{"text":"bye","n":0}"#),
        ("FAKE_EXIT_AFTER", "1"),
    ]);
    let r = start(stage, "fake_json");
    send(&r.tx, 4, 480, 48_000);
    // 30 s: under a full workspace run the fake child can take its time to start and exit.
    match result_within(r.handle, 30).await {
        Err(StageError::Failed(m)) => {
            assert!(m.starts_with("fake_json: "), "{m}");
            assert!(m.contains("exited with 3"), "{m}");
            assert!(m.contains("fake_lines: sent lines"), "stderr tail in {m}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    drop(r.tx);
}

/// `cu8` input: each sample is `(x + 1) * 127.5` in a byte, so 0.5 / -0.5 is BF 40, and
/// 480 samples are 960 bytes (as cs16 they would be 1 920).
#[tokio::test]
async fn cu8_input_is_converted() {
    let reg = registry(&[json_row("fake_cu8", "cu8", "stdin", &[])]);
    let stage = ExternalDecodeStage::new(Arc::new(reg)).with_env([
        ("FAKE_LINES", r#"{"text":"{head}","n":{bytes}}"#),
        ("FAKE_AFTER_BYTES", "960"),
    ]);
    let mut r = start(stage, "fake_cu8");
    send(&r.tx, 1, 480, 48_000);
    let v = next_event(&mut r.rx_out, 10).await.expect("an event");
    assert_eq!(v["text"], "bf40bf40bf40bf40", "{v}");
    assert_eq!(v["n"], 960, "{v}");
    drop(r.tx);
    assert!(matches!(result_within(r.handle, 10).await, Ok(())));
}

/// `via: fifo`: the input goes to a named pipe the program is given as `$fifo`.
#[tokio::test]
async fn fifo_input_is_fed() {
    let reg = registry(&[json_row("fake_fifo", "cs8", "fifo", &["--in", "$fifo"])]);
    let stage = ExternalDecodeStage::new(Arc::new(reg)).with_env([
        ("FAKE_LINES", r#"{"text":"{head}","n":{bytes}}"#),
        ("FAKE_AFTER_BYTES", "960"),
    ]);
    let mut r = start(stage, "fake_fifo");
    send(&r.tx, 1, 480, 48_000);
    let v = next_event(&mut r.rx_out, 10).await.expect("an event");
    // cs8: 0.5 * 127 rounds to 64 (0x40), -0.5 to -64 (0xc0).
    assert_eq!(v["text"], "40c040c040c040c0", "{v}");
    assert_eq!(v["n"], 960, "{v}");
    drop(r.tx);
    assert!(matches!(result_within(r.handle, 10).await, Ok(())));
}

#[tokio::test]
async fn an_unavailable_row_fails_naming_the_handler_and_suggestion() {
    let mut reg = registry(&[json_row("fake_json", "cs16", "stdin", &[])]);
    let reason =
        "fake-dec: missing (apt install fake-dec; https://example.org/fake-dec) — modes: fake_json";
    reg.set_program("fake_json", None, Some(reason.into()));
    let r = start(ExternalDecodeStage::new(Arc::new(reg)), "fake_json");
    match result_within(r.handle, 5).await {
        Err(StageError::Failed(m)) => {
            assert_eq!(m, format!("fake_json: {reason}"));
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    // An unknown id, and a stage whose registry was never filled.
    let reg = registry(&[]);
    let r = start(ExternalDecodeStage::new(Arc::new(reg)), "nope");
    match result_within(r.handle, 5).await {
        Err(StageError::Failed(m)) => assert_eq!(m, "external_decode: unknown mode nope"),
        other => panic!("expected Failed, got {other:?}"),
    }
    let r = start(
        ExternalDecodeStage::deferred(ModesHandle::default()),
        "fake_json",
    );
    match result_within(r.handle, 5).await {
        Err(StageError::Failed(m)) => assert_eq!(m, "external_decode: no mode registry"),
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn params_take_the_mode_only() {
    let stage = ExternalDecodeStage::deferred(ModesHandle::default());
    let s = &stage.spec().params_schema;
    assert_eq!(stage.spec().kind, "external_decode");
    assert_eq!(stage.spec().inputs, [StreamKind::Iq]);
    assert_eq!(stage.spec().outputs, [StreamKind::Event]);
    assert_eq!(s.get("additionalProperties"), Some(&json!(false)), "{s:?}");
    assert_eq!(s.get("required"), Some(&json!(["mode"])), "{s:?}");
}
