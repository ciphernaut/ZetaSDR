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
/// The aircraft data-link rows' programs, pinned to a path none lives at, whatever the
/// host has.
/// The pager decoder's program, pinned to a path none lives at, whatever the host has.
const NO_MULTIMON: (&str, &str) = (
    "ZETA_HANDLER_MULTIMON_NG",
    "/nonexistent/zeta-test/multimon-ng",
);
const NO_AIRCRAFT_LINKS: [(&str, &str); 2] = [
    ("ZETA_HANDLER_DUMPVDL2", "/nonexistent/zeta-test/dumpvdl2"),
    ("ZETA_HANDLER_DUMPHFDL", "/nonexistent/zeta-test/dumphfdl"),
];

/// `zeta --state-dir <dir>/state --modes-dir <dir>/modes modes check <args>` with no
/// remote node, no operator rows but the test's own, and dump1090, the sonde decoders,
/// dumpvdl2 and dumphfdl missing.
fn check(dir: &Path, args: &[&str]) -> Command {
    let modes = dir.join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    let mut cmd = Command::cargo_bin("zeta").unwrap();
    // A developer shell's fixture directory must not leak into any test.
    cmd.env_remove("ZETA_FIXTURES_DIR")
        .env_remove("ZETA_NODE")
        .env_remove("ZETA_SECRET")
        .env("XDG_CONFIG_HOME", common::no_xdg_config())
        .env("ZETA_HANDLER_DUMP1090", NO_DUMP1090)
        .envs(NO_SONDE)
        .envs(NO_AIRCRAFT_LINKS)
        .env(NO_MULTIMON.0, NO_MULTIMON.1)
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
fn modes_check_passes_the_in_crate_rows() {
    let dir = tempfile::tempdir().unwrap();
    let (out, code) = stdout(&mut check(dir.path(), &[]));
    for id in [
        "acars_decode",
        "ais_decode",
        "aprs_decode",
        "pocsag_decode",
        "rds_decode",
    ] {
        assert!(out.lines().any(|l| l == format!("{id}: ok")), "{out}");
    }
    assert!(
        out.lines()
            .any(|l| l == "adsb_decode: skipped: dump1090 missing"),
        "{out}"
    );
    // FLEX's program is pinned missing here.
    assert!(
        out.lines()
            .any(|l| l == "flex_decode: skipped: multimon-ng missing"),
        "{out}"
    );
    // DFM has no test yet; the RS41 and M20 decoders are pinned missing here.
    assert!(
        out.lines().any(|l| l == "dfm_decode: skipped: no test"),
        "{out}"
    );
    for (id, program) in [("rs41_decode", "rs41mod"), ("m20_decode", "m20mod")] {
        assert!(
            out.lines()
                .any(|l| l == format!("{id}: skipped: {program} missing")),
            "{out}"
        );
    }
    for (id, program) in [("vdl2_decode", "dumpvdl2"), ("hfdl_decode", "dumphfdl")] {
        assert!(
            out.lines()
                .any(|l| l == format!("{id}: skipped: {program} missing")),
            "{out}"
        );
    }
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
        err.contains(
            "unknown mode nope (known: acars_decode, adsb_decode, ais_decode, aprs_decode, \
             dfm_decode, flex_decode, hfdl_decode, m20_decode, pocsag_decode, rds_decode, rs41_decode, \
             vdl2_decode"
        ),
        "{err}"
    );
}

/// An operator row: `aprs_decode` under `id` with `test`, in `<dir>/modes`.
fn write_row(dir: &Path, id: &str, test: serde_json::Value) {
    let mut row: serde_json::Value =
        serde_json::from_str(include_str!("../../../modes/aprs_decode.json")).unwrap();
    row["test"] = test;
    write_whole_row(dir, id, row);
}

/// An operator row: `row` under `id`, in `<dir>/modes/<id>.json`.
fn write_whole_row(dir: &Path, id: &str, mut row: serde_json::Value) {
    row["id"] = id.into();
    write_row_json(dir, &row);
}

/// `row` as an operator row file `<dir>/modes/<id>.json`.
fn write_row_json(dir: &Path, row: &serde_json::Value) {
    let id = row["id"].as_str().expect("a row id");
    let modes = dir.join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    std::fs::write(
        modes.join(format!("{id}.json")),
        serde_json::to_string_pretty(row).unwrap(),
    )
    .unwrap();
}

/// A frame row is checked over the `framing` synthesiser family: its chain cuts the
/// synthesised frames and the first event is a CRC-clean frame.
#[test]
fn a_frame_row_is_checked_over_the_framing_family() {
    let dir = tempfile::tempdir().unwrap();
    let framing = serde_json::json!({"sync": {"hex": "2dd4", "bits": 16},
        "length": {"fixed_bits": 272}, "crc": "crc16-ccitt"});
    let row = serde_json::json!({
        "id": "fsk_test_frames",
        "name": "FSK test frames",
        "family": "general",
        "band": {"bandwidth_hz": 12500, "centers_hz": [433920000]},
        "modulation": ["fsk", "fm"],
        "label": "FSK frames",
        "protocol": "frame",
        "decoder": {
            "kind": "frame",
            "demod": {"modulation": "fsk", "symbol_rate_baud": 4800, "order": 2, "tone_spacing_hz": null},
            "framing": framing,
            "parser": "opaque"
        },
        "event": {
            "schema": {"type": "object", "properties": {
                "protocol": {"type": "string"},
                "sync": {"type": "string"},
                "length_bytes": {"type": "integer"},
                "bytes": {"type": "string"},
                "crc_ok": {"type": "boolean"},
                "payload": {"type": "string"}
            }},
            "summary": "frame {length_bytes} B"
        },
        "identify": {"proof": "field", "field": "crc_ok", "min_events": 1, "dwell_ms": 10000,
            "min_span_ms": 1000, "quick": "frame"},
        "catalogue": [],
        "test": {
            "kind": "synth", "family": "framing", "modulation": "fsk", "symbol_rate_baud": 4800,
            "deviation_hz": 2400.0, "frames": 8, "body_bytes": 32, "framing": framing,
            "repeat": 1, "snr_db": 20, "expect": {"crc_ok": true}, "within_ms": 10000
        }
    });
    write_row_json(dir.path(), &row);
    let (out, code) = stdout(&mut check(dir.path(), &["fsk_test_frames"]));
    assert_eq!(out.trim(), "fsk_test_frames: ok", "{out}");
    assert_eq!(code, 0);
}

/// An operator frame row naming a parser is checked over the `framing` family: the
/// parser's own check is what lets an event through, so the first event is its fields.
#[test]
fn an_operator_parser_row_is_checked_over_the_framing_family() {
    let dir = tempfile::tempdir().unwrap();
    let framing = serde_json::json!({"sync": {"hex": "0564", "bits": 16},
        "length": {"to_next_sync": true}, "crc": null});
    let row = serde_json::json!({
        "id": "dnp3_link",
        "name": "DNP3 link",
        "family": "iot",
        "band": {"bandwidth_hz": 12500, "centers_hz": [433920000]},
        "modulation": ["fsk", "fm"],
        "label": "DNP3",
        "protocol": "dnp3",
        "decoder": {
            "kind": "frame",
            "demod": {"modulation": "fsk", "symbol_rate_baud": 9600, "order": 2, "tone_spacing_hz": null},
            "framing": framing,
            "parser": "dnp3"
        },
        "event": {
            "schema": {"type": "object", "properties": {
                "protocol": {"type": "string"},
                "destination": {"type": "integer"},
                "source": {"type": "integer"}
            }},
            "summary": "dnp3 {source} -> {destination}"
        },
        "identify": {"proof": "event", "min_events": 1, "dwell_ms": 10000,
            "min_span_ms": 1000, "quick": "frame", "priority": 0},
        "catalogue": [],
        "test": {
            "kind": "synth", "family": "framing", "modulation": "fsk", "symbol_rate_baud": 9600,
            "deviation_hz": 2400.0, "frames": 6, "body_bytes": 16,
            "bodies": vec!["0bc4010000008c85c0c1013c020652c3"; 6], "framing": framing,
            "repeat": 1, "snr_db": 20, "expect": {"destination": 1, "source": 0},
            "within_ms": 10000
        }
    });
    write_row_json(dir.path(), &row);
    let (out, code) = stdout(&mut check(dir.path(), &["--strict", "dnp3_link"]));
    assert_eq!(out.trim(), "dnp3_link: ok", "{out}");
    assert_eq!(code, 0);
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

/// A fixture row (`aprs_fixture`) whose test path is `path`.
fn write_fixture_row(dir: &Path, path: &str) {
    write_row(
        dir,
        "aprs_fixture",
        serde_json::json!({
            "kind": "fixture",
            "path": path,
            "expect": {"src": "VK3ABC", "fcs_ok": true},
            "within_ms": 5000
        }),
    );
}

#[test]
fn a_relative_fixture_is_found_under_zeta_fixtures_dir() {
    let dir = tempfile::tempdir().unwrap();
    let fx = dir.path().join("fx");
    std::fs::create_dir_all(&fx).unwrap();
    let iq = aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 0.0);
    write_fixture(&fx, "aprs", &iq, 48_000, 144_800_000);
    write_fixture_row(dir.path(), "aprs.sigmf-meta");
    let (out, code) = stdout(check(dir.path(), &["aprs_fixture"]).env("ZETA_FIXTURES_DIR", &fx));
    assert_eq!(out.trim(), "aprs_fixture: ok", "{out}");
    assert_eq!(code, 0);
}

#[test]
fn a_relative_fixture_defaults_to_modes_under_the_current_directory() {
    let dir = tempfile::tempdir().unwrap();
    let modes = dir.path().join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    let iq = aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 0.0);
    write_fixture(&modes, "aprs", &iq, 48_000, 144_800_000);
    write_fixture_row(dir.path(), "aprs.sigmf-meta");
    let (out, code) = stdout(check(dir.path(), &["aprs_fixture"]).current_dir(dir.path()));
    assert_eq!(out.trim(), "aprs_fixture: ok", "{out}");
    assert_eq!(code, 0);
}

#[test]
fn a_missing_fixture_is_skipped_naming_the_lookup_and_strict_fails_it() {
    let dir = tempfile::tempdir().unwrap();
    // The cwd is canonical so the message's `<cwd>/modes` matches what the process sees.
    let cwd = dir.path().canonicalize().unwrap();
    write_fixture_row(&cwd, "fixtures/none.sigmf-meta");
    let why = format!(
        "fixture fixtures/none.sigmf-meta not found (ZETA_FIXTURES_DIR unset; looked in {}/modes)",
        cwd.display()
    );
    let (out, code) = stdout(
        check(&cwd, &["aprs_fixture"])
            .env_remove("ZETA_FIXTURES_DIR")
            .current_dir(&cwd),
    );
    assert_eq!(out.trim(), format!("aprs_fixture: skipped: {why}"), "{out}");
    assert_eq!(code, 0);
    let (out, code) = stdout(
        check(&cwd, &["--strict", "aprs_fixture"])
            .env_remove("ZETA_FIXTURES_DIR")
            .current_dir(&cwd),
    );
    assert_eq!(out.trim(), format!("aprs_fixture: fail: {why}"), "{out}");
    assert_eq!(code, 1);
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

/// The M20 row proves itself against the real `m20mod`: the synthesised frames pass
/// through the row's own chain and the decoder reports the serial and the position.
#[test]
fn m20_decode_passes_its_synthesised_check() {
    let Some(m20mod) = which("m20mod") else {
        eprintln!("m20mod not on this host");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let (out, code) =
        stdout(check(dir.path(), &["--strict", "m20_decode"]).env("ZETA_HANDLER_M20MOD", m20mod));
    assert_eq!(out.trim(), "m20_decode: ok", "{out}");
    assert_eq!(code, 0);
}

/// The FLEX row proves itself against the real multimon-ng: the synthesised frames pass
/// through the row's own chain and the decoder reports the capcode and the text.
#[test]
fn flex_decode_passes_its_synthesised_check() {
    let Some(multimon) = which("multimon-ng") else {
        eprintln!("multimon-ng not on this host");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let (out, code) =
        stdout(check(dir.path(), &["--strict", "flex_decode"]).env(NO_MULTIMON.0, multimon));
    assert_eq!(out.trim(), "flex_decode: ok", "{out}");
    assert_eq!(code, 0);
}

/// zeta-ext's stand-in for an external decoder: prints a file's lines whatever its input.
fn fake_lines() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../zeta-ext/tests/fake_lines")
}

/// One of zeta-ext's synthetic decoder output lines (`crates/zeta-ext/tests/data`).
fn ext_vector(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../zeta-ext/tests/data")
        .join(name)
}

/// The embedded row `text` as an operator row `id` run by `fake_lines` printing `vector`,
/// checked on an fsk burst (the fake ignores its input) for `expect`: the row's own
/// `decoder.output.fields` map turns the decoder's line into the event.
fn fake_program_row(dir: &Path, text: &str, id: &str, vector: &str, expect: serde_json::Value) {
    let mut row: serde_json::Value = serde_json::from_str(text).unwrap();
    row["decoder"]["programs"] = serde_json::json!([fake_lines().display().to_string()]);
    row["decoder"]["args"] =
        serde_json::json!(["--lines-file", ext_vector(vector).display().to_string()]);
    row["test"] = serde_json::json!({
        "kind": "synth", "family": "fsk", "symbol_rate_baud": 1200, "deviation_hz": 4500,
        "order": 2, "bits": "pattern:aa", "repeat": 1,
        "expect": expect, "within_ms": 10000
    });
    write_whole_row(dir, id, row);
}

/// The VDL2 row's pointers turn a dumpvdl2 ADS-C line into the event, the registration's
/// leading dot trimmed as `acars_decode` spells it.
#[test]
fn the_vdl2_row_decodes_through_the_fake_program() {
    let dir = tempfile::tempdir().unwrap();
    fake_program_row(
        dir.path(),
        include_str!("../../../modes/vdl2_decode.json"),
        "vdl2_fake",
        "dumpvdl2-adsc.json",
        serde_json::json!({"icao": "7C0000", "reg": "VH-ZET"}),
    );
    let (out, code) = stdout(&mut check(dir.path(), &["vdl2_fake"]));
    assert_eq!(out.trim(), "vdl2_fake: ok", "{out}");
    assert_eq!(code, 0);
}

/// The HFDL row keys an uplink on the aircraft it is for (`dst`).
#[test]
fn the_hfdl_row_decodes_an_uplink_through_the_fake_program() {
    let dir = tempfile::tempdir().unwrap();
    fake_program_row(
        dir.path(),
        include_str!("../../../modes/hfdl_decode.json"),
        "hfdl_fake",
        "dumphfdl-uplink.json",
        serde_json::json!({"icao": "7C0000"}),
    );
    let (out, code) = stdout(&mut check(dir.path(), &["hfdl_fake"]));
    assert_eq!(out.trim(), "hfdl_fake: ok", "{out}");
    assert_eq!(code, 0);
}

/// The embedded `pocsag_decode` row (its test block included) as an operator row
/// `pocsag_voc` naming one handler, `program`.
fn write_pocsag_with_handler(dir: &Path, program: &str) {
    let mut row: serde_json::Value =
        serde_json::from_str(include_str!("../../../modes/pocsag_decode.json")).unwrap();
    row["decoder"]["handlers"] = serde_json::json!([{
        "programs": [program],
        "install": {"debian": "nothing", "url": "https://example.invalid"}
    }]);
    write_whole_row(dir, "pocsag_voc", row);
}

/// A graph row whose handler is missing is skipped naming it, and `--strict` fails it.
#[test]
fn a_graph_row_with_a_missing_handler_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    write_pocsag_with_handler(dir.path(), "zeta-no-such-handler");
    let (out, code) = stdout(&mut check(dir.path(), &["pocsag_voc"]));
    assert_eq!(
        out.trim(),
        "pocsag_voc: skipped: zeta-no-such-handler missing",
        "{out}"
    );
    assert_eq!(code, 0);
    let (out, code) = stdout(&mut check(dir.path(), &["--strict", "pocsag_voc"]));
    assert_eq!(
        out.trim(),
        "pocsag_voc: fail: zeta-no-such-handler missing",
        "{out}"
    );
    assert_eq!(code, 1);
}

/// A graph row whose handler is found runs its check, and a pass is recorded in
/// `modes-checked.json` as a program row's is (a plain graph row's is not).
#[test]
fn a_graph_row_with_its_handler_found_records_its_pass() {
    let dir = tempfile::tempdir().unwrap();
    write_pocsag_with_handler(dir.path(), &fake_lines().display().to_string());
    let (out, code) = stdout(&mut check(dir.path(), &["pocsag_voc"]));
    assert_eq!(out.trim(), "pocsag_voc: ok", "{out}");
    assert_eq!(code, 0);
    let (out, code) = stdout(&mut check(dir.path(), &["pocsag_decode"]));
    assert_eq!(out.trim(), "pocsag_decode: ok", "{out}");
    assert_eq!(code, 0);
    let text = std::fs::read_to_string(dir.path().join("state/modes-checked.json")).unwrap();
    let checked: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(checked["pocsag_voc"].is_u64(), "{text}");
    assert!(checked.get("pocsag_decode").is_none(), "{text}");
}
