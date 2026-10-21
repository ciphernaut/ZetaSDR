//! `examples/python/cb_watch.py` against a real serving process.

mod common;

use std::path::PathBuf;
use std::process::Command;

use common::{ServeProc, zeta_remote};
use serde_json::Value;

const SECRET: &str = "a-long-enough-secret";

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/python/cb_watch.py")
}

fn jobs(url: &str) -> Value {
    let out = zeta_remote(url, SECRET, &["job", "list"]).output().unwrap();
    serde_json::from_slice(&out.stdout).expect("job list is JSON")
}

#[test]
fn cb_watch_dry_run_lists_the_channels_in_the_capture() {
    let help = Command::new("python3")
        .arg("-I")
        .arg(script())
        .arg("--help")
        .output()
        .expect("run python3");
    assert!(help.status.success(), "--help: {help:?}");
    assert!(String::from_utf8_lossy(&help.stdout).contains("--open-db"));

    let dir = tempfile::tempdir().unwrap();
    // A 48 kHz capture centred on ch20: ch59 and ch60 lie inside it, nothing else does.
    let audio = zeta_dsp::fixtures::tone(48_000, 48_000, 1_000.0, 0.5);
    let iq = zeta_dsp::fixtures::nbfm_modulate(&audio, 48_000, 48_000, 5_000.0, 0.0);
    let meta = zeta_dsp::fixtures::write_fixture(dir.path(), "cb", &iq, 48_000, 476_900_000);
    let serve = ServeProc::start_with(
        dir.path(),
        SECRET,
        &["--source-file", &format!("cb={}", meta.display())],
    );
    let before = jobs(&serve.url);
    let out = Command::new("python3")
        .arg("-I")
        .arg(script())
        .args(["--node", &serve.url, "--secret-env", "CB_WATCH_TEST_SECRET"])
        .args(["--source", "cb", "--dry-run"])
        .env("CB_WATCH_TEST_SECRET", SECRET)
        .output()
        .expect("run python3");
    assert!(out.status.success(), "--dry-run: {out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    for ch in ["ch20 ", "ch59 ", "ch60 "] {
        assert!(text.contains(ch), "{ch} missing from: {text}");
    }
    assert_eq!(text.matches("\nch").count(), 3, "{text}");
    assert_eq!(jobs(&serve.url), before, "a dry run starts nothing");
}
