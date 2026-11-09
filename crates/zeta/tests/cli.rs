//! End-to-end tests of the `zeta` binary.

mod common;

use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use serde_json::Value;
use zeta_source_file::fixtures::write_tone;

/// The `zeta` binary with `ZETA_NODE` and `ZETA_SECRET` removed, so a caller's environment
/// cannot send a test to a remote node, and `XDG_CONFIG_HOME` pointed at a directory with
/// no mode rows, so the developer's own (`~/.config/zetasdr/modes`) never reach a test.
/// Every test here starts from this.
fn zeta_bin() -> Command {
    let mut cmd = Command::cargo_bin("zeta").unwrap();
    cmd.env_remove("ZETA_NODE")
        .env_remove("ZETA_SECRET")
        .env("XDG_CONFIG_HOME", common::no_xdg_config())
        // multimon-ng and c2dec are pinned missing, whatever the host has: a test that
        // wants the FLEX or the M17 row ready sets them again.
        .env(
            "ZETA_HANDLER_MULTIMON_NG",
            "/nonexistent/zeta-test/multimon-ng",
        )
        .env("ZETA_HANDLER_C2DEC", "/nonexistent/zeta-test/c2dec");
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

/// An unreadable job record and an unreadable source record are warned about, each as
/// what it is.
#[test]
fn unreadable_records_are_warned_about_by_kind() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    for sub in ["jobs", "sources"] {
        std::fs::create_dir_all(state.join(sub)).unwrap();
    }
    std::fs::write(state.join("jobs").join("garbage.json"), "nope").unwrap();
    std::fs::write(state.join("sources").join("tone.json"), "nope").unwrap();
    zeta(dir.path(), &["source", "list"])
        .assert()
        .success()
        .stderr(
            contains("zeta: warning: unreadable job record garbage.json")
                .and(contains(
                    "zeta: warning: unreadable source record tone.json",
                ))
                .and(contains("job record tone.json").not()),
        );
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
        "acars_decode",
        "adsb_decode",
        "measure",
    ] {
        assert!(kinds.contains(&k), "stages missing {k}: {kinds:?}");
    }
    let errors = v["value"]["error"]["oneOf"]
        .as_array()
        .expect("error oneOf");
    assert!(
        errors
            .iter()
            .any(|e| e["properties"]["error"]["const"] == "duplicate"),
        "error schema lacks duplicate: {errors:?}"
    );
}

#[test]
fn plans_prints_the_table_and_refuses_an_unknown_id() {
    let dir = tempfile::tempdir().unwrap();
    let out = zeta(dir.path(), &["plans"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["result"], "channel_plans");
    let plans = v["value"]["plans"].as_array().expect("plans array");
    assert_eq!(plans.len(), 3);
    assert_eq!(plans[0]["id"], "uhf_cb_au");
    assert_eq!(plans[1]["id"], "acars_vhf");
    assert_eq!(plans[2]["id"], "ais_vhf");
    assert_eq!(plans[0]["channels"].as_array().unwrap().len(), 80);
    let out = zeta(dir.path(), &["plans", "--plan", "uhf_cb_au"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["value"]["plans"][0]["id"], "uhf_cb_au");
    let out = zeta(dir.path(), &["plans", "--plan", "nope"])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["error"], "other");
    assert_eq!(
        v["message"],
        "unknown plan nope (known: uhf_cb_au, acars_vhf, ais_vhf)"
    );
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
fn source_file_flag_parses_a_tune_range() {
    let dir = tempfile::tempdir().unwrap();
    let meta = write_tone(dir.path());
    let flag = format!(
        "retune={},tune_min_hz=50000000,tune_max_hz=150000000",
        meta.display()
    );
    let run = |args: &[&str]| {
        let mut cmd = zeta_bin();
        cmd.arg("--state-dir")
            .arg(dir.path().join("state"))
            .arg("--source-file")
            .arg(&flag)
            .arg("--json")
            .args(args);
        cmd
    };
    let out = run(&["source", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    let src = &v["value"][0];
    assert_eq!(src["id"], "retune");
    let caps = &src["capabilities"];
    assert!(
        caps["tune_min_hz"].to_string().contains("50000000"),
        "{caps}"
    );
    assert!(
        caps["tune_max_hz"].to_string().contains("150000000"),
        "{caps}"
    );
    // A tune inside the range moves the capture's centre.
    let out = run(&[
        "source",
        "ensure",
        "--source",
        "retune",
        "--center-hz",
        "120000000",
        "--bandwidth-hz",
        "12500",
    ])
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    assert_eq!(v["value"]["center_hz"], 120_000_000, "{v}");
    assert_eq!(v["value"]["bandwidth_hz"], 48_000, "{v}");
    // Outside it: out_of_range naming the range.
    let out = run(&[
        "source",
        "ensure",
        "--source",
        "retune",
        "--center-hz",
        "160000000",
        "--bandwidth-hz",
        "12500",
    ])
    .assert()
    .code(2)
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    assert_eq!(v["error"], "out_of_range", "{v}");
    assert_eq!(v["max"], 150_000_000, "{v}");
    // A malformed or partial range is a usage error naming the form.
    for bad in [
        format!("r={},tune_min_hz=5", meta.display()),
        format!("r={},tune_min_hz=x,tune_max_hz=9", meta.display()),
        format!("r={},tune_min_hz=9,tune_max_hz=5", meta.display()),
        format!("r={},tune_min_hz=5,tune_max_hz=9,gain=3", meta.display()),
    ] {
        zeta_bin()
            .arg("--state-dir")
            .arg(dir.path().join("state"))
            .args(["--source-file", &bad, "source", "list"])
            .assert()
            .code(1)
            .stderr(contains("ID=PATH.sigmf-meta[,tune_min_hz=N,tune_max_hz=M]"));
    }
}

/// `source pin` holds a file source with a tune range at a band across one-shot
/// processes: `source list` shows it, `source unpin` releases it, and a second
/// unpin is `other`.
#[test]
fn source_pin_and_unpin_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let meta = write_tone(dir.path());
    let flag = format!(
        "retune={},tune_min_hz=50000000,tune_max_hz=150000000",
        meta.display()
    );
    let run = |args: &[&str]| {
        let mut cmd = zeta_bin();
        cmd.arg("--state-dir")
            .arg(dir.path().join("state"))
            .arg("--source-file")
            .arg(&flag)
            .arg("--json")
            .args(args);
        cmd
    };
    let out = run(&[
        "source",
        "pin",
        "--source",
        "retune",
        "--center-hz",
        "120000000",
        "--bandwidth-hz",
        "12500",
    ])
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    assert_eq!(v["result"], "tuning", "{v}");
    assert_eq!(v["value"]["center_hz"], 120_000_000, "{v}");
    let out = run(&["source", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    let pin = &v["value"][0]["pin"];
    assert_eq!(pin["center_hz"], 120_000_000, "{v}");
    assert_eq!(pin["bandwidth_hz"], 12_500, "{v}");
    let out = run(&["source", "unpin", "--source", "retune"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out), serde_json::json!({"result": "ok"}));
    let out = run(&["source", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(json(&out)["value"][0]["pin"].is_null());
    let out = run(&["source", "unpin", "--source", "retune"])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["error"], "other", "{v}");
    assert_eq!(v["message"], "source retune is not pinned", "{v}");
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
fn job_start_elastic_flag_round_trips_to_job_list() {
    let dir = tempfile::tempdir().unwrap();
    let start = [
        "job",
        "start",
        "--kind",
        "spectrum",
        "--elastic",
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
    let id = json(&out)["value"].as_str().expect("job id").to_owned();
    let out = zeta(dir.path(), &["job", "list"])
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
        .find(|j| j["id"] == id.as_str())
        .expect("started job listed")
        .clone();
    assert_eq!(job["elastic"], true, "{job}");
    assert_eq!(job["kind"], "spectrum", "{job}");
}

/// The op of the last line of the state directory's `ops.log`.
fn last_logged_op(dir: &Path) -> Value {
    let log = std::fs::read_to_string(dir.join("state").join("ops.log")).unwrap();
    let line = log.lines().last().expect("an op logged");
    serde_json::from_str::<Value>(line).unwrap()["op"].clone()
}

#[test]
fn job_start_replace_flag_round_trips_to_the_op() {
    let dir = tempfile::tempdir().unwrap();
    let start = |replace: &str| {
        let mut args = vec![
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
            "--replace",
        ];
        args.push(replace);
        zeta(dir.path(), &args)
    };
    // Unknown ids: the request is refused naming the first, and nothing starts.
    let out = start("01AAAAAAAAAAAAAAAAAAAAAAAA,01BBBBBBBBBBBBBBBBBBBBBBBB")
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["error"], "unknown", "{v}");
    assert_eq!(v["value"], "01AAAAAAAAAAAAAAAAAAAAAAAA", "{v}");
    let op = last_logged_op(dir.path());
    assert_eq!(
        op["replace"],
        serde_json::json!(["01AAAAAAAAAAAAAAAAAAAAAAAA", "01BBBBBBBBBBBBBBBBBBBBBBBB"]),
        "{op}"
    );
    // An empty list is none.
    start("").assert().success();
    assert_eq!(last_logged_op(dir.path())["replace"], serde_json::json!([]));
}

#[test]
fn source_ensure_replace_flag_round_trips_to_the_op() {
    let dir = tempfile::tempdir().unwrap();
    let ensure = |extra: &[&str]| {
        let mut args = vec![
            "source",
            "ensure",
            "--source",
            "tone",
            "--center-hz",
            "100000000",
            "--bandwidth-hz",
            "48000",
        ];
        args.extend_from_slice(extra);
        zeta(dir.path(), &args)
    };
    ensure(&["--replace", "01AAAAAAAAAAAAAAAAAAAAAAAA"])
        .assert()
        .code(2)
        .stdout(contains("\"unknown\""));
    assert_eq!(
        last_logged_op(dir.path())["replace"],
        serde_json::json!(["01AAAAAAAAAAAAAAAAAAAAAAAA"])
    );
    ensure(&[]).assert().success();
    assert_eq!(last_logged_op(dir.path())["replace"], serde_json::json!([]));
}

#[test]
fn job_retune_one_shot_is_refused_for_a_job_not_running_here() {
    let dir = tempfile::tempdir().unwrap();
    let out = zeta(
        dir.path(),
        &[
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
        ],
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let id = json(&out)["value"].as_str().expect("job id").to_string();
    let out = zeta(
        dir.path(),
        &[
            "job",
            "retune",
            "--job",
            &id,
            "--center-hz",
            "100010000",
            "--bandwidth-hz",
            "10000",
        ],
    )
    .assert()
    .code(2)
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    assert_eq!(v["error"], "other", "{v}");
    assert!(
        v["message"]
            .as_str()
            .unwrap()
            .contains("is not running in this node"),
        "{v}"
    );
    let out = zeta(dir.path(), &["job", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let jobs = json(&out);
    let rec = jobs["value"]
        .as_array()
        .and_then(|a| a.iter().find(|j| j["id"] == id.as_str()))
        .unwrap_or_else(|| panic!("{jobs}"));
    assert_eq!(rec["tuning"]["center_hz"], 100_000_000u64, "{rec}");
    assert_eq!(rec["tuning"]["bandwidth_hz"], 48_000u64, "{rec}");
}

#[test]
fn job_retune_replace_flag_round_trips_to_the_op() {
    let dir = tempfile::tempdir().unwrap();
    let retune = |replace: &str| {
        zeta(
            dir.path(),
            &[
                "job",
                "retune",
                "--job",
                "01AAAAAAAAAAAAAAAAAAAAAAAA",
                "--center-hz",
                "1",
                "--bandwidth-hz",
                "1",
                "--replace",
                replace,
            ],
        )
    };
    retune("01BBBBBBBBBBBBBBBBBBBBBBBB,01CCCCCCCCCCCCCCCCCCCCCCCC")
        .assert()
        .code(2);
    let op = last_logged_op(dir.path());
    assert_eq!(op["op"], "job_retune", "{op}");
    assert_eq!(
        op["replace"],
        serde_json::json!(["01BBBBBBBBBBBBBBBBBBBBBBBB", "01CCCCCCCCCCCCCCCCCCCCCCCC"]),
        "{op}"
    );
    retune("").assert().code(2);
    assert_eq!(last_logged_op(dir.path())["replace"], serde_json::json!([]));
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
    let pin: Vec<&str> = ["source", "pin"].into_iter().chain(band).collect();
    let unpin = ["source", "unpin", "--source", "tone"];
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
    let retune = [
        "job",
        "retune",
        "--job",
        "j",
        "--center-hz",
        "100000000",
        "--bandwidth-hz",
        "48000",
    ];
    let want = format!("zeta serve (pid {pid}) owns this state dir");
    for args in [
        &start[..],
        &ensure[..],
        &pin[..],
        &unpin[..],
        &put[..],
        &stop[..],
        &retune[..],
    ] {
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
fn job_start_adsb_decode_prints_adsb_event() {
    if zeta_ext::find_dump1090().is_none() {
        eprintln!("skipped: no dump1090");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let iq = zeta_dsp::fixtures::modes_frame_iq(0x4840D6, "ZETA1", 200, 2_400_000);
    let meta = zeta_dsp::fixtures::write_fixture(dir.path(), "adsb", &iq, 2_400_000, 1_090_000_000);
    let src = format!("adsb={}", meta.display());
    let out = zeta(
        dir.path(),
        &[
            "--source-file",
            &src,
            "job",
            "start",
            "--kind",
            "adsb_decode",
            "--source",
            "adsb",
            "--center-hz",
            "1090000000",
            "--bandwidth-hz",
            "2400000",
            "--then-tail",
            "1,10000",
        ],
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    let frames = v[1]["value"]["frames"].as_array().expect("frames array");
    assert!(!frames.is_empty(), "{v}");
    assert!(
        frames.iter().all(|f| f["payload"]["protocol"] == "adsb"),
        "{v}"
    );
    assert!(
        frames
            .iter()
            .any(|f| f["payload"]["icao"] == "4840D6" && f["payload"]["callsign"] == "ZETA1"),
        "{v}"
    );
}

#[test]
fn job_start_acars_decode_prints_acars_event() {
    let dir = tempfile::tempdir().unwrap();
    let iq =
        zeta_dsp::fixtures::acars_packet_iq("VH-ZET", "H1", "#M1BPOS/ZETASDR TEST", 48_000, 0.0);
    let meta = zeta_dsp::fixtures::write_fixture(dir.path(), "acars", &iq, 48_000, 131_550_000);
    let src = format!("acars={}", meta.display());
    let out = zeta(
        dir.path(),
        &[
            "--source-file",
            &src,
            "job",
            "start",
            "--kind",
            "acars_decode",
            "--source",
            "acars",
            "--center-hz",
            "131550000",
            "--bandwidth-hz",
            "10000",
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
    assert_eq!(frames[0]["payload"]["protocol"], "acars");
    assert_eq!(frames[0]["payload"]["reg"], "VH-ZET");
    assert_eq!(frames[0]["payload"]["label"], "H1");
    assert_eq!(frames[0]["payload"]["crc_ok"], true);
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

/// `identify` on the tone: `rs41_decode`, out of its range, is `skipped_band` at once,
/// then the first row tried (`ais_decode`) is `no_frame` after its dwell; the job is
/// still trying the next when the tail ends, so it is stopped (and its child with it).
#[test]
fn job_start_identify_then_tail_prints_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let out = zeta(
        dir.path(),
        &[
            "job",
            "start",
            "--kind",
            "identify",
            "--source",
            "tone",
            "--center-hz",
            "100000000",
            "--bandwidth-hz",
            "12500",
            // `stream_tail` waits at most 10 000 ms: one second's measure and POCSAG's 5 s
            // dwell, the first row tried (AIS and APRS live elsewhere, so are skipped).
            "--then-tail",
            "6,10000",
        ],
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    let all = v[1]["value"]["frames"].as_array().expect("frames array");
    // The run's one `origin` event leads, then its candidates.
    let (origin, frames): (Vec<_>, Vec<_>) = all
        .iter()
        .partition(|f| f["payload"]["protocol"] == "origin");
    assert_eq!(origin.len(), 1, "{v}");
    // M17 (unavailable: c2dec is pinned missing), the rows out of range (a sonde row,
    // AIS, FLEX) and the tried one (POCSAG).
    assert_eq!(frames.len(), 5, "{v}");
    for f in &frames {
        let p = &f["payload"];
        assert_eq!(p["protocol"], "candidate", "{v}");
        let summary = format!(
            "{}: {}",
            p["mode"].as_str().unwrap(),
            p["outcome"].as_str().unwrap()
        );
        assert_eq!(p["summary"], summary.as_str(), "{v}");
    }
    let outcomes: Vec<(&str, &str)> = frames
        .iter()
        .map(|f| {
            (
                f["payload"]["mode"].as_str().unwrap(),
                f["payload"]["outcome"].as_str().unwrap(),
            )
        })
        .collect();
    // Candidates come in score order, each at its row's turn: a row that cannot run
    // (here the sonde rows, out of range) is published when reached, between trials.
    assert!(outcomes.iter().any(|o| o.1 == "skipped_band"), "{v}");
    let tried = frames
        .iter()
        .find(|f| f["payload"]["outcome"] == "no_frame")
        .unwrap_or_else(|| panic!("{v}"));
    assert!(tried["payload"]["job"].is_string(), "{v}");
    assert_eq!(v[2], serde_json::json!({"stopped": true}));
    let id = v[0]["value"].as_str().expect("job id");
    let jobs = zeta(dir.path(), &["job", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let jobs = json(&jobs);
    let running: Vec<&Value> = jobs["value"]
        .as_array()
        .expect("jobs array")
        .iter()
        .filter(|j| j["state"]["status"] == "running")
        .collect();
    assert!(running.is_empty(), "identify {id} left {running:?}");
}

/// `identify` with `--then-tail` on the POCSAG page at a POCSAG centre: it proves within
/// the tail and ends `stopped`, which is a success; the proved `pocsag_decode` job is
/// stopped, or with `--keep` left running. Returns the printed JSON and the jobs listed after.
fn identify_on_pocsag(dir: &Path, keep: bool) -> (Value, Vec<Value>) {
    let iq = zeta_dsp::fixtures::pocsag_packet_iq(1234567, 3, "HELLO ZETA", 1200, 48_000, 0.0);
    let meta = zeta_dsp::fixtures::write_fixture(dir, "pager", &iq, 48_000, 148_637_500);
    let src = format!("pager={}", meta.display());
    let mut args = vec![
        "--source-file",
        &src,
        "job",
        "start",
        "--kind",
        "identify",
        "--source",
        "pager",
        "--center-hz",
        "148637500",
        "--bandwidth-hz",
        "12500",
        "--then-tail",
        "3,10000",
    ];
    if keep {
        args.push("--keep");
    }
    let out = zeta(dir, &args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    let jobs = zeta(dir, &["--source-file", &src, "job", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let jobs = json(&jobs)["value"].as_array().expect("jobs array").clone();
    (v, jobs)
}

fn running_kinds(jobs: &[Value]) -> Vec<String> {
    jobs.iter()
        .filter(|j| j["state"]["status"] == "running")
        .map(|j| j["kind"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn then_tail_on_a_proving_identify_prints_frames_and_stops_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let (v, jobs) = identify_on_pocsag(dir.path(), false);
    assert_eq!(v[1]["result"], "frames", "{v}");
    let frames = v[1]["value"]["frames"].as_array().expect("frames array");
    assert!(
        frames
            .iter()
            .any(|f| f["payload"]["protocol"] == "candidate"
                && f["payload"]["summary"] == "pocsag_decode: proved"),
        "{v}"
    );
    assert_eq!(v[2], serde_json::json!({"stopped": true}));
    assert!(running_kinds(&jobs).is_empty(), "{jobs:?}");
}

#[test]
fn then_tail_on_a_proving_identify_with_keep_leaves_the_child_running() {
    let dir = tempfile::tempdir().unwrap();
    let (v, jobs) = identify_on_pocsag(dir.path(), true);
    assert_eq!(v[1]["result"], "frames", "{v}");
    assert_eq!(v[2], serde_json::json!({"stopped": false}));
    assert_eq!(running_kinds(&jobs), ["pocsag_decode"], "{jobs:?}");
}

/// A one-shot `watch` with `--then-tail` over a 1.024 MHz recording carrying the POCSAG
/// page 300 kHz above its centre every 2 s: the tail is its event stream (never the
/// spectrum rows between its stages), holding `detect`'s announcement and the watch's
/// `identifying` event; the watch is stopped after and leaves no job running.
#[test]
fn watch_then_tail_prints_signal_events() {
    const RATE: u32 = 1_024_000;
    const CENTER: u64 = 148_337_500;
    let dir = tempfile::tempdir().unwrap();
    let mut iq = zeta_dsp::synth::pocsag_packet_iq(1234567, 3, "HELLO ZETA", 1200, RATE, 300_000.0);
    iq.resize(2 * RATE as usize, num_complex::Complex::new(0.0, 0.0));
    zeta_dsp::synth::awgn(&mut iq, 20.0, 1);
    let meta = zeta_dsp::fixtures::write_fixture(dir.path(), "band", &iq, RATE, CENTER);
    let src = format!("band={}", meta.display());
    let (center, width) = (CENTER.to_string(), RATE.to_string());
    let out = zeta(
        dir.path(),
        &[
            "--source-file",
            &src,
            "job",
            "start",
            "--kind",
            "watch",
            "--source",
            "band",
            "--center-hz",
            &center,
            "--bandwidth-hz",
            &width,
            "--then-tail",
            "2,10000",
        ],
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    assert_eq!(v[1]["result"], "frames", "{v}");
    let frames = v[1]["value"]["frames"].as_array().expect("frames array");
    assert_eq!(frames.len(), 2, "{v}");
    let signal = &frames[0]["payload"];
    assert_eq!(signal["protocol"], "signal", "{v}");
    assert_eq!(signal["state"], "new", "{v}");
    let at = signal["center_hz"].as_u64().unwrap();
    assert!(at.abs_diff(CENTER + 300_000) <= 6_000, "{v}");
    let watch = &frames[1]["payload"];
    assert_eq!(watch["protocol"], "watch", "{v}");
    assert_eq!(watch["state"], "identifying", "{v}");
    assert_eq!(watch["signal"]["center_hz"], at, "{v}");
    assert_eq!(v[2], serde_json::json!({"stopped": true}));
    let jobs = zeta(dir.path(), &["--source-file", &src, "job", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let jobs = json(&jobs)["value"].as_array().expect("jobs array").clone();
    assert!(running_kinds(&jobs).is_empty(), "{jobs:?}");
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
    // A 1 003 Hz source (17 x 59) channelises at itself, and no rate within
    // 1000 ppm of it resamples to 48 kHz audio under the snap factor.
    let iq = zeta_dsp::util::nco(2_000, 100.0, 1_003.0);
    let meta = zeta_dsp::fixtures::write_fixture(dir.path(), "cd", &iq, 1_003, 100_000_000);
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
            // The job's first block is the source's second, about 2 s in (2000 samples
            // at 1003 Hz): the tail outlasts it, and ends when the failed job's stream
            // closes.
            "--then-tail",
            "1,5000",
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
    assert!(
        reason.contains("beyond the 2048 limit and no rate within 1000 ppm snaps"),
        "{v}"
    );
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

/// The worker subcommand of a build without the driver exits 1 naming the rebuild, as
/// `--source-soapy` and `soapy find` do; a node never spawns it in such a build.
#[cfg(not(soapy))]
#[test]
fn soapy_worker_without_the_cfg_exits_1_naming_the_rebuild() {
    zeta_bin()
        .args(["soapy-worker", "--id", "x", "--args", "driver=hackrf"])
        .assert()
        .code(1)
        .stderr(contains("built without soapy"));
}

/// `soapy-worker` is the node's own child, not an operator command: it is left out of
/// `--help` but still answers its own `--help`.
#[test]
fn soapy_worker_is_hidden_from_help() {
    zeta_bin()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("soapy-worker").not());
    zeta_bin()
        .args(["soapy-worker", "--help"])
        .assert()
        .success()
        .stdout(contains("--args"));
}

/// The node passes a cached measured cap back to the worker as `--rate-cap-hz`, so a
/// respawn skips the rate probe.
#[test]
fn soapy_worker_takes_a_rate_cap() {
    zeta_bin()
        .args(["soapy-worker", "--help"])
        .assert()
        .success()
        .stdout(contains("--rate-cap-hz"));
    zeta_bin()
        .args(["soapy-worker", "--id", "x", "--args", "driver=hackrf"])
        .args(["--rate-cap-hz", "fast"])
        .assert()
        .code(1)
        .stderr(contains("invalid value 'fast' for '--rate-cap-hz"));
}

/// `--soapy-in-process` is a global flag: a node without a Soapy source accepts it.
#[test]
fn soapy_in_process_is_accepted_as_a_global_flag() {
    let dir = tempfile::tempdir().unwrap();
    zeta(dir.path(), &["--soapy-in-process", "source", "list"])
        .assert()
        .success();
}

#[test]
fn node_mode_rejects_source_and_plugin_flags() {
    for flags in [
        ["--source-kiwi", "k=h:1"],
        ["--source-node", "pi=tone@http://127.0.0.1:9"],
        ["--source-vrt", "v=udp://127.0.0.1:4991"],
        ["--source-soapy", "h=driver=hackrf"],
        ["--plugin", "gain=/x"],
        ["--themes-dir", "/x"],
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
fn knowledge_acma_flag_is_refused_with_node() {
    zeta_bin()
        .args(["--node", "http://127.0.0.1:9", "--secret", "s"])
        .args(["--knowledge-acma", "http://127.0.0.1:9", "source", "list"])
        .assert()
        .code(1)
        .stderr(contains("configured on the serving node"));
}

#[test]
fn knowledge_acma_from_the_environment_is_ignored_with_node() {
    // Nothing listens on port 9: the command fails on the connection, never as a usage
    // error over the node-local settings a sourced `zeta.env` puts in the environment.
    let out = zeta_bin()
        .env("ZETA_KNOWLEDGE_ACMA", "http://127.0.0.1:9")
        .env("ZETA_BURSTS_KEEP_MB", "1")
        .env("ZETA_BURSTS_KEEP_DAYS", "1")
        .args([
            "--node",
            "http://127.0.0.1:9",
            "--secret",
            "s",
            "source",
            "list",
        ])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!text.contains("configured on the serving node"), "{text}");
    assert!(!out.status.success(), "{text}");
    // A bad value in the environment is ignored too.
    let out = zeta_bin()
        .env("ZETA_KNOWLEDGE_ACMA", "ftp://x")
        .args([
            "--node",
            "http://127.0.0.1:9",
            "--secret",
            "s",
            "source",
            "list",
        ])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&out.stderr).contains("--knowledge-acma"));
    // The explicit flag is still refused.
    zeta_bin()
        .env("ZETA_BURSTS_KEEP_MB", "1")
        .args(["--node", "http://127.0.0.1:9", "--secret", "s"])
        .args(["--bursts-keep-mb", "2", "source", "list"])
        .assert()
        .code(1)
        .stderr(contains("configured on the serving node"));
}

#[test]
fn knowledge_acma_with_a_bad_url_is_a_usage_error() {
    for bad in ["ftp://x", "http://h:1,reach_km=0", "http://h:1,reach_km=x"] {
        zeta_bin()
            .args(["--knowledge-acma", bad, "source", "list"])
            .assert()
            .code(1)
            .stderr(contains("--knowledge-acma"));
    }
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
    assert_eq!(v["result"], "classification", "{v}");
    let labels = v["value"]["labels"].as_array().expect("labels array");
    // A 0.5 s span is shorter than every row's `min_span_ms`.
    let tried = v["value"]["tried"].as_array().expect("tried array");
    assert!(!tried.is_empty(), "{v}");
    assert!(tried.iter().all(|t| t["outcome"] == "skipped_span"), "{v}");
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

#[test]
fn job_attach_one_shot_names_an_unknown_job() {
    let dir = tempfile::tempdir().unwrap();
    let out = zeta(
        dir.path(),
        &["job", "attach", "--kind", "transcribe", "--job", "NOPE"],
    )
    .assert()
    .code(2)
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    assert_eq!(v["error"], "unknown", "{v}");
    assert_eq!(v["field"], "job", "{v}");
    assert_eq!(v["value"], "NOPE", "{v}");
}

/// `serve` with a speech daemon; over `--node`: `job start` a `wfm_demod` on a speech
/// fixture, `job attach --kind transcribe` to it, then `stream tail` its event streams
/// (`vad`'s and `transcribe`'s) until the daemon's text arrives as a `transcript` event.
#[test]
fn job_attach_over_node_tails_a_transcript_event() {
    let dir = tempfile::tempdir().unwrap();
    let meta = common::speech_fixture(dir.path());
    let sock = dir.path().join("voiced.sock");
    let _daemon = zeta_speech::testing::FakeDaemon::replying(&sock, "ok:hello from the daemon\n");
    let src = format!("speech={}", meta.display());
    let node = common::ServeProc::start_full(
        dir.path(),
        NODE_SECRET,
        &["--source-file", &src],
        &["--stt-socket", sock.to_str().unwrap()],
        &[],
    );
    let remote = |args: &[&str]| {
        json(
            &common::zeta_remote(&node.url, NODE_SECRET, args)
                .assert()
                .success()
                .get_output()
                .stdout
                .clone(),
        )
    };
    let wfm = remote(&[
        "job",
        "start",
        "--kind",
        "wfm_demod",
        "--source",
        "speech",
        "--center-hz",
        "104700000",
        "--bandwidth-hz",
        "200000",
    ]);
    let wfm = wfm["value"].as_str().expect("job id").to_string();
    let att = remote(&["job", "attach", "--kind", "transcribe", "--job", &wfm]);
    assert_eq!(att["result"], "job", "{att}");
    let att = att["value"].as_str().expect("job id").to_string();
    let jobs = remote(&["job", "list"]);
    let rec = jobs["value"]
        .as_array()
        .and_then(|a| a.iter().find(|j| j["id"] == att.as_str()))
        .unwrap_or_else(|| panic!("{att} not in {jobs}"))
        .clone();
    assert_eq!(rec["kind"], "transcribe", "{rec}");
    assert_eq!(rec["input"], wfm.as_str(), "{rec}");
    assert_eq!(rec["state"]["status"], "running", "{rec}");
    let streams = remote(&["stream", "list"]);
    let events: Vec<String> = streams["value"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["provenance"]["job"] == att.as_str() && s["kind"] == "event")
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(events.len(), 2, "{streams}");
    let tails: Vec<_> = events
        .into_iter()
        .map(|s| {
            let url = node.url.clone();
            std::thread::spawn(move || {
                let args = [
                    "stream",
                    "tail",
                    "--stream",
                    &s,
                    "--max-frames",
                    "1",
                    "--timeout-ms",
                    "10000",
                ];
                json(
                    &common::zeta_remote(&url, NODE_SECRET, &args)
                        .assert()
                        .success()
                        .get_output()
                        .stdout
                        .clone(),
                )
            })
        })
        .collect();
    let frames: Vec<Value> = tails
        .into_iter()
        .flat_map(|t| {
            t.join().unwrap()["value"]["frames"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .collect();
    let t = frames
        .iter()
        .find(|f| f["payload"]["protocol"] == "transcript")
        .unwrap_or_else(|| panic!("no transcript in {frames:?}"));
    assert_eq!(t["payload"]["text"], "hello from the daemon", "{t}");
    assert_eq!(t["job"], att.as_str(), "{t}");
    assert!(
        frames.iter().any(|f| f["payload"]["protocol"] == "vad"),
        "{frames:?}"
    );
}

#[test]
fn ops_tail_prints_the_last_lines() {
    let dir = tempfile::tempdir().unwrap();
    zeta(dir.path(), &["source", "list"]).assert().success();
    zeta(dir.path(), &["job", "list"]).assert().success();
    let log = std::fs::read_to_string(dir.path().join("state").join("ops.log")).unwrap();
    let last = log.lines().last().expect("two ops logged");
    assert_eq!(log.lines().count(), 2);

    let out = zeta_bin()
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .args(["ops", "tail", "--lines", "1"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(String::from_utf8(out).unwrap(), format!("{last}\n"));
    assert!(last.contains("\"job_list\""));

    zeta_bin()
        .args(["--node", "http://127.0.0.1:1", "ops", "tail"])
        .assert()
        .code(1)
        .stderr(contains(
            "ops tail reads the state directory: not supported with --node",
        ));
}

#[test]
fn transcripts_tail_prints_the_last_lines_filtered_by_job() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let first = r#"{"t_ns":1,"job":"J1","input":"A1","source":"tone","center_hz":100000000,"bandwidth_hz":48000,"start_ns":10,"end_ns":20,"text":"one","language":"en","dropped":false,"error":null}"#;
    let second = r#"{"t_ns":2,"job":"J2","input":"A2","source":"tone","center_hz":100000000,"bandwidth_hz":48000,"start_ns":30,"end_ns":40,"text":"two","language":"en","dropped":false,"error":null}"#;
    std::fs::write(
        state.join("transcripts.log"),
        format!("{first}\n{second}\n"),
    )
    .unwrap();
    let tail = |args: &[&str]| {
        let out = zeta_bin()
            .arg("--state-dir")
            .arg(&state)
            .args(["transcripts", "tail"])
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8(out).unwrap()
    };
    assert_eq!(tail(&["--lines", "1"]), format!("{second}\n"));
    assert_eq!(tail(&["--job", "J1"]), format!("{first}\n"));
    assert_eq!(tail(&["--job", "A1"]), format!("{first}\n"));
    assert_eq!(tail(&[]), format!("{first}\n{second}\n"));

    zeta_bin()
        .args(["--node", "http://127.0.0.1:1", "transcripts", "tail"])
        .assert()
        .code(1)
        .stderr(contains(
            "transcripts tail reads the state directory: not supported with --node",
        ));
}

#[test]
fn events_tail_prints_the_last_lines_filtered_by_job_source_and_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let first = r#"{"t_ns":1,"source":"b210","job":"J1","channel":null,"center_hz":131450000,"bandwidth_hz":12500,"start_ns":10,"end_ns":20,"event":{"protocol":"acars","reg":"C-FGEO"}}"#;
    let second = r#"{"t_ns":2,"source":"b210","job":"J2","channel":null,"center_hz":144800000,"bandwidth_hz":12500,"start_ns":30,"end_ns":40,"event":{"protocol":"aprs","from":"VK1ABC"}}"#;
    let third = r#"{"t_ns":3,"source":"hackrf","job":"J3","channel":null,"center_hz":148600000,"bandwidth_hz":12500,"start_ns":50,"end_ns":60,"event":{"protocol":"pocsag","address":1}}"#;
    std::fs::write(
        state.join("events.log"),
        format!("{first}\n{second}\n{third}\n"),
    )
    .unwrap();
    let tail = |args: &[&str]| {
        let out = zeta_bin()
            .arg("--state-dir")
            .arg(&state)
            .args(["events", "tail"])
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8(out).unwrap()
    };
    assert_eq!(tail(&["--lines", "1"]), format!("{third}\n"));
    assert_eq!(tail(&["--job", "J1"]), format!("{first}\n"));
    assert_eq!(tail(&["--source", "b210"]), format!("{first}\n{second}\n"));
    assert_eq!(tail(&["--protocol", "aprs"]), format!("{second}\n"));
    assert_eq!(
        tail(&["--source", "b210", "--protocol", "pocsag"]),
        String::new()
    );
    assert_eq!(tail(&[]), format!("{first}\n{second}\n{third}\n"));

    zeta_bin()
        .args(["--node", "http://127.0.0.1:1", "events", "tail"])
        .assert()
        .code(1)
        .stderr(contains(
            "events tail reads the state directory: not supported with --node",
        ));
}

/// `source gain` takes exactly one of `--db N` and `--auto` (a usage error, exit
/// 1, otherwise); a file source has no adjustable gain (`other`, exit 2).
#[test]
fn source_gain_cli_requires_db_or_auto() {
    let dir = tempfile::tempdir().unwrap();
    zeta(dir.path(), &["source", "gain", "--source", "tone"])
        .assert()
        .code(1);
    zeta(
        dir.path(),
        &["source", "gain", "--source", "tone", "--db", "20", "--auto"],
    )
    .assert()
    .code(1);
    let out = zeta(
        dir.path(),
        &["source", "gain", "--source", "tone", "--auto"],
    )
    .assert()
    .code(2)
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    assert_eq!(v["error"], "other", "{v}");
    assert!(
        v["message"]
            .as_str()
            .unwrap_or_default()
            .contains("source has no adjustable gain"),
        "{v}"
    );
    let out = zeta(
        dir.path(),
        &["source", "gain", "--source", "tone", "--db", "-5.5"],
    )
    .assert()
    .code(2)
    .get_output()
    .stdout
    .clone();
    assert_eq!(json(&out)["message"], "source has no adjustable gain");
    let out = zeta(dir.path(), &["source", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(json(&out)["value"][0]["gain"].is_null());
}

/// `source free` against a serving node (`--node`) lists the idle sources that cover the
/// band as `free` and the held one under `busy` with its holder; the recording that
/// cannot cover it is in neither. A one-shot `source free` reads while `serve` owns the
/// state directory.
#[test]
fn source_free_prints_free_and_busy() {
    let dir = tempfile::tempdir().unwrap();
    let meta = write_tone(dir.path());
    let ranged = |id: &str| {
        format!(
            "{id}={},tune_min_hz=400000000,tune_max_hz=406000000",
            meta.display()
        )
    };
    let (held, idle) = (ranged("held"), ranged("idle"));
    let flags = ["--source-file", &held, "--source-file", &idle];
    let node = common::ServeProc::start_with(dir.path(), NODE_SECRET, &flags);
    let band = ["--center-hz", "403000000", "--bandwidth-hz", "20000"];
    let start: Vec<&str> = ["job", "start", "--kind", "passthrough", "--source", "held"]
        .into_iter()
        .chain(band)
        .collect();
    let out = common::zeta_remote(&node.url, NODE_SECRET, &start)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let job = json(&out)["value"].clone();
    let free: Vec<&str> = ["source", "free"].into_iter().chain(band).collect();
    let out = common::zeta_remote(&node.url, NODE_SECRET, &free)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["result"], "source_free", "{v}");
    assert_eq!(v["value"]["free"], serde_json::json!(["idle"]), "{v}");
    assert_eq!(
        v["value"]["busy"],
        serde_json::json!([{
            "id": "held", "holders": [job], "elastic": [], "pin": null, "fault": null
        }]),
        "{v}"
    );
    // The same state directory, owned by the serving node: a read still runs one-shot.
    let mut local = free.clone();
    local.splice(0..0, flags);
    let out = zeta(dir.path(), &local)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(
        v["value"]["free"],
        serde_json::json!(["held", "idle"]),
        "{v}"
    );
    assert_eq!(v["value"]["busy"], serde_json::json!([]), "{v}");
}

/// The test stand-in for dump1090 (`crates/zeta-ext/tests/fake_dump1090`), absolute.
fn fake_dump1090() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../zeta-ext/tests/fake_dump1090")
        .canonicalize()
        .unwrap()
}

/// The test stand-in for an external decoder (`crates/zeta-ext/tests/fake_lines`),
/// absolute.
fn fake_lines() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../zeta-ext/tests/fake_lines")
        .canonicalize()
        .unwrap()
}

/// The sonde rows' programs, each pinned to `path` whatever the host has.
const SONDE_HANDLERS: [&str; 3] = [
    "ZETA_HANDLER_RS41MOD",
    "ZETA_HANDLER_DFM09MOD",
    "ZETA_HANDLER_M20MOD",
];

/// The aircraft data-link rows' programs, each pinned to `path` whatever the host has.
const AIRCRAFT_LINK_HANDLERS: [&str; 2] = ["ZETA_HANDLER_DUMPVDL2", "ZETA_HANDLER_DUMPHFDL"];

/// The FLEX row's program, pinned like the others.
const PAGER_HANDLERS: [&str; 1] = ["ZETA_HANDLER_MULTIMON_NG"];

/// The M17 row's vocoder, pinned like the others.
const VOICE_HANDLERS: [&str; 1] = ["ZETA_HANDLER_C2DEC"];

/// `zeta modes list` (one-shot) prints the embedded rows, all ready; `--family`
/// narrows it and `modes show` prints one row.
#[test]
fn modes_list_prints_the_embedded_rows() {
    let dir = tempfile::tempdir().unwrap();
    let modes = dir.path().join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    let modes_dir = modes.display().to_string();
    // The program rows are ready only with their programs: pin them to the fakes,
    // whatever the host has.
    let mut cmd = zeta(dir.path(), &["--modes-dir", &modes_dir, "modes", "list"]);
    cmd.env("ZETA_HANDLER_DUMP1090", fake_dump1090());
    for var in SONDE_HANDLERS
        .iter()
        .chain(&AIRCRAFT_LINK_HANDLERS)
        .chain(&PAGER_HANDLERS)
        .chain(&VOICE_HANDLERS)
    {
        cmd.env(var, fake_lines());
    }
    let out = cmd.assert().success().get_output().stdout.clone();
    let v = json(&out);
    assert_eq!(v["result"], "modes", "{v}");
    let rows = v["value"]["modes"].as_array().expect("modes array");
    let ids: Vec<&str> = rows.iter().filter_map(|m| m["id"].as_str()).collect();
    assert_eq!(
        ids,
        [
            "acars_decode",
            "adsb_decode",
            "ais_decode",
            "aprs_decode",
            "dfm_decode",
            "flex_decode",
            "hfdl_decode",
            "m17_decode",
            "m20_decode",
            "pocsag_decode",
            "rds_decode",
            "rs41_decode",
            "vdl2_decode"
        ]
    );
    assert!(rows.iter().all(|m| m["state"] == "ready"), "{v}");
    let out = zeta(
        dir.path(),
        &[
            "--modes-dir",
            &modes_dir,
            "modes",
            "list",
            "--family",
            "aviation",
        ],
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    assert_eq!(json(&out)["value"]["modes"].as_array().unwrap().len(), 4);
    let out = zeta(
        dir.path(),
        &["--modes-dir", &modes_dir, "modes", "show", "rds_decode"],
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    assert_eq!(v["result"], "mode", "{v}");
    assert_eq!(v["value"]["mode"]["row"]["label"], "RDS", "{v}");
    let out = zeta(
        dir.path(),
        &["--modes-dir", &modes_dir, "modes", "show", "nope"],
    )
    .assert()
    .code(2)
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    assert_eq!(v["error"], "unknown", "{v}");
    assert_eq!(v["field"], "id", "{v}");
    assert_eq!(v["value"], "nope", "{v}");
    assert_eq!(v["known"][0], "acars_decode", "{v}");
}

/// The handler report: with no dump1090 on `PATH` the startup line says so with the
/// package and URL, and `modes list` shows `adsb_decode` unavailable with that reason;
/// `ZETA_HANDLER_DUMP1090` naming a program makes it found and ready.
#[test]
fn handler_report_lists_missing_programs_with_suggestions() {
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    let missing = "zeta: handlers: dump1090: missing (apt install dump1090-fa; https://github.com/flightaware/dump1090) — modes: adsb_decode";
    let out = zeta(dir.path(), &["modes", "list"])
        .env("PATH", &empty)
        .env_remove("ZETA_HANDLER_DUMP1090")
        .env_remove("ZETA_DUMP1090")
        .assert()
        .success()
        .stderr(contains(missing))
        .get_output()
        .clone();
    let v = json(&out.stdout);
    let rows = v["value"]["modes"].as_array().expect("modes array");
    let adsb = rows.iter().find(|m| m["id"] == "adsb_decode").unwrap();
    assert_eq!(adsb["state"], "unavailable", "{v}");
    let reason = adsb["reason"].as_str().unwrap();
    assert!(reason.contains("apt install dump1090-fa"), "{reason}");
    // The graph rows need no program and stay ready.
    let aprs = rows.iter().find(|m| m["id"] == "aprs_decode").unwrap();
    assert_eq!(aprs["state"], "ready", "{v}");

    let fake = fake_dump1090();
    let out = zeta(dir.path(), &["modes", "list"])
        .env("PATH", &empty)
        .env("ZETA_HANDLER_DUMP1090", &fake)
        .assert()
        .success()
        .stderr(contains(format!(
            "zeta: handlers: dump1090: found {} (",
            fake.display()
        )))
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    let rows = v["value"]["modes"].as_array().expect("modes array");
    let adsb = rows.iter().find(|m| m["id"] == "adsb_decode").unwrap();
    assert_eq!(adsb["state"], "ready", "{v}");
    assert_eq!(adsb["program"], fake.display().to_string(), "{v}");
}

/// The sonde rows' programs get a report line each, with the package and URL of the
/// rows, and a row whose program is missing lists unavailable.
#[test]
fn the_handler_report_names_the_sonde_programs() {
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    let nowhere = dir.path().join("no-such-program");
    let mut cmd = zeta(dir.path(), &["modes", "list", "--state", "unavailable"]);
    cmd.env("PATH", &empty);
    for var in SONDE_HANDLERS {
        cmd.env(var, &nowhere);
    }
    let out = cmd.assert().success().get_output().clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    for (program, mode) in [
        ("rs41mod", "rs41_decode"),
        ("dfm09mod", "dfm_decode"),
        ("m20mod", "m20_decode"),
    ] {
        let line = format!(
            "zeta: handlers: {program}: missing (apt install sonde-decoders; https://github.com/rs1729/RS) — modes: {mode}"
        );
        assert!(stderr.contains(&line), "{line} not in {stderr}");
    }
    let v = json(&out.stdout);
    let ids: Vec<&str> = v["value"]["modes"]
        .as_array()
        .expect("modes array")
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    assert!(ids.contains(&"rs41_decode"), "{v}");
}

/// `rs41_decode` runs its handler on the NBFM demod's audio under a WAV header: the
/// fake (pinned as `rs41mod`) reads the header, says what it held, then prints one
/// rs1729-shaped line, published with the row's summary, position and GPS time.
#[test]
fn rs41_decode_runs_the_handler_on_demodulated_audio() {
    let dir = tempfile::tempdir().unwrap();
    let line = r#"{"frame": 4321, "id": "S1234567", "datetime": "2026-10-28T02:15:30.000Z", "lat": -27.4, "lon": 153.1, "alt": 12000.0, "vel_h": 3.2, "heading": 90.0, "vel_v": 5.0, "sats": 9, "bt": 65535, "batt": 2.9}"#;
    let out = zeta(
        dir.path(),
        &[
            "job",
            "start",
            "--kind",
            "rs41_decode",
            "--source",
            "tone",
            "--center-hz",
            "100000000",
            "--bandwidth-hz",
            "12500",
            "--then-tail",
            "2,5000",
        ],
    )
    .env("ZETA_HANDLER_RS41MOD", fake_lines())
    .env("ZETA_PLUGIN_ENV_FAKE_WAV", "1")
    .env("ZETA_PLUGIN_ENV_FAKE_LINES", line)
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    let frames = v[1]["value"]["frames"].as_array().expect("frames array");
    assert_eq!(frames.len(), 2, "{v}");
    assert_eq!(
        frames[0]["payload"]["wav"],
        serde_json::json!({"rate_hz": 48000, "bits": 16, "channels": 1}),
        "{v}"
    );
    let p = &frames[1]["payload"];
    assert_eq!(p["protocol"], "rs41", "{v}");
    assert_eq!(
        p["summary"], "S1234567 -27.4000,153.1000 12000 m +5.0 m/s",
        "{v}"
    );
    assert_eq!(
        p["position"],
        serde_json::json!({"lat": -27.4, "lon": 153.1, "alt_m": 12000.0}),
        "{v}"
    );
    assert_eq!(p["t_ns"], 1_793_153_730_000_000_000u64, "{v}");
    assert_eq!(v[2]["stopped"], true, "{v}");
}

/// Without `--modes-dir` the CLI reads `$XDG_CONFIG_HOME/zetasdr/modes`; with it, only
/// the directory given.
#[test]
fn the_default_modes_dir_is_the_xdg_one() {
    let dir = tempfile::tempdir().unwrap();
    let xdg = dir.path().join("xdg");
    let modes = xdg.join("zetasdr").join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    let mut row: Value =
        serde_json::from_str(include_str!("../../../modes/pocsag_decode.json")).unwrap();
    row["name"] = "Local pagers".into();
    std::fs::write(modes.join("my_pocsag.json"), row.to_string()).unwrap();
    let empty = dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let show = |extra: &[&str]| {
        let mut args = extra.to_vec();
        args.extend(["modes", "show", "pocsag_decode"]);
        let out = zeta(dir.path(), &args)
            .env("XDG_CONFIG_HOME", &xdg)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        json(&out)["value"]["mode"].clone()
    };
    let m = show(&[]);
    assert_eq!(m["row"]["name"], "Local pagers", "{m}");
    assert_eq!(
        m["file"],
        modes.join("my_pocsag.json").display().to_string(),
        "{m}"
    );
    let empty = empty.display().to_string();
    let m = show(&["--modes-dir", &empty]);
    assert_eq!(m["row"]["name"], "POCSAG pagers", "{m}");
    assert_eq!(m["file"], "modes/pocsag_decode.json", "{m}");
}

/// `zeta themes list` prints the built-in themes, ready.
#[test]
fn themes_list_prints_the_builtins() {
    let dir = tempfile::tempdir().unwrap();
    let themes = dir.path().join("themes");
    std::fs::create_dir_all(&themes).unwrap();
    let themes_dir = themes.display().to_string();
    let out = zeta(dir.path(), &["--themes-dir", &themes_dir, "themes", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["result"], "themes", "{v}");
    let rows = v["value"]["themes"].as_array().expect("themes array");
    let ids: Vec<&str> = rows.iter().filter_map(|t| t["id"].as_str()).collect();
    assert!(ids.contains(&"console") && ids.contains(&"plain"), "{v}");
    assert!(rows.iter().all(|t| t["state"] == "ready"), "{v}");
}

/// `zeta sets` prints the two built-in sets, ready, with no running instance.
#[test]
fn zeta_sets_prints_the_two_builtin_sets() {
    let dir = tempfile::tempdir().unwrap();
    let sets = dir.path().join("sets");
    std::fs::create_dir_all(&sets).unwrap();
    let sets_dir = sets.display().to_string();
    let out = zeta(dir.path(), &["--sets-dir", &sets_dir, "sets"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["result"], "sets", "{v}");
    let rows = v["value"].as_array().expect("sets array");
    let ids: Vec<&str> = rows.iter().filter_map(|s| s["id"].as_str()).collect();
    assert_eq!(ids, ["airband", "pagers_au"], "{v}");
    assert!(rows.iter().all(|s| s["state"] == "ready"), "{v}");
    assert!(rows.iter().all(|s| s["running"].is_null()), "{v}");
}

/// `zeta set stop` of a set that does not run stops nothing; `zeta set start` of an
/// unknown set names the known ones and of an unmapped role names it.
#[test]
fn zeta_set_start_and_stop_answer_as_the_ops() {
    let dir = tempfile::tempdir().unwrap();
    let out = zeta(dir.path(), &["set", "stop", "--set", "airband"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["result"], "set_stopped", "{v}");
    assert_eq!(v["value"]["stopped"], serde_json::json!([]), "{v}");
    assert_eq!(v["value"]["ended"], serde_json::json!([]), "{v}");
    zeta(
        dir.path(),
        &["set", "start", "--set", "nope", "--radio", "wide=tone"],
    )
    .assert()
    .failure()
    .stdout(contains("airband").and(contains("pagers_au")));
    zeta(
        dir.path(),
        &["set", "start", "--set", "airband", "--radio", "wide=tone"],
    )
    .assert()
    .failure()
    .stdout(contains("radios.second: unmapped"));
}

/// `zeta set start --set X` without `--radio` reads `set_list` and sends every role of
/// the set as `null`, so the node picks: airband's roles are sent, and the answer names
/// the first role the node cannot fill (the tone file covers no airband band), not
/// `unmapped`.
#[test]
fn zeta_set_start_without_radio_sends_every_role_as_null() {
    let dir = tempfile::tempdir().unwrap();
    let out = zeta(dir.path(), &["set", "start", "--set", "airband"])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["error"], "other", "{v}");
    let m = v["message"].as_str().unwrap();
    assert!(m.starts_with("radios.wide: no free source covers"), "{v}");
    let op = last_logged_op(dir.path());
    assert_eq!(
        op["radios"],
        serde_json::json!({"wide": null, "second": null}),
        "{op}"
    );
    // An unknown set is still the op's own answer.
    zeta(dir.path(), &["set", "start", "--set", "nope"])
        .assert()
        .code(2)
        .stdout(contains("airband").and(contains("pagers_au")));
}

/// A role the node cannot fill on a source that covers its band is a structured
/// `conflict`: here a source pinned beside the role's band, with the pin's way out.
#[test]
fn zeta_set_start_pick_failure_is_a_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let meta = write_tone(dir.path());
    let flag = format!(
        "retune={},tune_min_hz=50000000,tune_max_hz=150000000",
        meta.display()
    );
    let sets = dir.path().join("sets");
    std::fs::create_dir_all(&sets).unwrap();
    std::fs::write(
        sets.join("one.json"),
        r#"{"id": "one", "name": "one", "notes": "", "radios": {"r": {"note": "any"}},
            "members": [{"id": "m", "radio": "r", "op": "job_start", "kind": "measure",
              "center_hz": 100000000, "bandwidth_hz": 20000, "deviation_hz": null,
              "sideband": null}]}"#,
    )
    .unwrap();
    let sets_dir = sets.display().to_string();
    let run = |args: &[&str]| {
        let mut cmd = zeta_bin();
        cmd.arg("--state-dir")
            .arg(dir.path().join("state"))
            .arg("--source-file")
            .arg(&flag)
            .arg("--sets-dir")
            .arg(&sets_dir)
            .arg("--json")
            .args(args);
        cmd
    };
    run(&[
        "source",
        "pin",
        "--source",
        "retune",
        "--center-hz",
        "120000000",
        "--bandwidth-hz",
        "12500",
    ])
    .assert()
    .success();
    let out = run(&["set", "start", "--set", "one"])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["error"], "conflict", "{v}");
    assert_eq!(v["source"], "retune", "{v}");
    assert_eq!(v["holders"], serde_json::json!([]), "{v}");
    let options = v["options"].as_array().unwrap();
    let first = options[0].as_str().unwrap();
    assert!(
        first.starts_with("radios.r: no free source covers 100000000 Hz ± 10000 Hz"),
        "{v}"
    );
    assert!(
        options.contains(&serde_json::json!("unpin source retune (source_unpin)")),
        "{v}"
    );
}

/// A theme folder that does not validate is logged at startup and listed as broken; the
/// node runs.
#[test]
fn a_broken_operator_theme_is_reported_at_startup() {
    let dir = tempfile::tempdir().unwrap();
    let themes = dir.path().join("themes");
    std::fs::create_dir_all(themes.join("mytheme")).unwrap();
    std::fs::write(
        themes.join("mytheme/theme.json"),
        r#"{"id":"mytheme","bogus":1}"#,
    )
    .unwrap();
    let themes_dir = themes.display().to_string();
    let out = zeta(dir.path(), &["--themes-dir", &themes_dir, "themes", "list"])
        .assert()
        .success()
        .stderr(contains("zeta: theme mytheme ("))
        .get_output()
        .stdout
        .clone();
    let rows = json(&out)["value"]["themes"].as_array().unwrap().clone();
    let t = rows.iter().find(|t| t["id"] == "mytheme").expect("mytheme");
    assert_eq!(t["state"], "broken");
    assert!(t["reason"].as_str().unwrap().contains("bogus"));
}

/// A row file that does not parse is logged at startup and listed as broken; the node runs.
#[test]
fn a_broken_operator_row_is_logged_and_the_node_runs() {
    let dir = tempfile::tempdir().unwrap();
    let modes = dir.path().join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    std::fs::write(
        modes.join("pocsag_decode.json"),
        r#"{"id":"pocsag_decode","bogus":1}"#,
    )
    .unwrap();
    let modes_dir = modes.display().to_string();
    let out = zeta(
        dir.path(),
        &[
            "--modes-dir",
            &modes_dir,
            "modes",
            "list",
            "--state",
            "broken",
        ],
    )
    .assert()
    .success()
    .stderr(contains("zeta: mode pocsag_decode").and(contains("bogus")))
    .get_output()
    .stdout
    .clone();
    let rows = json(&out)["value"]["modes"].as_array().unwrap().clone();
    assert_eq!(rows.len(), 1);
    assert!(rows[0]["reason"].as_str().unwrap().contains("bogus"));
    assert_eq!(
        rows[0]["file"],
        modes.join("pocsag_decode.json").display().to_string()
    );
    zeta(dir.path(), &["--modes-dir", &modes_dir, "source", "list"])
        .assert()
        .success()
        .stderr(contains("zeta: mode pocsag_decode"));
}

/// `job start --kind` takes a built-in kind's name or any mode id: the node refuses an
/// id no row has; a kind that is neither is a usage error.
#[test]
fn job_start_accepts_a_mode_id_string() {
    let dir = tempfile::tempdir().unwrap();
    let start = |kind: &str| {
        zeta(
            dir.path(),
            &[
                "job",
                "start",
                "--kind",
                kind,
                "--source",
                "tone",
                "--center-hz",
                "100000000",
                "--bandwidth-hz",
                "12500",
            ],
        )
    };
    let out = start("pocsag_decode")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["result"], "job");
    let out = start("nonesuch_decode")
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["error"], "unknown", "{v}");
    assert_eq!(v["field"], "kind", "{v}");
    assert_eq!(v["value"], "nonesuch_decode", "{v}");
    let known: Vec<&str> = v["known"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap())
        .collect();
    assert!(
        known.contains(&"fm_demod") && known.contains(&"pocsag_decode"),
        "{v}"
    );
    start("Nope")
        .assert()
        .code(1)
        .stderr(contains("Nope").and(contains("mode id")));
}

/// The tone fixture relabelled at 476.9 MHz, playing as a radio tunable over 470-480 MHz.
fn cb_source(dir: &Path) -> String {
    let tone = write_tone(dir);
    let meta = std::fs::read_to_string(&tone).unwrap();
    let field = "\"core:frequency\":100000000";
    assert!(meta.contains(field), "{meta}");
    let path = dir.join("cb.sigmf-meta");
    std::fs::write(&path, meta.replace(field, "\"core:frequency\":476900000")).unwrap();
    std::fs::copy(tone.with_extension("sigmf-data"), dir.join("cb.sigmf-data")).unwrap();
    format!(
        "cb={},tune_min_hz=470000000,tune_max_hz=480000000",
        path.display()
    )
}

#[test]
fn plan_start_cli_maps_to_the_op_and_parses_channel_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let run = |channels: &str| {
        let mut cmd = zeta_bin();
        cmd.arg("--state-dir")
            .arg(dir.path().join("state"))
            .arg("--source-file")
            .arg(cb_source(dir.path()))
            .arg("--json")
            .args([
                "plan",
                "start",
                "--plan",
                "uhf_cb_au",
                "--kind",
                "fm_demod",
                "--source",
                "cb",
                "--channels",
                channels,
            ]);
        cmd
    };
    let out = run("2,1-2,1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    let started: Vec<u64> = v["value"]["started"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["channel"].as_u64().unwrap())
        .collect();
    assert_eq!(started, [1, 2], "{v}");
    let op = last_logged_op(dir.path());
    assert_eq!(op["op"], "plan_start", "{op}");
    assert_eq!(op["channels"], serde_json::json!([1, 2]), "{op}");
    // The recording is 48 kHz wide, so two adjacent channels are what it can hold.
    run("8-5").assert().code(1).stderr(contains("channels"));
}

#[test]
fn mix_start_cli_maps_to_the_op() {
    let dir = tempfile::tempdir().unwrap();
    let start = |extra: &[&str]| {
        let mut args = vec!["mix", "start"];
        args.extend_from_slice(extra);
        zeta(dir.path(), &args)
    };
    let out = start(&[]).assert().success().get_output().stdout.clone();
    let v = json(&out);
    assert_eq!(v["result"], "job", "{v}");
    let id = v["value"].as_str().expect("job id").to_owned();
    assert_eq!(
        last_logged_op(dir.path()),
        serde_json::json!({"op": "mix_start", "sources": null})
    );
    // The persisted record is running: the same set answers the same id.
    let out = start(&[]).assert().success().get_output().stdout.clone();
    assert_eq!(json(&out)["value"], id.as_str());
    // A list naming the source is another mix.
    let out = start(&["--sources", "tone"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let other = json(&out);
    assert_ne!(other["value"], id.as_str(), "{other}");
    assert_eq!(
        last_logged_op(dir.path()),
        serde_json::json!({"op": "mix_start", "sources": ["tone"]})
    );
    let out = start(&["--sources", "nope"])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["error"], "unknown");
    let out = zeta(
        dir.path(),
        &[
            "job",
            "start",
            "--kind",
            "mix",
            "--source",
            "tone",
            "--center-hz",
            "100000000",
            "--bandwidth-hz",
            "12500",
        ],
    )
    .assert()
    .code(2)
    .get_output()
    .stdout
    .clone();
    assert!(
        json(&out)
            .to_string()
            .contains("mix takes no band: use mix_start"),
        "{out:?}"
    );
}

#[test]
fn a_source_flag_named_node_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let meta = write_tone(dir.path());
    zeta_bin()
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .arg("--source-file")
        .arg(format!("node={}", meta.display()))
        .args(["source", "list"])
        .assert()
        .code(1)
        .stderr(contains(
            "source id \"node\" is reserved for the node's own streams",
        ));
}

/// `station set` persists across one-shot processes; `station get` reads it back with the
/// two default antennas plus the declared one, and `station clear` forgets it.
#[test]
fn station_set_get_clear_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let mut cmd = zeta_bin();
        cmd.arg("--state-dir")
            .arg(dir.path().join("state"))
            .arg("--json")
            .args(args);
        cmd
    };
    run(&[
        "station",
        "set",
        "--lat",
        "-27.38",
        "--lon",
        "153.12",
        "--alt-m",
        "4",
        "--antenna",
        "yagi=430000000-440000000,azimuth_deg=90",
    ])
    .assert()
    .success();
    let out = run(&["station", "get"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["result"], "station", "{v}");
    assert_eq!(v["value"]["region"], "AU", "{v}");
    assert_eq!(v["value"]["antennas"].as_array().unwrap().len(), 3, "{v}");
    assert_eq!(v["value"]["antennas"][2]["azimuth_deg"], 90.0, "{v}");
    let out = run(&["station", "clear"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        json(&out),
        serde_json::json!({"result": "station", "value": null})
    );
}

/// `capture list` answers an empty store, narrowed or not; `capture show`/`classify`/
/// `delete` name the unknown id (exit 2) and `delete` is a mutating command.
#[test]
fn capture_commands_round_trip_on_an_empty_store() {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let mut cmd = zeta_bin();
        cmd.arg("--state-dir")
            .arg(dir.path().join("state"))
            .arg("--json")
            .args(args);
        cmd
    };
    for args in [
        &["capture", "list"][..],
        &[
            "capture",
            "list",
            "--center-hz",
            "433920000",
            "--bandwidth-hz",
            "10000",
        ][..],
    ] {
        let out = run(args).assert().success().get_output().stdout.clone();
        assert_eq!(
            json(&out),
            serde_json::json!({"result": "captures", "value": []})
        );
    }
    run(&["capture", "list", "--center-hz", "433920000"])
        .assert()
        .failure();
    for verb in ["show", "classify", "delete"] {
        let out = run(&["capture", verb, "--capture", "01NOPE"])
            .assert()
            .code(2)
            .get_output()
            .stdout
            .clone();
        let v = json(&out);
        assert_eq!(v["error"], "unknown", "{v}");
        assert_eq!(v["field"], "capture", "{v}");
    }
}

#[test]
fn bursts_keep_flags_reach_the_store() {
    use zeta_core::bursts::{BurstPolicy, BurstStore, NewCapture};
    use zeta_core::types::{Hz, JobId, SourceId};
    let dir = tempfile::tempdir().unwrap();
    {
        let store = BurstStore::open(
            &dir.path().join("state"),
            BurstPolicy {
                keep_bytes: u64::MAX,
                keep_days: 7,
                expire_at_open: true,
            },
        )
        .unwrap();
        // About 1 MB each (131072 samples of 8 bytes = 1 MiB).
        let samples = vec![num_complex::Complex::new(0.1f32, 0.2); 131_072];
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        for i in 0..3u64 {
            store
                .write(
                    NewCapture {
                        source: SourceId("tone".into()),
                        set: JobId("set".into()),
                        center_hz: Hz(100_000_000),
                        bandwidth_hz: Hz(12_500),
                        sample_rate_hz: Hz(48_000),
                        start_ns: now - 1_000_000_000 * (10 - i),
                        end_ns: now,
                        features: serde_json::json!({}),
                        signal: None,
                        job: JobId("job".into()),
                        watch: None,
                        allocations: serde_json::json!([]),
                    },
                    &samples,
                )
                .unwrap();
        }
    }
    let list = |args: &[&str]| -> usize {
        let out = zeta(dir.path(), args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        json(&out)["value"].as_array().unwrap().len()
    };
    // A one-shot with no policy flags never expires at open, even captures its default
    // 7 days would drop (the index's write times made ancient here).
    let index = dir.path().join("state/bursts/index.json");
    let mut v: Value = serde_json::from_slice(&std::fs::read(&index).unwrap()).unwrap();
    for r in v.as_array_mut().unwrap() {
        r["written_ns"] = 1.into();
        r["start_ns"] = 1.into();
    }
    std::fs::write(&index, serde_json::to_vec(&v).unwrap()).unwrap();
    assert_eq!(list(&["capture", "list"]), 3);
    // Under a running serve (this test's pid holds the lock) a one-shot's own policy
    // never expires either: serve's policy is the one in force.
    let lock = dir.path().join("state/serve.lock");
    std::fs::write(&lock, std::process::id().to_string()).unwrap();
    assert_eq!(list(&["--bursts-keep-days", "1", "capture", "list"]), 3);
    std::fs::remove_file(&lock).unwrap();
    // With no serve, an explicit policy is applied at open.
    for r in v.as_array_mut().unwrap() {
        r["written_ns"] = 2_000_000_000_000_000_000u64.into();
    }
    std::fs::write(&index, serde_json::to_vec(&v).unwrap()).unwrap();
    assert_eq!(list(&["--bursts-keep-mb", "1", "capture", "list"]), 1);
    for (flag, name) in [
        ("--bursts-keep-mb", "bursts-keep-mb"),
        ("--bursts-keep-days", "bursts-keep-days"),
    ] {
        let out = zeta(dir.path(), &[flag, "0", "capture", "list"])
            .assert()
            .failure()
            .get_output()
            .stderr
            .clone();
        assert!(String::from_utf8_lossy(&out).contains(name));
    }
}

#[test]
fn source_position_flag_shows_in_source_list_and_refuses_unknown_ids() {
    let dir = tempfile::tempdir().unwrap();
    let out = zeta(
        dir.path(),
        &[
            "--source-position",
            "tone=-27.3842,153.1175,4",
            "source",
            "list",
        ],
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let v = json(&out);
    assert_eq!(
        v["value"][0]["position"],
        serde_json::json!({"lat": -27.3842, "lon": 153.1175, "alt_m": 4.0, "from": "configured"})
    );

    let dir = tempfile::tempdir().unwrap();
    zeta(
        dir.path(),
        &["--source-position", "nope=0,0", "source", "list"],
    )
    .assert()
    .code(1)
    .stderr(contains("--source-position: unknown source nope"));

    let dir = tempfile::tempdir().unwrap();
    zeta(
        dir.path(),
        &["--source-position", "tone=-91,0", "source", "list"],
    )
    .assert()
    .code(1)
    .stderr(contains("--source-position: lat out of range"));
}
