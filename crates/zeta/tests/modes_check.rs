//! `zeta modes check [ID] [--strict]`: each row's `test` run on a node over a synthesised or
//! fixture capture.

mod common;

use std::path::Path;

use assert_cmd::Command;
use zeta_dsp::fixtures::{aprs_packet_iq, write_fixture};

/// A path no dump1090 lives at: the adsb row is missing here whatever the host has.
const NO_DUMP1090: &str = "/nonexistent/zeta-test/dump1090";
/// The sonde rows' programs, pinned to a path none lives at, whatever the host has.
const NO_SONDE: [(&str, &str); 3] = [
    ("ZETA_HANDLER_RS41MOD", "/nonexistent/zeta-test/rs41mod"),
    ("ZETA_HANDLER_DFM09MOD", "/nonexistent/zeta-test/dfm09mod"),
    ("ZETA_HANDLER_M20MOD", "/nonexistent/zeta-test/m20mod"),
];

/// `zeta --state-dir <dir>/state --modes-dir <dir>/modes modes check <args>` with no
/// remote node, no operator rows but the test's own, and dump1090 and the sonde decoders
/// missing.
fn check(dir: &Path, args: &[&str]) -> Command {
    let modes = dir.join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    let mut cmd = Command::cargo_bin("zeta").unwrap();
    cmd.env_remove("ZETA_NODE")
        .env_remove("ZETA_SECRET")
        .env("XDG_CONFIG_HOME", common::no_xdg_config())
        .env("ZETA_HANDLER_DUMP1090", NO_DUMP1090)
        .envs(NO_SONDE)
        .arg("--state-dir")
        .arg(dir.join("state"))
        .arg("--modes-dir")
        .arg(&modes)
        .args(["modes", "check"])
        .args(args);
    cmd
}

fn stdout(cmd: &mut Command) -> (String, i32) {
    let out = cmd.output().unwrap();
    (
        String::from_utf8(out.stdout).unwrap(),
        out.status.code().unwrap(),
    )
}

#[test]
fn modes_check_passes_the_four_in_crate_rows() {
    let dir = tempfile::tempdir().unwrap();
    let (out, code) = stdout(&mut check(dir.path(), &[]));
    for id in ["acars_decode", "aprs_decode", "pocsag_decode", "rds_decode"] {
        assert!(out.lines().any(|l| l == format!("{id}: ok")), "{out}");
    }
    assert!(
        out.lines()
            .any(|l| l == "adsb_decode: skipped: dump1090 missing"),
        "{out}"
    );
    // DFM and M20 have no test yet; RS41's decoder is pinned missing here.
    for id in ["dfm_decode", "m20_decode"] {
        assert!(
            out.lines().any(|l| l == format!("{id}: skipped: no test")),
            "{out}"
        );
    }
    assert!(
        out.lines()
            .any(|l| l == "rs41_decode: skipped: rs41mod missing"),
        "{out}"
    );
    assert_eq!(code, 0, "{out}");
}

#[test]
fn modes_check_strict_fails_on_a_missing_handler() {
    let dir = tempfile::tempdir().unwrap();
    let (out, code) = stdout(&mut check(dir.path(), &["--strict", "adsb_decode"]));
    assert_eq!(out.trim(), "adsb_decode: fail: dump1090 missing", "{out}");
    assert_eq!(code, 1);
}

#[test]
fn an_unknown_id_lists_the_known_ones() {
    let dir = tempfile::tempdir().unwrap();
    let out = check(dir.path(), &["nope"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(
        err.contains("unknown mode nope (known: acars_decode, adsb_decode, aprs_decode"),
        "{err}"
    );
}

/// An operator row: `aprs_decode` under `id` with `test`, in `<dir>/modes`.
fn write_row(dir: &Path, id: &str, test: serde_json::Value) {
    let mut row: serde_json::Value =
        serde_json::from_str(include_str!("../../../modes/aprs_decode.json")).unwrap();
    row["id"] = id.into();
    row["test"] = test;
    let modes = dir.join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    std::fs::write(
        modes.join(format!("{id}.json")),
        serde_json::to_string_pretty(&row).unwrap(),
    )
    .unwrap();
}

#[test]
fn a_fixture_test_row_is_checked() {
    let dir = tempfile::tempdir().unwrap();
    let iq = aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 0.0);
    let meta = write_fixture(dir.path(), "aprs", &iq, 48_000, 144_800_000);
    write_row(
        dir.path(),
        "aprs_fixture",
        serde_json::json!({
            "kind": "fixture",
            "path": meta.display().to_string(),
            "expect": {"src": "VK3ABC", "fcs_ok": true},
            "within_ms": 5000
        }),
    );
    let (out, code) = stdout(&mut check(dir.path(), &["aprs_fixture"]));
    assert_eq!(out.trim(), "aprs_fixture: ok", "{out}");
    assert_eq!(code, 0);
}

#[test]
fn a_failing_expect_prints_the_diff() {
    let dir = tempfile::tempdir().unwrap();
    write_row(
        dir.path(),
        "aprs_wrong",
        serde_json::json!({
            "kind": "synth", "family": "aprs", "src": "VK3ABC", "dst": "APRS", "info": ">hi",
            "expect": {"src": "VK9XYZ", "fcs_ok": true},
            "within_ms": 5000
        }),
    );
    let (out, code) = stdout(&mut check(dir.path(), &["aprs_wrong"]));
    let line = out.trim();
    assert!(line.starts_with("aprs_wrong: fail: "), "{out}");
    assert!(
        line.contains("src: expected \"VK9XYZ\", got \"VK3ABC\""),
        "{out}"
    );
    assert!(!line.contains("fcs_ok"), "{out}");
    assert_eq!(code, 1);
}

#[test]
fn a_test_block_that_does_not_parse_fails_the_row() {
    let dir = tempfile::tempdir().unwrap();
    write_row(
        dir.path(),
        "aprs_bogus",
        serde_json::json!({
            "kind": "synth", "family": "aprs", "src": "VK3ABC", "dst": "APRS", "info": ">hi",
            "colour": "red", "expect": {}, "within_ms": 1000
        }),
    );
    let (out, code) = stdout(&mut check(dir.path(), &["aprs_bogus"]));
    assert!(out.starts_with("aprs_bogus: fail: "), "{out}");
    assert!(out.contains("colour"), "{out}");
    assert_eq!(code, 1);
}

#[test]
fn a_row_without_a_test_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    write_row(dir.path(), "aprs_untested", serde_json::Value::Null);
    let (out, code) = stdout(&mut check(dir.path(), &["aprs_untested"]));
    assert_eq!(out.trim(), "aprs_untested: skipped: no test", "{out}");
    assert_eq!(code, 0);
    // `--strict` makes a skip a fail, a missing test as much as a missing program.
    let (out, code) = stdout(&mut check(dir.path(), &["--strict", "dfm_decode"]));
    assert_eq!(out.trim(), "dfm_decode: fail: no test", "{out}");
    assert_eq!(code, 1);
}

#[test]
fn check_is_local_only() {
    let out = Command::cargo_bin("zeta")
        .unwrap()
        .args([
            "--node",
            "http://127.0.0.1:1",
            "--secret",
            "0123456789abcdef",
            "modes",
            "check",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("modes check"),
    );
}

/// A malformed synth block (a pocsag function-0 page with alphabetic text, an ICAO over 24
/// bits) is that row's `fail`, not a crash: the other rows are still checked and no
/// scratch directory is left in the state dir.
#[test]
fn a_bad_synth_block_fails_its_row_and_the_rest_still_run() {
    let dir = tempfile::tempdir().unwrap();
    write_row(
        dir.path(),
        "aprs_badpage",
        serde_json::json!({
            "kind": "synth", "family": "pocsag", "address": 1, "function": 0,
            "text": "HELLO", "baud": 1200,
            "expect": {}, "within_ms": 1000
        }),
    );
    write_row(
        dir.path(),
        "aprs_badicao",
        serde_json::json!({
            "kind": "synth", "family": "modes_ident", "icao": 16777216, "callsign": "ZETA01",
            "repeats": 1, "expect": {}, "within_ms": 1000
        }),
    );
    let (out, code) = stdout(&mut check(dir.path(), &[]));
    assert!(
        out.lines()
            .any(|l| l.starts_with("aprs_badpage: fail: ") && l.contains("not numeric")),
        "{out}"
    );
    assert!(
        out.lines()
            .any(|l| l.starts_with("aprs_badicao: fail: ") && l.contains("24 bits")),
        "{out}"
    );
    assert!(out.lines().any(|l| l == "aprs_decode: ok"), "{out}");
    assert_eq!(code, 1);
    let left: Vec<_> = std::fs::read_dir(dir.path().join("state"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with("check-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

/// A row's synth block may name the generic `fsk` family: it parses, the signal is
/// produced and the row's decoder runs (the verdict is the decoder's, not a parse error).
#[test]
fn a_row_test_may_name_the_fsk_family() {
    let dir = tempfile::tempdir().unwrap();
    write_row(
        dir.path(),
        "fsk_probe",
        serde_json::json!({
            "kind": "synth", "family": "fsk", "symbol_rate_baud": 1200, "deviation_hz": 4500,
            "order": 2, "bits": "pattern:aa", "repeat": 1,
            "expect": {}, "within_ms": 2000
        }),
    );
    let (out, _) = stdout(&mut check(dir.path(), &["fsk_probe"]));
    let line = out.trim();
    assert!(line.starts_with("fsk_probe: "), "{out}");
    assert!(!line.starts_with("fsk_probe: fail: row:"), "{out}");
    assert!(!line.contains("unknown variant"), "{out}");
}

/// `modes check` writes the state dir, so a running `zeta serve` refuses it like any
/// mutating one-shot.
#[test]
fn check_is_refused_while_serve_owns_the_state_dir() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let pid = std::process::id();
    std::fs::write(state.join("serve.lock"), pid.to_string()).unwrap();
    let out = check(dir.path(), &[]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.contains(&format!("zeta serve (pid {pid}) owns this state dir")),
        "{text}"
    );
}

/// Where `program` lives on this host, if anywhere on `PATH`.
fn which(program: &str) -> Option<String> {
    let out = std::process::Command::new("which")
        .arg(program)
        .output()
        .ok()?;
    let path = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !path.is_empty()).then_some(path)
}

/// Where `rs41mod` lives on this host, if anywhere on `PATH`.
fn rs41mod() -> Option<String> {
    which("rs41mod")
}

/// Each sonde row's `args` are flags its program takes: rs1729's decoders read an unknown
/// option as the input file (`error: open <flag>`), which `modes check` cannot see on a
/// row whose `test` is null. On empty stdin a program that took its flags stops at the
/// missing WAV header instead.
#[test]
fn sonde_programs_accept_their_rows_args() {
    let modes = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modes");
    for id in ["rs41_decode", "dfm_decode", "m20_decode"] {
        let text = std::fs::read_to_string(modes.join(format!("{id}.json"))).unwrap();
        let row: serde_json::Value = serde_json::from_str(&text).unwrap();
        let program = row["decoder"]["programs"][0].as_str().unwrap();
        let Some(path) = which(program) else {
            eprintln!("{program} not on this host");
            continue;
        };
        let args: Vec<&str> = row["decoder"]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        let out = std::process::Command::new(&path)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("error: open"),
            "{id}: {program} {args:?} refused a flag: {stderr}"
        );
    }
}

/// The RS41 row proves itself against the real `rs41mod`: the synthesised frames pass
/// through the row's own chain and the decoder reports the serial.
#[test]
fn rs41_decode_passes_its_synthesised_check() {
    let Some(rs41mod) = rs41mod() else {
        eprintln!("rs41mod not on this host");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let (out, code) =
        stdout(check(dir.path(), &["rs41_decode"]).env("ZETA_HANDLER_RS41MOD", rs41mod));
    assert_eq!(out.trim(), "rs41_decode: ok", "{out}");
    assert_eq!(code, 0);
}
