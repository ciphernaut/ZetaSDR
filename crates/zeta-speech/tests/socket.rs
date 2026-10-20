//! `SocketBackend` against the fake daemon.

use std::path::Path;
use std::time::{Duration, Instant};

use zeta_speech::testing::FakeDaemon;
use zeta_speech::{SocketBackend, SpeechBackend, SpeechError, wav_s16_mono};

const TIMEOUT: Duration = Duration::from_secs(5);

async fn backend(sock: &Path, staging: &Path, timeout: Duration) -> SocketBackend {
    SocketBackend::new(sock.to_path_buf(), staging.to_path_buf(), timeout).await
}

fn staged_left(staging: &Path) -> usize {
    std::fs::read_dir(staging).map(|d| d.count()).unwrap_or(0)
}

#[tokio::test]
async fn ok_answer_is_the_transcript_and_the_staged_file_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let (sock, staging) = (dir.path().join("v.sock"), dir.path().join("stt"));
    let daemon = FakeDaemon::replying(&sock, "ok:hello world\n");
    let b = backend(&sock, &staging, TIMEOUT).await;
    assert_eq!(b.languages(), ["en"]);
    assert!(
        b.name().contains(&sock.display().to_string()),
        "{}",
        b.name()
    );

    let t = b.transcribe(&wav_s16_mono(&[0; 160]), "en").await.unwrap();
    assert_eq!(
        (t.text.as_str(), t.language.as_str()),
        ("hello world", "en")
    );
    let files = daemon.files();
    assert_eq!(files.len(), 1);
    assert!(
        files[0].existed,
        "the file exists while the daemon reads it"
    );
    assert!(files[0].path.starts_with(&staging));
    assert!(files[0].path.extension().is_some_and(|e| e == "wav"));
    assert!(
        !files[0].path.exists(),
        "the staged file is removed afterwards"
    );
    assert_eq!(daemon.commands()[0], "languages");
}

#[tokio::test]
async fn error_answer_is_a_daemon_error() {
    let dir = tempfile::tempdir().unwrap();
    let (sock, staging) = (dir.path().join("v.sock"), dir.path().join("stt"));
    let _daemon = FakeDaemon::replying(&sock, "error:boom\n");
    let b = backend(&sock, &staging, TIMEOUT).await;
    let err = b
        .transcribe(&wav_s16_mono(&[0; 16]), "en")
        .await
        .unwrap_err();
    assert_eq!(err, SpeechError::Daemon("boom".to_string()));
    assert_eq!(err.to_string(), "speech daemon: boom");
    assert_eq!(staged_left(&staging), 0);
}

#[tokio::test]
async fn no_socket_is_unreachable() {
    let dir = tempfile::tempdir().unwrap();
    let (sock, staging) = (dir.path().join("absent.sock"), dir.path().join("stt"));
    let b = backend(&sock, &staging, TIMEOUT).await;
    assert_eq!(b.languages(), ["en"]);
    let err = b
        .transcribe(&wav_s16_mono(&[0; 16]), "en")
        .await
        .unwrap_err();
    let SpeechError::Unreachable(msg) = &err else {
        panic!("want Unreachable, got {err:?}");
    };
    assert!(
        msg.starts_with(&format!("speech daemon at {}: ", sock.display())),
        "{msg}"
    );
    assert_eq!(staged_left(&staging), 0);
}

#[tokio::test]
async fn a_daemon_that_never_answers_languages_is_assumed_english_within_2_s() {
    let dir = tempfile::tempdir().unwrap();
    let (sock, staging) = (dir.path().join("v.sock"), dir.path().join("stt"));
    let daemon = FakeDaemon::silent(&sock);
    let started = Instant::now();
    // A long call timeout: the probe has its own 2 s bound.
    let b = backend(&sock, &staging, Duration::from_secs(30)).await;
    let took = started.elapsed();
    assert!(took < Duration::from_millis(2_100), "{took:?}");
    assert!(took >= Duration::from_secs(2), "{took:?}");
    assert_eq!(b.languages(), ["en"]);
    assert_eq!(daemon.commands(), ["languages"]);
}

#[tokio::test]
async fn a_daemon_that_never_answers_times_out() {
    let dir = tempfile::tempdir().unwrap();
    let (sock, staging) = (dir.path().join("v.sock"), dir.path().join("stt"));
    let _daemon = FakeDaemon::silent(&sock);
    let timeout = Duration::from_millis(300);
    let b = backend(&sock, &staging, timeout).await;
    assert_eq!(b.languages(), ["en"]);
    let started = Instant::now();
    let err = b
        .transcribe(&wav_s16_mono(&[0; 16]), "en")
        .await
        .unwrap_err();
    assert_eq!(err, SpeechError::Timeout(timeout));
    assert!(started.elapsed() < timeout + Duration::from_millis(100));
    assert_eq!(staged_left(&staging), 0);
}

#[tokio::test]
async fn an_unlisted_language_is_refused_without_a_connection() {
    let dir = tempfile::tempdir().unwrap();
    let (sock, staging) = (dir.path().join("v.sock"), dir.path().join("stt"));
    let daemon = FakeDaemon::replying(&sock, "ok:hallo\n");
    let b = backend(&sock, &staging, TIMEOUT).await;
    let after_construction = daemon.connections();
    let err = b
        .transcribe(&wav_s16_mono(&[0; 16]), "de")
        .await
        .unwrap_err();
    assert_eq!(
        err,
        SpeechError::Unsupported {
            language: "de".to_string(),
            supported: vec!["en".to_string()],
        }
    );
    assert_eq!(daemon.connections(), after_construction);
}

#[tokio::test]
async fn languages_the_daemon_lists_are_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let (sock, staging) = (dir.path().join("v.sock"), dir.path().join("stt"));
    let _daemon = FakeDaemon::with_languages(&sock, "ok:hallo welt\n", " EN, de ");
    let b = backend(&sock, &staging, TIMEOUT).await;
    assert_eq!(b.languages(), ["en", "de"]);
    let t = b.transcribe(&wav_s16_mono(&[0; 16]), "de").await.unwrap();
    assert_eq!((t.text.as_str(), t.language.as_str()), ("hallo welt", "de"));
}

#[tokio::test]
async fn an_overlong_answer_is_a_daemon_error() {
    let dir = tempfile::tempdir().unwrap();
    let (sock, staging) = (dir.path().join("v.sock"), dir.path().join("stt"));
    let _daemon = FakeDaemon::start(&sock, |_| Some("x".repeat(70_000)));
    let b = backend(&sock, &staging, TIMEOUT).await;
    let err = b
        .transcribe(&wav_s16_mono(&[0; 16]), "en")
        .await
        .unwrap_err();
    assert_eq!(err, SpeechError::Daemon("answer too long".to_string()));
    assert_eq!(staged_left(&staging), 0);
}
