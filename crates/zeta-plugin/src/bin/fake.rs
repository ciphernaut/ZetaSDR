//! `zeta-plugin-fake --mode <m> [--pidfile <path>]`: a test double for the plugin host,
//! speaking [`zeta_plugin::wire`] on its stdio. Every mode says `hello` with kind `fake`
//! (except `no-hello`), then waits for `run`:
//!
//! - `echo`: iq in, iq out; each input frame is sent back on the run's first output.
//! - `crash-mid-frame`: after the first input frame, writes `0x00`, a valid iq header
//!   promising 8000 body bytes and only 12 of them, then exits 3.
//! - `wrong-kind`: declares `[iq]` outputs, answers the first input frame with audio.
//! - `no-hello`: says nothing for 10 s, then exits.
//! - `loose-schema`: its `params_schema` lacks `additionalProperties`.
//! - `multi-input`: declares `[iq, audio]` inputs.
//! - `error`: sends `{"type":"error","message":"boom"}` after `run`, then exits 1.
//! - `lag-report`: iq in, event out: `{"lagged":N}` per `lagged`, `{"samples":n}` per
//!   frame.
//! - `stall`: `lag-report` that stops reading for 1 s after `run`, so its stdin fills.
//! - `fork`: `echo` that first starts `sleep 30` as its own child (its pid also goes to the
//!   pidfile), to check that the host kills what a plugin starts.
//! - `env`: iq in, event out; on `run` sends `{"env":{…},"cwd":"…"}`, its environment and
//!   working directory.
//! - `ignore-stop`: echoes, ignores `stop`, and never exits on its own after `run`.
//!
//! `closed`, `stop` (outside `ignore-stop`) and stdin EOF end the process with status 0.
//! `--pidfile` appends the process id, one per line, so a test can check it is gone.

use std::io::Write;
use std::time::Duration;

use serde_json::json;
use tokio::io::BufReader;
use zeta_core::stream::{Frame, Payload};
use zeta_core::types::{Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};
use zeta_plugin::wire::{
    Control, Hello, Message, PROTOCOL, encode_control, encode_frame, read_message,
};

fn send(bytes: &[u8]) {
    let mut out = std::io::stdout().lock();
    // The host going away is the end of the test; nothing to report.
    let _ = out.write_all(bytes).and_then(|()| out.flush());
}

fn send_frame(f: &Frame) {
    send(&encode_frame(f).expect("fake frames encode"));
}

fn prov() -> Provenance {
    Provenance {
        source: SourceId("fake".into()),
        job: None,
        channel: None,
        span: Span {
            start_ns: Timestamp(0),
            end_ns: Timestamp(0),
        },
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let mode = arg("--mode").unwrap_or_else(|| "echo".into());
    if let Some(path) = arg("--pidfile") {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("pidfile opens");
        writeln!(f, "{}", std::process::id()).expect("pidfile writes");
    }

    if mode == "fork" {
        // Deliberately never waited on: the host must kill it, not this process.
        #[allow(clippy::zombie_processes)]
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep starts");
        if let Some(path) = arg("--pidfile") {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(path)
                .expect("pidfile opens");
            writeln!(f, "{}", child.id()).expect("pidfile writes");
        }
    }
    if mode == "no-hello" {
        tokio::time::sleep(Duration::from_secs(10)).await;
        return;
    }
    let outputs = match mode.as_str() {
        "lag-report" | "stall" | "env" => vec![StreamKind::Event],
        _ => vec![StreamKind::Iq],
    };
    let params_schema = if mode == "loose-schema" {
        json!({"type": "object", "properties": {}})
    } else {
        json!({"type": "object", "additionalProperties": false})
    };
    send(&encode_control(&Control::Hello(Hello {
        protocol: PROTOCOL,
        kind: "fake".into(),
        inputs: if mode == "multi-input" {
            vec![StreamKind::Iq, StreamKind::Audio]
        } else {
            vec![StreamKind::Iq]
        },
        outputs,
        params_schema,
    })));

    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut out: Option<StreamId> = None;
    loop {
        let msg = match read_message(&mut stdin).await {
            Ok(Some(m)) => m,
            Ok(None) | Err(_) if mode == "ignore-stop" => {
                tokio::time::sleep(Duration::from_secs(60)).await;
                return;
            }
            Ok(None) => return,
            Err(e) => {
                eprintln!("fake: {e}");
                std::process::exit(2);
            }
        };
        match msg {
            Message::Control(Control::Run(r)) => {
                out = r.outputs.first().map(|o| o.stream.clone());
                if mode == "env"
                    && let Some(o) = &out
                {
                    let env: serde_json::Map<String, serde_json::Value> = std::env::vars()
                        .map(|(k, v)| (k, serde_json::Value::String(v)))
                        .collect();
                    let cwd = std::env::current_dir().expect("cwd");
                    send_frame(&Frame::new(
                        o.clone(),
                        prov(),
                        Payload::Event(json!({"env": env, "cwd": cwd.display().to_string()})),
                    ));
                }
                if mode == "stall" {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                if mode == "error" {
                    send(&encode_control(&Control::Error {
                        message: "boom".into(),
                    }));
                    std::process::exit(1);
                }
            }
            Message::Control(Control::Stop) if mode == "ignore-stop" => {
                eprintln!("fake: ignoring stop");
            }
            Message::Control(Control::Stop | Control::Closed { .. }) => return,
            Message::Control(Control::Lagged { missed }) => {
                if matches!(mode.as_str(), "lag-report" | "stall")
                    && let Some(o) = &out
                {
                    send_frame(&Frame::new(
                        o.clone(),
                        prov(),
                        Payload::Event(json!({ "lagged": missed })),
                    ));
                }
            }
            Message::Control(other) => {
                eprintln!("fake: unexpected {other:?}");
                std::process::exit(2);
            }
            Message::Frame(mut f) => {
                let Some(o) = out.clone() else {
                    eprintln!("fake: frame before run");
                    std::process::exit(2);
                };
                match mode.as_str() {
                    "crash-mid-frame" => {
                        let header = serde_json::to_vec(&json!({
                            "type": "frame", "stream": o, "kind": "iq", "source": "fake",
                            "job": null, "channel": null, "start_ns": 0, "end_ns": 0,
                            "payload": {"sample_rate_hz": 48000, "center_hz": 0, "samples": 1000}
                        }))
                        .expect("json");
                        let mut b = vec![0x00];
                        b.extend_from_slice(&(header.len() as u32).to_le_bytes());
                        b.extend_from_slice(&header);
                        b.extend_from_slice(&8000u32.to_le_bytes());
                        b.extend_from_slice(&[0u8; 12]);
                        send(&b);
                        std::process::exit(3);
                    }
                    "wrong-kind" => send_frame(&Frame::new(
                        o,
                        prov(),
                        Payload::Audio {
                            sample_rate_hz: zeta_core::types::Hz(48_000),
                            channels: 1,
                            samples: vec![0.5f32; 4].into(),
                        },
                    )),
                    "lag-report" | "stall" => {
                        let n = match &f.payload {
                            Payload::Iq(b) => b.samples.len(),
                            _ => 0,
                        };
                        send_frame(&Frame::new(
                            o,
                            prov(),
                            Payload::Event(json!({ "samples": n })),
                        ));
                    }
                    _ => {
                        f.stream = o;
                        send_frame(&f);
                    }
                }
            }
        }
    }
}
