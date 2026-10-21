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
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(
        help.contains("--open-db") && help.contains("--squelch-margin-db"),
        "{help}"
    );

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

/// A child process killed when dropped, a failed assertion included.
struct Killed(std::process::Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn cb_watch_fixes_the_squelch_of_a_channel_it_opens() {
    let dir = tempfile::tempdir().unwrap();
    // ch20 carries a tone 20 dB over noise; the watcher opens it and fixes its squelch at
    // the measured floor + 6 dB (dBFS in the channel) before anything is attached.
    let audio = zeta_dsp::fixtures::tone(48_000, 48_000, 1_000.0, 0.5);
    let mut iq: Vec<_> = zeta_dsp::fixtures::nbfm_modulate(&audio, 48_000, 48_000, 5_000.0, 0.0)
        .into_iter()
        .map(|c| c * 0.1)
        .collect();
    // 40 dB under the -20 dBFS carrier: -60 dBFS noise.
    zeta_dsp::fixtures::awgn(&mut iq, 40.0, 7);
    let meta = zeta_dsp::fixtures::write_fixture(dir.path(), "cb", &iq, 48_000, 476_900_000);
    let serve = ServeProc::start_with(
        dir.path(),
        SECRET,
        &["--source-file", &format!("cb={}", meta.display())],
    );
    let _watcher = Killed(
        Command::new("python3")
            .arg("-I")
            .arg(script())
            .args(["--node", &serve.url, "--secret-env", "CB_WATCH_TEST_SECRET"])
            .args(["--source", "cb", "--idle-s", "600"])
            .env("CB_WATCH_TEST_SECRET", SECRET)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("run python3"),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let squelch_db = loop {
        let list = jobs(&serve.url);
        let demod = list
            .as_array()
            .or_else(|| list["value"].as_array())
            .expect("a job array")
            .iter()
            .find(|j| {
                j["kind"] == "fm_demod"
                    && j["tuning"]["center_hz"] == 476_900_000
                    && j["state"]["status"] == "running"
            })
            .map(|j| j["id"].as_str().unwrap().to_string());
        let fixed = demod.and_then(|id| {
            let out = zeta_remote(&serve.url, SECRET, &["graph", "get", "--job", &id])
                .output()
                .unwrap();
            let g: Value = serde_json::from_slice(&out.stdout).ok()?;
            let g = if g.get("value").is_some() {
                g["value"].clone()
            } else {
                g
            };
            g["nodes"]
                .as_array()?
                .iter()
                .find(|n| n["kind"] == "nbfm_demod")
                .and_then(|n| n["params"]["squelch_db"].as_i64())
        });
        if let Some(db) = fixed {
            break db;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no fm_demod on ch20 with a fixed squelch_db: {}",
            jobs(&serve.url)
        );
        std::thread::sleep(std::time::Duration::from_millis(250));
    };
    // -60 dBFS per-sample noise: about -65.8 dBFS in a 12 500 Hz channel of 48 kHz, + 6
    // (the row's 15th-percentile floor reads a few dB low with few FFTs per row).
    eprintln!("cb_watch fixed squelch_db {squelch_db}");
    assert!((-66..=-56).contains(&squelch_db), "squelch_db {squelch_db}");
}
