//! Identity confirmation (plan 32, design 4.6) end to end: a `zeta serve` child with a
//! `--modes-dir` row whose subject declares `identity` (`src` against the register's
//! `callsign`, the SSID stripped), run by zeta-ext's `fake_lines` (pinned with
//! `ZETA_HANDLER_FAKE_LINES`, as `ui.rs` pins it), and `--knowledge-acma` pointing at a
//! 127.0.0.1 stub of the register service answering one synthetic `tx` row on the tone
//! band with callsign `XX4EX`. `fake_lines` prints one event every 0.5 s; once the job's
//! origins are looked up, the node confirms the lit site from it (once, however often it
//! repeats, and again after a fresh lookup), or, for another callsign, changes nothing.
//! The `origin` frames are counted in the node's `events.log`, which carries the
//! source's feed and so every frame of the job's stream.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::ServeProc;
use serde_json::{Value, json};
use zeta_client::NodeClient;

const SECRET: &str = "origin-identity-secret-0123";
/// The tone fixture's centre (`common::ServeProc` adds it as `tone`).
const TONE_HZ: u64 = 100_000_000;
/// The test row's band, as `ui.rs`'s `fake_pos` row is started on `tone`.
const BAND_HZ: u64 = 40_000;
/// `fake_lines` prints its line at once, then again after every this many input bytes:
/// 0.5 s of the 48 kHz cs16 IQ. Whenever the lookup lands, a later line follows it.
const REPEAT_BYTES: &str = "96000";
/// Decoded events seen after a point, before an absence there is believed: each one
/// went through the tee too, which reads the same stream.
const SETTLE_EVENTS: usize = 3;
/// How long an end state may take to show.
const WITHIN: Duration = Duration::from_secs(20);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root above crates/zeta")
        .to_path_buf()
}

/// The register's assignments: one `tx` row on the tone band, near the demo station.
fn register_rows() -> Value {
    json!({
        "columns": ["FREQUENCY", "BANDWIDTH", "EMISSION", "DEVICE_TYPE", "TRANSMITTER_POWER",
            "TRANSMITTER_POWER_UNIT", "LICENCE_NO", "SITE_NAME", "LATITUDE", "LONGITUDE",
            "LICENCEE", "CALL_SIGN"],
        "rows": [[TONE_HZ, 25000, "16K0F3E", "T", 5, "W", "0000001", "Example Hill",
            -27.35, 153.08, "Example Water Authority", "XX4EX"]],
        "truncated": false, "rowCount": 1
    })
}

/// The JSON-RPC answer to one MCP request; `None` for a notification.
fn mcp_answer(req: &Value) -> Option<Value> {
    let id = req["id"].clone();
    let result = match req["method"].as_str()? {
        "initialize" => json!({"protocolVersion": "2025-03-26"}),
        "tools/call" => {
            let out = match req["params"]["name"].as_str() {
                Some("execute_sql") => register_rows(),
                _ => json!({"match_count": 0, "allocation": null}),
            };
            json!({"content": [{"type": "text", "text": out.to_string()}]})
        }
        _ => return None,
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// One HTTP/1.1 request on `s`, answered and closed.
fn answer(mut s: TcpStream) {
    let mut r = BufReader::new(s.try_clone().expect("clone stream"));
    let mut start = String::new();
    if r.read_line(&mut start).is_err() {
        return;
    }
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).is_err() || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; len];
    if r.read_exact(&mut body).is_err() {
        return;
    }
    let (status, extra, text) = if start.starts_with("GET /health") {
        ("200 OK", "", r#"{"status":"ok"}"#.to_string())
    } else {
        let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        match mcp_answer(&req) {
            Some(v) => ("200 OK", "mcp-session-id: session-0\r\n", v.to_string()),
            None => ("202 Accepted", "", String::new()),
        }
    };
    let _ = write!(
        s,
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n{extra}\
         content-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    );
}

/// A stub of the register service on 127.0.0.1; its `http://HOST:PORT`. It serves as long
/// as the test process lives.
fn register_stub() -> String {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind the register stub");
    let url = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for s in l.incoming().map_while(Result::ok) {
            std::thread::spawn(move || answer(s));
        }
    });
    url
}

/// A `fake_lines` row shaped as `ui.rs`'s `fake_pos` row (so no sweep tries it) whose
/// events carry `src`, its subject matched by `src` against the `callsign`.
fn identity_row() -> Value {
    json!({
        "id": "fake_ident",
        "name": "Fake identities (tests)",
        "family": "general",
        "band": {"bandwidth_hz": BAND_HZ, "centers_hz": [999000000], "min_hz": BAND_HZ},
        "modulation": [],
        "label": "FAKE",
        "protocol": "fake_ident",
        "decoder": {
            "kind": "program",
            "programs": ["fake_lines"],
            "args": [],
            "input": {"stream": "iq", "format": "cs16", "rate_hz": 48000, "via": "stdin",
                      "container": "raw"},
            "output": {"via": "stdout", "port": null, "parser": "json_lines"},
            "install": {"url": "https://example.invalid/fake", "debian": null,
                        "fedora": null, "brew": null}
        },
        "event": {
            "schema": {"type": "object", "properties": {"src": {"type": "string"}}},
            "summary": "{src}"
        },
        "identify": {"proof": "event", "min_events": 1, "dwell_ms": 3000,
                     "min_span_ms": 1000, "quick": null},
        "catalogue": [],
        "test": null,
        "subject": {
            "key": "src",
            "name": "stations",
            "waterfall": "summary",
            "columns": [],
            "heading": null,
            "label": null,
            "identity": {"field": "src", "register": "callsign", "strip": "ssid"}
        }
    })
}

/// A node serving the identity row, with `fake_lines` printing `line`, and the register stub.
fn node(dir: &Path, line: &str) -> ServeProc {
    let modes = dir.join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    std::fs::write(modes.join("fake_ident.json"), identity_row().to_string()).unwrap();
    let fake_lines = repo_root().join("crates/zeta-ext/tests/fake_lines");
    let register = register_stub();
    ServeProc::start_full(
        dir,
        SECRET,
        &[
            "--modes-dir",
            modes.to_str().unwrap(),
            "--knowledge-acma",
            &register,
        ],
        &[],
        &[
            ("ZETA_HANDLER_FAKE_LINES", fake_lines.to_str().unwrap()),
            ("ZETA_PLUGIN_ENV_FAKE_LINES", line),
            ("ZETA_PLUGIN_ENV_FAKE_AFTER_BYTES", "0"),
            ("ZETA_PLUGIN_ENV_FAKE_REPEAT_BYTES", REPEAT_BYTES),
        ],
    )
}

async fn op(c: &NodeClient, v: Value) -> Value {
    match c.apply_value(v.clone()).await.expect("the node answers") {
        Ok(r) => r["value"].clone(),
        Err(e) => panic!("{v} refused: {e}"),
    }
}

/// Set the demo station, start the identity row on the tone band and look its origins up;
/// the job's id.
async fn start_and_look_up(c: &NodeClient) -> String {
    op(
        c,
        json!({"op": "station_set", "lat": -27.3842, "lon": 153.1175, "alt_m": 4.0,
               "region": "AU", "antennas": null}),
    )
    .await;
    let job = op(
        c,
        json!({"op": "job_start", "kind": "fake_ident", "source": "tone",
               "center_hz": TONE_HZ, "bandwidth_hz": BAND_HZ, "deviation_hz": null,
               "sideband": null, "elastic": false, "replace": []}),
    )
    .await;
    let job = job.as_str().expect("a job id").to_string();
    look_up(c, &job).await;
    job
}

/// `origin_lookup` by `job`; the XX4EX row must be lit.
async fn look_up(c: &NodeClient, job: &str) {
    let a = op(
        c,
        json!({"op": "origin_lookup", "job": job, "source": null, "center_hz": null,
               "bandwidth_hz": null, "modulation": null}),
    )
    .await;
    let calls: Vec<&str> = a["origins"]
        .as_array()
        .expect("origins")
        .iter()
        .filter_map(|o| o["allocation"]["callsign"].as_str())
        .collect();
    assert_eq!(calls, ["XX4EX"], "the XX4EX row is lit: {a}");
}

/// The `events.log` lines of `job` whose event's `protocol` is `protocol`, each with its
/// `t_ns` (when it was logged) and `event`.
fn logged(dir: &Path, job: &str, protocol: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(dir.join("state/events.log")).unwrap_or_default();
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|l| l["job"] == job && l["event"]["protocol"] == protocol)
        .collect()
}

/// The `origin` frames of `job` in `events.log`, once there are at least `n`.
async fn origin_frames(dir: &Path, job: &str, n: usize) -> Vec<Value> {
    until(WITHIN, async || {
        let f: Vec<Value> = logged(dir, job, "origin")
            .into_iter()
            .map(|l| l["event"].clone())
            .collect();
        (f.len() >= n).then_some(f)
    })
    .await
    .unwrap_or_else(|| {
        panic!(
            "fewer than {n} origin frames: {:?}",
            logged(dir, job, "origin")
        )
    })
}

/// Wait until [`SETTLE_EVENTS`] decoded events of `job` were logged after the last
/// `origin` frame was.
async fn settle(dir: &Path, job: &str) {
    let last = logged(dir, job, "origin")
        .last()
        .map(|l| l["t_ns"].as_u64().unwrap())
        .expect("an origin frame");
    until(WITHIN, async || {
        let after = logged(dir, job, "fake_ident")
            .iter()
            .filter(|l| l["t_ns"].as_u64().unwrap() > last)
            .count();
        (after >= SETTLE_EVENTS).then_some(())
    })
    .await
    .expect("decoded events after the last origin frame");
}

/// Poll until `job`'s kept origins lead with one confirmed by `XX4EX`; that origin.
async fn confirmed(c: &NodeClient, job: &str) -> Value {
    let Some(origin) = until(WITHIN, async || {
        let ctx = context(c, job).await;
        let o = ctx["origins"]["origins"][0].clone();
        (o["confirmed"] == json!({"field": "src", "value": "XX4EX"})).then_some(o)
    })
    .await
    else {
        panic!("no confirmation: {}", context(c, job).await);
    };
    origin
}

/// Poll `f` until it gives `Some`, at most `within`.
async fn until<T>(within: Duration, mut f: impl AsyncFnMut() -> Option<T>) -> Option<T> {
    let end = Instant::now() + within;
    loop {
        if let Some(v) = f().await {
            return Some(v);
        }
        if Instant::now() >= end {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn context(c: &NodeClient, job: &str) -> Value {
    op(c, json!({"op": "job_context", "job": job})).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_decoded_callsign_confirms_a_lit_site_and_republishes() {
    let dir = tempfile::tempdir().unwrap();
    let serve = node(dir.path(), r#"{"src":"XX4EX-7"}"#);
    let c = NodeClient::new(&serve.url, SECRET).unwrap();
    let job = start_and_look_up(&c).await;
    let origin = confirmed(&c, &job).await;
    assert_eq!(origin["score"], 1.0);
    assert_eq!(origin["allocation"]["callsign"], "XX4EX");
    let summary = "confirmed XX4EX: Example Water Authority, Example Hill";
    let frames = origin_frames(dir.path(), &job, 2).await;
    assert!(frames[0]["origins"][0]["confirmed"].is_null(), "{frames:?}");
    assert_eq!(frames[1]["summary"], summary);
    assert_eq!(frames[1]["origins"][0]["confirmed"]["value"], "XX4EX");
    // The same callsign heard again republishes nothing.
    settle(dir.path(), &job).await;
    assert_eq!(logged(dir.path(), &job, "origin").len(), 2);
    // A fresh lookup replaces the kept origins; the station confirms them again.
    look_up(&c, &job).await;
    let frames = origin_frames(dir.path(), &job, 4).await;
    assert!(frames[2]["origins"][0]["confirmed"].is_null(), "{frames:?}");
    assert_eq!(frames[3]["summary"], summary);
    assert_eq!(confirmed(&c, &job).await["score"], 1.0);
    settle(dir.path(), &job).await;
    assert_eq!(logged(dir.path(), &job, "origin").len(), 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mismatching_callsign_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let serve = node(dir.path(), r#"{"src":"XX4ZZ"}"#);
    let c = NodeClient::new(&serve.url, SECRET).unwrap();
    let job = start_and_look_up(&c).await;
    // The lookup's frame logged, then several XX4ZZ events after it.
    origin_frames(dir.path(), &job, 1).await;
    settle(dir.path(), &job).await;
    let ctx = context(&c, &job).await;
    let origins = ctx["origins"]["origins"].as_array().expect("origins kept");
    assert!(origins.iter().all(|o| o["confirmed"].is_null()), "{ctx}");
    assert_eq!(logged(dir.path(), &job, "origin").len(), 1);
}
