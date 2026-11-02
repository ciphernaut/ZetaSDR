//! `sink_sondehub` against a local listener that speaks just enough HTTP/1.1 to stand
//! in for the SondeHub API.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Stage, StageContext, StageError};
use zeta_core::station::Station;
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Provenance, SourceId, Span, StreamId, Timestamp};
use zeta_ext::SondehubStage;

/// One request the listener read: method, path and parsed JSON body.
struct Seen {
    method: String,
    path: String,
    body: Value,
}

/// A listener answering every request `status`; its URL and the requests it read.
async fn api(status: u16) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/sondes/telemetry", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let (head, len) = loop {
                    let mut chunk = [0u8; 4096];
                    let n = s.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..i]).to_string();
                        let len = head
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().ok())?
                            })
                            .unwrap_or(0);
                        buf.drain(..i + 4);
                        break (head, len);
                    }
                };
                while buf.len() < len {
                    let mut chunk = [0u8; 4096];
                    let n = s.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let mut first = head.lines().next().unwrap_or("").split(' ');
                let (method, path) = (
                    first.next().unwrap_or("").to_string(),
                    first.next().unwrap_or("").to_string(),
                );
                let body = serde_json::from_slice(&buf[..len]).unwrap_or(Value::Null);
                log.lock().unwrap().push(Seen { method, path, body });
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = s.write_all(reply.as_bytes()).await;
                let _ = s.shutdown().await;
            });
        }
    });
    (url, seen)
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

fn rs41(id: &str, frame: u64) -> Value {
    json!({
        "protocol": "rs41", "t_ns": 5, "summary": "x", "id": id, "frame": frame,
        "datetime": "2026-10-29T01:02:03.000Z", "lat": -27.5, "lon": 153.0, "alt": 1200.5,
        "vel_h": 5.5, "vel_v": -2.0, "heading": 270.0, "sats": 9, "batt": 2.7,
        "position": {"lat": -27.5, "lon": 153.0, "alt_m": 1200.5}
    })
}

struct Running {
    tx: broadcast::Sender<Frame>,
    context: Arc<Mutex<Value>>,
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<Result<(), StageError>>,
}

impl Running {
    fn send(&self, ev: Value) {
        let f = Frame::new(StreamId::new(), prov(), Payload::Event(ev));
        self.tx.send(f).unwrap();
    }

    fn sondehub(&self) -> Value {
        self.context.lock().unwrap()["sondehub"].clone()
    }
}

fn start(url: &str, batch_ms: u64) -> Running {
    let params = json!({
        "callsign": "TEST", "lat": -27.5, "lon": 153.0, "alt_m": 10.0,
        "batch_ms": batch_ms, "url": url
    });
    start_with(params, None)
}

fn start_with(params: Value, station: Option<Station>) -> Running {
    let bus = Arc::new(StreamBus::new(64));
    let (tx, rx_in) = broadcast::channel(64);
    let context = Arc::new(Mutex::new(json!({})));
    let c = context.clone();
    let ctx = StageContext::new(SourceId("s".into()))
        .with_note(Arc::new(move |v: Value| {
            let mut cx = c.lock().unwrap();
            for (k, x) in v.as_object().unwrap() {
                cx[k] = x.clone();
            }
        }))
        .with_station(station);
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    let handle = tokio::spawn(async move {
        SondehubStage::new()
            .run(params, vec![rx_in], vec![], bus, c2, ctx)
            .await
    });
    Running {
        tx,
        context,
        cancel,
        handle,
    }
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let end = tokio::time::Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(tokio::time::Instant::now() < end, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn stop(r: Running) {
    r.cancel.cancel();
    let end = r.handle.await.unwrap();
    assert!(matches!(end, Err(StageError::Cancelled)), "{end:?}");
}

#[tokio::test]
async fn a_batch_is_posted_with_the_station_fields() {
    let (url, seen) = api(200).await;
    let r = start(&url, 300);
    r.send(rs41("R1234567", 10));
    r.send(rs41("R1234567", 11));
    until("a request", || !seen.lock().unwrap().is_empty()).await;
    until("sent counted", || r.sondehub()["sent"] == 2).await;
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].method, "PUT");
        assert_eq!(seen[0].path, "/sondes/telemetry");
        let a = seen[0].body.as_array().expect("a JSON array");
        assert_eq!(a.len(), 2, "{a:?}");
        let e = &a[0];
        assert_eq!(e["software_name"], "zetasdr");
        assert_eq!(e["software_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(e["uploader_callsign"], "TEST");
        assert_eq!(e["uploader_position"], json!([-27.5, 153.0, 10.0]));
        assert!(e["time_received"].as_str().unwrap().ends_with('Z'), "{e}");
        assert_eq!(e["datetime"], "2026-10-29T01:02:03.000Z");
        assert_eq!(e["manufacturer"], "Vaisala");
        assert_eq!(e["type"], "RS41");
        assert_eq!(e["serial"], "R1234567");
        assert_eq!(e["frame"], 10);
        assert_eq!(a[1]["frame"], 11);
        for (k, v) in [
            ("lat", json!(-27.5)),
            ("lon", json!(153.0)),
            ("alt", json!(1200.5)),
            ("vel_h", json!(5.5)),
            ("vel_v", json!(-2.0)),
            ("heading", json!(270.0)),
            ("sats", json!(9)),
            ("batt", json!(2.7)),
        ] {
            assert_eq!(e[k], v, "{k}");
        }
    }
    let c = r.sondehub();
    assert_eq!(c["failed"], 0, "{c}");
    stop(r).await;
}

#[tokio::test]
async fn events_without_a_position_are_skipped() {
    let (url, seen) = api(200).await;
    let r = start(&url, 300);
    let mut no_pos = rs41("R1", 1);
    no_pos.as_object_mut().unwrap().remove("position");
    r.send(no_pos);
    r.send(json!({"protocol": "pocsag", "position": {"lat": 1.0, "lon": 2.0, "alt_m": 3.0}}));
    // SondeHub rejects telemetry without a datetime, a frame or a numeric altitude.
    for (key, value) in [
        ("datetime", None),
        ("frame", None),
        (
            "position",
            Some(json!({"lat": -27.5, "lon": 153.0, "alt_m": null})),
        ),
        ("position", Some(json!({"lat": -27.5, "lon": 153.0}))),
    ] {
        let mut ev = rs41("R3", 3);
        let o = ev.as_object_mut().unwrap();
        match value {
            None => o.remove(key),
            Some(v) => o.insert(key.into(), v),
        };
        r.send(ev);
    }
    r.send(rs41("R2", 2));
    until("a request", || !seen.lock().unwrap().is_empty()).await;
    {
        let seen = seen.lock().unwrap();
        let a = seen[0].body.as_array().unwrap();
        assert_eq!(a.len(), 1, "{a:?}");
        assert_eq!(a[0]["serial"], "R2");
    }
    // Skipped, not failed.
    until("the batch counted", || r.sondehub()["sent"] == 1).await;
    assert_eq!(r.sondehub()["failed"], 0);
    // A batch of only skipped events sends nothing.
    r.send(json!({"protocol": "dfm", "id": "D1"}));
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(seen.lock().unwrap().len(), 1);
    stop(r).await;
}

#[tokio::test]
async fn a_failed_post_counts_and_does_not_retry_in_a_loop() {
    let (url, seen) = api(500).await;
    let r = start(&url, 300);
    r.send(rs41("R1", 1));
    until("failed counted", || r.sondehub()["failed"] == 1).await;
    let c = r.sondehub();
    assert_eq!(c["sent"], 0, "{c}");
    assert!(c["last_error"].as_str().unwrap().contains("500"), "{c}");
    // No retry of that batch: one request while nothing new arrives.
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(seen.lock().unwrap().len(), 1);
    // The next batch still goes.
    r.send(rs41("R1", 2));
    until("a second request", || seen.lock().unwrap().len() == 2).await;
    until("second failure", || r.sondehub()["failed"] == 2).await;
    stop(r).await;
}

#[tokio::test]
async fn an_unreachable_api_counts_a_failure() {
    let r = start("http://127.0.0.1:1/", 200);
    r.send(rs41("R1", 1));
    until("failed counted", || r.sondehub()["failed"] == 1).await;
    assert!(r.sondehub()["last_error"].is_string());
    stop(r).await;
}

fn station() -> Station {
    Station {
        lat: -27.38,
        lon: 153.12,
        alt_m: 40.0,
        region: "AU".into(),
        antennas: vec![],
    }
}

#[tokio::test]
async fn an_omitted_position_takes_the_station() {
    let (url, seen) = api(200).await;
    let params = json!({"callsign": "TEST", "batch_ms": 300, "url": url});
    let r = start_with(params, Some(station()));
    r.send(rs41("R1", 1));
    until("sent counted", || r.sondehub()["sent"] == 1).await;
    {
        let seen = seen.lock().unwrap();
        let e = &seen[0].body[0];
        assert_eq!(e["uploader_position"], json!([-27.3842, 153.1175, 4.0]));
    }
    stop(r).await;
}

#[tokio::test]
async fn an_omitted_position_without_a_station_idles_with_a_context_note() {
    let params = json!({"callsign": "TEST"});
    let r = start_with(params, None);
    until("the idle note", || r.sondehub()["idle"] == true).await;
    let note = r.sondehub();
    assert!(
        note["last_error"]
            .as_str()
            .unwrap()
            .starts_with("sink.params.lat: no station set (station set --lat --lon --alt-m)"),
        "{note}"
    );
    assert_eq!(note["sent"], 0);
    // It keeps consuming events and never ends on its own (a decoder it rides on runs on).
    r.send(rs41("R1", 1));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!r.handle.is_finished());
    stop(r).await;
}
