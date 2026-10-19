//! End-to-end tests of the `zeta` binary.

mod common;

use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
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
        "pocsag_decode",
        "rds_decode",
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
fn job_start_pocsag_decode_prints_pocsag_event() {
    let dir = tempfile::tempdir().unwrap();
    let iq = zeta_dsp::fixtures::pocsag_packet_iq(1234567, 3, "HELLO ZETA", 1200, 48_000, 0.0);
    let meta = zeta_dsp::fixtures::write_fixture(dir.path(), "pager", &iq, 48_000, 152_000_000);
    let src = format!("pager={}", meta.display());
    let out = zeta(
        dir.path(),
        &[
            "--source-file",
            &src,
            "job",
            "start",
            "--kind",
            "pocsag_decode",
            "--source",
            "pager",
            "--center-hz",
            "152000000",
            "--bandwidth-hz",
            "12500",
            "--then-tail",
            "1,5000",
        ],
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    let frames = v[1]["value"]["frames"].as_array().expect("frames array");
    assert_eq!(frames.len(), 1, "{v}");
    assert_eq!(frames[0]["payload"]["protocol"], "pocsag");
    assert_eq!(frames[0]["payload"]["address"], 1234567);
    assert_eq!(frames[0]["payload"]["text"], "HELLO ZETA");
}

#[test]
fn job_start_rds_decode_prints_rds_event() {
    let dir = tempfile::tempdir().unwrap();
    let iq = zeta_dsp::fixtures::rds_station_iq(0x2AB1, "ZETA FM ", "HELLO FROM ZETASDR", 240_000);
    let meta = zeta_dsp::fixtures::write_fixture(dir.path(), "fm", &iq, 240_000, 100_000_000);
    let src = format!("fm={}", meta.display());
    let out = zeta(
        dir.path(),
        &[
            "--source-file",
            &src,
            "job",
            "start",
            "--kind",
            "rds_decode",
            "--source",
            "fm",
            "--center-hz",
            "100000000",
            "--bandwidth-hz",
            "200000",
            "--then-tail",
            "50,5000",
        ],
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    let frames = v[1]["value"]["frames"].as_array().expect("frames array");
    assert!(
        frames.iter().all(|f| f["payload"]["protocol"] == "rds"),
        "{v}"
    );
    assert!(
        frames
            .iter()
            .any(|f| f["payload"]["ps"] == "ZETA FM " && f["payload"]["pi"] == "2AB1"),
        "{v}"
    );
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
fn scan_kind_is_accepted_and_lists_as_running() {
    let dir = tempfile::tempdir().unwrap();
    // A scan over the file source's own band is one step: rows from the
    // registered `fft` stage, tagged with that step.
    let out = zeta(
        dir.path(),
        &[
            "job",
            "start",
            "--kind",
            "scan",
            "--source",
            "tone",
            "--center-hz",
            "100000000",
            "--bandwidth-hz",
            "48000",
            "--then-tail",
            "2,5000",
            "--keep",
        ],
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    let id = v[0]["value"].as_str().expect("job id").to_owned();
    let frames = v[1]["value"]["frames"].as_array().expect("frames");
    assert_eq!(frames.len(), 2, "{v}");
    for f in frames {
        assert_eq!(f["channel"], "scan/0", "{v}");
        assert_eq!(f["payload"]["center_hz"], 100_000_000, "{v}");
        assert_eq!(f["payload"]["size"], 1024, "{v}");
    }
    assert_eq!(v[2], serde_json::json!({"stopped": false}));

    let listed = |dir: &Path| {
        let out = zeta(dir, &["job", "list"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        json(&out)["value"]
            .as_array()
            .expect("jobs array")
            .iter()
            .find(|j| j["id"] == id.as_str())
            .expect("job listed")
            .clone()
    };
    let job = listed(dir.path());
    assert_eq!(job["kind"], "scan");
    assert_eq!(job["state"]["status"], "running");
    zeta(dir.path(), &["job", "stop", "--job", &id])
        .assert()
        .success();
    assert_eq!(listed(dir.path())["state"]["status"], "stopped");
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
    // A prime source rate channelises at itself, and resampling 999 983 Hz to
    // 48 kHz audio reduces to a ratio beyond the resampler's limit.
    let iq = zeta_dsp::util::nco(2_000, 100.0, 999_983.0);
    let meta = zeta_dsp::fixtures::write_fixture(dir.path(), "cd", &iq, 999_983, 100_000_000);
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
    assert!(reason.contains("beyond the 100000 limit"), "{v}");
}

#[test]
fn job_start_accepts_deviation_flag() {
    let dir = tempfile::tempdir().unwrap();
    let out = zeta(
        dir.path(),
        &[
            "job",
            "start",
            "--kind",
            "fm_demod",
            "--source",
            "tone",
            "--center-hz",
            "100000000",
            "--bandwidth-hz",
            "12500",
            "--deviation-hz",
            "3000",
        ],
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let id = json(&out)["value"].as_str().expect("job id").to_owned();
    let out = zeta(dir.path(), &["graph", "get", "--job", &id])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let g = json(&out);
    let nodes = g["value"]["nodes"].as_array().expect("nodes");
    let nbfm = nodes
        .iter()
        .find(|n| n["kind"] == "nbfm_demod")
        .expect("nbfm node");
    assert_eq!(nbfm["params"]["deviation_hz"], 3000);
}

/// `examples/python/gain.py` as an absolute path.
fn gain_py() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/python/gain.py")
        .canonicalize()
        .expect("examples/python/gain.py")
        .display()
        .to_string()
}

fn have_python3() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn source_list_shows_kiwi_capabilities_without_connecting() {
    let dir = tempfile::tempdir().unwrap();
    let out = zeta_bin()
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .args([
            "--source-kiwi",
            "kiwi0=127.0.0.1:9/hunter2",
            "source",
            "list",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains("hunter2"), "password leaked: {text}");
    assert!(!String::from_utf8_lossy(&out.stderr).contains("hunter2"));
    let v = json(&out.stdout);
    let s = v["value"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "kiwi0")
        .expect("kiwi0 listed");
    assert_eq!(s["capabilities"]["remote"], true);
    assert_eq!(s["capabilities"]["push"], true);
    assert_eq!(
        s["capabilities"]["max_bandwidth_hz"],
        serde_json::json!({"asserted": 12000})
    );
    assert!(s["current"].is_null(), "{s}");
    assert!(s["fault"].is_null(), "{s}");
}

#[test]
fn duplicate_source_id_across_kinds_rejected() {
    zeta_bin()
        .args(["--source-file", "a=x.sigmf-meta", "--source-kiwi", "a=h:1"])
        .args(["source", "list"])
        .assert()
        .code(1)
        .stderr(contains("\"a\""))
        .stderr(contains("more than once"));
}

#[test]
fn kiwi_flag_errors_never_echo_the_password() {
    zeta_bin()
        .args(["--source-kiwi", "k=h/hunter2", "source", "list"])
        .assert()
        .code(1)
        .stderr(contains("expected ID=HOST:PORT"))
        .stderr(contains("hunter2").not());
    let dir = tempfile::tempdir().unwrap();
    zeta_bin()
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .args(["--source-kiwi", "k=h:1/hunter 2", "source", "list"])
        .assert()
        .code(1)
        .stderr(contains("--source-kiwi k=h:1"))
        .stderr(contains("hunter").not());
}

#[test]
fn plugin_flag_registers_stage_in_schema() {
    if !have_python3() {
        eprintln!("skipped: no python3");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let out = zeta_bin()
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .arg("--plugin")
        .arg(format!("gain={}", gain_py()))
        .arg("schema")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    let gain = v["value"]["stages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["kind"] == "gain")
        .expect("gain stage in schema");
    assert_eq!(
        gain["params_schema"]["required"],
        serde_json::json!(["factor"])
    );
}

#[test]
fn plugin_kind_colliding_with_builtin_rejected() {
    let dir = tempfile::tempdir().unwrap();
    zeta_bin()
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .arg("--plugin")
        .arg(format!("channel={}", gain_py()))
        .args(["source", "list"])
        .assert()
        .code(1)
        .stderr(contains("collides"));
}

#[test]
fn failed_plugin_probe_is_a_startup_error_naming_the_kind() {
    let dir = tempfile::tempdir().unwrap();
    zeta_bin()
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .args([
            "--plugin",
            "nope=/nonexistent/zeta-plugin",
            "source",
            "list",
        ])
        .assert()
        .code(1)
        .stderr(contains("--plugin nope="))
        .stderr(contains("cannot start"));
    zeta_bin()
        .args(["--plugin", "Gain=/x", "source", "list"])
        .assert()
        .code(1)
        .stderr(contains("must match"));
}

#[cfg(not(soapy))]
#[test]
fn soapy_without_cfg_is_explained() {
    let dir = tempfile::tempdir().unwrap();
    zeta_bin()
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .args(["--source-soapy", "h=driver=hackrf", "source", "list"])
        .assert()
        .code(1)
        .stderr(contains("--source-soapy h=driver=hackrf"))
        .stderr(contains("--cfg soapy"));
    zeta_bin()
        .args(["soapy", "find", "--filter", "driver=hackrf"])
        .assert()
        .code(1)
        .stderr(contains("--cfg soapy"))
        .stderr(contains("libsoapysdr-dev"));
}

#[test]
fn node_mode_rejects_source_and_plugin_flags() {
    for flags in [
        ["--source-kiwi", "k=h:1"],
        ["--source-node", "pi=tone@http://127.0.0.1:9"],
        ["--source-vrt", "v=udp://127.0.0.1:4991"],
        ["--source-soapy", "h=driver=hackrf"],
        ["--plugin", "gain=/x"],
    ] {
        zeta_bin()
            .args(["--node", "http://127.0.0.1:9", "--secret", "s"])
            .args(flags)
            .args(["source", "list"])
            .assert()
            .code(1)
            .stderr(contains("configured on the serving node"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn job_start_on_fake_kiwi_measures() {
    use zeta_source_kiwi::testing::{FakeKiwi, FakeScript};
    let fake = FakeKiwi::start(FakeScript::default()).await;
    let port = fake.addr().port();
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let out = tokio::task::spawn_blocking(move || {
        zeta_bin()
            .arg("--state-dir")
            .arg(state)
            .arg("--source-kiwi")
            .arg(format!("k=127.0.0.1:{port}"))
            .args(["job", "start", "--kind", "measure", "--source", "k"])
            .args(["--center-hz", "7100000", "--bandwidth-hz", "10000"])
            .args(["--then-tail", "1,5000"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone()
    })
    .await
    .unwrap();
    let v = json(&out);
    let frames = v[1]["value"]["frames"].as_array().expect("frames");
    assert!(!frames.is_empty(), "{v}");
    let center = frames[0]["payload"]["center_hz"]
        .as_f64()
        .expect("center_hz is a number");
    assert!(
        (7_095_000.0..=7_105_000.0).contains(&center),
        "center_hz {center} not within 5 kHz of 7 100 000: {v}"
    );
    assert!(fake.connections() >= 1);
}

/// `ZETA_KIWI_PASSWORD_<ID>` supplies the password when the flag has none: it reaches the
/// Kiwi's `SET auth` and is printed nowhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kiwi_password_from_env_reaches_kiwi_and_is_never_printed() {
    use zeta_source_kiwi::testing::{FakeKiwi, FakeScript};
    let fake = FakeKiwi::start(FakeScript::default()).await;
    let port = fake.addr().port();
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let out = tokio::task::spawn_blocking(move || {
        zeta_bin()
            .env("ZETA_KIWI_PASSWORD_MY_KIWI", "hunter2")
            .arg("--state-dir")
            .arg(state)
            .arg("--source-kiwi")
            .arg(format!("my-kiwi=127.0.0.1:{port}"))
            .args(["job", "start", "--kind", "measure", "--source", "my-kiwi"])
            .args(["--center-hz", "7100000", "--bandwidth-hz", "10000"])
            .args(["--then-tail", "1,3000"])
            .assert()
            .success()
            .get_output()
            .clone()
    })
    .await
    .unwrap();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!all.contains("hunter2"), "password printed: {all}");
    assert!(
        fake.commands()
            .iter()
            .any(|c| c == "SET auth t=kiwi p=hunter2"),
        "{:?}",
        fake.commands()
    );
}

/// Secret of the serving node in the federation tests (16+ characters: no short-secret warning).
const NODE_SECRET: &str = "s3cret-long-enough";

/// Node B: a one-shot `zeta --source-node pi=SOURCE@URL job start … --then-tail 1,8000`
/// with `ZETA_NODE_SECRET_PI` set; returns stdout+stderr after asserting success and
/// that neither contains the secret or a bearer header.
fn federated_job(dir: &Path, url: &str, remote: &str, job: &[&str]) -> Value {
    let out = zeta_bin()
        .env("ZETA_NODE_SECRET_PI", NODE_SECRET)
        .arg("--state-dir")
        .arg(dir.join("state"))
        .arg("--source-node")
        .arg(format!("pi={remote}@{url}"))
        .args(["--json", "job", "start", "--source", "pi"])
        .args(job)
        .args(["--then-tail", "1,8000"])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .get_output()
        .clone();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!all.contains(NODE_SECRET), "secret printed: {all}");
    assert!(!all.contains("Bearer"), "bearer printed: {all}");
    json(&out.stdout)
}

/// Poll node A's `job list` for up to 3 s until the one `passthrough` job on `remote` is
/// stopped (a local job start tunes twice with the same band; the second reuses the job).
fn assert_remote_passthrough_stopped(url: &str, remote: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let out = common::zeta_remote(url, NODE_SECRET, &["job", "list"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let v = json(&out);
        let jobs: Vec<&Value> = v["value"]
            .as_array()
            .expect("jobs array")
            .iter()
            .filter(|j| j["kind"] == "passthrough" && j["source"] == remote)
            .collect();
        assert_eq!(jobs.len(), 1, "one passthrough job on {remote}: {v}");
        if jobs.iter().all(|j| j["state"]["status"] == "stopped") {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "remote passthrough job not stopped within 3 s: {v}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[test]
fn measure_over_federation_labels_remote_tone_as_carrier() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let node_a = common::ServeProc::start(a.path(), NODE_SECRET);
    let v = federated_job(
        b.path(),
        &node_a.url,
        "tone",
        &[
            "--kind",
            "measure",
            "--center-hz",
            "100000000",
            "--bandwidth-hz",
            "48000",
        ],
    );
    let frames = v[1]["value"]["frames"].as_array().expect("frames");
    assert_eq!(frames.len(), 1, "{v}");
    assert_eq!(frames[0]["source"], "pi");
    assert_eq!(frames[0]["payload"]["label"]["name"], "carrier", "{v}");
    assert_eq!(v[2]["stopped"], true);
    assert_remote_passthrough_stopped(&node_a.url, "tone");
}

#[test]
fn aprs_decode_over_federation_yields_the_packet() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let src = aprs_source(a.path());
    let node_a = common::ServeProc::start_with(a.path(), NODE_SECRET, &["--source-file", &src]);
    let v = federated_job(
        b.path(),
        &node_a.url,
        "aprs",
        &[
            "--kind",
            "aprs_decode",
            "--center-hz",
            "144800000",
            "--bandwidth-hz",
            "12500",
        ],
    );
    let frames = v[1]["value"]["frames"].as_array().expect("frames");
    assert_eq!(frames.len(), 1, "{v}");
    assert_eq!(frames[0]["source"], "pi");
    assert_eq!(frames[0]["payload"]["src"], "VK3ABC", "{v}");
    assert_remote_passthrough_stopped(&node_a.url, "aprs");
}

#[test]
fn source_node_without_secret_is_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    for empty in [None, Some("")] {
        let mut cmd = zeta_bin();
        cmd.env_remove("ZETA_NODE_SECRET_PI");
        if let Some(v) = empty {
            cmd.env("ZETA_NODE_SECRET_PI", v);
        }
        cmd.arg("--state-dir")
            .arg(dir.path().join("state"))
            .args([
                "--source-node",
                "pi=tone@http://127.0.0.1:9",
                "source",
                "list",
            ])
            .assert()
            .code(1)
            .stderr(contains("--source-node pi: set ZETA_NODE_SECRET_PI"));
    }
}

#[test]
fn source_node_bad_flag_is_usage_error_naming_the_part() {
    zeta_bin()
        .env("ZETA_NODE_SECRET_PI", NODE_SECRET)
        .args(["--source-node", "pi=tone@https://h:1", "source", "list"])
        .assert()
        .code(1)
        .stderr(contains("http://"))
        .stderr(contains("ID=SOURCE@URL[,max_bandwidth_hz=N]"));
}

#[test]
fn source_node_is_rejected_with_node() {
    zeta_bin()
        .env("ZETA_NODE_SECRET_PI", NODE_SECRET)
        .args(["--node", "http://127.0.0.1:9", "--secret", "s"])
        .args([
            "--source-node",
            "pi=tone@http://127.0.0.1:9",
            "source",
            "list",
        ])
        .assert()
        .code(1)
        .stderr(contains("configured on the serving node"));
}

/// Send a 1 kHz tone at 48 kHz centred on 1 MHz to `to` over UDP in real time, 10 ms
/// blocks, context once a second as `VrtEncoder` does. Stops when `stop` is set or after
/// 10 s.
fn send_vrt_tone(to: std::net::SocketAddr, stop: &std::sync::atomic::AtomicBool) {
    use num_complex::Complex;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};
    use zeta_vrt::{IqMeta, VrtEncoder};
    const RATE: u64 = 48_000;
    const BLOCK: u64 = 480;
    let meta = IqMeta {
        center_hz: 1_000_000,
        sample_rate_hz: RATE,
        bandwidth_hz: RATE,
    };
    let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut enc = VrtEncoder::new(5);
    let start = Instant::now();
    for n in 0..1000u64 {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let t0_ns = n * BLOCK * 1_000_000_000 / RATE;
        let samples: Vec<Complex<f32>> = (n * BLOCK..(n + 1) * BLOCK)
            .map(|i| {
                let ph = 2.0 * std::f64::consts::PI * 1_000.0 * i as f64 / RATE as f64;
                Complex::new(0.5 * ph.cos() as f32, 0.5 * ph.sin() as f32)
            })
            .collect();
        for p in enc.encode(t0_ns, meta, &samples) {
            sock.send_to(&p, to).unwrap();
        }
        let due = start + Duration::from_millis((n + 1) * 10);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }
}

#[test]
fn source_vrt_measure_labels_sent_tone_as_carrier() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let dir = tempfile::tempdir().unwrap();
    // A free loopback UDP port for zeta to bind.
    let to = std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let stop = AtomicBool::new(false);
    let out = std::thread::scope(|s| {
        s.spawn(|| send_vrt_tone(to, &stop));
        let out = zeta_bin()
            .arg("--state-dir")
            .arg(dir.path().join("state"))
            .arg("--source-vrt")
            .arg(format!("t=udp://{to}"))
            .args([
                "--json", "job", "start", "--kind", "measure", "--source", "t",
            ])
            .args(["--center-hz", "1000000", "--bandwidth-hz", "48000"])
            .args(["--then-tail", "1,8000"])
            .timeout(std::time::Duration::from_secs(30))
            .output()
            .unwrap();
        stop.store(true, Ordering::Relaxed);
        out
    });
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json(&out.stdout);
    let frames = v[1]["value"]["frames"].as_array().expect("frames");
    assert_eq!(frames.len(), 1, "{v}");
    assert_eq!(frames[0]["source"], "t");
    assert_eq!(frames[0]["payload"]["label"]["name"], "carrier", "{v}");
    assert_eq!(v[2]["stopped"], true);
}

#[test]
fn source_vrt_non_lan_address_is_usage_error() {
    zeta_bin()
        .args(["--source-vrt", "t=udp://8.8.8.8:4991", "source", "list"])
        .assert()
        .code(1)
        .stderr(contains("not a LAN address"))
        .stderr(contains("ID=udp://ADDR:PORT"));
}

/// `serve` stopped by SIGTERM (systemd stop) shuts down like Ctrl-C: it exits 0 and
/// releases the remote node's `passthrough` job.
#[test]
fn serve_sigterm_releases_remote_lease() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let node_a = common::ServeProc::start(a.path(), NODE_SECRET);
    let mut node_b = common::ServeProc::start_env(
        b.path(),
        NODE_SECRET,
        &["--source-node", &format!("pi=tone@{}", node_a.url)],
        &[("ZETA_NODE_SECRET_PI", NODE_SECRET)],
    );
    common::zeta_remote(
        &node_b.url,
        NODE_SECRET,
        &[
            "job",
            "start",
            "--source",
            "pi",
            "--kind",
            "measure",
            "--center-hz",
            "100000000",
            "--bandwidth-hz",
            "48000",
        ],
    )
    .timeout(std::time::Duration::from_secs(30))
    .assert()
    .success();
    let running = common::zeta_remote(&node_a.url, NODE_SECRET, &["job", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&running);
    assert!(
        v["value"]
            .as_array()
            .expect("jobs")
            .iter()
            .any(|j| j["kind"] == "passthrough" && j["state"]["status"] == "running"),
        "no running passthrough on node A: {v}"
    );
    let status = node_b
        .terminate(std::time::Duration::from_secs(5))
        .expect("serve exits within 5 s of SIGTERM");
    assert!(status.success(), "serve exit status {status:?}");
    assert_remote_passthrough_stopped(&node_a.url, "tone");
}

/// Run `zeta serve` (no `--listen`) until it says it is serving; return what it printed on stderr.
fn serve_banner(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("zeta"));
    cmd.arg("--state-dir")
        .arg(dir.join("state"))
        .arg("serve")
        .args(args)
        .env_remove("ZETA_NODE")
        .env_remove("ZETA_SECRET")
        .env_remove("ZETA_LLM_URL")
        .env_remove("ZETA_LLM_MODEL")
        .envs(env.iter().copied())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let mut seen = String::new();
    for line in lines.by_ref() {
        let line = line.unwrap();
        seen.push_str(&line);
        seen.push('\n');
        if line.contains("serving; Ctrl-C") {
            break;
        }
    }
    child.kill().unwrap();
    child.wait().unwrap();
    seen
}

#[test]
fn serve_warns_when_llm_flags_are_given_without_listen() {
    let dir = tempfile::tempdir().unwrap();
    let flags = ["--llm-url", "http://127.0.0.1:1", "--llm-model", "m"];
    let seen = serve_banner(dir.path(), &flags, &[]);
    assert!(seen.contains("ignored without --listen"), "{seen}");
    let dir = tempfile::tempdir().unwrap();
    let env = [
        ("ZETA_LLM_URL", "http://127.0.0.1:1"),
        ("ZETA_LLM_MODEL", "m"),
    ];
    let seen = serve_banner(dir.path(), &[], &env);
    assert!(!seen.contains("ignored without --listen"), "{seen}");
}

#[test]
fn wfm_demod_job_streams_48k_audio_from_a_wfm_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let audio = zeta_dsp::fixtures::tone(48_000, 48_000, 1_000.0, 0.5);
    let iq = zeta_dsp::fixtures::wfm_modulate(&audio, 48_000, 240_000, 75_000.0, 0.0, true);
    let meta = zeta_dsp::fixtures::write_fixture(dir.path(), "wfm", &iq, 240_000, 104_900_000);
    let src = format!("wfm={}", meta.display());
    let out = zeta(
        dir.path(),
        &[
            "--source-file",
            &src,
            "job",
            "start",
            "--kind",
            "wfm_demod",
            "--source",
            "wfm",
            "--center-hz",
            "104900000",
            "--bandwidth-hz",
            "200000",
            "--then-tail",
            "3,8000",
        ],
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    assert_eq!(v[0]["result"], "job", "{v}");
    assert_eq!(v[1]["result"], "frames", "{v}");
    let frames = v[1]["value"]["frames"].as_array().expect("frames array");
    assert_eq!(frames.len(), 3, "{v}");
    for f in frames {
        assert_eq!(f["kind"], "audio", "{f}");
        assert_eq!(f["payload"]["sample_rate_hz"], 48_000, "{f}");
        let peak = f["payload"]["peak"].as_f64().unwrap();
        assert!(peak > 0.05 && peak <= 1.0, "peak {peak}");
    }
}

#[test]
fn classify_names_the_wfm_fixture_fm_broadcast_radio() {
    // The wfm_demod fixture: 1 s of 240 kHz broadcast FM with pilot at 104.9 MHz.
    let dir = tempfile::tempdir().unwrap();
    let audio = zeta_dsp::fixtures::tone(48_000, 48_000, 1_000.0, 0.5);
    let iq = zeta_dsp::fixtures::wfm_modulate(&audio, 48_000, 240_000, 75_000.0, 0.0, true);
    let meta = zeta_dsp::fixtures::write_fixture(dir.path(), "wfm", &iq, 240_000, 104_900_000);
    let src = format!("wfm={}", meta.display());
    let out = zeta(
        dir.path(),
        &[
            "--source-file",
            &src,
            "classify",
            "--source",
            "wfm",
            "--start-ns",
            "0",
            "--end-ns",
            "500000000",
            "--center-hz",
            "104900000",
            "--bandwidth-hz",
            "240000",
        ],
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    assert_eq!(v["result"], "labels", "{v}");
    let labels = v["value"].as_array().expect("labels array");
    assert!(
        labels
            .iter()
            .any(|l| l["layer"] == 2 && l["name"] == "FM Broadcast Radio"),
        "{v}"
    );
}

#[test]
fn am_demod_job_streams_48k_audio() {
    let dir = tempfile::tempdir().unwrap();
    let audio = zeta_dsp::fixtures::tone(48_000, 48_000, 1_000.0, 0.5);
    let iq = zeta_dsp::fixtures::am_modulate(&audio, 48_000, 48_000, 0.5, 0.0);
    let meta = zeta_dsp::fixtures::write_fixture(dir.path(), "am", &iq, 48_000, 121_500_000);
    let src = format!("am={}", meta.display());
    let out = zeta(
        dir.path(),
        &[
            "--source-file",
            &src,
            "job",
            "start",
            "--kind",
            "am_demod",
            "--source",
            "am",
            "--center-hz",
            "121500000",
            "--bandwidth-hz",
            "10000",
            "--then-tail",
            "3,8000",
        ],
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    assert_eq!(v[0]["result"], "job", "{v}");
    assert_eq!(v[1]["result"], "frames", "{v}");
    let frames = v[1]["value"]["frames"].as_array().expect("frames array");
    assert_eq!(frames.len(), 3, "{v}");
    for f in frames {
        assert_eq!(f["kind"], "audio", "{f}");
        assert_eq!(f["payload"]["sample_rate_hz"], 48_000, "{f}");
        let peak = f["payload"]["peak"].as_f64().unwrap();
        assert!(peak > 0.01 && peak <= 1.0, "peak {peak}");
    }
}

#[test]
fn ssb_demod_job_with_lsb_streams_audio() {
    let dir = tempfile::tempdir().unwrap();
    let audio: Vec<f32> = zeta_dsp::fixtures::tone(48_000, 48_000, 700.0, 0.7)
        .iter()
        .zip(zeta_dsp::fixtures::tone(48_000, 48_000, 1_900.0, 0.3))
        .map(|(a, b)| a + b)
        .collect();
    let iq = zeta_dsp::fixtures::ssb_modulate(&audio, 48_000, 48_000, false, 0.0);
    let meta = zeta_dsp::fixtures::write_fixture(dir.path(), "ssb", &iq, 48_000, 7_100_000);
    let src = format!("ssb={}", meta.display());
    let start = |kind: &str| {
        vec![
            "--source-file".to_owned(),
            src.clone(),
            "job".into(),
            "start".into(),
            "--kind".into(),
            kind.into(),
            "--source".into(),
            "ssb".into(),
            "--center-hz".into(),
            "7100000".into(),
            "--bandwidth-hz".into(),
            "6000".into(),
            "--sideband".into(),
            "lsb".into(),
        ]
    };
    let mut args = start("ssb_demod");
    args.extend(["--then-tail".to_owned(), "3,8000".into()]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = zeta(dir.path(), &args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v[0]["result"], "job", "{v}");
    assert_eq!(v[1]["result"], "frames", "{v}");
    let frames = v[1]["value"]["frames"].as_array().expect("frames array");
    assert_eq!(frames.len(), 3, "{v}");
    for f in frames {
        assert_eq!(f["kind"], "audio", "{f}");
        assert_eq!(f["payload"]["sample_rate_hz"], 48_000, "{f}");
        let peak = f["payload"]["peak"].as_f64().unwrap();
        assert!(peak > 0.01 && peak <= 1.0, "peak {peak}");
    }

    // `--sideband` with another kind is refused by the op's own check.
    let args = start("am_demod");
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = zeta(dir.path(), &args)
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["error"], "other", "{v}");
    assert_eq!(
        v["message"], "sideband applies only to kind ssb_demod; pass null (kind is am_demod)",
        "{v}"
    );
}

#[test]
fn ring_budget_flag_is_logged() {
    let dir = tempfile::tempdir().unwrap();
    // 64 MB / (48 kHz * 8 B) = 174.76 s, capped at 10 s; a zero budget gives the 1 s floor.
    for (mb, want) in [
        ("64", "ring for tone: 10.00 s at "),
        ("0", "ring for tone: 1.00 s at "),
    ] {
        let args = [
            "--ring-budget-mb",
            mb,
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
        ];
        let out = zeta(dir.path(), &args).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(want), "{mb}: stderr was {err}");
    }
}
