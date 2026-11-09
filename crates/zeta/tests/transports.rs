//! `zeta serve --listen` and `--node`, against a real serving process.

mod common;

use assert_cmd::Command;
use common::{ServeProc, zeta_remote};
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use serde_json::Value;

const SECRET: &str = "a-long-enough-secret";

fn json(out: &[u8]) -> Value {
    serde_json::from_slice(out).expect("stdout is JSON")
}

fn plain() -> Command {
    let mut c = Command::cargo_bin("zeta").unwrap();
    c.env_remove("ZETA_NODE").env_remove("ZETA_SECRET");
    c
}

#[test]
fn remote_job_start_then_list_sees_running_job() {
    let dir = tempfile::tempdir().unwrap();
    let p = ServeProc::start(dir.path(), SECRET);
    assert_eq!(p.dir, dir.path());
    let out = zeta_remote(
        &p.url,
        SECRET,
        &[
            "job",
            "start",
            "--kind",
            "passthrough",
            "--source",
            "tone",
            "--center-hz",
            "100000000",
            "--bandwidth-hz",
            "48000",
        ],
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let started = json(&out);
    let id = started["value"]
        .as_str()
        .unwrap_or_else(|| panic!("no job id in {started}"))
        .to_string();

    let out = zeta_remote(&p.url, SECRET, &["job", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let jobs = json(&out);
    let found = jobs["value"].as_array().is_some_and(|a| {
        a.iter()
            .any(|j| j["id"] == id.as_str() && j["state"]["status"] == "running")
    });
    assert!(found, "running job {id} not in {jobs}");

    let out = zeta_remote(&p.url, SECRET, &["stream", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let streams = json(&out);
    let owned = streams["value"]
        .as_array()
        .is_some_and(|a| a.iter().any(|s| s["provenance"]["job"] == id.as_str()));
    assert!(owned, "no stream from job {id} in {streams}");
}

#[test]
fn remote_op_error_exits_2_with_json() {
    let dir = tempfile::tempdir().unwrap();
    let p = ServeProc::start(dir.path(), SECRET);
    let out = zeta_remote(
        &p.url,
        SECRET,
        &[
            "source",
            "ensure",
            "--source",
            "tone",
            "--center-hz",
            "100000",
            "--bandwidth-hz",
            "48000",
        ],
    )
    .assert()
    .code(2)
    .get_output()
    .stdout
    .clone();
    let e = json(&out);
    assert_eq!(e["suggested"], 100_000_000u64, "{e}");
}

#[test]
fn remote_wrong_secret_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let p = ServeProc::start(dir.path(), SECRET);
    zeta_remote(
        &p.url,
        "wrong",
        &[
            "job",
            "start",
            "--kind",
            "passthrough",
            "--source",
            "tone",
            "--center-hz",
            "100000000",
            "--bandwidth-hz",
            "48000",
        ],
    )
    .assert()
    .code(1)
    .stderr(contains("rejected the secret"));
    let out = zeta_remote(&p.url, SECRET, &["job", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["value"].as_array().map(Vec::len), Some(0));
}

#[test]
fn remote_unreachable_exits_1_naming_url() {
    zeta_remote("http://127.0.0.1:9", "s", &["job", "list"])
        .assert()
        .code(1)
        .stderr(contains("unreachable"))
        .stderr(contains("http://127.0.0.1:9"));
}

#[test]
fn node_flag_rejects_local_flags() {
    plain()
        .args(["--node", "http://127.0.0.1:9", "--secret", "s"])
        .args(["--source-file", "x=y", "source", "list"])
        .assert()
        .code(1);
    plain()
        .args(["--node", "http://127.0.0.1:9", "source", "list"])
        .assert()
        .code(1)
        .stderr(contains("requires --secret"));
}

#[test]
fn listen_requires_secret_and_lan() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    plain()
        .arg("--state-dir")
        .arg(&state)
        .args(["serve", "--listen", "127.0.0.1:0"])
        .assert()
        .code(1)
        .stderr(contains("--listen requires --secret"));
    plain()
        .arg("--state-dir")
        .arg(&state)
        .args(["serve", "--listen", "8.8.8.8:7979", "--secret", "s"])
        .assert()
        .code(1)
        .stderr(contains("LAN only"));
    assert!(!state.join("serve.lock").exists());
}

fn stdout_of(args: &[&str]) -> String {
    let out = plain()
        .args(args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(out).unwrap()
}

#[test]
fn committed_skill_matches_rendering() {
    plain()
        .args(["agents-md", "--skill", "--check"])
        .assert()
        .success();
}

#[test]
fn skill_has_frontmatter_and_full_guide() {
    let skill = stdout_of(&["agents-md", "--skill"]);
    let guide = stdout_of(&["agents-md"]);
    assert!(skill.starts_with("---\nname: zetasdr\n"));
    assert!(skill.ends_with(&guide));
}

#[test]
fn guide_documents_transports() {
    let guide = stdout_of(&["agents-md"]);
    for needle in [
        "--node URL",
        "ZETA_SECRET",
        "zeta serve --listen",
        "zeta-mcp --node URL",
        "zeta ask",
    ] {
        assert!(guide.contains(needle), "guide lacks {needle}");
    }
}

#[tokio::test]
async fn serve_serves_committed_agents_md() {
    let dir = tempfile::tempdir().unwrap();
    let p = ServeProc::start(dir.path(), SECRET);
    let client = zeta_client::NodeClient::new(&p.url, SECRET).unwrap();
    let served = client.agents_md().await.unwrap();
    let committed =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../AGENTS.md")).unwrap();
    assert_eq!(served, committed);
}

#[test]
fn stt_socket_and_stt_node_are_exclusive() {
    let dir = tempfile::tempdir().unwrap();
    plain()
        .env_remove("ZETA_STT_SOCKET")
        .env("ZETA_STT_NODE_SECRET", SECRET)
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .args(["serve", "--listen", "127.0.0.1:0", "--secret", SECRET])
        .args(["--stt-socket", "/nonexistent/voiced.sock"])
        .args(["--stt-node", "http://127.0.0.1:9"])
        .assert()
        .code(1)
        .stderr(contains("--stt-socket and --stt-node are exclusive"));
}

#[test]
fn stt_socket_and_stt_node_are_exclusive_for_every_command() {
    let dir = tempfile::tempdir().unwrap();
    plain()
        .env_remove("ZETA_STT_SOCKET")
        .env_remove("ZETA_STT_NODE")
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .args(["--stt-socket", "/nonexistent/voiced.sock"])
        .args(["--stt-node", "http://127.0.0.1:9", "source", "list"])
        .assert()
        .code(1)
        .stdout("")
        .stderr(contains("--stt-socket and --stt-node are exclusive"));
    plain()
        .env_remove("ZETA_STT_SOCKET")
        .env_remove("ZETA_STT_NODE")
        .args(["--node", "http://127.0.0.1:9", "--secret", SECRET])
        .args(["--stt-socket", "/nonexistent/voiced.sock"])
        .args(["--stt-node", "http://127.0.0.1:9", "source", "list"])
        .assert()
        .code(1)
        .stderr(contains("--stt-socket and --stt-node are exclusive"));
}

#[test]
fn node_flag_ignores_speech_flags() {
    // Nothing listens on the node, so the request itself fails: the flags were let through.
    for flag in ["--stt-socket", "--stt-node"] {
        plain()
            .env_remove("ZETA_STT_SOCKET")
            .env_remove("ZETA_STT_NODE")
            .args(["--node", "http://127.0.0.1:9", "--secret", SECRET])
            .args([flag, "x", "source", "list"])
            .assert()
            .code(1)
            .stdout("")
            .stderr(contains("unreachable"))
            .stderr(contains("with --node").not());
    }
}

#[test]
fn stt_node_requires_its_secret() {
    let dir = tempfile::tempdir().unwrap();
    plain()
        .env_remove("ZETA_STT_SOCKET")
        .env_remove("ZETA_STT_NODE_SECRET")
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .args(["serve", "--listen", "127.0.0.1:0", "--secret", SECRET])
        .args(["--stt-node", "http://127.0.0.1:9"])
        .assert()
        .code(1)
        .stderr(contains("ZETA_STT_NODE_SECRET"));
}

#[test]
fn then_tail_with_node_is_usage_error() {
    // Rejected before any request, so no node needs to be listening.
    plain()
        .args([
            "--node",
            "http://127.0.0.1:9",
            "--secret",
            SECRET,
            "job",
            "start",
            "--kind",
            "aprs_decode",
            "--source",
            "aprs",
            "--center-hz",
            "144800000",
            "--bandwidth-hz",
            "12500",
            "--then-tail",
            "1,1000",
        ])
        .assert()
        .code(1)
        .stdout("")
        .stderr(contains("--then-tail is not supported with --node yet"));
}

#[test]
fn remote_job_retune_moves_a_running_job() {
    let dir = tempfile::tempdir().unwrap();
    let p = ServeProc::start(dir.path(), SECRET);
    let remote = |args: &[&str]| {
        json(
            &zeta_remote(&p.url, SECRET, args)
                .assert()
                .success()
                .get_output()
                .stdout
                .clone(),
        )
    };
    let started = remote(&[
        "job",
        "start",
        "--kind",
        "passthrough",
        "--source",
        "tone",
        "--center-hz",
        "100000000",
        "--bandwidth-hz",
        "48000",
    ]);
    let id = started["value"].as_str().expect("job id").to_string();
    let moved = remote(&[
        "job",
        "retune",
        "--job",
        &id,
        "--center-hz",
        "100010000",
        "--bandwidth-hz",
        "10000",
    ]);
    assert_eq!(moved["value"], id.as_str(), "{moved}");
    let jobs = remote(&["job", "list"]);
    let all = jobs["value"].as_array().expect("jobs");
    let pass: Vec<&Value> = all.iter().filter(|j| j["kind"] == "passthrough").collect();
    assert_eq!(pass.len(), 1, "{jobs}");
    let rec = pass[0];
    assert_eq!(rec["id"], id.as_str(), "{rec}");
    assert_eq!(rec["state"]["status"], "running", "{rec}");
    assert_eq!(rec["tuning"]["center_hz"], 100_010_000u64, "{rec}");
    assert_eq!(rec["tuning"]["bandwidth_hz"], 10_000u64, "{rec}");
}

#[test]
fn stream_save_without_node_is_a_usage_error() {
    plain()
        .args(["stream", "save", "--stream", "s1", "--path", "x.png"])
        .assert()
        .code(1)
        .stdout("")
        .stderr(contains("use --node"));
}

#[tokio::test(flavor = "multi_thread")]
async fn stream_save_on_an_audio_stream_says_it_has_no_still() {
    use zeta_core::types::{Provenance, SourceId, Span, StreamKind, Timestamp};

    let dir = tempfile::tempdir().unwrap();
    let ts = zeta_ws::testing::spawn(dir.path()).await;
    let id = ts.node.bus().register(
        StreamKind::Audio,
        Provenance {
            source: SourceId("tone".into()),
            job: None,
            channel: None,
            span: Span {
                start_ns: Timestamp(0),
                end_ns: Timestamp(0),
            },
        },
    );
    let (url, stream) = (ts.url.clone(), id.0.clone());
    let path = dir.path().join("x.png").to_string_lossy().into_owned();
    tokio::task::spawn_blocking(move || {
        zeta_remote(
            &url,
            "s3cret",
            &["stream", "save", "--stream", &stream, "--path", &path],
        )
        .assert()
        .code(1)
        .stdout("")
        .stderr(contains(format!(
            "stream {stream} is audio, not video; IQ is saved with `zeta iq read`, audio has no still"
        )));
    })
    .await
    .unwrap();
    ts.stop().await;
}

/// The real CLI path against an in-process server: `zeta_ws`'s test server, whose bus the
/// test feeds, because no stage a served node can start today publishes a video frame.
#[tokio::test(flavor = "multi_thread")]
async fn stream_save_writes_the_first_video_frame() {
    use std::sync::Arc;
    use std::time::Duration;
    use zeta_core::stream::{Frame, Payload, VideoFormat};
    use zeta_core::types::{Provenance, SourceId, Span, StreamKind, Timestamp};

    let dir = tempfile::tempdir().unwrap();
    let ts = zeta_ws::testing::spawn(dir.path()).await;
    let prov = Provenance {
        source: SourceId("tone".into()),
        job: None,
        channel: None,
        span: Span {
            start_ns: Timestamp(0),
            end_ns: Timestamp(0),
        },
    };
    let bus = ts.node.bus();
    let id = bus.register(StreamKind::Video, prov.clone());
    // 720 wide: the node, not the CLI, encodes the PNG, and `sharp` keeps the full width.
    let pixels: Vec<u8> = (0..720 * 8).map(|i| i as u8).collect();
    let feeder = tokio::spawn({
        let (bus, id) = (bus.clone(), id.clone());
        async move {
            loop {
                bus.publish(Frame::new(
                    id.clone(),
                    prov.clone(),
                    Payload::Video {
                        width: 720,
                        height: 8,
                        format: VideoFormat::Gray8,
                        frame: Arc::from(pixels.clone()),
                    },
                ));
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    });
    let out = dir.path().join("still.png");
    let (url, path, stream) = (
        ts.url.clone(),
        out.to_string_lossy().into_owned(),
        id.0.clone(),
    );
    let stdout = tokio::task::spawn_blocking(move || {
        zeta_remote(
            &url,
            "s3cret",
            &["stream", "save", "--stream", &stream, "--path", &path],
        )
        .assert()
        .success()
        .get_output()
        .stdout
        .clone()
    })
    .await
    .unwrap();
    feeder.abort();
    let saved = json(&stdout);
    assert_eq!(
        (saved["width"].as_u64(), saved["height"].as_u64()),
        (Some(720), Some(8))
    );
    let png = std::fs::read(&out).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    // The IHDR chunk: width and height, big-endian, after the signature and chunk header.
    assert_eq!(&png[12..16], b"IHDR");
    let be = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
    assert_eq!((be(16), be(20)), (720, 8));
    assert_eq!(saved["bytes"].as_u64(), Some(png.len() as u64));
    ts.stop().await;
}
