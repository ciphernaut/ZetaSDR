//! The web app's end-to-end scenarios (`crates/zeta-ui/tests/e2e/ui.py`, Playwright with
//! headless Chromium) against a real `zeta serve` with the `tone`, `aprs`, `wfm` and `pocsag`
//! fixtures (`wfm`: a 1 kHz tone, wideband FM with pilot, 240 kHz, 2 s looping, at 104.9 MHz;
//! `pocsag`: one 1200-baud alphanumeric page `HELLO ZETA` to address 1234567, function 3, at
//! 48 kHz, looping, at 148.6375 MHz) and `speech` fixtures (`speech`: a 2 s pause then 3 s of
//! speech-like bursts, wideband FM with pilot, 240 kHz, looping, at 104.7 MHz), `retune`
//! (the `tone` recording relabelled at 250 MHz and tunable over 200-300 MHz: a tune moves its
//! capture's centre) and `scanner` (the `tone` recording tunable over 50-150 MHz, the source
//! the page's automatic scans pick for a view around `tone`: `retune` sorts first in
//! `source_list` but its range does not contain 100 MHz) and `cb` (the `tone` recording
//! relabelled at 476.9 MHz, channel 20 of the CB plan), `quiet` (1 s of low complex noise at
//! 20 MHz, which `measure` labels `noise`; see [`quiet_fixture`]), and `--modes-dir` holding the
//! `fake_pos` program row (zeta-ext's `fake_lines`, printing two fixes of one sonde 2 s into a
//! job; see [`fake_pos_row`]); the node's state directory is passed to ui.py in
//! `ZETA_UI_STATE_DIR`. That node has a fake
//! `voiced` (`--stt-socket`, zeta-speech's `FakeDaemon`) answering `ok:what sources are there`
//! to every valid staged WAV; a second node without a speech daemon, with only the `speech`
//! fixture, is passed in `ZETA_UI_URL_NOSTT`.
//!
//! The run is killed and the test fails after 300 s of wall clock. `ZETA_UI_ONLY=a,b` runs only
//! those scenarios.
//!
//! Skipped, with `ui e2e skipped: <reason>` on stdout, only when Playwright for `python3` or
//! its Chromium is missing. `python3` runs without `-I`: Playwright is commonly installed in
//! the user site-packages, which `-I` hides.

mod common;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use common::{ServeProc, speech_fixture};
use zeta_speech::testing::FakeDaemon;

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

/// A fake `voiced` at `path` (zeta-speech's [`FakeDaemon`]): answers `ok:<SPOKEN>` to a
/// `file:` line naming a valid staged WAV ([`check_staged_wav`]), `error:<reason>` to any
/// other `file:` line, and `error:unknown command` to `languages` (as `local-voice` does).
/// Serves as long as the test process lives.
fn fake_voiced(path: &Path) -> FakeDaemon {
    FakeDaemon::start(path, |line| {
        Some(match line.strip_prefix("file:") {
            None => "error:unknown command\n".to_string(),
            Some(_) => match check_staged_wav(line) {
                Ok(()) => format!("ok:{SPOKEN}\n"),
                Err(reason) => format!("error:{reason}\n"),
            },
        })
    })
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

/// The `tone` recording (zeta-source-file's fixture) as `<stem>.sigmf-{meta,data}` in `dir`,
/// labelled at `center_hz`; the meta path.
fn tone_at(dir: &Path, stem: &str, center_hz: u64) -> PathBuf {
    let tone = zeta_source_file::fixtures::write_tone(dir);
    let meta = std::fs::read_to_string(&tone).expect("read tone meta");
    let field = "\"core:frequency\":100000000";
    assert!(meta.contains(field), "tone meta without {field}: {meta}");
    let meta = meta.replace(field, &format!("\"core:frequency\":{center_hz}"));
    std::fs::copy(
        tone.with_extension("sigmf-data"),
        dir.join(format!("{stem}.sigmf-data")),
    )
    .expect("copy tone data");
    let path = dir.join(format!("{stem}.sigmf-meta"));
    std::fs::write(&path, meta).expect("write meta");
    path
}

#[test]
fn quiet_fixture_measures_as_noise() {
    let dir = tempfile::tempdir().unwrap();
    let meta = quiet_fixture(dir.path());
    let out = Command::new(env!("CARGO_BIN_EXE_zeta"))
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .arg("--source-file")
        .arg(format!("quiet={}", meta.display()))
        .args([
            "job",
            "start",
            "--kind",
            "measure",
            "--source",
            "quiet",
            "--center-hz",
            "20000000",
            "--bandwidth-hz",
            "9600",
            "--then-tail",
            "1,3000",
        ])
        .output()
        .expect("run zeta");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "zeta exited {}:\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("JSON on stdout");
    let label = &v[1]["value"]["frames"][0]["payload"]["label"]["name"];
    assert_eq!(label, "noise", "{stdout}");
}

/// The `quiet` recording as `quiet.sigmf-{meta,data}` in `dir`, labelled at 20 MHz: 48 000
/// samples (1 s at 48 kHz) of complex noise, each part uniform in -0.01..0.01 from the LCG
/// [`zeta_dsp::fixtures::awgn_real`] uses (seed 7), so `measure` labels it `noise`; the
/// meta path.
fn quiet_fixture(dir: &Path) -> PathBuf {
    let mut state: u64 = 7;
    let mut u = || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let unit = ((state >> 11) + 1) as f64 / (1u64 << 53) as f64;
        ((unit * 2.0 - 1.0) * 0.01) as f32
    };
    let iq: Vec<num_complex::Complex<f32>> = (0..48_000)
        .map(|_| num_complex::Complex::new(u(), u()))
        .collect();
    zeta_dsp::fixtures::write_fixture(dir, "quiet", &iq, 48_000, 20_000_000)
}

/// Wall-clock limit on the whole ui.py run: measured 256.3 s and 256.9 s on 2026-11-01 (plan 18
/// added 31 s), so 330 s. The margin covers the plan-15 scenario
/// `coarse_layer_leaves_masked_bins_unpainted` (about 10 s) that joins on rebase.
const DEADLINE: Duration = Duration::from_secs(330);

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

/// The `fake_pos` mode row: a `json_lines` program row run by zeta-ext's `fake_lines` (pinned
/// with `ZETA_HANDLER_FAKE_LINES`), whose events carry a `position`.
///
/// The row is `ready` on this node, so identify, watch and classify see it in every scenario,
/// and with `proof: event` it would "prove" itself on any signal (`fake_lines` prints its lines
/// whatever the input). It is shaped so those sweeps never try it on the suite's boxes: a
/// 40 kHz `min_hz` does not fit the ~12.5 kHz boxes they draw (`skipped_band`), the far centre
/// (999 MHz) takes away the score's centre term and an empty `modulation` its layer-1 term.
/// The map scenarios start it explicitly at its own 40 kHz width on `tone`.
fn fake_pos_row() -> serde_json::Value {
    serde_json::json!({
        "id": "fake_pos",
        "name": "Fake positions (UI tests)",
        "family": "general",
        "band": {"bandwidth_hz": 40000, "centers_hz": [999000000], "min_hz": 40000},
        "modulation": [],
        "label": "FAKE",
        "protocol": "fake_pos",
        "decoder": {
            "kind": "program",
            "programs": ["fake_lines"],
            "args": [],
            "input": {"stream": "iq", "format": "cs16", "rate_hz": 48000, "via": "stdin",
                      "container": "raw"},
            "output": {"via": "stdout", "port": null, "parser": "json_lines"},
            "install": {"url": "https://example.invalid/fake", "debian": null,
                        "fedora": null, "brew": null}
        },
        "event": {
            "schema": {
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "lat": {"type": "number"},
                    "lon": {"type": "number"},
                    "alt": {"type": "number"}
                }
            },
            "summary": "{id} {lat:.2},{lon:.2}"
        },
        "position": {"lat": "lat", "lon": "lon", "alt_m": "alt"},
        "identify": {"proof": "event", "min_events": 1, "dwell_ms": 3000,
                     "min_span_ms": 1000, "quick": null},
        "catalogue": [],
        "test": null
    })
}

/// What `fake_lines` prints for a `fake_pos` job: one sonde moving from -27.40,153.10 to
/// -27.41,153.11.
const FAKE_POS_LINES: &str = "{\"id\":\"S1\",\"lat\":-27.40,\"lon\":153.10,\"alt\":1000}\n\
{\"id\":\"S1\",\"lat\":-27.41,\"lon\":153.11,\"alt\":1200}";

/// `fake_lines` prints its lines after this many input bytes: 2 s of the 48 kHz cs16 IQ, so
/// the page has subscribed to the job's events by then.
const FAKE_POS_AFTER_BYTES: &str = "384000";

/// A modes directory under `dir` holding `fake_pos.json`; its path.
fn fake_pos_modes(dir: &Path) -> PathBuf {
    let modes = dir.join("modes");
    std::fs::create_dir_all(&modes).expect("create modes dir");
    let row = serde_json::to_string_pretty(&fake_pos_row()).unwrap();
    std::fs::write(modes.join("fake_pos.json"), row).expect("write fake_pos.json");
    modes
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
    // A fixture for the node the rebuild scenario starts and restarts by itself.
    let rebuild_meta =
        zeta_dsp::fixtures::write_fixture(dir.path(), "rebuild", &iq, 48_000, 144_800_000);
    let audio = zeta_dsp::fixtures::tone(WFM_AUDIO_RATE, 2 * WFM_AUDIO_RATE as usize, 1_000.0, 0.9);
    let iq =
        zeta_dsp::fixtures::wfm_modulate(&audio, WFM_AUDIO_RATE, WFM_RATE, 75_000.0, 0.0, true);
    let meta = zeta_dsp::fixtures::write_fixture(dir.path(), "wfm", &iq, WFM_RATE, 104_900_000);
    let wfm = format!("wfm={}", meta.display());
    let iq = zeta_dsp::fixtures::pocsag_packet_iq(1_234_567, 3, "HELLO ZETA", 1200, 48_000, 0.0);
    let meta = zeta_dsp::fixtures::write_fixture(dir.path(), "pocsag", &iq, 48_000, 148_637_500);
    let pocsag = format!("pocsag={}", meta.display());
    let speech = format!("speech={}", speech_fixture(dir.path()).display());
    // The tone recording played as a radio tunable over 200-300 MHz (well away from `tone`, so
    // the page's automatic scans around `tone` never pick it), and as one tunable over
    // 50-150 MHz (the scanner).
    let retune = format!(
        "retune={},tune_min_hz=200000000,tune_max_hz=300000000",
        tone_at(dir.path(), "retune", 250_000_000).display()
    );
    let scanner = format!(
        "scanner={},tune_min_hz=50000000,tune_max_hz=150000000",
        zeta_source_file::fixtures::write_tone(dir.path()).display()
    );
    // The tone recording labelled at 476.9 MHz, channel 20 of the node's CB plan.
    let cb = format!("cb={}", tone_at(dir.path(), "cb", 476_900_000).display());
    // Noise only, at 20 MHz: a fitted demodulator finds no signal there.
    let quiet = format!("quiet={}", quiet_fixture(dir.path()).display());
    let sock_dir = socket_dir();
    let sock = sock_dir.path().join("voiced.sock");
    let _voiced = fake_voiced(&sock);
    let sock = sock.to_str().expect("socket path is UTF-8");
    let modes = fake_pos_modes(dir.path());
    let modes = modes.to_str().expect("modes path is UTF-8");
    let fake_lines = repo_root().join("crates/zeta-ext/tests/fake_lines");
    let fake_lines = fake_lines.to_str().expect("fake_lines path is UTF-8");
    let serve = ServeProc::start_full(
        dir.path(),
        SECRET,
        &[
            "--modes-dir",
            modes,
            "--source-file",
            &aprs,
            "--source-file",
            &wfm,
            "--source-file",
            &pocsag,
            "--source-file",
            &speech,
            "--source-file",
            &retune,
            "--source-file",
            &scanner,
            "--source-file",
            &cb,
            "--source-file",
            &quiet,
        ],
        &["--stt-socket", sock],
        &[
            ("ZETA_HANDLER_FAKE_LINES", fake_lines),
            ("ZETA_PLUGIN_ENV_FAKE_LINES", FAKE_POS_LINES),
            ("ZETA_PLUGIN_ENV_FAKE_AFTER_BYTES", FAKE_POS_AFTER_BYTES),
        ],
    );
    // No speech daemon: XDG_RUNTIME_DIR points where no voiced.sock is, so no default applies.
    let nostt_dir = tempfile::tempdir().unwrap();
    let runtime = nostt_dir.path().to_str().expect("tempdir is UTF-8");
    let nostt_speech = format!("speech={}", speech_fixture(nostt_dir.path()).display());
    let nostt = ServeProc::start_env(
        nostt_dir.path(),
        SECRET,
        &["--source-file", &nostt_speech],
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
        .env("ZETA_BIN", env!("CARGO_BIN_EXE_zeta"))
        .env("ZETA_UI_REBUILD_META", &rebuild_meta)
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
