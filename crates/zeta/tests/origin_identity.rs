//! Identity confirmation (plan 32, design 4.6) end to end: a `zeta serve` child with a
//! `--modes-dir` row whose subject declares `identity` (`src` against the register's
//! `callsign`, the SSID stripped), run by zeta-ext's `fake_lines` (pinned with
//! `ZETA_HANDLER_FAKE_LINES`, as `ui.rs` pins it), and `--knowledge-acma` pointing at
//! zeta-ext's register stub (`testing::acma_stub`) on 127.0.0.1 answering one synthetic
//! `tx` row on the tone band with callsign `XX4EX`. `fake_lines` prints one event every 0.5 s; once the job's
//! origins are looked up, the node confirms the lit site from it (once, however often it
//! repeats, and again after a fresh lookup), or, for another callsign, changes nothing.
//! The `origin` frames are counted in the node's `events.log`, which carries the
//! source's feed and so every frame of the job's stream.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::ServeProc;
use serde_json::{Value, json};
use zeta_client::NodeClient;
use zeta_ext::testing::acma_stub::{self, Mode};

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

/// The register's assignments: one `tx` row on the tone band, near the demo station, in
/// the stub's column order (`acma_stub::COLUMNS`).
fn register_rows() -> Vec<Value> {
    vec![json!([
        TONE_HZ,
        25000,
        "16K0F3E",
        "T",
        5,
        "W",
        "0000001",
        "Example Hill",
        -27.35,
        153.08,
        "Example Water Authority",
        "XX4EX"
    ])]
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
    serve(
        dir,
        &identity_row(),
        &[
            ("ZETA_PLUGIN_ENV_FAKE_LINES", line),
            ("ZETA_PLUGIN_ENV_FAKE_AFTER_BYTES", "0"),
            ("ZETA_PLUGIN_ENV_FAKE_REPEAT_BYTES", REPEAT_BYTES),
        ],
    )
}

/// A node serving `row` alone, `fake_lines` pinned and run with `env`, and the register
/// stub.
fn serve(dir: &Path, row: &Value, env: &[(&str, &str)]) -> ServeProc {
    let modes = dir.join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    let file = format!("{}.json", row["id"].as_str().unwrap());
    std::fs::write(modes.join(file), row.to_string()).unwrap();
    let fake_lines = repo_root().join("crates/zeta-ext/tests/fake_lines");
    let register = acma_stub::serve_on_thread(register_rows(), Mode::Normal);
    let mut env = env.to_vec();
    env.push(("ZETA_HANDLER_FAKE_LINES", fake_lines.to_str().unwrap()));
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
        &env,
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
    let job = start(c, "fake_ident").await;
    look_up(c, &job).await;
    job
}

/// Set the demo station and start the row `kind` on the tone band; the job's id.
async fn start(c: &NodeClient, kind: &str) -> String {
    op(
        c,
        json!({"op": "station_set", "lat": -27.3842, "lon": 153.1175, "alt_m": 4.0,
               "region": "AU", "antennas": null}),
    )
    .await;
    let job = op(
        c,
        json!({"op": "job_start", "kind": kind, "source": "tone",
               "center_hz": TONE_HZ, "bandwidth_hz": BAND_HZ, "deviation_hz": null,
               "sideband": null, "elastic": false, "replace": []}),
    )
    .await;
    job.as_str().expect("a job id").to_string()
}

/// `origin_lookup` by `job`; the XX4EX row must be lit. The answer.
async fn look_up(c: &NodeClient, job: &str) -> Value {
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
    a
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
    settle_on(dir, job, "fake_ident", SETTLE_EVENTS).await;
}

/// Wait until `n` decoded events of `job` (of `protocol`) were logged after the last
/// `origin` frame was.
async fn settle_on(dir: &Path, job: &str, protocol: &str, n: usize) {
    let last = logged(dir, job, "origin")
        .last()
        .map(|l| l["t_ns"].as_u64().unwrap())
        .expect("an origin frame");
    until(WITHIN, async || {
        let after = logged(dir, job, protocol)
            .iter()
            .filter(|l| l["t_ns"].as_u64().unwrap() > last)
            .count();
        (after >= n).then_some(())
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
    // A fresh lookup replaces the kept origins and keeps their confirmation: its frame
    // already carries it, so the station heard again republishes nothing.
    look_up(&c, &job).await;
    let frames = origin_frames(dir.path(), &job, 3).await;
    let first = &frames[2]["origins"][0];
    assert_eq!(
        first["confirmed"],
        json!({"field": "src", "value": "XX4EX"}),
        "{frames:?}"
    );
    assert_eq!(first["score"], 1.0);
    assert_eq!(confirmed(&c, &job).await["score"], 1.0);
    settle(dir.path(), &job).await;
    assert_eq!(logged(dir.path(), &job, "origin").len(), 3);
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

/// The station row's decoded protocol.
const GS: &str = "fake_gs";
/// `fake_lines` repeats every 0.2 s of the 48 kHz cs16 IQ.
const REPEAT_FAST_BYTES: &str = "38400";
/// 2 s of the IQ before `fake_lines` prints, so the lookup usually lands first.
const AFTER_LOOKUP_BYTES: &str = "384000";

/// The station row's lines: an aircraft addressing `GS1`, `GS1`'s own frame, an aircraft
/// addressing `GS2`; so `GS1` is new, then rises to `from`, then `GS2` is new.
const GS_LINES: &str = concat!(
    r#"{"kind":"ac","icao":"7C0001","to_gs":"GS1"}"#,
    "\n",
    r#"{"kind":"gs","icao":"GS1","gs":"GS1"}"#,
    "\n",
    r#"{"kind":"ac","icao":"7C0002","to_gs":"GS2"}"#,
    "\n"
);

/// [`identity_row`] reshaped for fixed stations (plan 35, design 4.1): an event of `kind`
/// `gs` is a station's own, named by `gs`; any other names the station it addresses in
/// `to_gs`. Its lines are read from `lines`.
fn station_row(lines: &Path) -> Value {
    let mut row = identity_row();
    row["id"] = GS.into();
    row["name"] = "Fake ground stations (tests)".into();
    row["protocol"] = GS.into();
    row["decoder"]["args"] = json!(["--lines-file", lines.to_str().unwrap()]);
    row["event"] = json!({
        "schema": {"type": "object", "properties": {
            "kind": {"type": "string"}, "icao": {"type": "string"},
            "gs": {"type": "string"}, "to_gs": {"type": "string"}}},
        "summary": "{icao}"
    });
    row["subject"] = json!({
        "key": "icao",
        "name": "aircraft",
        "waterfall": "summary",
        "columns": [],
        "heading": null,
        "label": null,
        "identity": null,
        "station": {"when": {"kind": "gs"}, "key": "gs", "addressed": "to_gs"}
    });
    row
}

/// A node serving the station row, `fake_lines` printing [`GS_LINES`] after `after` input
/// bytes and again every `repeat` more (`0`: never).
fn station_node(dir: &Path, after: &str, repeat: &str) -> ServeProc {
    let lines = dir.join("gs.lines");
    std::fs::write(&lines, GS_LINES).unwrap();
    serve(
        dir,
        &station_row(&lines),
        &[
            ("ZETA_PLUGIN_ENV_FAKE_AFTER_BYTES", after),
            ("ZETA_PLUGIN_ENV_FAKE_REPEAT_BYTES", repeat),
        ],
    )
}

/// `from` and `to` of `key` in a `stations` tally (0 for a key not there).
fn counts(stations: &Value, key: &str) -> (u64, u64) {
    let s = &stations[key];
    (
        s["from"].as_u64().unwrap_or(0),
        s["to"].as_u64().unwrap_or(0),
    )
}

/// Poll until `job_context.stations` passes `ok`; the stations.
async fn stations_until(c: &NodeClient, job: &str, ok: impl Fn(&Value) -> bool) -> Value {
    let Some(s) = until(WITHIN, async || {
        let s = context(c, job).await["stations"].clone();
        ok(&s).then_some(s)
    })
    .await
    else {
        panic!("stations never as expected: {}", context(c, job).await);
    };
    s
}

/// The single burst's tally: `GS1` heard once and addressed once, `GS2` addressed once.
fn one_burst(s: &Value) -> bool {
    counts(s, "GS1") == (1, 1) && counts(s, "GS2") == (0, 1)
}

/// Both keys seen and `GS1`'s grade risen: the immediate writes are done.
fn both_keys(s: &Value) -> bool {
    counts(s, "GS1").0 >= 1 && counts(s, "GS2").1 >= 1
}

/// Every origin of an `origin` frame or answer `a` is heard, graded `from`.
fn all_heard_from(a: &Value) -> bool {
    let origins = a["origins"].as_array().expect("origins");
    !origins.is_empty() && origins.iter().all(|o| o["heard"]["grade"] == "from")
}

#[tokio::test(flavor = "multi_thread")]
async fn station_frames_fill_the_tally_and_republish_with_heard() {
    let dir = tempfile::tempdir().unwrap();
    let serve = station_node(dir.path(), AFTER_LOOKUP_BYTES, "0");
    let c = NodeClient::new(&serve.url, SECRET).unwrap();
    let job = start(&c, GS).await;
    look_up(&c, &job).await;
    stations_until(&c, &job, one_burst).await;
    // Whether the lookup or the burst came first, the last frame carries the tally.
    let Some(last) = until(WITHIN, async || {
        let f = logged(dir.path(), &job, "origin").pop()?["event"].clone();
        one_burst(&f["stations"]).then_some(f)
    })
    .await
    else {
        panic!(
            "no origin frame with the tally: {:?}",
            logged(dir.path(), &job, "origin")
        );
    };
    assert!(all_heard_from(&last), "{last}");
    let summary = last["summary"].as_str().unwrap();
    assert!(summary.starts_with("2 ground stations on "), "{summary}");
    assert!(
        summary.contains("GS1 heard 1 frame, GS2 addressed by 1 frame; "),
        "{summary}"
    );
    let kept = &context(&c, &job).await["origins"];
    assert!(
        one_burst(&kept["stations"]) && all_heard_from(kept),
        "{kept}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_lookup_answers_heard_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let serve = station_node(dir.path(), "0", "0");
    let c = NodeClient::new(&serve.url, SECRET).unwrap();
    let job = start(&c, GS).await;
    // Written with no answer kept: the tally only, nothing published.
    stations_until(&c, &job, one_burst).await;
    assert!(logged(dir.path(), &job, "origin").is_empty());
    let a = look_up(&c, &job).await;
    assert!(one_burst(&a["stations"]) && all_heard_from(&a), "{a}");
    let frames = origin_frames(dir.path(), &job, 1).await;
    let summary = frames[0]["summary"].as_str().unwrap();
    assert!(summary.starts_with("2 ground stations on "), "{summary}");
    assert!(all_heard_from(&frames[0]), "{frames:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_tally_is_written_within_the_window_not_per_frame() {
    let dir = tempfile::tempdir().unwrap();
    let serve = station_node(dir.path(), "0", REPEAT_FAST_BYTES);
    let c = NodeClient::new(&serve.url, SECRET).unwrap();
    let job = start(&c, GS).await;
    look_up(&c, &job).await;
    stations_until(&c, &job, both_keys).await;
    settle_on(dir.path(), &job, GS, SETTLE_EVENTS).await;
    let n = logged(dir.path(), &job, "origin").len();
    // The lookup's frame, and one per new key or grade rise after it: at most four.
    assert!((1..=4).contains(&n), "{n} origin frames");
    // Three seconds of repeats (15 bursts of 3 lines) republish nothing.
    settle_on(dir.path(), &job, GS, 45).await;
    let frames = logged(dir.path(), &job, "origin");
    assert_eq!(frames.len(), n, "a repeat republished: {frames:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_flushes_the_counts_the_window_held() {
    let dir = tempfile::tempdir().unwrap();
    let serve = station_node(dir.path(), "0", REPEAT_FAST_BYTES);
    let c = NodeClient::new(&serve.url, SECRET).unwrap();
    let job = start(&c, GS).await;
    look_up(&c, &job).await;
    let before = stations_until(&c, &job, both_keys).await;
    // Repeats counted and held back by the window, then the stop: the supervisor writes
    // them, the tee having ended first.
    settle_on(dir.path(), &job, GS, 9).await;
    op(&c, json!({"op": "job_stop", "job": job})).await;
    let after = stations_until(&c, &job, |s| counts(s, "GS1").1 > counts(&before, "GS1").1).await;
    assert!(
        counts(&after, "GS2").1 > counts(&before, "GS2").1,
        "{after}"
    );
    let flushed = until(WITHIN, async || {
        let last = logged(dir.path(), &job, "origin").pop()?;
        (last["event"]["stations"] == after).then_some(())
    })
    .await;
    assert!(
        flushed.is_some(),
        "not republished with the flush: {:?}",
        logged(dir.path(), &job, "origin")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_continues_the_counts() {
    let dir = tempfile::tempdir().unwrap();
    let mut first = station_node(dir.path(), "0", REPEAT_BYTES);
    let c = NodeClient::new(&first.url, SECRET).unwrap();
    let job = start(&c, GS).await;
    look_up(&c, &job).await;
    let before = stations_until(&c, &job, both_keys).await;
    first.terminate(Duration::from_secs(10));
    drop(first);
    let total = |s: &Value| -> u64 {
        ["GS1", "GS2"]
            .iter()
            .map(|k| counts(s, k).0 + counts(s, k).1)
            .sum()
    };
    let serve = station_node(dir.path(), "0", REPEAT_BYTES);
    let c = NodeClient::new(&serve.url, SECRET).unwrap();
    // The resumed run's first write: no count went back, and the total grew.
    let after = stations_until(&c, &job, |s| total(s) > total(&before)).await;
    for k in ["GS1", "GS2"] {
        let (b, a) = (counts(&before, k), counts(&after, k));
        assert!(a.0 >= b.0 && a.1 >= b.1, "{k} reset: {before} then {after}");
    }
}
