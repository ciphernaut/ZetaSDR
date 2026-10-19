//! `POST /stt`: a WAV body bridged to a fake `voiced` on a Unix socket.

use std::path::{Path, PathBuf};

use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use zeta_ws::Limits;
use zeta_ws::testing::{TestOpts, TestServer, spawn, spawn_with};

const BEARER: &str = "Bearer s3cret";

fn wav(extra: usize) -> Vec<u8> {
    let mut b = b"RIFF\x24\x00\x00\x00WAVEfmt ".to_vec();
    b.resize(b.len() + extra, 0);
    b
}

/// A fake daemon answering every connection with `reply`; yields each `file:` path it saw.
fn fake_daemon(sock: &Path, reply: &'static str) -> mpsc::UnboundedReceiver<(PathBuf, bool)> {
    let listener = UnixListener::bind(sock).unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let (conn, _) = listener.accept().await.unwrap();
            let (r, mut w) = conn.into_split();
            let mut line = String::new();
            BufReader::new(r).read_line(&mut line).await.unwrap();
            let path = line.strip_prefix("file:").expect("file: command");
            let path = PathBuf::from(path.trim_end_matches('\n'));
            let _ = tx.send((path.clone(), path.exists()));
            w.write_all(reply.as_bytes()).await.unwrap();
        }
    });
    rx
}

/// A daemon that answers `n` bytes with no newline.
fn endless_daemon(sock: &Path, n: usize) {
    let listener = UnixListener::bind(sock).unwrap();
    tokio::spawn(async move {
        let (conn, _) = listener.accept().await.unwrap();
        let (r, mut w) = conn.into_split();
        let mut line = String::new();
        BufReader::new(r).read_line(&mut line).await.unwrap();
        let _ = w.write_all(&vec![b'x'; n]).await;
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    });
}

async fn with_socket(dir: &Path, sock: &Path, limits: Limits) -> TestServer {
    spawn_with(
        dir,
        TestOpts {
            stt_socket: Some(sock.to_path_buf()),
            limits,
            ..TestOpts::default()
        },
    )
    .await
}

async fn stt(ts: &TestServer, ctype: &str, body: Vec<u8>) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/stt", ts.url))
        .header("authorization", BEARER)
        .header("content-type", ctype)
        .body(body)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn stt_forwards_wav_to_the_daemon_and_returns_text() {
    let dir = tempfile::tempdir().unwrap();
    let socks = tempfile::tempdir().unwrap();
    let sock = socks.path().join("v.sock");
    let mut seen = fake_daemon(&sock, "ok:hello world\n");
    let ts = with_socket(dir.path(), &sock, Limits::default()).await;

    let resp = stt(&ts, "audio/wav", wav(100)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.json::<Value>().await.unwrap(),
        json!({"text": "hello world"})
    );
    let (path, existed) = seen.recv().await.unwrap();
    assert!(existed, "the file must exist while the daemon reads it");
    assert!(path.starts_with(dir.path().join("state").join("stt")));
    assert!(!path.exists(), "the temp file is removed afterwards");
    ts.stop().await;
}

#[tokio::test]
async fn stt_accepts_x_wav_and_requires_auth() {
    let dir = tempfile::tempdir().unwrap();
    let socks = tempfile::tempdir().unwrap();
    let sock = socks.path().join("v.sock");
    let _seen = fake_daemon(&sock, "ok:hi\n");
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
    assert_eq!(v["message"], "serve has no speech daemon (--stt-socket)");
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
    let left = std::fs::read_dir(dir.path().join("state").join("stt"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(left, 0, "no temp file left behind");
    ts.stop().await;
}

#[tokio::test]
async fn stt_daemon_error_is_502() {
    let dir = tempfile::tempdir().unwrap();
    let socks = tempfile::tempdir().unwrap();
    let sock = socks.path().join("v.sock");
    let _seen = fake_daemon(&sock, "error:model not loaded\n");
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
    let _seen = fake_daemon(&sock, "ok:x\n");
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
    let listener = UnixListener::bind(&sock).unwrap();
    tokio::spawn(async move {
        let (conn, _) = listener.accept().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        drop(conn);
    });
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
    endless_daemon(&sock, 70_000);
    let ts = with_socket(dir.path(), &sock, Limits::default()).await;
    let resp = stt(&ts, "audio/wav", wav(1)).await;
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["message"], "speech daemon: answer too long");
    let left = std::fs::read_dir(dir.path().join("state").join("stt"))
        .unwrap()
        .count();
    assert_eq!(left, 0, "no staged file left");
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
    let mut seen = fake_daemon(&sock, "ok:hi\n");
    let ts = with_socket(&rel, &sock, Limits::default()).await;
    assert_eq!(stt(&ts, "audio/wav", wav(1)).await.status(), StatusCode::OK);
    let (path, existed) = seen.recv().await.unwrap();
    assert!(path.is_absolute(), "{}", path.display());
    assert!(existed);
    ts.stop().await;
}
