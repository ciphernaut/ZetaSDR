//! The web app's end-to-end scenarios (`crates/zeta-ui/tests/e2e/ui.py`, Playwright with
//! headless Chromium) against a real `zeta serve` with the `tone`, `aprs`, `wfm` and `pocsag`
//! fixtures (`wfm`: a 1 kHz tone, wideband FM with pilot, 240 kHz, 2 s looping, at 104.9 MHz;
//! `pocsag`: one 1200-baud alphanumeric page `HELLO ZETA` to address 1234567, function 3, at
//! 48 kHz, looping, at 148.6375 MHz); the
//! node's state directory is passed to ui.py in `ZETA_UI_STATE_DIR`. That node has a fake
//! `voiced` (`--stt-socket`) answering `ok:what sources are there` to every `file:` line; a
//! second node without a speech daemon is passed in `ZETA_UI_URL_NOSTT`.
//!
//! The run is killed and the test fails after 300 s of wall clock. `ZETA_UI_ONLY=a,b` runs only
//! those scenarios.
//!
//! Skipped, with `ui e2e skipped: <reason>` on stdout, only when Playwright for `python3` or
//! its Chromium is missing. `python3` runs without `-I`: Playwright is commonly installed in
//! the user site-packages, which `-I` hides.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use common::ServeProc;

const SECRET: &str = "ui-secret-0123456789";
/// What the fake speech daemon "hears" in every recording.
const SPOKEN: &str = "what sources are there";
/// Unix socket paths are limited to 108 bytes.
const SOCKET_PATH_MAX: usize = 100;

/// The `wfm` fixture: its IQ rate and the rate of the tone it carries.
const WFM_RATE: u32 = 240_000;
const WFM_AUDIO_RATE: u32 = 48_000;

/// Peak |sample| a recording of the fake microphone's tone must exceed.
const MIN_PEAK: i32 = 1000;

/// What the real daemon is handed: `file:<absolute path>` naming a 16 kHz mono s16le PCM WAV
/// with the canonical 44-byte header. Also requires a non-silent recording.
fn check_staged_wav(line: &str) -> Result<(), String> {
    let line = line.trim_end_matches(['\n', '\r']);
    let path = line
        .strip_prefix("file:")
        .ok_or_else(|| format!("unexpected command {line:?}"))?;
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err(format!("staged path {} is not absolute", path.display()));
    }
    let wav = std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    if wav.len() < 44 || &wav[0..4] != b"RIFF" || &wav[8..16] != b"WAVEfmt " {
        return Err(format!("{} is not a RIFF/WAVE file", path.display()));
    }
    let u16_at = |at: usize| u16::from_le_bytes([wav[at], wav[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes(wav[at..at + 4].try_into().unwrap());
    let fmt = (u32_at(16), u16_at(20), u16_at(22), u32_at(24), u16_at(34));
    if fmt != (16, 1, 1, 16_000, 16) {
        return Err(format!(
            "fmt (chunk, format, channels, rate, bits) {fmt:?}, want (16, 1, 1, 16000, 16)"
        ));
    }
    if &wav[36..40] != b"data" || u32_at(40) as usize != wav.len() - 44 {
        return Err("data chunk missing or its length is wrong".to_string());
    }
    let peak = wav[44..]
        .chunks_exact(2)
        .map(|b| i32::from(i16::from_le_bytes([b[0], b[1]])).abs())
        .max()
        .unwrap_or(0);
    if peak <= MIN_PEAK {
        return Err(format!("recording is silent (peak {peak})"));
    }
    Ok(())
}

/// A fake `voiced` at `path`: answers `ok:<SPOKEN>` to a `file:` line naming a valid staged
/// WAV ([`check_staged_wav`]), `error:<reason>` otherwise. Lives as long as the test process.
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
            let answer = match check_staged_wav(&line) {
                Ok(()) => format!("ok:{SPOKEN}\n"),
                Err(reason) => format!("error:{reason}\n"),
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

/// Wall-clock limit on the whole ui.py run.
const DEADLINE: Duration = Duration::from_secs(300);

/// Waits for `child` until `deadline`, killing it then; its exit status (None when killed)
/// and everything it wrote. Its pipes are drained on threads so it never blocks on them.
fn wait_with_deadline(
    mut child: Child,
    deadline: Duration,
) -> (Option<ExitStatus>, String, String) {
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll ui.py") {
            break Some(status);
        }
        if started.elapsed() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    (
        status,
        out.join().unwrap_or_default(),
        err.join().unwrap_or_default(),
    )
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
    let audio = zeta_dsp::fixtures::tone(WFM_AUDIO_RATE, 2 * WFM_AUDIO_RATE as usize, 1_000.0, 0.9);
    let iq =
        zeta_dsp::fixtures::wfm_modulate(&audio, WFM_AUDIO_RATE, WFM_RATE, 75_000.0, 0.0, true);
    let meta = zeta_dsp::fixtures::write_fixture(dir.path(), "wfm", &iq, WFM_RATE, 104_900_000);
    let wfm = format!("wfm={}", meta.display());
    let iq = zeta_dsp::fixtures::pocsag_packet_iq(1_234_567, 3, "HELLO ZETA", 1200, 48_000, 0.0);
    let meta = zeta_dsp::fixtures::write_fixture(dir.path(), "pocsag", &iq, 48_000, 148_637_500);
    let pocsag = format!("pocsag={}", meta.display());
    let sock_dir = socket_dir();
    let sock = sock_dir.path().join("voiced.sock");
    fake_voiced(&sock);
    let sock = sock.to_str().expect("socket path is UTF-8");
    let serve = ServeProc::start_full(
        dir.path(),
        SECRET,
        &[
            "--source-file",
            &aprs,
            "--source-file",
            &wfm,
            "--source-file",
            &pocsag,
        ],
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
    let mut cmd = Command::new("python3");
    cmd.arg("crates/zeta-ui/tests/e2e/ui.py")
        .args([&serve.url, SECRET]);
    if let Some(only) = std::env::var_os("ZETA_UI_ONLY") {
        cmd.arg(only);
    }
    let child = cmd
        .env("ZETA_UI_STATE_DIR", dir.path().join("state"))
        .env("ZETA_UI_URL_NOSTT", &nostt.url)
        .current_dir(repo_root())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run ui.py");
    let (status, stdout, stderr) = wait_with_deadline(child, DEADLINE);
    println!("{stdout}");
    let Some(status) = status else {
        panic!(
            "ui.py still running after {} s; killed\n--- stdout\n{stdout}\n--- stderr\n{stderr}",
            DEADLINE.as_secs()
        );
    };
    assert!(
        status.success(),
        "ui.py failed ({status}):\n--- stdout\n{stdout}\n--- stderr\n{stderr}"
    );
    drop(serve);
    drop(nostt);
}
