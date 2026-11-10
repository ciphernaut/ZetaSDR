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

/// `json_row` reading s16 audio at 48 kHz through an NBFM demod, under `container`.
fn audio_row(id: &str, container: &str) -> Value {
    let mut r = json_row(id, "s16", "stdin", &[]);
    r["decoder"]["input"] = json!({
        "stream": "audio", "format": "s16", "rate_hz": 48_000, "via": "stdin",
        "container": container
    });
    r["decoder"]["demod"] = json!({"kind": "nbfm", "deviation_hz": 5000, "audio_rate_hz": 48_000});
    r
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

fn audio_frame(n: usize, rate_hz: u64) -> Frame {
    Frame::new(
        StreamId::new(),
        prov(),
        Payload::Audio {
            sample_rate_hz: Hz(rate_hz),
            channels: 1,
            samples: vec![0.25f32; n].into(),
        },
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

/// Set in the re-run of this test binary that carries the secrets in its environment.
const LEAK_CHILD: &str = "ZETA_EXTERNAL_ENV_LEAK_CHILD";
/// The variables of ours a program must never see.
const SECRETS: [&str; 3] = ["ZETA_SECRET", "ZETA_NODE_SECRET_X", "ZETA_KIWI_PASSWORD_Y"];

/// The program runs with the cleared environment: secrets in the node's own environment
/// never reach it, `PATH` and `PYTHONUNBUFFERED` do. The secrets are put in the parent's
/// environment by re-running this one test in a child process that has them (no
/// `set_var` in a threaded test binary).
#[tokio::test]
async fn the_child_sees_only_the_pass_through_environment() {
    if std::env::var_os(LEAK_CHILD).is_none() {
        let exe = std::env::current_exe().unwrap();
        let mut cmd = std::process::Command::new(exe);
        cmd.args([
            "--exact",
            "the_child_sees_only_the_pass_through_environment",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(LEAK_CHILD, "1");
        for s in SECRETS {
            cmd.env(s, "hunter2");
        }
        let out = cmd.output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{text}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(text.contains("1 passed"), "{text}");
        return;
    }
    for s in SECRETS {
        assert_eq!(std::env::var(s).as_deref(), Ok("hunter2"), "{s} set");
    }
    let reg = registry(&[json_row("fake_env", "cs16", "stdin", &[])]);
    let stage = ExternalDecodeStage::new(Arc::new(reg)).with_env([("FAKE_PRINT_ENV", "1")]);
    let mut r = start(stage, "fake_env");
    send(&r.tx, 4, 480, 48_000);
    let v = next_event(&mut r.rx_out, 10).await.expect("an event");
    let names: Vec<&str> = v["text"].as_str().unwrap().split(' ').collect();
    for s in SECRETS {
        assert!(!names.contains(&s), "{s} reached the program: {names:?}");
    }
    assert!(!names.iter().any(|n| n.starts_with("ZETA_")), "{names:?}");
    assert!(names.contains(&"PATH"), "{names:?}");
    assert!(names.contains(&"PYTHONUNBUFFERED"), "{names:?}");
    drop(r.tx);
    assert!(matches!(result_within(r.handle, 10).await, Ok(())));
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

/// `via: fifo`: the input goes to a named pipe the program is given as `$fifo`, in a
/// directory of the run's own under the stage's run directory (made when missing),
/// removed when the run ends.
#[tokio::test]
async fn fifo_input_is_fed() {
    let reg = Arc::new(registry(&[json_row(
        "fake_fifo",
        "cs8",
        "fifo",
        &["--in", "$fifo"],
    )]));
    let dir = tempfile::tempdir().unwrap();
    let run_dir = dir.path().join("state").join("run");
    let stage = ExternalDecodeStage::new(reg.clone())
        .with_run_dir(&run_dir)
        .with_env([
            (
                "FAKE_LINES",
                r#"{"text":"{head}","n":{bytes},"path":"{in}"}"#,
            ),
            ("FAKE_AFTER_BYTES", "960"),
        ]);
    let mut r = start(stage, "fake_fifo");
    send(&r.tx, 1, 480, 48_000);
    let v = next_event(&mut r.rx_out, 10).await.expect("an event");
    // cs8: 0.5 * 127 rounds to 64 (0x40), -0.5 to -64 (0xc0).
    assert_eq!(v["text"], "40c040c040c040c0", "{v}");
    assert_eq!(v["n"], 960, "{v}");
    let fifo = PathBuf::from(v["path"].as_str().unwrap());
    assert!(
        fifo.starts_with(&run_dir),
        "{} not under {}",
        fifo.display(),
        run_dir.display()
    );
    drop(r.tx);
    assert!(matches!(result_within(r.handle, 10).await, Ok(())));
    assert_eq!(
        std::fs::read_dir(&run_dir).unwrap().count(),
        0,
        "fifo dir left behind"
    );

    // Without a run directory a fifo row fails naming the mode, never guessing a place.
    let r = start(ExternalDecodeStage::new(reg), "fake_fifo");
    match result_within(r.handle, 5).await {
        Err(StageError::Failed(m)) => {
            assert_eq!(m, "fake_fifo: fifo input needs a run directory")
        }
        other => panic!("expected Failed, got {other:?}"),
    }
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

/// An audio row hears 48 kHz audio frames under one WAV header: the fake reads the
/// header (its first line names what it said), then the samples.
#[tokio::test]
async fn an_audio_row_feeds_wav_to_the_program() {
    let reg = registry(&[audio_row("fake_wav", "wav")]);
    let stage = ExternalDecodeStage::new(Arc::new(reg)).with_env([
        ("FAKE_WAV", "1"),
        ("FAKE_LINES", r#"{"text":"sonde","n":{bytes}}"#),
        ("FAKE_AFTER_BYTES", "1920"),
    ]);
    let mut r = start(stage, "fake_wav");
    for _ in 0..2 {
        r.tx.send(audio_frame(480, 48_000)).unwrap();
    }
    let v = next_event(&mut r.rx_out, 10).await.expect("the wav line");
    assert_eq!(
        v["wav"],
        json!({"rate_hz": 48_000, "bits": 16, "channels": 1}),
        "{v}"
    );
    let v = next_event(&mut r.rx_out, 10).await.expect("the fake line");
    assert_eq!(v["protocol"], "fake_wav", "{v}");
    assert_eq!(v["summary"], "sonde #1920", "{v}");
    assert_eq!(v["t_ns"], 5, "{v}");
    drop(r.tx);
    assert!(matches!(result_within(r.handle, 10).await, Ok(())));
}

/// A rate change restarts the converter, never the header: the fake exits 3 on a second
/// `RIFF`, which would fail the stage.
#[tokio::test]
async fn a_wav_header_is_written_once_per_run() {
    let reg = registry(&[audio_row("fake_wav", "wav")]);
    let stage = ExternalDecodeStage::new(Arc::new(reg)).with_env([
        ("FAKE_WAV", "1"),
        ("FAKE_LINES", r#"{"text":"done","n":{bytes}}"#),
        ("FAKE_AFTER_BYTES", "2000"),
    ]);
    let mut r = start(stage, "fake_wav");
    r.tx.send(audio_frame(480, 48_000)).unwrap();
    r.tx.send(audio_frame(480, 24_000)).unwrap();
    r.tx.send(audio_frame(480, 48_000)).unwrap();
    let v = next_event(&mut r.rx_out, 10).await.expect("the wav line");
    assert_eq!(v["wav"]["rate_hz"], 48_000, "{v}");
    // More than the first frame's 960 bytes arrived: the samples after the restart too.
    let v = next_event(&mut r.rx_out, 10).await.expect("the fake line");
    assert_eq!(v["text"], "done", "{v}");
    drop(r.tx);
    assert!(matches!(result_within(r.handle, 10).await, Ok(())));
}

/// `container: raw` writes bare samples: a program wanting a header refuses them.
#[tokio::test]
async fn raw_audio_has_no_header() {
    let reg = registry(&[audio_row("fake_raw", "raw")]);
    let stage = ExternalDecodeStage::new(Arc::new(reg)).with_env([("FAKE_WAV", "1")]);
    let r = start(stage, "fake_raw");
    for _ in 0..2 {
        r.tx.send(audio_frame(480, 48_000)).unwrap();
    }
    match result_within(r.handle, 30).await {
        Err(StageError::Failed(m)) => {
            assert!(m.starts_with("fake_raw: "), "{m}");
            assert!(m.contains("fake_lines exited with 3"), "{m}");
            assert!(m.contains("error: wav header"), "{m}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    drop(r.tx);
}

/// A row's frames must be the stream its input names.
#[tokio::test]
async fn frames_of_the_other_stream_fail_the_stage() {
    let reg = registry(&[audio_row("fake_wav", "wav")]);
    let stage = ExternalDecodeStage::new(Arc::new(reg)).with_env([("FAKE_WAV", "1")]);
    let r = start(stage, "fake_wav");
    send(&r.tx, 1, 480, 48_000);
    match result_within(r.handle, 30).await {
        Err(StageError::Failed(m)) => assert_eq!(m, "fake_wav: expects audio frames"),
        other => panic!("expected Failed, got {other:?}"),
    }
    let reg = registry(&[json_row("fake_json", "cs16", "stdin", &[])]);
    let r = start(ExternalDecodeStage::new(Arc::new(reg)), "fake_json");
    r.tx.send(audio_frame(480, 48_000)).unwrap();
    match result_within(r.handle, 30).await {
        Err(StageError::Failed(m)) => assert_eq!(m, "fake_json: expects iq frames"),
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn params_take_the_mode_only() {
    let stage = ExternalDecodeStage::deferred(ModesHandle::default());
    let s = &stage.spec().params_schema;
    assert_eq!(stage.spec().kind, "external_decode");
    assert_eq!(stage.spec().inputs, [StreamKind::Iq, StreamKind::Audio]);
    assert_eq!(stage.spec().outputs, [StreamKind::Event]);
    assert_eq!(s.get("additionalProperties"), Some(&json!(false)), "{s:?}");
    assert_eq!(s.get("required"), Some(&json!(["mode"])), "{s:?}");
}

// The vectors in tests/data are SYNTHETIC: built by hand to the shape of the upstream
// formatters (dumpvdl2, dumphfdl, libacars), with invented values, until off-air
// fixtures replace them.
fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn vector(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(data(name)).unwrap()).unwrap()
}

/// The vdl2 row's `decoder.output.fields` map.
fn vdl2_map() -> Value {
    let tags = "/vdl2/avlc/acars/arinc622/adsc/tags";
    let first = |leaf: &str| {
        json!([
            format!("{tags}/0/basic_report/{leaf}"),
            format!("{tags}/1/basic_report/{leaf}"),
            format!("{tags}/0/emerg_basic_report/{leaf}")
        ])
    };
    json!({
        "icao": "/vdl2/avlc/src/addr",
        "addr_type": "/vdl2/avlc/src/type",
        "status": "/vdl2/avlc/src/status",
        "dst": "/vdl2/avlc/dst/addr",
        "frame_type": "/vdl2/avlc/frame_type",
        "reg": {"pointers": ["/vdl2/avlc/acars/reg"], "trim": "."},
        "flight": "/vdl2/avlc/acars/flight",
        "label": "/vdl2/avlc/acars/label",
        "msg_num": "/vdl2/avlc/acars/msg_num",
        "ack": "/vdl2/avlc/acars/ack",
        "text": "/vdl2/avlc/acars/msg_text",
        "crc_ok": "/vdl2/avlc/acars/crc_ok",
        "lat": first("lat"),
        "lon": first("lon"),
        "alt_ft": first("alt"),
        "sig_level": "/vdl2/sig_level",
        "noise_level": "/vdl2/noise_level"
    })
}

fn field_map(v: Value) -> std::collections::BTreeMap<String, zeta_core::modes::FieldPath> {
    serde_json::from_value(v).unwrap()
}

/// The vdl2 row's `derive`: an aircraft's text position report fills an empty fix.
fn vdl2_derive() -> Value {
    json!([{
        "when": {"addr_type": "Aircraft"},
        "from": "text",
        "extract": "acars_position",
        "into": {"lat": "lat", "lon": "lon", "alt_ft": "alt_ft"}
    }])
}

/// A program row printing the lines of `lines` through `fake_lines`; with `map` (the
/// vdl2 one) its schema lists every mapped field, and the position and derive are the
/// vdl2 ones.
fn mapped_row(id: &str, lines: &str, map: Option<Value>) -> Value {
    let lines = data(lines).display().to_string();
    let mut props = serde_json::Map::new();
    for k in map.iter().flat_map(|m| m.as_object().unwrap().keys()) {
        props.insert(
            k.clone(),
            json!({"type": ["string", "number", "boolean", "null"]}),
        );
    }
    let mut r = row(
        id,
        &fake("fake_lines"),
        &["--lines-file", &lines],
        "cs16",
        48_000,
        "stdin",
        "stdout",
        "json_lines",
        "line",
        Value::Object(props),
    );
    if let Some(m) = map {
        r["decoder"]["output"]["fields"] = m;
        r["position"] = json!({"lat": "lat", "lon": "lon", "alt_m": "alt_ft", "alt_unit": "ft"});
        r["derive"] = vdl2_derive();
    }
    r
}

#[test]
fn apply_fields_takes_the_first_present_pointer_and_copies_values_as_they_are() {
    let map = field_map(vdl2_map());
    let line = vector("dumpvdl2-adsc.json");
    let ev = zeta_ext::apply_fields(&map, &line);
    assert_eq!(ev["icao"], "7C0000");
    assert_eq!(ev["reg"], "VH-ZET", "the leading dot is trimmed");
    assert_eq!(line["vdl2"]["avlc"]["acars"]["reg"], ".VH-ZET");
    assert_eq!(ev["flight"], "ZET1");
    assert_eq!(ev["text"], "/BNECAYA.ADS.VH-ZET0712...");
    assert_eq!(ev["lat"], json!(-27.38));
    assert_eq!(ev["lon"], json!(153.12));
    assert_eq!(ev["alt_ft"], json!(37000));
    assert_eq!(ev["crc_ok"], json!(true));
    assert_eq!(ev["sig_level"], json!(-21.512345));
    assert!(ev.get("vdl2").is_none(), "{ev}");
    let one = field_map(json!({"avlc": "/vdl2/avlc/src", "gone": "/vdl2/nope"}));
    let ev = zeta_ext::apply_fields(&one, &line);
    assert_eq!(ev["avlc"]["type"], "Aircraft", "an object is copied");
    assert!(ev.get("gone").is_none(), "{ev}");
    let trim = field_map(json!({
        "f": {"pointers": ["/f"], "trim": " "},
        "n": {"pointers": ["/n"], "trim": " "}
    }));
    let ev = zeta_ext::apply_fields(&trim, &json!({"f": "ZET1  ", "n": 5}));
    assert_eq!(ev["f"], "ZET1");
    assert_eq!(ev["n"], 5);
}

#[test]
fn a_mapped_line_without_the_listed_tag_has_no_position() {
    let reg = registry(&[mapped_row("m_xid", "dumpvdl2-xid.json", Some(vdl2_map()))]);
    let fields = zeta_ext::apply_fields(&field_map(vdl2_map()), &vector("dumpvdl2-xid.json"));
    assert_eq!(fields["icao"], "7C0000");
    for k in ["reg", "lat", "lon"] {
        assert!(fields.get(k).is_none(), "{k} in {fields}");
    }
    let ev = zeta_core::modes::summary::envelope(&reg.get("m_xid").unwrap().row, 1, fields);
    assert_eq!(ev["icao"], "7C0000");
    assert!(ev.get("position").is_none(), "{ev}");
}

#[test]
fn a_non_numeric_fix_is_no_position() {
    let reg = registry(&[mapped_row("m_na", "dumpvdl2-adsc.json", Some(vdl2_map()))]);
    let mut line = vector("dumpvdl2-adsc.json");
    line["vdl2"]["avlc"]["acars"]["arinc622"]["adsc"]["tags"][0]["basic_report"]["lat"] =
        json!("n/a");
    let fields = zeta_ext::apply_fields(&field_map(vdl2_map()), &line);
    let ev = zeta_core::modes::summary::envelope(&reg.get("m_na").unwrap().row, 1, fields);
    assert_eq!(ev["icao"], "7C0000");
    assert!(ev.get("position").is_none(), "{ev}");
}

/// `line` through the vdl2 map and the envelope of the vdl2-shaped row `id`.
fn vdl2_event(id: &str, lines: &str, line: &Value) -> Value {
    let reg = registry(&[mapped_row(id, lines, Some(vdl2_map()))]);
    let fields = zeta_ext::apply_fields(&field_map(vdl2_map()), line);
    zeta_core::modes::summary::envelope(&reg.get(id).unwrap().row, 1, fields)
}

#[test]
fn a_text_position_report_gives_a_position() {
    let line = vector("dumpvdl2-h1pos.json");
    let ev = vdl2_event("m_h1", "dumpvdl2-h1pos.json", &line);
    assert_eq!(ev["icao"], "7C0000", "{ev}");
    assert_eq!(ev["reg"], "VH-ZET", "{ev}");
    assert_eq!(ev["label"], "H1", "{ev}");
    let near = |v: &Value, want: f64| (v.as_f64().unwrap() - want).abs() < 0.001;
    assert!(near(&ev["lat"], -27.385), "{ev}");
    assert!(near(&ev["position"]["lat"], -27.385), "{ev}");
    assert!(near(&ev["position"]["lon"], 153.1183), "{ev}");
    assert_eq!(ev["alt_ft"], json!(16500), "{ev}");
    // 16 500 ft in metres.
    let alt_m = ev["position"]["alt_m"].as_f64().unwrap();
    assert!((alt_m - 5029.2).abs() < 0.5, "{ev}");
}

#[test]
fn a_ground_station_frame_is_not_parsed() {
    let mut line = vector("dumpvdl2-h1pos.json");
    line["vdl2"]["avlc"]["src"]["type"] = json!("Ground station");
    let ev = vdl2_event("m_gs", "dumpvdl2-h1pos.json", &line);
    assert_eq!(ev["addr_type"], "Ground station", "{ev}");
    assert!(ev["text"].as_str().unwrap().contains("POSS27231E153071"));
    for k in ["position", "lat", "lon", "alt_ft"] {
        assert!(ev.get(k).is_none(), "{k} in {ev}");
    }
}

#[test]
fn an_adsc_fix_wins_over_a_text_report() {
    let mut line = vector("dumpvdl2-adsc.json");
    line["vdl2"]["avlc"]["acars"]["msg_text"] = json!("POSN10000W020000,ALPHA,025131,300,BRAVO");
    let ev = vdl2_event("m_both", "dumpvdl2-adsc.json", &line);
    assert_eq!(ev["lat"], json!(-27.38), "{ev}");
    assert_eq!(ev["lon"], json!(153.12), "{ev}");
    assert_eq!(ev["alt_ft"], json!(37000), "{ev}");
    assert_eq!(ev["position"]["lat"], json!(-27.38), "{ev}");
}

#[test]
fn the_hfdl_list_form_keys_downlinks_and_uplinks_on_the_aircraft() {
    let map = field_map(json!({
        "icao": [
            "/hfdl/lpdu/src/ac_info/icao",
            "/hfdl/lpdu/dst/ac_info/icao",
            "/hfdl/lpdu/ac_info/icao"
        ],
        "gs_id": ["/hfdl/spdu/src/id", "/hfdl/lpdu/hfnpdu/gs/id"]
    }));
    // Downlink: the aircraft is the source; the station is the one the performance-data
    // PDU names (an LPDU's `src`/`dst` are both always present, so they cannot tell).
    let down = zeta_ext::apply_fields(&map, &vector("dumphfdl-downlink.json"));
    assert_eq!(down["icao"], "7C0000");
    assert_eq!(down["gs_id"], 4);
    // Uplink: the aircraft is the destination; the PDU names no station, so no `gs_id`.
    let up = zeta_ext::apply_fields(&map, &vector("dumphfdl-uplink.json"));
    assert_eq!(up["icao"], "7C0000");
    assert!(up.get("gs_id").is_none(), "{up}");
    // A squitter: no aircraft, its own station.
    let spdu = zeta_ext::apply_fields(&map, &vector("dumphfdl-spdu.json"));
    assert!(spdu.get("icao").is_none(), "{spdu}");
    assert_eq!(spdu["gs_id"], 4);
    // A logon PDU carries the aircraft's address directly on the LPDU.
    let logon = json!({"hfdl": {"lpdu": {"ac_info": {"icao": "7C0001"}}}});
    assert_eq!(zeta_ext::apply_fields(&map, &logon)["icao"], "7C0001");
}

#[tokio::test]
async fn a_row_with_fields_publishes_the_mapped_event_and_without_them_the_whole_object() {
    let reg = Arc::new(registry(&[
        mapped_row("m_with", "dumpvdl2-adsc.json", Some(vdl2_map())),
        mapped_row("m_without", "dumpvdl2-adsc.json", None),
    ]));
    let mut r = start(ExternalDecodeStage::new(reg.clone()), "m_with");
    send(&r.tx, 4, 480, 48_000);
    let v = next_event(&mut r.rx_out, 10).await.expect("an event");
    assert_eq!(v["icao"], "7C0000", "{v}");
    assert_eq!(v["position"]["lat"], json!(-27.38), "{v}");
    assert!(v.get("vdl2").is_none(), "{v}");
    drop(r.tx);
    assert!(matches!(result_within(r.handle, 10).await, Ok(())));

    let mut r = start(ExternalDecodeStage::new(reg), "m_without");
    send(&r.tx, 4, 480, 48_000);
    let v = next_event(&mut r.rx_out, 10).await.expect("an event");
    assert_eq!(v["vdl2"]["avlc"]["src"]["addr"], "7C0000", "{v}");
    assert!(v.get("icao").is_none(), "{v}");
    drop(r.tx);
    assert!(matches!(result_within(r.handle, 10).await, Ok(())));
}
