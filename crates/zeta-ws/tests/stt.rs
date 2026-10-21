//! `POST /stt` and `GET /stt/languages`: a WAV body bridged to a speech backend (a
//! `SocketBackend` on a fake `voiced`, or an in-process fake).

use std::path::Path;
use std::sync::Arc;

use reqwest::StatusCode;
use serde_json::{Value, json};
use zeta_speech::testing::{FakeBackend, FakeDaemon};
use zeta_speech::{NodeBackend, SocketBackend, SpeechBackend};
use zeta_ws::Limits;
use zeta_ws::testing::{TestOpts, TestServer, spawn, spawn_with};

const BEARER: &str = "Bearer s3cret";

fn wav(extra: usize) -> Vec<u8> {
    let mut b = b"RIFF\x24\x00\x00\x00WAVEfmt ".to_vec();
    b.resize(b.len() + extra, 0);
    b
}

async fn with_speech(dir: &Path, speech: Arc<dyn SpeechBackend>, limits: Limits) -> TestServer {
    spawn_with(
        dir,
        TestOpts {
            speech: Some(speech),
            limits,
            ..TestOpts::default()
        },
    )
    .await
}

/// A server whose backend is the daemon at `sock`, staging in `<dir>/state/stt` as serve does.
async fn with_socket(dir: &Path, sock: &Path, limits: Limits) -> TestServer {
    let staging = dir.join("state").join("stt");
    let backend = SocketBackend::new(sock.to_path_buf(), staging, limits.stt_timeout).await;
    with_speech(dir, Arc::new(backend), limits).await
}

async fn stt_at(ts: &TestServer, query: &str, ctype: &str, body: Vec<u8>) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/stt{query}", ts.url))
        .header("authorization", BEARER)
        .header("content-type", ctype)
        .body(body)
        .send()
        .await
        .unwrap()
}

async fn stt(ts: &TestServer, ctype: &str, body: Vec<u8>) -> reqwest::Response {
    stt_at(ts, "", ctype, body).await
}

fn staged_left(dir: &Path) -> usize {
    std::fs::read_dir(dir.join("state").join("stt"))
        .map(|d| d.count())
        .unwrap_or(0)
}

#[tokio::test]
async fn stt_forwards_wav_to_the_daemon_and_returns_text() {
    let dir = tempfile::tempdir().unwrap();
    let socks = tempfile::tempdir().unwrap();
    let sock = socks.path().join("v.sock");
    let daemon = FakeDaemon::replying(&sock, "ok:hello world\n");
    let ts = with_socket(dir.path(), &sock, Limits::default()).await;

    let resp = stt(&ts, "audio/wav", wav(100)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.json::<Value>().await.unwrap(),
        json!({"text": "hello world"})
    );
    let files = daemon.files();
    assert_eq!(files.len(), 1);
    assert!(
        files[0].existed,
        "the file must exist while the daemon reads it"
    );
    assert!(
        files[0]
            .path
            .starts_with(dir.path().join("state").join("stt"))
    );
    assert!(
        !files[0].path.exists(),
        "the temp file is removed afterwards"
    );
    ts.stop().await;
}

#[tokio::test]
async fn stt_accepts_x_wav_and_requires_auth() {
    let dir = tempfile::tempdir().unwrap();
    let socks = tempfile::tempdir().unwrap();
    let sock = socks.path().join("v.sock");
    let _daemon = FakeDaemon::replying(&sock, "ok:hi\n");
    let ts = with_socket(dir.path(), &sock, Limits::default()).await;
    assert_eq!(
        stt(&ts, "audio/x-wav", wav(1)).await.status(),
        StatusCode::OK
    );
    let anon = reqwest::Client::new()
        .post(format!("{}/stt", ts.url))
        .header("content-type", "audio/wav")
        .body(wav(1))
        .send()
        .await
        .unwrap();
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);
    ts.stop().await;
}

#[tokio::test]
async fn stt_without_daemon_is_503() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let resp = stt(&ts, "audio/wav", wav(1)).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(
        v["message"],
        "serve has no speech backend (--stt-socket or --stt-node)"
    );
    ts.stop().await;
}

#[tokio::test]
async fn stt_unreachable_daemon_is_503_naming_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let socks = tempfile::tempdir().unwrap();
    let sock = socks.path().join("absent.sock");
    let ts = with_socket(dir.path(), &sock, Limits::default()).await;
    let resp = stt(&ts, "audio/wav", wav(1)).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v: Value = resp.json().await.unwrap();
    let msg = v["message"].as_str().unwrap();
    assert!(
        msg.starts_with(&format!("speech daemon at {}: ", sock.display())),
        "{msg}"
    );
    assert_eq!(staged_left(dir.path()), 0, "no temp file left behind");
    ts.stop().await;
}

#[tokio::test]
async fn stt_daemon_error_is_502() {
    let dir = tempfile::tempdir().unwrap();
    let socks = tempfile::tempdir().unwrap();
    let sock = socks.path().join("v.sock");
    let _daemon = FakeDaemon::replying(&sock, "error:model not loaded\n");
    let ts = with_socket(dir.path(), &sock, Limits::default()).await;
    let resp = stt(&ts, "audio/wav", wav(1)).await;
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["error"], "other");
    assert_eq!(v["message"], "speech daemon: model not loaded");
    ts.stop().await;
}

#[tokio::test]
async fn stt_rejects_non_wav_and_oversize() {
    let dir = tempfile::tempdir().unwrap();
    let socks = tempfile::tempdir().unwrap();
    let sock = socks.path().join("v.sock");
    let _daemon = FakeDaemon::replying(&sock, "ok:x\n");
    let ts = with_socket(dir.path(), &sock, Limits::default()).await;
    let not_riff = stt(&ts, "audio/wav", b"OggS0000".to_vec()).await;
    assert_eq!(not_riff.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let wrong_type = stt(&ts, "application/json", wav(1)).await;
    assert_eq!(wrong_type.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let max = Limits::default().stt_max_bytes;
    let mut big = wav(0);
    big.resize(max + 1, 0);
    assert_eq!(
        stt(&ts, "audio/wav", big).await.status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    ts.stop().await;
}

#[tokio::test]
async fn stt_times_out_with_504() {
    let dir = tempfile::tempdir().unwrap();
    let socks = tempfile::tempdir().unwrap();
    let sock = socks.path().join("v.sock");
    let _daemon = FakeDaemon::silent(&sock);
    let limits = Limits {
        stt_timeout: std::time::Duration::from_millis(200),
        ..Limits::default()
    };
    let ts = with_socket(dir.path(), &sock, limits).await;
    let resp = stt(&ts, "audio/wav", wav(1)).await;
    assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
    ts.stop().await;
}

#[tokio::test]
async fn stt_bounds_the_daemon_reply() {
    let dir = tempfile::tempdir().unwrap();
    let socks = tempfile::tempdir().unwrap();
    let sock = socks.path().join("v.sock");
    let _daemon = FakeDaemon::start(&sock, |_| Some("x".repeat(70_000)));
    let ts = with_socket(dir.path(), &sock, Limits::default()).await;
    let resp = stt(&ts, "audio/wav", wav(1)).await;
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["message"], "speech daemon: answer too long");
    assert_eq!(staged_left(dir.path()), 0, "no staged file left");
    ts.stop().await;
}

#[tokio::test]
async fn stt_sends_the_daemon_an_absolute_path_for_a_relative_state_dir() {
    let cwd = std::env::current_dir().unwrap();
    let held = tempfile::tempdir_in(&cwd).unwrap();
    let rel = held.path().strip_prefix(&cwd).unwrap().to_path_buf();
    assert!(rel.is_relative());
    let socks = tempfile::tempdir().unwrap();
    let sock = socks.path().join("v.sock");
    let daemon = FakeDaemon::replying(&sock, "ok:hi\n");
    let ts = with_socket(&rel, &sock, Limits::default()).await;
    assert_eq!(stt(&ts, "audio/wav", wav(1)).await.status(), StatusCode::OK);
    let files = daemon.files();
    assert!(files[0].path.is_absolute(), "{}", files[0].path.display());
    assert!(files[0].existed);
    ts.stop().await;
}

#[tokio::test]
async fn stt_unsupported_language_is_422_listing_the_supported() {
    let dir = tempfile::tempdir().unwrap();
    let socks = tempfile::tempdir().unwrap();
    let sock = socks.path().join("v.sock");
    let daemon = FakeDaemon::replying(&sock, "ok:hallo\n");
    let ts = with_socket(dir.path(), &sock, Limits::default()).await;
    let resp = stt_at(&ts, "?lang=de", "audio/wav", wav(1)).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        resp.json::<Value>().await.unwrap(),
        json!({"error": "unsupported", "language": "de", "supported": ["en"]})
    );
    assert!(daemon.files().is_empty(), "the daemon is not asked");
    ts.stop().await;
}

#[tokio::test]
async fn stt_passes_the_language_and_defaults_to_english() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeBackend::new("bonjour").with_languages(&["en", "fr"]));
    let ts = with_speech(dir.path(), fake.clone(), Limits::default()).await;
    let resp = stt_at(&ts, "?lang=fr", "audio/wav", wav(1)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.json::<Value>().await.unwrap(),
        json!({"text": "bonjour"})
    );
    assert_eq!(stt(&ts, "audio/wav", wav(1)).await.status(), StatusCode::OK);
    let langs: Vec<String> = fake.calls().into_iter().map(|(_, l)| l).collect();
    assert_eq!(langs, ["fr", "en"]);
    ts.stop().await;
}

async fn get_languages(ts: &TestServer, auth: Option<&str>) -> reqwest::Response {
    let mut req = reqwest::Client::new().get(format!("{}/stt/languages", ts.url));
    if let Some(auth) = auth {
        req = req.header("authorization", auth);
    }
    req.send().await.unwrap()
}

#[tokio::test]
async fn stt_languages_names_the_backend_and_its_languages() {
    let dir = tempfile::tempdir().unwrap();
    let socks = tempfile::tempdir().unwrap();
    let sock = socks.path().join("v.sock");
    let _daemon = FakeDaemon::with_languages(&sock, "ok:x\n", "en,de");
    let ts = with_socket(dir.path(), &sock, Limits::default()).await;
    let resp = get_languages(&ts, Some(BEARER)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.json::<Value>().await.unwrap(),
        json!({"backend": format!("socket:{}", sock.display()), "languages": ["en", "de"]})
    );
    assert_eq!(
        get_languages(&ts, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    ts.stop().await;
}

#[tokio::test]
async fn stt_languages_without_a_backend_is_503() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let resp = get_languages(&ts, Some(BEARER)).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    ts.stop().await;
}

#[tokio::test]
async fn stt_hop_to_a_forwarding_node_is_508() {
    let dir = tempfile::tempdir().unwrap();
    let to_nowhere = NodeBackend::new("http://127.0.0.1:9", "s").await.unwrap();
    let ts = with_speech(dir.path(), Arc::new(to_nowhere), Limits::default()).await;
    let resp = reqwest::Client::new()
        .post(format!("{}/stt", ts.url))
        .header("authorization", BEARER)
        .header("content-type", "audio/wav")
        .header("x-zeta-stt-hop", "1")
        .body(wav(1))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::LOOP_DETECTED);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["message"], "stt forwarding loop");
    ts.stop().await;
}

#[tokio::test]
async fn stt_hop_to_a_local_backend_is_served() {
    let dir = tempfile::tempdir().unwrap();
    let ts = with_speech(
        dir.path(),
        Arc::new(FakeBackend::new("hi")),
        Limits::default(),
    )
    .await;
    let resp = reqwest::Client::new()
        .post(format!("{}/stt", ts.url))
        .header("authorization", BEARER)
        .header("content-type", "audio/wav")
        .header("x-zeta-stt-hop", "1")
        .body(wav(1))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    ts.stop().await;
}

#[tokio::test]
async fn stt_queued_calls_get_their_queue_wait_and_busy_is_503() {
    // One slot, a 300 ms daemon, and a call timeout equal to the queue wait (as serve has
    // them): the second call waits for the first and still answers; the third waits past
    // the queue wait and is told busy. None is cut off at the call timeout (504).
    let dir = tempfile::tempdir().unwrap();
    let socks = tempfile::tempdir().unwrap();
    let sock = socks.path().join("v.sock");
    let _daemon = FakeDaemon::with_delay(&sock, "ok:hi\n", std::time::Duration::from_millis(300));
    let wait = std::time::Duration::from_millis(450);
    let limits = Limits {
        stt_timeout: wait,
        ..Limits::default()
    };
    let staging = dir.path().join("state").join("stt");
    let backend = SocketBackend::with_limits(sock.clone(), staging, wait, 1, wait).await;
    let ts = with_speech(dir.path(), Arc::new(backend), limits).await;
    let calls = (0..3).map(|_| async {
        let resp = stt(&ts, "audio/wav", wav(1)).await;
        let status = resp.status();
        (status, resp.json::<Value>().await.unwrap())
    });
    let mut answers = futures_util::future::join_all(calls).await;
    answers.sort_by_key(|(s, _)| s.as_u16());
    let statuses: Vec<u16> = answers.iter().map(|(s, _)| s.as_u16()).collect();
    assert_eq!(statuses, [200, 200, 503], "{answers:?}");
    assert_eq!(answers[2].1["error"], "busy", "{answers:?}");
    ts.stop().await;
}
