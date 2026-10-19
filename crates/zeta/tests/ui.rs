//! The web app's end-to-end scenarios (`crates/zeta-ui/tests/e2e/ui.py`, Playwright with
//! headless Chromium) against a real `zeta serve` with the `tone` and `aprs` fixtures; the
//! node's state directory is passed to ui.py in `ZETA_UI_STATE_DIR`. That node has a fake
//! `voiced` (`--stt-socket`) answering `ok:what sources are there` to every `file:` line; a
//! second node without a speech daemon is passed in `ZETA_UI_URL_NOSTT`.
//!
//! Skipped, with `ui e2e skipped: <reason>` on stdout, only when Playwright for `python3` or
//! its Chromium is missing. `python3` runs without `-I`: Playwright is commonly installed in
//! the user site-packages, which `-I` hides.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::ServeProc;

const SECRET: &str = "ui-secret-0123456789";
/// What the fake speech daemon "hears" in every recording.
const SPOKEN: &str = "what sources are there";
/// Unix socket paths are limited to 108 bytes.
const SOCKET_PATH_MAX: usize = 100;

/// A fake `voiced` at `path`: answers `ok:<SPOKEN>` to each `file:` line, `error:` otherwise.
/// The listener thread lives as long as the test process.
fn fake_voiced(path: &Path) {
    let listener = UnixListener::bind(path).expect("bind fake voiced socket");
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let Ok(reader) = conn.try_clone() else {
                continue;
            };
            let mut line = String::new();
            if BufReader::new(reader).read_line(&mut line).is_err() {
                continue;
            }
            let answer = if line.starts_with("file:") {
                format!("ok:{SPOKEN}\n")
            } else {
                format!("error:unexpected command {line:?}\n")
            };
            let _ = conn.write_all(answer.as_bytes());
        }
    });
}

/// A directory short enough to hold a Unix socket: the usual temp dir, else cargo's target tmp.
fn socket_dir() -> tempfile::TempDir {
    let fits = |d: &tempfile::TempDir| {
        d.path().as_os_str().len() + "/voiced.sock".len() <= SOCKET_PATH_MAX
    };
    let dir = tempfile::tempdir().unwrap();
    if fits(&dir) {
        return dir;
    }
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    assert!(
        fits(&dir),
        "no temp dir short enough for a Unix socket: {}",
        dir.path().display()
    );
    dir
}

/// Why the e2e run cannot happen on this machine, if it cannot.
fn skip_reason() -> Option<String> {
    let import = Command::new("python3")
        .args(["-c", "import playwright.sync_api"])
        .output();
    match import {
        Err(e) => return Some(format!("python3 not runnable: {e}")),
        Ok(out) if !out.status.success() => {
            return Some("python3 cannot import playwright.sync_api".to_string());
        }
        Ok(_) => {}
    }
    let Some(home) = std::env::var_os("HOME") else {
        return Some("HOME is not set".to_string());
    };
    let cache = Path::new(&home).join(".cache/ms-playwright");
    let has_chromium = std::fs::read_dir(&cache)
        .map(|dir| {
            dir.filter_map(Result::ok)
                .any(|e| e.file_name().to_string_lossy().starts_with("chromium"))
        })
        .unwrap_or(false);
    if !has_chromium {
        return Some(format!("no chromium* under {}", cache.display()));
    }
    None
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root above crates/zeta")
        .to_path_buf()
}

#[test]
fn ui_scenarios_pass_in_headless_chromium() {
    if let Some(reason) = skip_reason() {
        println!("ui e2e skipped: {reason}");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let iq = zeta_dsp::fixtures::aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 0.0);
    let meta = zeta_dsp::fixtures::write_fixture(dir.path(), "aprs", &iq, 48_000, 144_800_000);
    let aprs = format!("aprs={}", meta.display());
    let sock_dir = socket_dir();
    let sock = sock_dir.path().join("voiced.sock");
    fake_voiced(&sock);
    let sock = sock.to_str().expect("socket path is UTF-8");
    let serve = ServeProc::start_full(
        dir.path(),
        SECRET,
        &["--source-file", &aprs],
        &["--stt-socket", sock],
        &[],
    );
    // No speech daemon: XDG_RUNTIME_DIR points where no voiced.sock is, so no default applies.
    let nostt_dir = tempfile::tempdir().unwrap();
    let runtime = nostt_dir.path().to_str().expect("tempdir is UTF-8");
    let nostt = ServeProc::start_env(
        nostt_dir.path(),
        SECRET,
        &[],
        &[("XDG_RUNTIME_DIR", runtime)],
    );
    let out = Command::new("python3")
        .arg("crates/zeta-ui/tests/e2e/ui.py")
        .args([&serve.url, SECRET])
        .env("ZETA_UI_STATE_DIR", dir.path().join("state"))
        .env("ZETA_UI_URL_NOSTT", &nostt.url)
        .current_dir(repo_root())
        .output()
        .expect("run ui.py");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    println!("{stdout}");
    assert!(
        out.status.success(),
        "ui.py failed ({}):\n--- stdout\n{stdout}\n--- stderr\n{stderr}",
        out.status
    );
    drop(serve);
    drop(nostt);
}
