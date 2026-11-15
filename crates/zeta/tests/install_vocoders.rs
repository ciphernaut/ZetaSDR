//! `tools/install-vocoders.sh`: its options, the notice it stops at, and that it parses.
//! Nothing here builds a vocoder or downloads anything.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The script, from the repository root.
fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/install-vocoders.sh")
}

#[test]
fn help_exits_zero_and_names_the_options() {
    let out = Command::new("bash")
        .arg(script())
        .arg("--help")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "--help failed: {out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    for option in ["--yes", "--src", "--prefix"] {
        assert!(
            text.contains(option),
            "--help does not name {option}: {text}"
        );
    }
}

#[test]
fn without_yes_it_stops_before_downloading() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let marker = dir.path().join("tool-ran");
    // Fakes for every tool the script needs, each leaving the marker if it runs, so the
    // test needs none of them on the host and sees any that runs before a yes.
    for tool in ["git", "cmake", "cc"] {
        let fake = bin.join(tool);
        std::fs::write(&fake, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    // The fakes first, so the script finds them before any the host has.
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("bash")
        .arg(script())
        .arg("--prefix")
        .arg(dir.path().join("prefix"))
        .env("PATH", path)
        .env_remove("CC")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "the script went on without yes: {text}"
    );
    assert!(text.contains("patent"), "no patent notice: {text}");
    assert!(
        text.contains("GPL-2.0-or-later"),
        "no licence notice: {text}"
    );
    assert!(!marker.exists(), "a tool ran before a yes: {text}");
}

#[test]
fn the_script_passes_bash_n() {
    let out = Command::new("bash")
        .arg("-n")
        .arg(script())
        .output()
        .unwrap();
    assert!(out.status.success(), "bash -n failed: {out:?}");
}
