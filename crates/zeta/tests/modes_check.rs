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
/// The pager decoder's program, pinned to a path none lives at, whatever the host has.
const NO_MULTIMON: (&str, &str) = (
    "ZETA_HANDLER_MULTIMON_NG",
    "/nonexistent/zeta-test/multimon-ng",
);
/// The M17 row's vocoder, pinned to a path none lives at, whatever the host has.
const NO_C2DEC: (&str, &str) = ("ZETA_HANDLER_C2DEC", "/nonexistent/zeta-test/c2dec");
/// The P25 row's vocoder, pinned to a path none lives at: its check runs without it.
const NO_ZETA_MBE: (&str, &str) = ("ZETA_HANDLER_ZETA_MBE", "/nonexistent/zeta-test/zeta-mbe");
/// OnAir's wrapper, pinned to a path none lives at: a test that runs it sets it again.
const NO_ONAIR_RX: (&str, &str) = ("ZETA_HANDLER_ONAIR_RX", "/nonexistent/zeta-test/onair-rx");
/// The aircraft data-link rows' programs, pinned to a path none lives at, whatever the
/// host has.
const NO_AIRCRAFT_LINKS: [(&str, &str); 2] = [
    ("ZETA_HANDLER_DUMPVDL2", "/nonexistent/zeta-test/dumpvdl2"),
    ("ZETA_HANDLER_DUMPHFDL", "/nonexistent/zeta-test/dumphfdl"),
];

/// `zeta --state-dir <dir>/state --modes-dir <dir>/modes modes check <args>` with no
/// remote node, no operator rows but the test's own, and dump1090, the sonde decoders,
/// dumpvdl2, dumphfdl, multimon-ng, c2dec, zeta-mbe and onair-rx missing.
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
        .env(NO_C2DEC.0, NO_C2DEC.1)
        .env(NO_ZETA_MBE.0, NO_ZETA_MBE.1)
        .env(NO_ONAIR_RX.0, NO_ONAIR_RX.1)
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
        // Their vocoder is optional and pinned missing here: the calls are decoded silent.
        "dmr_decode",
        "m17_decode",
        "p25_decode",
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

/// A demodulator row has no capture to decode: it is checked by expanding it at a
/// synthetic tuning, so it passes even `--strict`.
#[test]
fn modes_check_checks_a_builtin_row_by_its_expansion() {
    let dir = tempfile::tempdir().unwrap();
    let (out, _) = stdout(&mut check(dir.path(), &["--strict"]));
    for id in ["fm_demod", "wfm_demod", "am_demod", "ssb_demod"] {
        assert!(out.lines().any(|l| l == format!("{id}: ok")), "{out}");
    }
    assert!(!out.contains("fm_demod: fail"), "{out}");
}

#[test]
fn modes_check_strict_fails_on_a_missing_handler() {
    let dir = tempfile::tempdir().unwrap();
    let (out, code) = stdout(&mut check(dir.path(), &["--strict", "adsb_decode"]));
    assert_eq!(out.trim(), "adsb_decode: fail: dump1090 missing", "{out}");
    assert_eq!(code, 1);
    // An optional handler missing passes without `--strict` and fails with it, as P25's.
    for (id, program) in [("m17_decode", "c2dec"), ("p25_decode", "zeta-mbe")] {
        let (out, code) = stdout(&mut check(dir.path(), &["--strict", id]));
        assert_eq!(
            out.trim(),
            format!("{id}: fail: {program} missing"),
            "{out}"
        );
        assert_eq!(code, 1);
    }
}

#[test]
fn an_unknown_id_lists_the_known_ones() {
    let dir = tempfile::tempdir().unwrap();
    let out = check(dir.path(), &["nope"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(
        err.contains(
            "unknown mode nope (known: acars_decode, adsb_decode, ais_decode, am_demod, \
             aprs_decode, ble_decode, dfm_decode, dmr_decode, dvbs_ip_decode, dvbt_decode, \
             flex_decode, fm_demod, gsmr_decode, hfdl_decode, lorawan_decode, lte_decode, \
             m17_decode, m20_decode, p25_decode, pocsag_decode, rds_decode, rs41_decode, \
             ssb_demod, tetra_decode, thread_decode, vdl2_decode"
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

/// The M17 row proves itself against the real `c2dec`: the synthesised stream passes
/// through the row's own chain and the stage reports the callsigns and the type.
#[test]
fn m17_decode_passes_its_synthesised_check() {
    let Some(c2dec) = which("c2dec") else {
        eprintln!("c2dec not on this host");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let (out, code) = stdout(check(dir.path(), &["--strict", "m17_decode"]).env(NO_C2DEC.0, c2dec));
    assert_eq!(out.trim(), "m17_decode: ok", "{out}");
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

/// The VDL2 row's `subject.station` on dumpvdl2 lines mapped by the row's own fields: a
/// ground station's frame is its own, keyed on its `icao`; an aircraft frame counts `to`
/// under the station it is addressed to (`dst`).
#[test]
fn the_vdl2_rows_station_counts_a_ground_stations_frame_and_an_aircraft_frame_to_it() {
    use zeta_core::modes::{FieldPath, Station};
    use zeta_core::node::{Grade, station_frame};
    let row: serde_json::Value =
        serde_json::from_str(include_str!("../../../modes/vdl2_decode.json")).unwrap();
    let fields: std::collections::BTreeMap<String, FieldPath> =
        serde_json::from_value(row["decoder"]["output"]["fields"].clone()).unwrap();
    let station: Station = serde_json::from_value(row["subject"]["station"].clone()).unwrap();
    // The XID fixture, an aircraft's frame, addressed to the synthetic station `1A2B3C`.
    let mut aircraft: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(ext_vector("dumpvdl2-xid.json")).unwrap())
            .unwrap();
    aircraft["vdl2"]["avlc"]["dst"]["addr"] = serde_json::json!("1A2B3C");
    let event = zeta_ext::apply_fields(&fields, &aircraft);
    assert_eq!(event["addr_type"], "Aircraft", "{event}");
    assert_eq!(
        station_frame(&station, &event),
        Some(("1A2B3C".to_string(), Grade::To)),
        "{event}"
    );
    // The same exchange the other way: the ground station's own frame to the aircraft.
    let mut ground = aircraft.clone();
    let avlc = &mut ground["vdl2"]["avlc"];
    avlc["src"] = serde_json::json!({"addr": "1A2B3C", "type": "Ground station"});
    avlc["dst"] = serde_json::json!({"addr": "7C0000", "type": "Aircraft"});
    let event = zeta_ext::apply_fields(&fields, &ground);
    assert_eq!(event["icao"], "1A2B3C", "{event}");
    assert_eq!(
        station_frame(&station, &event),
        Some(("1A2B3C".to_string(), Grade::From)),
        "{event}"
    );
}

/// The VDL2 row's derive reads an aircraft's H1 text position report when the frame
/// carries no ADS-C report.
#[test]
fn the_vdl2_row_derives_a_text_position_through_the_fake_program() {
    let dir = tempfile::tempdir().unwrap();
    fake_program_row(
        dir.path(),
        include_str!("../../../modes/vdl2_decode.json"),
        "vdl2_h1",
        "dumpvdl2-h1pos.json",
        serde_json::json!({
            "icao": "7C0000",
            "addr_type": "Aircraft",
            "lat": {"near": -27.385, "within": 0.001},
            "lon": {"near": 153.1183, "within": 0.001},
            "alt_ft": 16500
        }),
    );
    let (out, code) = stdout(&mut check(dir.path(), &["vdl2_h1"]));
    assert_eq!(out.trim(), "vdl2_h1: ok", "{out}");
    assert_eq!(code, 0);
}

/// The HFDL row maps an aircraft frame's source type, the value its derive gates on.
#[test]
fn the_hfdl_row_maps_an_aircraft_source_type() {
    let dir = tempfile::tempdir().unwrap();
    fake_program_row(
        dir.path(),
        include_str!("../../../modes/hfdl_decode.json"),
        "hfdl_down",
        "dumphfdl-downlink.json",
        serde_json::json!({"icao": "7C0000", "src_type": "Aircraft"}),
    );
    let (out, code) = stdout(&mut check(dir.path(), &["hfdl_down"]));
    assert_eq!(out.trim(), "hfdl_down: ok", "{out}");
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

/// `--handler` checks one handler of a mode: `adsb_decode`'s own dump1090 is missing here,
/// and the handler file `alt` (`fake_lines` printing the identity the row's test expects)
/// passes. The pass is recorded as `adsb_decode@alt`, apart from the row's own, which stays
/// unchecked, and `modes list` shows it on that handler only.
#[test]
fn modes_check_checks_a_named_handler_and_records_it_apart() {
    let dir = tempfile::tempdir().unwrap();
    let lines = dir.path().join("alt.lines");
    std::fs::write(&lines, r#"{"icao": "7C6B28"}"#).unwrap();
    let handlers = dir.path().join("handlers");
    std::fs::create_dir_all(&handlers).unwrap();
    let file = serde_json::json!({
        "id": "alt",
        "name": "Alternative",
        "programs": [fake_lines().display().to_string()],
        "install": {"url": "https://example.invalid/alt"},
        "modes": {
            "adsb_decode": {
                "args": ["--lines-file", lines.display().to_string()],
                "input": {"stream": "iq", "format": "cs16", "rate_hz": 2400000, "via": "stdin"},
                "output": {"via": "stdout", "port": null, "parser": "json_lines"}
            }
        }
    });
    std::fs::write(handlers.join("alt.json"), file.to_string()).unwrap();
    let (out, code) = stdout(
        check(dir.path(), &["adsb_decode", "--handler", "alt"])
            .arg("--handlers-dir")
            .arg(&handlers),
    );
    assert_eq!(out.trim(), "adsb_decode@alt: ok", "{out}");
    assert_eq!(code, 0);
    let text = std::fs::read_to_string(dir.path().join("state/modes-checked.json")).unwrap();
    let checked: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(checked["adsb_decode@alt"].is_u64(), "{text}");
    assert!(checked.get("adsb_decode").is_none(), "{text}");
    // `modes list` reads it back onto the handler, the row's own still unchecked.
    let out = Command::cargo_bin("zeta")
        .unwrap()
        .env_remove("ZETA_NODE")
        .env("XDG_CONFIG_HOME", common::no_xdg_config())
        .env("ZETA_HANDLER_DUMP1090", NO_DUMP1090)
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .arg("--handlers-dir")
        .arg(&handlers)
        .args(["modes", "list"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let adsb = v["value"]["modes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "adsb_decode")
        .unwrap()
        .clone();
    assert!(adsb["checked"].is_null(), "{adsb}");
    assert!(adsb["handlers"][0]["checked"].is_null(), "{adsb}");
    assert_eq!(adsb["handlers"][1]["id"], "alt", "{adsb}");
    assert!(adsb["handlers"][1]["checked"].is_u64(), "{adsb}");
    // A handler the mode does not have is refused, naming the ones it has.
    let out = check(dir.path(), &["adsb_decode", "--handler", "nope"])
        .arg("--handlers-dir")
        .arg(&handlers)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("mode adsb_decode has no handler nope (its handlers: dump1090, alt, onair)"),
        "{err}"
    );
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

/// A row whose only missing handler is optional runs its check without it and passes,
/// with no `checked` recorded (not every handler was found); `--strict` fails it naming
/// the program.
#[test]
fn an_optional_handler_missing_passes_and_strict_fails() {
    let dir = tempfile::tempdir().unwrap();
    let mut row: serde_json::Value =
        serde_json::from_str(include_str!("../../../modes/m17_decode.json")).unwrap();
    row["decoder"]["handlers"] = serde_json::json!([{
        "programs": ["zeta-no-such-handler"],
        "optional": true,
        "install": {"url": "https://example.invalid"}
    }]);
    write_whole_row(dir.path(), "m17_voc", row);
    let (out, code) = stdout(&mut check(dir.path(), &["m17_voc"]));
    assert_eq!(out.trim(), "m17_voc: ok", "{out}");
    assert_eq!(code, 0);
    assert!(
        !dir.path().join("state/modes-checked.json").exists(),
        "a pass without every handler is not recorded"
    );
    let (out, code) = stdout(&mut check(dir.path(), &["--strict", "m17_voc"]));
    assert_eq!(
        out.trim(),
        "m17_voc: fail: zeta-no-such-handler missing",
        "{out}"
    );
    assert_ne!(code, 0);
}

#[test]
fn video_decode_check_passes_on_the_synth() {
    let dir = tempfile::tempdir().unwrap();
    let (out, _) = stdout(&mut check(dir.path(), &["video_decode"]));
    assert!(
        out.lines().any(|l| l.starts_with("video_decode: ok")),
        "{out}"
    );
}

/// `--run-dir` reaches a program row's run in a one-shot command: `modes check` on a fifo
/// row, whose program records the fifo it is given before it runs `fake_lines`. The fifo
/// was in a subdirectory of the run's own under the given directory, removed after.
#[test]
fn a_fifo_rows_run_makes_its_subdirectory_under_the_run_dir() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let run = dir.path().join("elsewhere").join("run");
    let record = dir.path().join("fifo-seen");
    let lines = dir.path().join("lines");
    std::fs::write(&lines, r#"{"text": "fifo ok", "n": 1}"#).unwrap();
    // The argument after `--in` is the fifo; the one after `--record`, where to note it.
    let program = dir.path().join("record_fifo");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$2\" > \"$4\"\nexec {} \"$@\"\n",
            fake_lines().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let row = serde_json::json!({
        "id": "fifo_rec",
        "name": "fifo_rec test row",
        "family": "general",
        "band": {"bandwidth_hz": 12500, "centers_hz": [], "min_hz": null},
        "modulation": [],
        "label": "FIFO_REC",
        "protocol": "fifo_rec",
        "decoder": {
            "kind": "program",
            "programs": [program.display().to_string()],
            "args": ["--in", "$fifo", "--record", record.display().to_string(),
                     "--lines-file", lines.display().to_string()],
            "input": {"stream": "iq", "format": "cs8", "rate_hz": 48000, "via": "fifo"},
            "output": {"via": "stdout", "port": null, "parser": "json_lines"},
            "install": {"debian": "fake-dec", "url": "https://example.org/fake-dec"}
        },
        "event": {
            "schema": {"type": "object", "properties": {
                "text": {"type": "string"}, "n": {"type": "integer"}}},
            "summary": "{text}"
        },
        "identify": {"proof": "event", "min_events": 1, "dwell_ms": 1000, "min_span_ms": 1000,
                     "quick": null},
        "catalogue": [],
        "test": {
            "kind": "synth", "family": "fsk", "symbol_rate_baud": 1200, "deviation_hz": 4500,
            "order": 2, "bits": "pattern:aa", "repeat": 1,
            "expect": {"text": "fifo ok"}, "within_ms": 10000
        }
    });
    write_row_json(dir.path(), &row);
    let (out, code) = stdout(check(dir.path(), &["fifo_rec"]).arg("--run-dir").arg(&run));
    assert_eq!(out.trim(), "fifo_rec: ok", "{out}");
    assert_eq!(code, 0);
    let fifo = std::path::PathBuf::from(std::fs::read_to_string(&record).unwrap().trim());
    let sub = fifo.parent().expect("the fifo's directory");
    assert_eq!(
        sub.parent(),
        Some(run.as_path()),
        "{} under {}",
        fifo.display(),
        run.display()
    );
    let name = sub.file_name().unwrap().to_string_lossy();
    assert!(name.starts_with("external-"), "{}", sub.display());
    assert!(!sub.exists(), "{} removed after the run", sub.display());
    assert!(
        !dir.path().join("state").join("run").exists(),
        "nothing under the default run directory"
    );
}

/// `zeta modes check` shows the version OnAir's file asks of `onair-rx`: the probe's
/// line names the version read, and a program older than the file's least version is
/// skipped (failed with `--strict`) with the probe's reason. A stand-in `onair-rx`
/// answers `--version`.
#[test]
fn modes_check_shows_the_version_a_handler_reported() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("onair-rx");
    let answer = |version: &str| {
        let tmp = dir.path().join(".onair-rx.new");
        std::fs::write(&tmp, format!("#!/bin/sh\necho 'onair-rx {version}'\n")).unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&tmp, &program).unwrap();
    };
    let run = |strict: bool| {
        let mut args = vec!["dfm_decode", "--handler", "onair"];
        if strict {
            args.push("--strict");
        }
        let out = check(dir.path(), &args)
            .env("ZETA_HANDLER_ONAIR_RX", &program)
            .output()
            .unwrap();
        (
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
            out.status.code().unwrap(),
        )
    };
    answer("0.2.0");
    let why = "onair-rx 0.2.0 is older than 0.4.0 (https://github.com/ciphernaut/onair-rx)";
    let (out, err, code) = run(false);
    assert_eq!(
        out.trim(),
        format!("dfm_decode@onair: skipped: {why}"),
        "{err}"
    );
    assert_eq!(code, 0);
    assert!(
        err.lines()
            .any(|l| l.starts_with(&format!("zeta: handlers: {why} — modes: "))),
        "{err}"
    );
    let (out, _, code) = run(true);
    assert_eq!(out.trim(), format!("dfm_decode@onair: fail: {why}"));
    assert_eq!(code, 1);

    answer("0.4.0");
    let (out, err, code) = run(false);
    assert_eq!(out.trim(), "dfm_decode@onair: skipped: no test", "{err}");
    assert_eq!(code, 0);
    let found = format!(
        "zeta: handlers: onair-rx: found {} (handler onair, version 0.4.0)",
        program.display()
    );
    assert!(err.lines().any(|l| l == found), "{err}");
}

/// The version probe runs its program with the cleared environment every external
/// program gets: the node's credentials never reach it, and `ZETA_PLUGIN_ENV_<NAME>`
/// does, as `<NAME>`. The stand-in `onair-rx` prints what it was given where OnAir's
/// pattern reads the version.
#[test]
fn the_version_probe_never_sees_the_nodes_secrets() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("onair-rx");
    std::fs::write(
        &program,
        "#!/bin/sh\necho \"onair-rx ${ZETA_SECRET}${ZETA_LLM_KEY}${ONAIR_VERSION}\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let run = |pass: Option<&str>| {
        let mut cmd = check(dir.path(), &["dfm_decode", "--handler", "onair"]);
        cmd.env("ZETA_HANDLER_ONAIR_RX", &program)
            .env("ZETA_SECRET", "9.9.9")
            .env("ZETA_LLM_KEY", "8.8.8");
        if let Some(v) = pass {
            cmd.env("ZETA_PLUGIN_ENV_ONAIR_VERSION", v);
        }
        let out = cmd.output().unwrap();
        (
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    };
    let (out, err) = run(None);
    let why = "onair-rx: its version could not be read (nothing it printed matched \
               ^onair-rx (\\d+(?:\\.\\d+)*))";
    assert_eq!(
        out.trim(),
        format!("dfm_decode@onair: skipped: {why}"),
        "{err}"
    );
    assert!(!err.contains("9.9.9") && !err.contains("8.8.8"), "{err}");
    let (out, err) = run(Some("0.4.1"));
    assert_eq!(out.trim(), "dfm_decode@onair: skipped: no test", "{err}");
    let found = format!(
        "zeta: handlers: onair-rx: found {} (handler onair, version 0.4.1)",
        program.display()
    );
    assert!(err.lines().any(|l| l == found), "{err}");
}

/// OnAir's entries for the bus modes and the polled ones that have a check pass
/// `zeta modes check <id> --handler onair`, each recorded as `<id>@onair`, and analog
/// video, which OnAir does not serve, is refused. It needs `onair-rx`: the test-only
/// `ZETA_TEST_ONAIR_RX` names it (an executable file, not looked up on `PATH`), passed
/// to the node as `ZETA_HANDLER_ONAIR_RX`; unset, the test says so and passes. The node's own `ZETA_HANDLER_ONAIR_RX` in the caller's
/// environment is never read: [`check`] pins it.
#[test]
fn onair_passes_its_modes_checks() {
    use std::os::unix::fs::PermissionsExt;
    // Unset: no onair-rx to test. Set: it must name an executable file.
    let Some(program) = std::env::var_os("ZETA_TEST_ONAIR_RX").map(std::path::PathBuf::from) else {
        eprintln!("onair check skipped: ZETA_TEST_ONAIR_RX is not set");
        return;
    };
    let executable = program
        .metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    assert!(
        executable,
        "ZETA_TEST_ONAIR_RX is set to {}, which is not an executable file",
        program.display()
    );
    let dir = tempfile::tempdir().unwrap();
    for id in [
        "pocsag_decode",
        "flex_decode",
        "acars_decode",
        "aprs_decode",
        "ais_decode",
        "vdl2_decode",
        "rs41_decode",
        "m20_decode",
        "rds_decode",
        "dmr_decode",
        "p25_decode",
        "m17_decode",
    ] {
        let (out, code) = stdout(
            check(dir.path(), &[id, "--handler", "onair", "--strict"])
                .env("ZETA_HANDLER_ONAIR_RX", &program)
                // The VDL2 row's recorded fixture lives in the repository's `modes`.
                .env(
                    "ZETA_FIXTURES_DIR",
                    concat!(env!("CARGO_MANIFEST_DIR"), "/../../modes"),
                ),
        );
        assert_eq!(out.trim(), format!("{id}@onair: ok"), "{out}");
        println!("{}", out.trim());
        assert_eq!(code, 0, "{id}");
    }
    // The modes only OnAir decodes (plan 47) are checked on pairs `onair-gen` makes into
    // a directory of their own (the script writes `onair/<mode>.sigmf-*`), not the
    // repository's fixtures.
    let ids = [
        "ble_decode",
        "lorawan_decode",
        "zigbee_decode",
        "thread_decode",
        "zwave_decode",
        "wifi_decode",
        "wisun_decode",
        "tetra_decode",
        "gsmr_decode",
        "lte_decode",
        "dvbs_ip_decode",
    ];
    match std::env::var_os("ZETA_TEST_ONAIR_GEN").map(std::path::PathBuf::from) {
        None => {
            for id in ids {
                println!("{id}@onair: not run, ZETA_TEST_ONAIR_GEN is not set");
            }
        }
        Some(gen_bin) => {
            let fixtures = tempfile::tempdir().unwrap();
            let made = Command::new(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/onair-fixtures.sh"
            ))
            .arg(&gen_bin)
            .arg(fixtures.path())
            .output()
            .unwrap();
            assert!(
                made.status.success(),
                "onair-fixtures.sh: {}",
                String::from_utf8_lossy(&made.stderr)
            );
            for id in ids {
                let (out, code) = stdout(
                    check(dir.path(), &[id, "--handler", "onair", "--strict"])
                        .env("ZETA_HANDLER_ONAIR_RX", &program)
                        .env("ZETA_FIXTURES_DIR", fixtures.path()),
                );
                assert_eq!(out.trim(), format!("{id}@onair: ok"), "{out}");
                println!("{}", out.trim());
                assert_eq!(code, 0, "{id}");
            }
        }
    }
    // Not run: hfdl_decode's fixture is still pending (its entry is validated by the
    // registry test); dfm_decode's and dvbt_decode's rows have no test (DVB-T's entry is
    // pinned against the wrapper's own lines instead).
    println!("hfdl_decode@onair: not run, its fixture is pending");
    println!("dfm_decode@onair: not run, its row has no test");
    println!("dvbt_decode@onair: not run, its row has no test");
    // Analog video is not OnAir's (it missed the listing bar): the check refuses the handler.
    let out = check(
        dir.path(),
        &["video_decode", "--handler", "onair", "--strict"],
    )
    .env("ZETA_HANDLER_ONAIR_RX", &program)
    .output()
    .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains("mode video_decode has no handler onair"),
        "{err}"
    );
    println!("video_decode@onair: not listed");
}

// Plan 47: a row with no decoder of its own is checked through the handler the policy
// picks.

/// `adsb_decode` as the operator row `nodec_decode` with no decoder of its own, checked
/// on a fixture of 100 ms of silence at 1090 MHz for the identity `7C6B28`.
fn write_nodec_row(dir: &Path) {
    let iq = vec![num_complex::Complex::new(0.0f32, 0.0); 240_000];
    let meta = write_fixture(dir, "nodec", &iq, 2_400_000, 1_090_000_000);
    let mut row: serde_json::Value =
        serde_json::from_str(include_str!("../../../modes/adsb_decode.json")).unwrap();
    row["decoder"] = serde_json::Value::Null;
    row["test"] = serde_json::json!({
        "kind": "fixture", "path": meta.display().to_string(),
        "expect": {"icao": "7C6B28"}, "within_ms": 5000
    });
    write_whole_row(dir, "nodec_decode", row);
}

/// A handler file `id` serving `nodec_decode` by `fake_lines` printing the identity the
/// row's test expects; the handlers directory.
fn write_nodec_handler(dir: &Path, id: &str) -> std::path::PathBuf {
    let lines = dir.join(format!("{id}.lines"));
    std::fs::write(&lines, r#"{"icao": "7C6B28"}"#).unwrap();
    let handlers = dir.join("handlers");
    std::fs::create_dir_all(&handlers).unwrap();
    let file = serde_json::json!({
        "id": id,
        "name": id,
        "programs": [fake_lines().display().to_string()],
        "install": {"url": "https://example.invalid/alt"},
        "modes": {
            "nodec_decode": {
                "args": ["--lines-file", lines.display().to_string()],
                "input": {"stream": "iq", "format": "cs16", "rate_hz": 2400000, "via": "stdin"},
                "output": {"via": "stdout", "port": null, "parser": "json_lines"}
            }
        }
    });
    std::fs::write(handlers.join(format!("{id}.json")), file.to_string()).unwrap();
    handlers
}

/// The `modes-checked.json` the checks above wrote.
fn checked(dir: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(dir.join("state/modes-checked.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn a_row_with_no_decoder_is_checked_through_the_policys_pick() {
    let dir = tempfile::tempdir().unwrap();
    write_nodec_row(dir.path());
    write_nodec_handler(dir.path(), "alt");
    let handlers = write_nodec_handler(dir.path(), "zed");
    let run = |args: &[&str]| stdout(check(dir.path(), args).arg("--handlers-dir").arg(&handlers));
    // No ranking: the first of the two by id.
    let (out, code) = run(&["nodec_decode"]);
    assert_eq!(out.trim(), "nodec_decode@alt: ok", "{out}");
    assert_eq!(code, 0);
    // Ranked, the other.
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(
        state.join("handler-policy.json"),
        r#"{"modes": {"nodec_decode": ["zed"]}, "all": []}"#,
    )
    .unwrap();
    let (out, code) = run(&["nodec_decode"]);
    assert_eq!(out.trim(), "nodec_decode@zed: ok", "{out}");
    assert_eq!(code, 0);
    let c = checked(dir.path());
    assert!(c["nodec_decode@alt"].is_u64(), "{c}");
    assert!(c["nodec_decode@zed"].is_u64(), "{c}");
    assert!(c.get("nodec_decode").is_none(), "{c}");
    // `--handler` names one, as for any row; the stand-in is none of its handlers.
    let (out, code) = run(&["nodec_decode", "--handler", "alt"]);
    assert_eq!(out.trim(), "nodec_decode@alt: ok", "{out}");
    assert_eq!(code, 0);
    let out = check(dir.path(), &["nodec_decode", "--handler", "none"])
        .arg("--handlers-dir")
        .arg(&handlers)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("mode nodec_decode has no handler none (its handlers: alt, zed)"),
        "{err}"
    );
}

#[test]
fn a_row_with_no_decoder_is_a_skip_with_no_handler() {
    let dir = tempfile::tempdir().unwrap();
    write_nodec_row(dir.path());
    let why = "no decoder of its own and no handler ready";
    let (out, code) = stdout(&mut check(dir.path(), &["nodec_decode"]));
    assert_eq!(out.trim(), format!("nodec_decode: skipped: {why}"), "{out}");
    assert_eq!(code, 0);
    let (out, code) = stdout(&mut check(dir.path(), &["--strict", "nodec_decode"]));
    assert_eq!(out.trim(), format!("nodec_decode: fail: {why}"), "{out}");
    assert_eq!(code, 1);
    // A handler whose program is missing is no ready handler either.
    let handlers = write_nodec_handler(dir.path(), "alt");
    let file = handlers.join("alt.json");
    let text = std::fs::read_to_string(&file).unwrap();
    let missing = text.replace(
        &fake_lines().display().to_string(),
        "/nonexistent/zeta-test/alt-rx",
    );
    std::fs::write(&file, missing).unwrap();
    let (out, code) = stdout(
        check(dir.path(), &["nodec_decode"])
            .arg("--handlers-dir")
            .arg(&handlers),
    );
    assert_eq!(out.trim(), format!("nodec_decode: skipped: {why}"), "{out}");
    assert_eq!(code, 0);
}
