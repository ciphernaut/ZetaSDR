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
//! 20 MHz, which `measure` labels `noise`; see [`quiet_fixture`]), `fsk` (framed 2-FSK at
//! 433.92 MHz, the Bits pane's; see [`fsk_fixture`]), and `--modes-dir` holding the
//! `fake_pos` program row (zeta-ext's `fake_lines`, printing two fixes of one sonde 2 s into a
//! job; see [`fake_pos_row`]); the node's state directory is passed to ui.py in
//! `ZETA_UI_STATE_DIR`. That node has a fake
//! `voiced` (`--stt-socket`, zeta-speech's `FakeDaemon`) answering `ok:what sources are there`
//! to every valid staged WAV; a second node without a speech daemon, with only the `speech`
//! fixture, is passed in `ZETA_UI_URL_NOSTT`; a third node with the `cb` fixture whose
//! `--llm-url` is a request-keyed fake model (zeta-agent's `serve_by_request` with
//! [`fake_llm_scripts`]) is passed in `ZETA_UI_URL_LLM`.
//!
//! The run is killed and the test fails after [`DEADLINE`] (690 s) of wall clock.
//! `ZETA_UI_ONLY=a,b` runs only those scenarios.
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

/// Frames in the `fsk` fixture.
const FSK_FRAMES: usize = 8;
/// Body bytes of each `fsk` frame.
const FSK_BODY_BYTES: usize = 136;
/// The `fsk` frames' sync word.
const FSK_SYNC: u16 = 0x2DD4;

/// The `fsk` recording as `fsk.sigmf-{meta,data}` in `dir`, labelled at 433.92 MHz: 64 bits
/// of `01` alternation (the slicer's preamble), then [`FSK_FRAMES`] frames of [`FSK_SYNC`],
/// [`FSK_BODY_BYTES`] pseudo-random bytes (an xorshift from a fixed seed; a body that would
/// put the sync anywhere but at a frame start is drawn again) and the CRC-16-CCITT of the
/// body, as 2-FSK at 4800 Bd with 2400 Hz deviation and a 50 ms gap, at 48 kHz (a period of
/// 1120 bits); the meta path.
fn fsk_fixture(dir: &Path) -> PathBuf {
    use zeta_dsp::crc::{CRC16_CCITT, crc};
    let mut s: u64 = 0x2DD4_0001_9E37_79B9;
    let mut bytes: Vec<u8> = vec![0x55; 8];
    let sync = FSK_SYNC.to_be_bytes();
    let mut made = 0;
    while made < FSK_FRAMES {
        let body: Vec<u8> = (0..FSK_BODY_BYTES)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 24) as u8
            })
            .collect();
        let mut frame = sync.to_vec();
        frame.extend(&body);
        frame.extend((crc(&CRC16_CCITT, &body) as u16).to_be_bytes());
        // The sync anywhere in the frame but its start, at any bit offset, redraws it.
        let bits: Vec<u8> = frame
            .iter()
            .flat_map(|b| (0..8).rev().map(move |i| (b >> i) & 1))
            .collect();
        let stray = (1..=bits.len() - 16).any(|i| {
            bits[i..i + 16]
                .iter()
                .fold(0u16, |w, &b| (w << 1) | u16::from(b))
                == FSK_SYNC
        });
        if stray {
            continue;
        }
        bytes.extend(frame);
        made += 1;
    }
    let bits: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let spec = zeta_dsp::synth::SynthSpec::Fsk {
        symbol_rate_baud: 4800,
        deviation_hz: 2400.0,
        order: 2,
        bits,
        repeat: 1,
        gap_ms: 50,
    };
    let iq = zeta_dsp::synth::synthesise(
        &spec,
        &zeta_dsp::synth::Impairments::default(),
        48_000,
        433_920_000,
    );
    zeta_dsp::fixtures::write_fixture(dir, "fsk", &iq, 48_000, 433_920_000)
}

/// Wall-clock limit on the whole ui.py run, about 1.25 times the slowest measured run: about
/// 474 s at plan 24, 500.8 s after plan 19.2's Bits scenario, 547.7 s run alone on 2026-11-04;
/// 1.25 × 547.7 s ≈ 685 s, so the limit is 690 s.
const DEADLINE: Duration = Duration::from_secs(690);

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

/// A `fake_lines` row shaped as [`fake_pos_row`], so no sweep tries it, printing the lines of
/// `lines_file` (`$FAKE_LINES` is node-wide, so two rows differ by this).
fn fake_subject_row(
    id: &str,
    lines_file: &Path,
    event: serde_json::Value,
    position: serde_json::Value,
    subject: serde_json::Value,
) -> serde_json::Value {
    let mut row = fake_pos_row();
    row["id"] = id.into();
    row["name"] = format!("{id} (UI tests)").into();
    row["protocol"] = id.into();
    row["decoder"]["args"] = serde_json::json!(["--lines-file", lines_file.to_str().unwrap()]);
    row["event"] = event;
    row["position"] = position;
    row["subject"] = subject;
    row
}

/// `fake_subjects`: three aircraft in separate messages as dump1090 sends them, every field in
/// every line, `null` when the message does not carry it. 7C0001 comes last, so it is the last
/// heard, and carries everything; 7C0003's callsign is markup, which must stay text.
fn fake_subjects_lines() -> String {
    let line =
        |icao: &str, callsign: &str, alt: &str, kt: &str, track: &str, lat: &str, lon: &str| {
            format!(
                "{{\"icao\":\"{icao}\",\"callsign\":{callsign},\"altitude_ft\":{alt},\
             \"ground_speed_kt\":{kt},\"track_deg\":{track},\"lat\":{lat},\"lon\":{lon}}}"
            )
        };
    let n = "null";
    [
        line("7C0002", "\"VOZ2\"", n, n, n, n, n),
        line("7C0002", n, "0", n, n, "-27.30", "153.20"),
        line("7C0003", "\"<b>JST3</b>\"", n, n, n, n, n),
        line("7C0001", "\"QFA1\"", n, n, n, n, n),
        line("7C0001", n, "21000", n, n, n, n),
        line("7C0001", n, n, n, n, "-27.40", "153.10"),
        line("7C0001", n, "21000", n, n, "-27.41", "153.11"),
        line("7C0001", n, n, "421", "245", n, n),
    ]
    .join("\n")
}

/// `fake_fold`'s lines: sonde S1 twice, S2 three times.
const FAKE_FOLD_LINES: &str = "{\"id\":\"S1\",\"alt\":1000}\n{\"id\":\"S2\",\"alt\":2000}\n\
{\"id\":\"S1\",\"alt\":1100}\n{\"id\":\"S2\",\"alt\":2100}\n{\"id\":\"S2\",\"alt\":2200}";

/// A modes directory under `dir` holding the fake rows and their lines files; its path.
fn fake_modes(dir: &Path) -> PathBuf {
    use serde_json::json;
    let modes = dir.join("modes");
    std::fs::create_dir_all(&modes).expect("create modes dir");
    let subjects_file = dir.join("fake_subjects.lines");
    std::fs::write(&subjects_file, fake_subjects_lines()).expect("write fake_subjects.lines");
    let fold_file = dir.join("fake_fold.lines");
    std::fs::write(&fold_file, FAKE_FOLD_LINES).expect("write fake_fold.lines");
    let number = json!({"type": ["number", "null"]});
    let text = json!({"type": ["string", "null"]});
    let rows = [
        fake_pos_row(),
        fake_subject_row(
            "fake_subjects",
            &subjects_file,
            json!({
                "schema": {"type": "object", "properties": {
                    "icao": {"type": "string"},
                    "callsign": text,
                    "altitude_ft": number,
                    "ground_speed_kt": number,
                    "track_deg": number,
                    "lat": number,
                    "lon": number,
                }},
                "summary": "{icao} {callsign?} {altitude_ft?} {lat?},{lon?}",
            }),
            json!({"lat": "lat", "lon": "lon", "alt_m": "altitude_ft", "alt_unit": "ft"}),
            json!({
                "key": "icao",
                "name": "aircraft",
                "waterfall": "summary",
                "columns": [
                    {"field": "callsign", "label": "Callsign", "unit": null},
                    {"field": "altitude_ft", "label": "Altitude", "unit": "ft"},
                    {"field": "ground_speed_kt", "label": "Speed", "unit": "kt"},
                    {"field": "track_deg", "label": "Track", "unit": "deg"},
                ],
                "heading": "track_deg",
                "label": "callsign",
            }),
        ),
        fake_subject_row(
            "fake_fold",
            &fold_file,
            json!({
                "schema": {"type": "object", "properties": {
                    "id": {"type": "string"},
                    "alt": number,
                }},
                "summary": "{id} {alt?:.0} m",
            }),
            serde_json::Value::Null,
            json!({
                "key": "id",
                "name": "sondes",
                "waterfall": "fold",
                "columns": [{"field": "alt", "label": "Altitude", "unit": "m"}],
                "heading": null,
                "label": null,
            }),
        ),
    ];
    for row in rows {
        let file = format!("{}.json", row["id"].as_str().unwrap());
        let text = serde_json::to_string_pretty(&row).unwrap();
        std::fs::write(modes.join(file), text).expect("write a fake row");
    }
    modes
}

/// A themes directory under `dir`: `fixture_theme`, an operator theme (accent `#ff00ff`, no
/// rail, a black-to-magenta ramp, no style.css), and a broken `console` override (an unknown
/// `footprints` key), which leaves the built-in console in force; its path.
fn fake_themes(dir: &Path) -> PathBuf {
    use serde_json::json;
    let themes = dir.join("themes");
    let fixture = json!({
        "id": "fixture_theme",
        "name": "Fixture",
        "description": "An operator theme for the e2e scenarios.",
        "color_scheme": "dark",
        "layout": {"rail": false, "ribbon_tilt_deg": 0},
        "tokens": {"--accent": "#ff00ff"},
        "ramp": [[0, [0, 0, 0]], [1, [255, 0, 255]]],
    });
    let mut broken = fixture.clone();
    broken["id"] = json!("console");
    broken["footprints"] = json!({"narrow": {"rail": false}});
    // Waterfall grounds the page has to normalise: a CSS colour that is neither #hex nor
    // comma rgb(), and one that is no colour at all.
    let mut hsl = fixture.clone();
    hsl["id"] = json!("hsl_bg");
    hsl["tokens"] = json!({"--wf-bg": "hsl(120 100% 25%)"});
    let mut bad = fixture.clone();
    bad["id"] = json!("bad_bg");
    bad["tokens"] = json!({"--wf-bg": "not-a-colour"});
    for theme in [fixture, broken, hsl, bad] {
        let folder = themes.join(theme["id"].as_str().unwrap());
        std::fs::create_dir_all(&folder).expect("create a theme folder");
        let text = serde_json::to_string_pretty(&theme).unwrap();
        std::fs::write(folder.join("theme.json"), text).expect("write a fake theme.json");
    }
    themes
}

/// The fake model's scripts, in `zeta-fake-llm --scripts` form, keyed by a substring of the
/// request: a refused stop, a refused capture delete, a fan-out through `plan_start`, a
/// page-only `view_set`, and a turn with reasoning and content.
fn fake_llm_scripts() -> serde_json::Value {
    use serde_json::json;
    use zeta_agent::fake::{text_response, tool_call_response, tool_call_response_with};
    let plan_start = json!({
        "plan": "uhf_cb_au", "channels": [20, 59], "kind": "fm_demod", "source": "cb",
        "deviation_hz": null, "sideband": null, "attach": null, "language": null, "replace": [],
    });
    json!([
        {"match": "stop the measure", "replies": [
            tool_call_response(&[("job_stop", json!({"job": "01NOPE"}))]),
            text_response("I was not allowed to stop it.", "stop"),
        ]},
        {"match": "every CB channel", "replies": [
            tool_call_response(&[("source_list", json!({}))]),
            tool_call_response(&[("plan_start", plan_start)]),
            text_response("Started 2 channels.", "stop"),
        ]},
        {"match": "delete the capture", "replies": [
            tool_call_response(&[("capture_delete", json!({"capture": "01NOPE"}))]),
            text_response("I was not allowed to delete it.", "stop"),
        ]},
        {"match": "stop all jobs", "replies": [
            tool_call_response(&[("jobs_stop", json!({"source": null, "kind": null, "plan": null}))]),
            text_response("Stopped them.", "stop"),
        ]},
        {"match": "switch to the plain theme", "replies": [
            tool_call_response(&[("view_set", json!({"key": "theme", "value": "plain"}))]),
            text_response("Switched to plain.", "stop"),
        ]},
        {"match": "think", "replies": [
            tool_call_response_with(
                &[("source_list", json!({}))],
                "Let me look.",
                Some("I should list the sources first."),
            ),
            text_response("one source", "stop"),
        ]},
    ])
}

/// Serves [`fake_llm_scripts`] on a free loopback port for the rest of the test, as
/// `zeta-fake-llm` does (`serve_by_request` on a thread of its own: cargo defines
/// `CARGO_BIN_EXE_zeta-fake-llm` only for zeta-agent's own tests, which spawn the binary);
/// its `http://127.0.0.1:PORT`.
fn start_fake_llm() -> String {
    let scripts = fake_llm_scripts()
        .as_array()
        .expect("scripts are an array")
        .iter()
        .map(|s| {
            let replies = s["replies"].as_array().unwrap().iter();
            let key = s["match"].as_str().unwrap().to_string();
            (key, replies.map(|r| (200, r.clone())).collect())
        })
        .collect();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the fake model");
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("fake model runtime");
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            let _ = zeta_agent::fake::serve_by_request(listener, scripts).await;
        });
    });
    url
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
    // Framed 2-FSK at 433.92 MHz: the Bits pane's stream, hypotheses and sync hits.
    let fsk = format!("fsk={}", fsk_fixture(dir.path()).display());
    let sock_dir = socket_dir();
    let sock = sock_dir.path().join("voiced.sock");
    let _voiced = fake_voiced(&sock);
    let sock = sock.to_str().expect("socket path is UTF-8");
    let modes = fake_modes(dir.path());
    let modes = modes.to_str().expect("modes path is UTF-8");
    let themes = fake_themes(dir.path());
    let themes = themes.to_str().expect("themes path is UTF-8");
    let fake_lines = repo_root().join("crates/zeta-ext/tests/fake_lines");
    let fake_lines = fake_lines.to_str().expect("fake_lines path is UTF-8");
    let serve = ServeProc::start_full(
        dir.path(),
        SECRET,
        &[
            "--modes-dir",
            modes,
            "--themes-dir",
            themes,
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
            "--source-file",
            &fsk,
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
    // A fake model answering by request, behind a node with `tone` and `cb`.
    let llm_dir = tempfile::tempdir().unwrap();
    let fake_llm = start_fake_llm();
    let cb_llm = format!(
        "cb={}",
        tone_at(llm_dir.path(), "cb", 476_900_000).display()
    );
    let llm = ServeProc::start_full(
        llm_dir.path(),
        SECRET,
        &["--source-file", &cb_llm],
        &["--llm-url", &fake_llm, "--llm-model", "fake"],
        &[],
    );
    // A ring of a few seconds (a 1 MB budget), so a box outlives the buffer behind it.
    let short_dir = tempfile::tempdir().unwrap();
    let shortring = ServeProc::start_env(short_dir.path(), SECRET, &["--ring-budget-mb", "1"], &[]);
    let mut cmd = Command::new("python3");
    cmd.arg("crates/zeta-ui/tests/e2e/ui.py")
        .args([&serve.url, SECRET]);
    if let Some(only) = std::env::var_os("ZETA_UI_ONLY") {
        cmd.arg(only);
    }
    let child = cmd
        .env("ZETA_UI_STATE_DIR", dir.path().join("state"))
        .env("ZETA_UI_URL_NOSTT", &nostt.url)
        .env("ZETA_UI_URL_LLM", &llm.url)
        .env("ZETA_UI_URL_SHORTRING", &shortring.url)
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
    drop(llm);
    drop(shortring);
}
