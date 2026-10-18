//! End-to-end tests of the `zeta` binary.

use std::path::Path;

use assert_cmd::Command;
use predicates::str::contains;
use serde_json::Value;
use zeta_source_file::fixtures::write_tone;

/// The `zeta` binary with `ZETA_NODE` and `ZETA_SECRET` removed, so a caller's environment
/// cannot send a test to a remote node. Every test here starts from this.
fn zeta_bin() -> Command {
    let mut cmd = Command::cargo_bin("zeta").unwrap();
    cmd.env_remove("ZETA_NODE").env_remove("ZETA_SECRET");
    cmd
}

/// `zeta --state-dir <dir>/state --source-file tone=<fixture> --json <args…>`.
fn zeta(dir: &Path, args: &[&str]) -> Command {
    let meta = write_tone(dir);
    let mut cmd = zeta_bin();
    cmd.arg("--state-dir")
        .arg(dir.join("state"))
        .arg("--source-file")
        .arg(format!("tone={}", meta.display()))
        .arg("--json")
        .args(args);
    cmd
}

fn json(out: &[u8]) -> Value {
    serde_json::from_slice(out).expect("stdout is JSON")
}

#[test]
fn schema_prints_json_with_op_result_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = zeta(dir.path(), &["schema"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["result"], "schema");
    for key in ["op", "result", "error", "stages"] {
        assert!(v["value"].get(key).is_some(), "schema missing {key}");
    }
    let kinds: Vec<&str> = v["value"]["stages"]
        .as_array()
        .expect("stages array")
        .iter()
        .filter_map(|s| s["kind"].as_str())
        .collect();
    for k in [
        "passthrough",
        "channel",
        "fft",
        "nbfm_demod",
        "aprs_decode",
        "measure",
    ] {
        assert!(kinds.contains(&k), "stages missing {k}: {kinds:?}");
    }
}

#[test]
fn source_list_shows_file_source() {
    let dir = tempfile::tempdir().unwrap();
    let out = zeta(dir.path(), &["source", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    let sources = v["value"].as_array().expect("sources array");
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0]["id"], "tone");
}

#[test]
fn ensure_dropped_zeros_exit_2_with_suggestion() {
    let dir = tempfile::tempdir().unwrap();
    let args = [
        "source",
        "ensure",
        "--source",
        "tone",
        "--center-hz",
        "100000",
        "--bandwidth-hz",
        "48000",
    ];
    let out = zeta(dir.path(), &args)
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["error"], "out_of_range");
    assert_eq!(v["field"], "center_hz");
    assert_eq!(v["suggested"], 100_000_000);
}

#[test]
fn job_start_persists_and_list_sees_it_in_new_process() {
    let dir = tempfile::tempdir().unwrap();
    let start = [
        "job",
        "start",
        "--kind",
        "passthrough",
        "--source",
        "tone",
        "--center-hz",
        "100000000",
        "--bandwidth-hz",
        "48000",
    ];
    let out = zeta(dir.path(), &start)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["result"], "job");
    let id = v["value"].as_str().expect("job id").to_owned();

    let out = zeta(dir.path(), &["job", "list"])
        .assert()
        .success()
        .stdout(contains(id.as_str()))
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    let jobs = v["value"].as_array().expect("jobs array");
    let job = jobs
        .iter()
        .find(|j| j["id"] == id.as_str())
        .expect("started job listed");
    assert_eq!(job["state"]["status"], "running");
}

#[test]
fn iq_read_one_shot_writes_capture() {
    let dir = tempfile::tempdir().unwrap();
    let out = zeta(
        dir.path(),
        &[
            "iq",
            "read",
            "--source",
            "tone",
            "--start-ns",
            "0",
            "--end-ns",
            "1000000000",
            "--center-hz",
            "100000000",
            "--bandwidth-hz",
            "48000",
        ],
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    let meta = std::path::PathBuf::from(v["value"]["path"].as_str().expect("capture path"));
    assert!(meta.exists(), "{}", meta.display());
    assert!(meta.starts_with(dir.path().join("state").join("captures")));
    assert!(meta.with_extension("sigmf-data").exists());
}

#[test]
fn mutating_one_shot_refused_while_serve_lock_held() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let pid = std::process::id();
    std::fs::write(state.join("serve.lock"), pid.to_string()).unwrap();
    let band = [
        "--source",
        "tone",
        "--center-hz",
        "100000000",
        "--bandwidth-hz",
        "48000",
    ];
    let start: Vec<&str> = ["job", "start", "--kind", "passthrough"]
        .into_iter()
        .chain(band)
        .collect();
    let ensure: Vec<&str> = ["source", "ensure"].into_iter().chain(band).collect();
    let graph = dir.path().join("g.json");
    std::fs::write(&graph, "{}").unwrap();
    let put = [
        "graph",
        "put",
        "--job",
        "j",
        "--file",
        graph.to_str().unwrap(),
    ];
    let stop = ["job", "stop", "--job", "j"];
    let want = format!("zeta serve (pid {pid}) owns this state dir");
    for args in [&start[..], &ensure[..], &put[..], &stop[..]] {
        let out = zeta(dir.path(), args)
            .assert()
            .code(2)
            .get_output()
            .stdout
            .clone();
        let v = json(&out);
        assert_eq!(v["error"], "other", "{args:?}");
        assert_eq!(v["message"], want.as_str(), "{args:?}");
    }
    assert!(
        !state.join("jobs").exists() || std::fs::read_dir(state.join("jobs")).unwrap().count() == 0
    );
    // Reads still work, and a stale lock (dead pid) does not block.
    zeta(dir.path(), &["job", "list"]).assert().success();
    std::fs::write(state.join("serve.lock"), "4194305").unwrap();
    zeta(dir.path(), &start).assert().success();
}

#[test]
fn repeated_source_file_id_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let meta = write_tone(dir.path());
    let tone = format!("tone={}", meta.display());
    zeta_bin()
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .args([
            "--source-file",
            &tone,
            "--source-file",
            &tone,
            "source",
            "list",
        ])
        .assert()
        .code(1)
        .stderr(contains("tone"))
        .stderr(contains("more than once"));
}

/// Write the APRS fixture in `dir`; returns the `--source-file aprs=<meta>` value.
fn aprs_source(dir: &Path) -> String {
    let iq = zeta_dsp::fixtures::aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 0.0);
    let meta = zeta_dsp::fixtures::write_fixture(dir, "aprs", &iq, 48_000, 144_800_000);
    format!("aprs={}", meta.display())
}

/// `job start` of an APRS decode on the `aprs` source, tailing `then_tail`, plus `extra`.
fn aprs_start(dir: &Path, then_tail: &str, extra: &[&str]) -> Command {
    let src = aprs_source(dir);
    let mut args = vec![
        "--source-file",
        &src,
        "job",
        "start",
        "--kind",
        "aprs_decode",
        "--source",
        "aprs",
        "--center-hz",
        "144800000",
        "--bandwidth-hz",
        "12500",
        "--then-tail",
        then_tail,
    ];
    args.extend_from_slice(extra);
    zeta(dir, &args)
}

/// The listed job `id`'s `state.status`.
fn job_status(dir: &Path, id: &str) -> String {
    let src = aprs_source(dir);
    let out = zeta(dir, &["--source-file", &src, "job", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    let job = v["value"]
        .as_array()
        .expect("jobs array")
        .iter()
        .find(|j| j["id"] == id)
        .expect("job listed")
        .clone();
    job["state"]["status"].as_str().unwrap().to_owned()
}

#[test]
fn job_start_then_tail_prints_aprs_event() {
    let dir = tempfile::tempdir().unwrap();
    let out = aprs_start(dir.path(), "1,5000", &[])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    eprintln!("{}", String::from_utf8_lossy(&out));
    let v = json(&out);
    assert_eq!(v[0]["result"], "job");
    assert_eq!(v[1]["result"], "frames");
    assert_eq!(v[1]["value"]["missed"], 0);
    let frames = v[1]["value"]["frames"].as_array().expect("frames array");
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["kind"], "event");
    assert_eq!(frames[0]["source"], "aprs");
    assert_eq!(frames[0]["job"], v[0]["value"]);
    assert_eq!(frames[0]["channel"], "144800000/12500");
    assert_eq!(frames[0]["payload"]["src"], "VK3ABC");
}

#[test]
fn then_tail_stops_job_unless_keep() {
    for (extra, stopped, status) in [
        (&[][..], true, "stopped"),
        (&["--keep"][..], false, "running"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let out = aprs_start(dir.path(), "1,5000", extra)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let v = json(&out);
        assert_eq!(v.as_array().map(Vec::len), Some(3), "{v}");
        assert_eq!(v[1]["result"], "frames");
        assert_eq!(v[2], serde_json::json!({"stopped": stopped}));
        let id = v[0]["value"].as_str().expect("job id");
        assert_eq!(job_status(dir.path(), id), status, "{extra:?}");
    }
}

#[test]
fn then_tail_malformed_value_is_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    aprs_start(dir.path(), "1", &[])
        .assert()
        .code(1)
        .stderr(contains("expected MAX_FRAMES,TIMEOUT_MS"));
}

#[test]
fn then_tail_reports_failed_job() {
    let dir = tempfile::tempdir().unwrap();
    // A 1 kHz source channelises at 1 kHz, too slow for the AFSK1200 decoder.
    let iq = zeta_dsp::util::nco(1_000, 100.0, 1_000.0);
    let meta = zeta_dsp::fixtures::write_fixture(dir.path(), "cd", &iq, 1_000, 100_000_000);
    let src = format!("cd={}", meta.display());
    let out = zeta(
        dir.path(),
        &[
            "--source-file",
            &src,
            "job",
            "start",
            "--kind",
            "aprs_decode",
            "--source",
            "cd",
            "--center-hz",
            "100000000",
            "--bandwidth-hz",
            "500",
            "--then-tail",
            "1,2000",
        ],
    )
    .assert()
    .code(2)
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    assert_eq!(v[0]["result"], "job");
    assert_eq!(v[1]["state"]["status"], "failed", "{v}");
    let reason = v[1]["state"]["reason"].as_str().unwrap_or_default();
    assert!(reason.contains("too low for afsk1200"), "{v}");
}
